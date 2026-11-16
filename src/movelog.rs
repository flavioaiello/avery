use std::collections::HashSet;

use crate::changelog::Outcome;
use crate::config::Settings;
use crate::error::{ToolError, ToolResult};
use crate::journal::{Journal, Messages, View, COOL_DOWN, HOUR, TEN_MINUTES};
use crate::json::{Json, Object};
use crate::texts::{
    MAX_ATTEMPTS_PER_10_MIN, MAX_MOVES_PER_HOUR, MOVES_BLOCKED, MOVES_TOO_LARGE, MOVES_UNREADABLE,
};

pub(crate) const LOG_VERSION: i64 = 1;

static MESSAGES: Messages = Messages {
    blocked: MOVES_BLOCKED,
    unreadable: MOVES_UNREADABLE,
    too_large: MOVES_TOO_LARGE,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MoveEntry {
    pub(crate) ts: i64,
    pub(crate) id: String,
    pub(crate) outcome: Outcome,
    pub(crate) account_id: String,
    pub(crate) account: String,
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) count: usize,
    pub(crate) created: bool,
}

impl MoveEntry {
    pub(crate) fn to_json(&self) -> Json {
        let mut object = Object::new();
        object.insert("v", Json::Int(LOG_VERSION));
        object.insert("ts", Json::Int(self.ts));
        object.insert("id", Json::from(&self.id));
        object.insert("outcome", Json::from(self.outcome.as_str()));
        object.insert("account_id", Json::from(&self.account_id));
        object.insert("account", Json::from(&self.account));
        object.insert("from", Json::from(&self.from));
        object.insert("to", Json::from(&self.to));
        object.insert("count", Json::from(self.count));
        object.insert("created", Json::from(self.created));
        Json::Obj(object)
    }

    pub(crate) fn from_json(value: &Json) -> Option<MoveEntry> {
        let object = value.as_object()?;
        if object.get("v") != Some(&Json::Int(LOG_VERSION)) {
            return None;
        }
        let text = |key: &str| object.get(key).and_then(Json::as_str).map(str::to_string);
        let ts = match object.get("ts") {
            Some(Json::Int(ts)) => *ts,
            _ => return None,
        };
        let count = match object.get("count") {
            Some(Json::Int(count)) => usize::try_from(*count).ok()?,
            _ => return None,
        };
        let created = match object.get("created") {
            Some(Json::Bool(created)) => *created,
            _ => return None,
        };
        Some(MoveEntry {
            ts,
            id: text("id").filter(|id| !id.is_empty())?,
            outcome: Outcome::parse(object.get("outcome")?.as_str()?)?,
            account_id: text("account_id")?,
            account: text("account")?,
            from: text("from")?,
            to: text("to")?,
            count,
            created,
        })
    }
}

fn journal(settings: &Settings) -> Journal {
    Journal::new(settings.moves_path(), &MESSAGES)
}

pub(crate) fn read_window(settings: &Settings, now: i64) -> ToolResult<View<MoveEntry>> {
    journal(settings).window(now, MoveEntry::from_json, |entry| entry.ts)
}

pub(crate) fn ensure_writable(settings: &Settings) -> ToolResult<()> {
    journal(settings).ensure_writable()
}

pub(crate) fn append(settings: &Settings, entry: &MoveEntry, strict: bool) -> ToolResult<()> {
    journal(settings).append(&entry.to_json(), strict)
}

fn attempts(entries: &[MoveEntry]) -> Vec<&MoveEntry> {
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

pub(crate) fn check_limits(entries: &[MoveEntry], now: i64) -> ToolResult<()> {
    let attempts = attempts(entries);
    let within = |entry: &MoveEntry, window: i64| now.saturating_sub(entry.ts) < window;
    let counted = attempts
        .iter()
        .filter(|entry| matches!(entry.outcome, Outcome::Done | Outcome::Pending))
        .filter(|entry| within(entry, HOUR))
        .count();
    if counted >= MAX_MOVES_PER_HOUR {
        return Err(ToolError::msg(format!(
            "Move limit reached ({MAX_MOVES_PER_HOUR} per hour). NOTHING was moved. Tell the user."
        )));
    }
    let recent = attempts
        .iter()
        .filter(|entry| within(entry, TEN_MINUTES))
        .count();
    if recent >= MAX_ATTEMPTS_PER_10_MIN {
        return Err(ToolError::msg(
            "Too many move requests in the last minutes. NOTHING was moved. Ask the user before you try again.",
        ));
    }
    if let Some(last) = attempts.last() {
        if matches!(last.outcome, Outcome::Declined | Outcome::Timeout) && within(last, COOL_DOWN) {
            return Err(ToolError::msg(
                "The user has just declined or ignored a move request. NOTHING was moved. Do not retry unless the user asks you to.",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;
    use crate::testutil::{settings_at, TempDir, TestResult};
    use std::fs;

    const NOW: i64 = 1_700_000_000;

    fn entry(id: &str, outcome: Outcome, ago: i64) -> MoveEntry {
        MoveEntry {
            ts: NOW - ago,
            id: id.to_string(),
            outcome,
            account_id: "ACC".to_string(),
            account: "Demo Gmail".to_string(),
            from: "INBOX".to_string(),
            to: "Werbung".to_string(),
            count: 3,
            created: false,
        }
    }

    fn refused(entries: &[MoveEntry], needle: &str) -> bool {
        matches!(check_limits(entries, NOW), Err(ToolError::Msg(message)) if message.contains(needle))
    }

    #[test]
    fn an_entry_survives_the_round_trip_and_unknown_shapes_are_rejected() -> TestResult {
        let original = entry("a1b2c3d4e5f6", Outcome::Done, 5);
        let text = json::dumps(&original.to_json(), false);
        assert_eq!(MoveEntry::from_json(&json::parse(&text)?), Some(original));
        for broken in [
            r#"{"v": 2, "ts": 1, "id": "x", "outcome": "done", "account_id": "A", "account": "A", "from": "", "to": "T", "count": 1, "created": false}"#,
            r#"{"v": 1, "ts": "1", "id": "x", "outcome": "done", "account_id": "A", "account": "A", "from": "", "to": "T", "count": 1, "created": false}"#,
            r#"{"v": 1, "ts": 1, "id": "", "outcome": "done", "account_id": "A", "account": "A", "from": "", "to": "T", "count": 1, "created": false}"#,
            r#"{"v": 1, "ts": 1, "id": "x", "outcome": "moved", "account_id": "A", "account": "A", "from": "", "to": "T", "count": 1, "created": false}"#,
            r#"{"v": 1, "ts": 1, "id": "x", "outcome": "done", "account_id": "A", "account": "A", "from": "", "to": "T", "count": -1, "created": false}"#,
            r#"{"v": 1, "ts": 1, "id": "x", "outcome": "done", "account_id": "A", "account": "A", "from": "", "to": "T", "count": 1, "created": "no"}"#,
            r#"{"v": 1, "ts": 1, "id": "x", "outcome": "done", "account_id": "A", "account": "A", "from": "", "count": 1, "created": false}"#,
            r#"[1]"#,
        ] {
            assert_eq!(
                MoveEntry::from_json(&json::parse(broken)?),
                None,
                "{broken}"
            );
        }
        Ok(())
    }

    #[test]
    fn a_pending_entry_with_a_final_one_counts_once() {
        let entries = [
            entry("a", Outcome::Pending, 30),
            entry("a", Outcome::Done, 29),
            entry("b", Outcome::Pending, 20),
        ];
        let seen: Vec<(&str, Outcome)> = attempts(&entries)
            .iter()
            .map(|e| (e.id.as_str(), e.outcome))
            .collect();
        assert_eq!(seen, [("a", Outcome::Done), ("b", Outcome::Pending)]);
    }

    #[test]
    fn six_attempts_in_ten_minutes_block_the_seventh() {
        let five: Vec<MoveEntry> = (0..5)
            .map(|n| entry(&format!("e{n}"), Outcome::Error, 100 + n))
            .collect();
        assert!(check_limits(&five, NOW).is_ok());
        let mut six = five.clone();
        six.push(entry("e5", Outcome::Error, 100));
        assert!(refused(&six, "Too many move requests"));
        let old: Vec<MoveEntry> = (0..6)
            .map(|n| entry(&format!("e{n}"), Outcome::Error, 700 + n))
            .collect();
        assert!(check_limits(&old, NOW).is_ok());
    }

    #[test]
    fn a_decline_or_a_timeout_pauses_the_model_for_twenty_seconds() {
        for outcome in [Outcome::Declined, Outcome::Timeout] {
            assert!(refused(
                &[entry("a", outcome, 5)],
                "just declined or ignored"
            ));
            assert!(check_limits(&[entry("a", outcome, 25)], NOW).is_ok());
        }
        assert!(check_limits(&[entry("a", Outcome::Error, 5)], NOW).is_ok());
        let later = [
            entry("a", Outcome::Declined, 40),
            entry("b", Outcome::Done, 5),
        ];
        assert!(check_limits(&later, NOW).is_ok());
    }

    #[test]
    fn the_hourly_cap_counts_done_and_pending_moves_only() {
        let spread = |outcome: Outcome, count: usize| -> Vec<MoveEntry> {
            (0..count)
                .map(|n| {
                    let ago = 700 + i64::try_from(n).unwrap_or(0) * 100;
                    entry(&format!("m{n}"), outcome, ago)
                })
                .collect()
        };
        assert!(check_limits(&spread(Outcome::Done, MAX_MOVES_PER_HOUR - 1), NOW).is_ok());
        assert!(refused(
            &spread(Outcome::Done, MAX_MOVES_PER_HOUR),
            "Move limit reached"
        ));
        assert!(refused(
            &spread(Outcome::Pending, MAX_MOVES_PER_HOUR),
            "Move limit reached"
        ));
        assert!(check_limits(&spread(Outcome::Error, MAX_MOVES_PER_HOUR), NOW).is_ok());
        assert!(check_limits(&spread(Outcome::Declined, MAX_MOVES_PER_HOUR), NOW).is_ok());
    }

    #[test]
    fn the_log_is_private_versioned_and_blocks_when_it_cannot_be_trusted() -> TestResult {
        let dir = TempDir::new()?;
        let settings = settings_at(&dir.path("mail/config.json"));
        ensure_writable(&settings)?;
        append(&settings, &entry("a", Outcome::Pending, 5), true)?;
        let view = read_window(&settings, NOW)?;
        assert_eq!(view.entries.len(), 1);
        assert!(fs::read_to_string(settings.moves_path())?.starts_with("{\"v\": 1,"));
        fs::write(settings.moves_path(), "garbage\n")?;
        assert_eq!(
            read_window(&settings, NOW),
            Err(ToolError::msg(MOVES_UNREADABLE))
        );
        Ok(())
    }
}
