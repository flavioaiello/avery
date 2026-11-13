use std::collections::HashSet;

use crate::calendar::{Action, EventFields};
use crate::config::Settings;
use crate::error::{ToolError, ToolResult};
use crate::journal::{Journal, Messages, View, COOL_DOWN, HOUR, TEN_MINUTES};
use crate::json::{Json, Object};
use crate::texts::{
    CHANGES_BLOCKED, CHANGES_TOO_LARGE, CHANGES_UNREADABLE, MAX_ATTEMPTS_PER_10_MIN,
};

pub(crate) const LOG_VERSION: i64 = 1;

static MESSAGES: Messages = Messages {
    blocked: CHANGES_BLOCKED,
    unreadable: CHANGES_UNREADABLE,
    too_large: CHANGES_TOO_LARGE,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Pending,
    Done,
    Declined,
    Timeout,
    Error,
}

impl Outcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Outcome::Pending => "pending",
            Outcome::Done => "done",
            Outcome::Declined => "declined",
            Outcome::Timeout => "timeout",
            Outcome::Error => "error",
        }
    }

    fn parse(text: &str) -> Option<Outcome> {
        match text {
            "pending" => Some(Outcome::Pending),
            "done" => Some(Outcome::Done),
            "declined" => Some(Outcome::Declined),
            "timeout" => Some(Outcome::Timeout),
            "error" => Some(Outcome::Error),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChangeEntry {
    pub(crate) ts: i64,
    pub(crate) id: String,
    pub(crate) action: Action,
    pub(crate) outcome: Outcome,
    pub(crate) calendar_id: String,
    pub(crate) calendar: String,
    pub(crate) uid: Option<String>,
    pub(crate) before: Option<EventFields>,
    pub(crate) after: Option<EventFields>,
    pub(crate) undoes: Option<String>,
}

fn optional<T>(value: Option<&Json>, parse: fn(&Json) -> Option<T>) -> Option<Option<T>> {
    match value {
        None | Some(Json::Null) => Some(None),
        Some(other) => parse(other).map(Some),
    }
}

fn optional_text(value: Option<&Json>) -> Option<Option<String>> {
    match value {
        None | Some(Json::Null) => Some(None),
        Some(Json::Str(text)) => Some(Some(text.clone())),
        Some(_) => None,
    }
}

impl ChangeEntry {
    pub(crate) fn to_json(&self) -> Json {
        let mut object = Object::new();
        object.insert("v", Json::Int(LOG_VERSION));
        object.insert("ts", Json::Int(self.ts));
        object.insert("id", Json::from(&self.id));
        object.insert("action", Json::from(self.action.as_str()));
        object.insert("outcome", Json::from(self.outcome.as_str()));
        object.insert("calendar_id", Json::from(&self.calendar_id));
        object.insert("calendar", Json::from(&self.calendar));
        object.insert("uid", self.uid.as_ref().map_or(Json::Null, Json::from));
        object.insert(
            "before",
            self.before
                .as_ref()
                .map_or(Json::Null, EventFields::to_json),
        );
        object.insert(
            "after",
            self.after.as_ref().map_or(Json::Null, EventFields::to_json),
        );
        object.insert(
            "undoes",
            self.undoes.as_ref().map_or(Json::Null, Json::from),
        );
        Json::Obj(object)
    }

    pub(crate) fn from_json(value: &Json) -> Option<ChangeEntry> {
        let object = value.as_object()?;
        if object.get("v") != Some(&Json::Int(LOG_VERSION)) {
            return None;
        }
        let text = |key: &str| object.get(key).and_then(Json::as_str).map(str::to_string);
        let id = text("id").filter(|id| !id.is_empty())?;
        let ts = match object.get("ts") {
            Some(Json::Int(ts)) => *ts,
            _ => return None,
        };
        Some(ChangeEntry {
            ts,
            id,
            action: Action::parse(object.get("action")?.as_str()?)?,
            outcome: Outcome::parse(object.get("outcome")?.as_str()?)?,
            calendar_id: text("calendar_id")?,
            calendar: text("calendar")?,
            uid: optional_text(object.get("uid"))?,
            before: optional(object.get("before"), EventFields::from_json)?,
            after: optional(object.get("after"), EventFields::from_json)?,
            undoes: optional_text(object.get("undoes"))?,
        })
    }
}

pub(crate) type ChangeView = View<ChangeEntry>;

fn journal(settings: &Settings) -> Journal {
    Journal::new(settings.changes_path(), &MESSAGES)
}

pub(crate) fn read_entries(settings: &Settings, limit: usize) -> ToolResult<ChangeView> {
    journal(settings).recent(limit, ChangeEntry::from_json)
}

pub(crate) fn read_window(settings: &Settings, now: i64) -> ToolResult<ChangeView> {
    journal(settings).window(now, ChangeEntry::from_json, |entry| entry.ts)
}

pub(crate) fn ensure_writable(settings: &Settings) -> ToolResult<()> {
    journal(settings).ensure_writable()
}

pub(crate) fn append(settings: &Settings, entry: &ChangeEntry, strict: bool) -> ToolResult<()> {
    journal(settings).append(&entry.to_json(), strict)
}

pub(crate) fn attempts(entries: &[ChangeEntry]) -> Vec<&ChangeEntry> {
    let finals: HashSet<&str> = entries
        .iter()
        .filter(|entry| entry.outcome != Outcome::Pending)
        .map(|entry| entry.id.as_str())
        .collect();
    entries
        .iter()
        .filter(|entry| !(entry.outcome == Outcome::Pending && finals.contains(entry.id.as_str())))
        .collect()
}

pub(crate) fn check_limits(entries: &[ChangeEntry], now: i64, max_per_hour: i64) -> ToolResult<()> {
    let by_the_model: Vec<ChangeEntry> = entries
        .iter()
        .filter(|entry| entry.undoes.is_none())
        .cloned()
        .collect();
    let attempts = attempts(&by_the_model);
    let within = |entry: &ChangeEntry, window: i64| now.saturating_sub(entry.ts) < window;
    let counted = attempts
        .iter()
        .filter(|entry| matches!(entry.outcome, Outcome::Done | Outcome::Pending))
        .filter(|entry| within(entry, HOUR))
        .count();
    if i64::try_from(counted).unwrap_or(i64::MAX) >= max_per_hour {
        return Err(ToolError::msg(format!(
            "Change limit reached ({max_per_hour} per hour). NOTHING was changed. Tell the user."
        )));
    }
    let recent = attempts
        .iter()
        .filter(|entry| within(entry, TEN_MINUTES))
        .count();
    if recent >= MAX_ATTEMPTS_PER_10_MIN {
        return Err(ToolError::msg(
            "Too many change requests in the last minutes. NOTHING was changed. Ask the user before you try again.",
        ));
    }
    if let Some(last) = attempts.last() {
        if matches!(last.outcome, Outcome::Declined | Outcome::Timeout) && within(last, COOL_DOWN) {
            return Err(ToolError::msg(
                "The user has just declined or ignored a change request. NOTHING was changed. Do not retry unless the user asks you to.",
            ));
        }
    }
    Ok(())
}

pub(crate) fn undoable(entries: &[ChangeEntry]) -> Vec<&ChangeEntry> {
    let undone: HashSet<&str> = entries
        .iter()
        .filter(|entry| entry.outcome == Outcome::Done)
        .filter_map(|entry| entry.undoes.as_deref())
        .collect();
    attempts(entries)
        .into_iter()
        .filter(|entry| entry.outcome == Outcome::Done && entry.undoes.is_none())
        .filter(|entry| !undone.contains(entry.id.as_str()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::civil::DateTime;
    use crate::testutil::{j, settings_at, TempDir, TestResult};
    use std::fs;

    pub(crate) fn fields(summary: &str) -> Result<EventFields, &'static str> {
        Ok(EventFields {
            summary: summary.to_string(),
            start: DateTime::parse("2026-11-20 09:00").ok_or("start")?,
            end: DateTime::parse("2026-11-20 10:00").ok_or("end")?,
            all_day: false,
            location: "Ort".to_string(),
            notes: "Zeile 1\nZeile 2".to_string(),
            url: String::new(),
        })
    }

    fn entry(ts: i64, id: &str, outcome: Outcome) -> ChangeEntry {
        ChangeEntry {
            ts,
            id: id.to_string(),
            action: Action::Update,
            outcome,
            calendar_id: "CAL".to_string(),
            calendar: "Privat".to_string(),
            uid: Some("U".to_string()),
            before: None,
            after: None,
            undoes: None,
        }
    }

    #[test]
    fn entries_round_trip_with_snapshots_and_carry_the_format_version() -> TestResult {
        let original = ChangeEntry {
            before: Some(fields("Alt")?),
            after: Some(fields("Neu")?),
            undoes: Some("abc".to_string()),
            ..entry(1_700_000_000, "id1", Outcome::Done)
        };
        let value = original.to_json();
        assert_eq!(
            value.as_object().and_then(|o| o.get("v")),
            Some(&Json::Int(1))
        );
        assert_eq!(ChangeEntry::from_json(&value), Some(original));
        let bare = entry(5, "id2", Outcome::Pending);
        assert_eq!(ChangeEntry::from_json(&bare.to_json()), Some(bare));
        Ok(())
    }

    #[test]
    fn unknown_versions_actions_and_shapes_are_not_read() -> TestResult {
        let good = r#"{"v": 1, "ts": 5, "id": "a", "action": "create", "outcome": "done", "calendar_id": "C", "calendar": "K", "uid": null, "before": null, "after": null, "undoes": null}"#;
        assert!(ChangeEntry::from_json(&j(good)?).is_some());
        for bad in [
            good.replace(r#""v": 1"#, r#""v": 2"#),
            good.replace(r#""v": 1, "#, ""),
            good.replace(r#""create""#, r#""wipe""#),
            good.replace(r#""done""#, r#""vanished""#),
            good.replace(r#""ts": 5"#, r#""ts": "5""#),
            good.replace(r#""id": "a""#, r#""id": """#),
            good.replace(r#""uid": null"#, r#""uid": 5"#),
            good.replace(r#""before": null"#, r#""before": {"summary": 1}"#),
            good.replace(r#""undoes": null"#, r#""undoes": []"#),
            good.replace(r#""calendar": "K", "#, ""),
        ] {
            assert_eq!(ChangeEntry::from_json(&j(&bad)?), None, "{bad}");
        }
        assert_eq!(ChangeEntry::from_json(&j("[]")?), None);
        Ok(())
    }

    #[test]
    fn the_log_is_private_append_only_and_fails_closed() -> TestResult {
        let dir = TempDir::new()?;
        let settings = settings_at(&dir.path("mail/config.json"));
        ensure_writable(&settings)?;
        append(&settings, &entry(10, "a", Outcome::Pending), true)?;
        append(&settings, &entry(11, "a", Outcome::Done), true)?;
        assert_eq!(read_entries(&settings, 10)?.entries.len(), 2);
        assert_eq!(read_window(&settings, 12)?.entries.len(), 2);
        fs::write(settings.changes_path(), "garbage\n")?;
        assert_eq!(
            read_window(&settings, 12),
            Err(ToolError::msg(CHANGES_UNREADABLE))
        );
        let blocked = settings_at(&dir.path("blocked/config.json"));
        fs::create_dir_all(blocked.changes_path())?;
        assert_eq!(
            ensure_writable(&blocked),
            Err(ToolError::msg(CHANGES_BLOCKED))
        );
        assert_eq!(
            append(&blocked, &entry(1, "a", Outcome::Pending), true),
            Err(ToolError::msg(CHANGES_BLOCKED))
        );
        Ok(())
    }

    #[test]
    fn a_final_entry_replaces_the_pending_one_with_the_same_id() {
        let entries = [
            entry(1, "a", Outcome::Pending),
            entry(2, "a", Outcome::Done),
            entry(3, "b", Outcome::Pending),
        ];
        let kept: Vec<(i64, Outcome)> = attempts(&entries)
            .iter()
            .map(|e| (e.ts, e.outcome))
            .collect();
        assert_eq!(kept, [(2, Outcome::Done), (3, Outcome::Pending)]);
    }

    fn many(count: i64, age: i64, outcome: Outcome, now: i64) -> Vec<ChangeEntry> {
        (0..count)
            .map(|index| entry(now - age - index, &format!("id{index}"), outcome))
            .collect()
    }

    #[test]
    fn the_hourly_limit_counts_done_and_unclear_changes_but_not_declined_ones() {
        let now = 1_700_000_000;
        assert_eq!(
            check_limits(&many(9, 1000, Outcome::Done, now), now, 10),
            Ok(())
        );
        assert!(matches!(
            check_limits(&many(10, 1000, Outcome::Done, now), now, 10),
            Err(ToolError::Msg(m)) if m.starts_with("Change limit reached (10 per hour)")
        ));
        assert!(check_limits(&many(10, 1000, Outcome::Pending, now), now, 10).is_err());
        assert_eq!(
            check_limits(&many(10, 4000, Outcome::Done, now), now, 10),
            Ok(())
        );
        assert_eq!(
            check_limits(&many(5, 1000, Outcome::Declined, now), now, 5),
            Ok(())
        );
        assert_eq!(
            check_limits(&many(20, 1000, Outcome::Error, now), now, 5),
            Ok(())
        );
    }

    #[test]
    fn six_attempts_in_ten_minutes_block_and_a_fresh_decline_cools_down() {
        let now = 1_700_000_000;
        let declines = many(6, 100, Outcome::Declined, now);
        assert!(matches!(
            check_limits(&declines, now, 10),
            Err(ToolError::Msg(m)) if m.starts_with("Too many change requests")
        ));
        assert_eq!(check_limits(&declines[..5], now, 10), Ok(()));
        let spread = many(6, 700, Outcome::Declined, now);
        assert_eq!(check_limits(&spread, now, 10), Ok(()));
        for outcome in [Outcome::Declined, Outcome::Timeout] {
            let fresh = [entry(now - 5, "x", outcome)];
            assert!(matches!(
                check_limits(&fresh, now, 10),
                Err(ToolError::Msg(m)) if m.starts_with("The user has just declined")
            ));
            let later = [entry(now - 25, "x", outcome)];
            assert_eq!(check_limits(&later, now, 10), Ok(()));
        }
        let error = [entry(now - 5, "x", Outcome::Error)];
        assert_eq!(check_limits(&error, now, 10), Ok(()));
    }

    #[test]
    fn undo_entries_do_not_use_up_the_budget_of_the_model() {
        let now = 1_700_000_000;
        let mut entries = many(9, 1000, Outcome::Done, now);
        for index in 0..5 {
            let mut undo = entry(now - 100 - index, &format!("u{index}"), Outcome::Done);
            undo.undoes = Some(format!("id{index}"));
            entries.push(undo);
        }
        assert_eq!(check_limits(&entries, now, 10), Ok(()));
    }

    #[test]
    fn only_finished_changes_that_were_not_undone_can_be_undone() {
        let mut undo = entry(5, "u1", Outcome::Done);
        undo.undoes = Some("b".to_string());
        let mut failed_undo = entry(6, "u2", Outcome::Error);
        failed_undo.undoes = Some("c".to_string());
        let entries = [
            entry(1, "a", Outcome::Done),
            entry(2, "b", Outcome::Done),
            entry(3, "c", Outcome::Done),
            entry(4, "d", Outcome::Declined),
            entry(4, "e", Outcome::Pending),
            undo,
            failed_undo,
        ];
        let ids: Vec<&str> = undoable(&entries).iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["a", "c"]);
    }
}
