use crate::calendar::{
    last_day, Approval, Calendar, Change, ChangeResult, Decision, EventFields, MAX_LOCATION,
    MAX_NOTES, MAX_SUMMARY,
};
use crate::changelog::{self, ChangeEntry, Outcome};
use crate::civil::{Date, DateTime};
use crate::context::Context;
use crate::error::{ToolError, ToolResult};
use crate::journal;
use crate::json::{Json, Object};
use crate::text::{char_len, clean, clean_str, one_line, one_line_str, to_bool};
use crate::texts::{APPROVAL_SECONDS, UNTRUSTED};

const CREATE_ARGS: [&str; 8] = [
    "calendar",
    "summary",
    "start",
    "end",
    "duration_minutes",
    "all_day",
    "location",
    "notes",
];
const UPDATE_ARGS: [&str; 8] = [
    "id",
    "calendar",
    "summary",
    "start",
    "end",
    "duration_minutes",
    "location",
    "notes",
];
const DELETE_ARGS: [&str; 2] = ["id", "calendar"];
const FIRST_YEAR: i64 = 1970;
const LAST_YEAR: i64 = 2100;
const DEFAULT_MINUTES: i64 = 60;
const MAX_MINUTES: i64 = 1440;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    Model { max_per_hour: i64 },
    Human,
}

fn guard(ctx: &Context, origin: Origin) -> ToolResult<()> {
    let now = journal::now_seconds();
    let history = changelog::read_window(&ctx.settings, now)?;
    if let Origin::Model { max_per_hour } = origin {
        changelog::check_limits(&history.entries, now, max_per_hour)?;
    }
    changelog::ensure_writable(&ctx.settings)
}

struct Attempt<'a> {
    ctx: &'a Context,
    calendar: &'a Calendar,
    change: &'a Change,
    id: &'a str,
    undoes: Option<&'a str>,
}

impl Attempt<'_> {
    fn log(&self, outcome: Outcome, result: Option<&ChangeResult>, strict: bool) -> ToolResult<()> {
        let finished = outcome == Outcome::Done;
        let entry = ChangeEntry {
            ts: journal::now_seconds(),
            id: self.id.to_string(),
            action: self.change.action(),
            outcome,
            calendar_id: self.calendar.id.clone(),
            calendar: one_line_str(&self.calendar.label, 100),
            uid: result
                .and_then(|found| found.uid.clone())
                .or_else(|| self.change.uid().map(str::to_string)),
            before: self.change.before().cloned(),
            after: if finished {
                result
                    .and_then(|found| found.after.clone())
                    .or_else(|| self.change.after().cloned())
            } else {
                self.change.after().cloned()
            },
            undoes: self.undoes.map(str::to_string),
        };
        changelog::append(&self.ctx.settings, &entry, strict)
    }
}

pub(crate) fn perform(
    ctx: &Context,
    calendar: &Calendar,
    change: &Change,
    origin: Origin,
    undoes: Option<&str>,
) -> ToolResult<ChangeResult> {
    guard(ctx, origin)?;
    let id = journal::attempt_id()?;
    let attempt = Attempt {
        ctx,
        calendar,
        change,
        id: &id,
        undoes,
    };
    attempt.log(Outcome::Pending, None, true)?;
    let approval = Approval {
        seconds: APPROVAL_SECONDS,
        undo: undoes.is_some(),
    };
    match ctx.calendar.change(calendar, change, &approval) {
        Ok(result) => {
            let outcome = match result.decision {
                Decision::Applied => Outcome::Done,
                Decision::Declined => Outcome::Declined,
                Decision::TimedOut => Outcome::Timeout,
            };
            attempt.log(outcome, Some(&result), false)?;
            Ok(result)
        }
        Err(ToolError::Msg(message)) => {
            attempt.log(Outcome::Error, None, false)?;
            Err(ToolError::Msg(message))
        }
        Err(other) => Err(other),
    }
}

fn unknown_parameters(args: &Object, allowed: &[&str]) -> Option<ToolError> {
    let unknown: Vec<&str> = args.keys().filter(|key| !allowed.contains(key)).collect();
    if unknown.is_empty() {
        return None;
    }
    let lowered: Vec<String> = unknown.iter().map(|key| key.to_lowercase()).collect();
    let mentions = |words: &[&str]| lowered.iter().any(|key| words.contains(&key.as_str()));
    let hint = if mentions(&[
        "attendees",
        "attendee",
        "guests",
        "invitees",
        "invite",
        "participants",
    ]) {
        " Events with attendees are not supported."
    } else if mentions(&["recurrence", "repeat", "rrule", "recurring"]) {
        " Recurring events are not supported."
    } else if mentions(&["alarm", "alarms", "reminder", "alert", "url", "link"]) {
        " Alerts and links are not supported."
    } else if mentions(&["all_day", "allday"]) {
        " The all-day setting of an existing event cannot be changed."
    } else {
        ""
    };
    let names: Vec<String> = unknown.iter().map(|key| clean_str(key, Some(30))).collect();
    Some(ToolError::msg(format!(
        "Unknown parameter(s): {}.{hint}",
        names.join(", ")
    )))
}

fn text_argument(
    args: &Object,
    key: &str,
    limit: usize,
    single_line: bool,
) -> ToolResult<Option<String>> {
    let text = match args.get(key) {
        None | Some(Json::Null) => return Ok(None),
        Some(Json::Str(_)) => {
            let value = args.get(key);
            if single_line {
                one_line(value, limit + 1)
            } else {
                clean(value, None)
            }
        }
        Some(_) => return Err(ToolError::msg(format!("Parameter '{key}' must be text."))),
    };
    if char_len(&text) > limit {
        return Err(ToolError::msg(format!(
            "Parameter '{key}' is too long (max {limit} characters)."
        )));
    }
    Ok(Some(text))
}

fn present<'a>(args: &'a Object, key: &str) -> Option<&'a Json> {
    args.get(key).filter(|value| **value != Json::Null)
}

fn moment(value: &Json, key: &str, all_day: bool) -> ToolResult<DateTime> {
    let invalid = || {
        if all_day {
            ToolError::msg(format!(
                "Parameter '{key}' must be a date like 2026-11-20 for an all-day event."
            ))
        } else {
            ToolError::msg(format!(
                "Parameter '{key}' must look like 2026-11-20 09:30 (local time)."
            ))
        }
    };
    let text = value.as_str().ok_or_else(invalid)?.trim();
    let parsed = if all_day {
        Date::parse(text).map(DateTime::midnight)
    } else {
        DateTime::parse(text)
    };
    match parsed {
        Some(found) if (FIRST_YEAR..=LAST_YEAR).contains(&found.date().year()) => Ok(found),
        _ => Err(invalid()),
    }
}

fn minutes_argument(args: &Object) -> ToolResult<Option<i64>> {
    match present(args, "duration_minutes") {
        None => Ok(None),
        Some(Json::Int(minutes)) if (1..=MAX_MINUTES).contains(minutes) => Ok(Some(*minutes)),
        Some(_) => Err(ToolError::msg(format!(
            "Parameter 'duration_minutes' must be a whole number from 1 to {MAX_MINUTES}."
        ))),
    }
}

fn shifted(start: DateTime, minutes: i64) -> ToolResult<DateTime> {
    start
        .add_minutes(minutes)
        .filter(|end| end.date().year() <= LAST_YEAR)
        .ok_or_else(|| ToolError::msg("The event would reach beyond the year 2100."))
}

fn next_day(date: Date) -> ToolResult<DateTime> {
    date.add_days(1)
        .filter(|next| next.year() <= LAST_YEAR)
        .map(DateTime::midnight)
        .ok_or_else(|| ToolError::msg("The event would reach beyond the year 2100."))
}

fn new_fields(args: &Object) -> ToolResult<EventFields> {
    let summary = text_argument(args, "summary", MAX_SUMMARY, true)?
        .filter(|text| !text.is_empty())
        .ok_or_else(|| ToolError::msg("Parameter 'summary' is required."))?;
    let all_day = to_bool(args.get("all_day"));
    let start_value =
        present(args, "start").ok_or_else(|| ToolError::msg("Parameter 'start' is required."))?;
    let start = moment(start_value, "start", all_day)?;
    let minutes = minutes_argument(args)?;
    let end_value = present(args, "end");
    let end = if all_day {
        if minutes.is_some() {
            return Err(ToolError::msg(
                "Parameter 'duration_minutes' is not used for all-day events; pass 'end' (the last day) instead.",
            ));
        }
        let last = match end_value {
            Some(value) => moment(value, "end", true)?,
            None => start,
        };
        if last < start {
            return Err(ToolError::msg(
                "Parameter 'end' must not be before 'start'.",
            ));
        }
        next_day(last.date())?
    } else {
        match (end_value, minutes) {
            (Some(_), Some(_)) => {
                return Err(ToolError::msg(
                    "Pass either 'end' or 'duration_minutes', not both.",
                ))
            }
            (Some(value), None) => moment(value, "end", false)?,
            (None, Some(minutes)) => shifted(start, minutes)?,
            (None, None) => shifted(start, DEFAULT_MINUTES)?,
        }
    };
    if !all_day && end <= start {
        return Err(ToolError::msg("Parameter 'end' must be after 'start'."));
    }
    Ok(EventFields {
        summary,
        start,
        end,
        all_day,
        location: text_argument(args, "location", MAX_LOCATION, true)?.unwrap_or_default(),
        notes: text_argument(args, "notes", MAX_NOTES, false)?.unwrap_or_default(),
        url: String::new(),
    })
}

fn updated_fields(before: &EventFields, args: &Object) -> ToolResult<EventFields> {
    let mut after = before.clone();
    if let Some(summary) = text_argument(args, "summary", MAX_SUMMARY, true)? {
        if summary.is_empty() {
            return Err(ToolError::msg("Parameter 'summary' must not be empty."));
        }
        after.summary = summary;
    }
    if let Some(location) = text_argument(args, "location", MAX_LOCATION, true)? {
        after.location = location;
    }
    if let Some(notes) = text_argument(args, "notes", MAX_NOTES, false)? {
        after.notes = notes;
    }
    let start_value = present(args, "start");
    let end_value = present(args, "end");
    let minutes = minutes_argument(args)?;
    if before.all_day {
        if minutes.is_some() {
            return Err(ToolError::msg(
                "Parameter 'duration_minutes' is not used for all-day events; pass 'end' (the last day) instead.",
            ));
        }
        let old_first = before.start.date();
        let old_last = last_day(before.start, before.end);
        let first = match start_value {
            Some(value) => moment(value, "start", true)?.date(),
            None => old_first,
        };
        let last = match end_value {
            Some(value) => moment(value, "end", true)?.date(),
            None if start_value.is_some() => first
                .add_days(old_last.days() - old_first.days())
                .ok_or_else(|| ToolError::msg("The event would reach beyond the year 2100."))?,
            None => old_last,
        };
        if last < first {
            return Err(ToolError::msg(
                "Parameter 'end' must not be before 'start'.",
            ));
        }
        after.start = DateTime::midnight(first);
        after.end = if start_value.is_some() || end_value.is_some() {
            next_day(last)?
        } else {
            before.end
        };
        return Ok(after);
    }
    let duration = before.end.total_minutes() - before.start.total_minutes();
    let start = match start_value {
        Some(value) => moment(value, "start", false)?,
        None => before.start,
    };
    let end = match (end_value, minutes) {
        (Some(_), Some(_)) => {
            return Err(ToolError::msg(
                "Pass either 'end' or 'duration_minutes', not both.",
            ))
        }
        (Some(value), None) => moment(value, "end", false)?,
        (None, Some(minutes)) => shifted(start, minutes)?,
        (None, None) if start_value.is_some() => shifted(start, duration)?,
        (None, None) => before.end,
    };
    if end < start || (end == start && duration != 0) {
        return Err(ToolError::msg("Parameter 'end' must be after 'start'."));
    }
    after.start = start;
    after.end = end;
    Ok(after)
}

fn existing(ctx: &Context, calendar: &Calendar, uid: &str) -> ToolResult<EventFields> {
    let detail = ctx.calendar.event(calendar, uid)?;
    if !detail.event.recurrence.is_empty() {
        return Err(ToolError::msg(
            "This is a recurring event; only single events can be changed or deleted here. NOTHING was changed. The user has to do it in Calendar.",
        ));
    }
    if detail.event.attendees != Some(0) {
        return Err(ToolError::msg(
            "This event has attendees; changing or deleting it would notify other people, so NOTHING was changed. The user has to do it in Calendar.",
        ));
    }
    let fields = EventFields::of(&detail);
    if !fields.fits_the_backup() {
        return Err(ToolError::msg(
            "The title, place, notes or link of this event are too long to be backed up for undo, so NOTHING was changed. The user has to do it in Calendar.",
        ));
    }
    Ok(fields)
}

fn id_argument(args: &Object) -> ToolResult<String> {
    match args.get("id") {
        Some(Json::Str(text))
            if !text.trim().is_empty() && text.trim() == clean_str(text.trim(), None) =>
        {
            Ok(text.trim().to_string())
        }
        _ => Err(ToolError::msg(
            "Parameter 'id' is required (text from list_events).",
        )),
    }
}

fn when(fields: &EventFields) -> String {
    let first = fields.start.date();
    if fields.all_day {
        let last = last_day(fields.start, fields.end);
        return if last > first {
            format!("{first} to {last} (all day)")
        } else {
            format!("{first} (all day)")
        };
    }
    if fields.start.date() == fields.end.date() {
        format!("{first} {}-{}", fields.start.clock(), fields.end.clock())
    } else {
        format!("{} - {}", fields.start, fields.end)
    }
}

fn describe(label: &str, uid: &str, fields: &EventFields) -> Vec<String> {
    let mut lines = vec![
        format!(
            "Calendar: {} | id: {}",
            one_line_str(label, 80),
            one_line_str(uid, 200)
        ),
        format!("Title: {}", one_line_str(&fields.summary, 200)),
        format!("When: {} (local time)", when(fields)),
    ];
    if !fields.location.is_empty() {
        lines.push(format!("Where: {}", one_line_str(&fields.location, 200)));
    }
    lines
}

fn report(performed: &ChangeResult, change: &Change, calendar: &Calendar) -> String {
    match performed.decision {
        Decision::Declined => return "The user did NOT approve the change in the dialog. NOTHING was changed. Do not retry unless the user asks you to.".to_string(),
        Decision::TimedOut => return "The user did not answer the approval dialog in time. NOTHING was changed. Ask the user whether to try again.".to_string(),
        Decision::Applied => {}
    }
    let uid = performed
        .uid
        .as_deref()
        .or_else(|| change.uid())
        .unwrap_or("-");
    let (headline, shown, wanted) = match change {
        Change::Create { after } => (
            "Created: the user approved and Calendar added the event.",
            performed.after.as_ref().unwrap_or(after),
            Some(after),
        ),
        Change::Update { after, .. } => (
            "Changed: the user approved and Calendar applied the change.",
            performed.after.as_ref().unwrap_or(after),
            Some(after),
        ),
        Change::Delete { before, .. } => (
            "Deleted: the user approved and Calendar removed the event. It was:",
            before,
            None,
        ),
    };
    let mut lines = vec![headline.to_string(), UNTRUSTED.to_string()];
    lines.extend(describe(&calendar.label, uid, shown));
    if wanted.is_some_and(|wanted| when(wanted) != when(shown)) {
        lines.push(
            "Note: Calendar stored the time differently than requested; check it with get_event."
                .to_string(),
        );
    }
    lines.join("\n")
}

pub(crate) fn create_event(ctx: &Context, args: &Object) -> ToolResult<String> {
    if let Some(error) = unknown_parameters(args, &CREATE_ARGS) {
        return Err(error);
    }
    let (calendar, settings) = ctx
        .settings
        .resolve_writable_calendar(args.get("calendar"))?;
    let change = Change::Create {
        after: new_fields(args)?,
    };
    let performed = perform(
        ctx,
        &calendar,
        &change,
        Origin::Model {
            max_per_hour: settings.max_per_hour,
        },
        None,
    )?;
    Ok(report(&performed, &change, &calendar))
}

pub(crate) fn update_event(ctx: &Context, args: &Object) -> ToolResult<String> {
    if let Some(error) = unknown_parameters(args, &UPDATE_ARGS) {
        return Err(error);
    }
    let (calendar, settings) = ctx
        .settings
        .resolve_writable_calendar(args.get("calendar"))?;
    let uid = id_argument(args)?;
    let before = existing(ctx, &calendar, &uid)?;
    let after = updated_fields(&before, args)?;
    if after == before {
        return Err(ToolError::msg(
            "Nothing to change: the event already has these values, or no field was given.",
        ));
    }
    let change = Change::Update { uid, before, after };
    let performed = perform(
        ctx,
        &calendar,
        &change,
        Origin::Model {
            max_per_hour: settings.max_per_hour,
        },
        None,
    )?;
    Ok(report(&performed, &change, &calendar))
}

pub(crate) fn delete_event(ctx: &Context, args: &Object) -> ToolResult<String> {
    if let Some(error) = unknown_parameters(args, &DELETE_ARGS) {
        return Err(error);
    }
    let (calendar, settings) = ctx
        .settings
        .resolve_writable_calendar(args.get("calendar"))?;
    let uid = id_argument(args)?;
    let before = existing(ctx, &calendar, &uid)?;
    let change = Change::Delete { uid, before };
    let performed = perform(
        ctx,
        &calendar,
        &change,
        Origin::Model {
            max_per_hour: settings.max_per_hour,
        },
        None,
    )?;
    Ok(report(&performed, &change, &calendar))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::CalendarEntry;
    use crate::changelog::read_entries;
    use crate::fake::FakeOptions;
    use crate::testutil::{args, settings_at, TempDir, TestResult};
    use std::fs;

    fn writer(dir: &TempDir, write: &str, approve: &str, options: FakeOptions) -> Context {
        let mut settings = settings_at(&dir.path("config.json"));
        settings.fake = true;
        settings.fake_write = write.to_string();
        Context::demo(
            settings,
            FakeOptions {
                approve: approve.to_string(),
                ..options
            },
        )
    }

    fn privat(dir: &TempDir, approve: &str) -> Context {
        writer(dir, "FAKE-CAL-PRIVAT", approve, FakeOptions::default())
    }

    fn calendar() -> Calendar {
        crate::calendar::with_labels(vec![CalendarEntry {
            id: "FAKE-CAL-PRIVAT".into(),
            name: "Demo Privat".into(),
        }])
        .remove(0)
    }

    const NEW: &str = r#"{"summary": "Tennis", "start": "2030-05-06 18:30", "end": "2030-05-06 19:45", "location": "Platz 3", "notes": "Schlaeger\nBaelle"}"#;

    fn entries(ctx: &Context) -> Result<Vec<ChangeEntry>, Box<dyn std::error::Error>> {
        Ok(read_entries(&ctx.settings, 50)?.entries)
    }

    fn found(ctx: &Context, uid: &str) -> ToolResult<crate::calendar::EventDetail> {
        ctx.calendar.event(&calendar(), uid)
    }

    #[test]
    fn an_approved_create_adds_the_event_and_logs_before_and_after() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = privat(&dir, "yes");
        let text = create_event(&ctx, &args(NEW)?)?;
        assert!(text.starts_with("Created: the user approved"), "{text}");
        assert!(text.contains(UNTRUSTED));
        assert!(text.contains("Calendar: Demo Privat | id: fake-new-1"));
        assert!(text.contains("Title: Tennis"));
        assert!(text.contains("When: 2030-05-06 18:30-19:45 (local time)"));
        assert!(text.contains("Where: Platz 3"));
        let detail = found(&ctx, "fake-new-1")?;
        assert_eq!(detail.notes, "Schlaeger\nBaelle");
        assert_eq!(detail.event.attendees, Some(0));
        let log = entries(&ctx)?;
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].outcome, Outcome::Pending);
        assert!(log[0].before.is_none() && log[0].after.is_some() && log[0].uid.is_none());
        assert_eq!(log[1].outcome, Outcome::Done);
        assert_eq!(log[1].uid.as_deref(), Some("fake-new-1"));
        assert_eq!(log[1].id, log[0].id);
        assert_eq!(
            log[1].after.as_ref().map(|a| a.summary.as_str()),
            Some("Tennis")
        );
        assert_eq!(log[1].calendar_id, "FAKE-CAL-PRIVAT");
        Ok(())
    }

    #[test]
    fn a_declined_or_ignored_create_changes_nothing_and_blocks_a_quick_retry() -> TestResult {
        for (approve, needle, outcome) in [
            ("no", "did NOT approve", Outcome::Declined),
            ("timeout", "did not answer", Outcome::Timeout),
        ] {
            let dir = TempDir::new()?;
            let ctx = privat(&dir, approve);
            let text = create_event(&ctx, &args(NEW)?)?;
            assert!(
                text.contains(needle) && text.contains("NOTHING was changed"),
                "{text}"
            );
            assert!(found(&ctx, "fake-new-1").is_err());
            let log = entries(&ctx)?;
            assert_eq!(
                log.iter().map(|e| e.outcome).collect::<Vec<_>>(),
                [Outcome::Pending, outcome]
            );
            assert_eq!(log[1].after, log[0].after);
            let again = create_event(&ctx, &args(NEW)?);
            assert!(
                matches!(&again, Err(ToolError::Msg(m)) if m.contains("just declined or ignored")),
                "{again:?}"
            );
            assert_eq!(entries(&ctx)?.len(), 2);
        }
        Ok(())
    }

    #[test]
    fn all_day_events_use_dates_and_end_on_the_following_midnight() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = privat(&dir, "yes");
        let text = create_event(
            &ctx,
            &args(
                r#"{"summary": "Urlaub", "start": "2030-07-01", "end": "2030-07-03", "all_day": true}"#,
            )?,
        )?;
        assert!(
            text.contains("When: 2030-07-01 to 2030-07-03 (all day)"),
            "{text}"
        );
        let fields = EventFields::of(&found(&ctx, "fake-new-1")?);
        assert_eq!(fields.start.to_string(), "2030-07-01 00:00");
        assert_eq!(fields.end.to_string(), "2030-07-04 00:00");
        assert!(fields.all_day);
        let single = create_event(
            &ctx,
            &args(r#"{"summary": "Feiertag", "start": "2030-12-24", "all_day": true}"#)?,
        )?;
        assert!(single.contains("When: 2030-12-24 (all day)"), "{single}");
        Ok(())
    }

    #[test]
    fn the_end_comes_from_end_or_a_duration_or_defaults_to_an_hour() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = privat(&dir, "yes");
        let ends = |input: &str, uid: &str| -> Result<String, Box<dyn std::error::Error>> {
            create_event(&ctx, &args(input)?)?;
            Ok(found(&ctx, uid)?.event.end.to_string())
        };
        assert_eq!(
            ends(
                r#"{"summary": "A", "start": "2030-05-06 09:00"}"#,
                "fake-new-1"
            )?,
            "2030-05-06 10:00"
        );
        assert_eq!(
            ends(
                r#"{"summary": "B", "start": "2030-05-06 09:00", "duration_minutes": 90}"#,
                "fake-new-2"
            )?,
            "2030-05-06 10:30"
        );
        assert_eq!(
            ends(
                r#"{"summary": "C", "start": "2030-05-06T23:30", "duration_minutes": 60}"#,
                "fake-new-3"
            )?,
            "2030-05-07 00:30"
        );
        Ok(())
    }

    #[test]
    fn bad_input_is_refused_before_anything_is_asked_or_logged() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = privat(&dir, "yes");
        let long_summary = format!(
            r#"{{"summary": "{}", "start": "2030-05-06 09:00"}}"#,
            "x".repeat(201)
        );
        let long_notes = format!(
            r#"{{"summary": "A", "start": "2030-05-06 09:00", "notes": "{}"}}"#,
            "n".repeat(1001)
        );
        for (input, needle) in [
            (r#"{"start": "2030-05-06 09:00"}"#.to_string(), "Parameter 'summary' is required"),
            (r#"{"summary": "  ", "start": "2030-05-06 09:00"}"#.to_string(), "Parameter 'summary' is required"),
            (r#"{"summary": "A"}"#.to_string(), "Parameter 'start' is required"),
            (long_summary, "Parameter 'summary' is too long (max 200 characters)"),
            (long_notes, "Parameter 'notes' is too long (max 1000 characters)"),
            (r#"{"summary": 5, "start": "2030-05-06 09:00"}"#.to_string(), "Parameter 'summary' must be text"),
            (r#"{"summary": "A", "start": "2030-05-06 09:00", "notes": ["x"]}"#.to_string(), "Parameter 'notes' must be text"),
            (r#"{"summary": "A", "start": "2030-05-06"}"#.to_string(), "Parameter 'start' must look like 2026-11-20 09:30"),
            (r#"{"summary": "A", "start": "2030-05-06 25:00"}"#.to_string(), "Parameter 'start'"),
            (r#"{"summary": "A", "start": "2030-05-06 09:00", "all_day": true}"#.to_string(), "must be a date"),
            (r#"{"summary": "A", "start": "1969-12-31 09:00"}"#.to_string(), "Parameter 'start'"),
            (r#"{"summary": "A", "start": "2101-01-01 09:00"}"#.to_string(), "Parameter 'start'"),
            (r#"{"summary": "A", "start": "2030-05-06 09:00", "end": "2030-05-06 09:00"}"#.to_string(), "must be after 'start'"),
            (r#"{"summary": "A", "start": "2030-05-06 09:00", "end": "2030-05-06 08:00"}"#.to_string(), "must be after 'start'"),
            (r#"{"summary": "A", "start": "2030-05-06 09:00", "end": "2030-05-06 10:00", "duration_minutes": 30}"#.to_string(), "either 'end' or 'duration_minutes'"),
            (r#"{"summary": "A", "start": "2030-05-06 09:00", "duration_minutes": 0}"#.to_string(), "whole number from 1 to 1440"),
            (r#"{"summary": "A", "start": "2030-05-06 09:00", "duration_minutes": 1441}"#.to_string(), "whole number from 1 to 1440"),
            (r#"{"summary": "A", "start": "2030-05-06 09:00", "duration_minutes": "60"}"#.to_string(), "whole number from 1 to 1440"),
            (r#"{"summary": "A", "start": "2030-05-06 09:00", "duration_minutes": 1.5}"#.to_string(), "whole number from 1 to 1440"),
            (r#"{"summary": "A", "start": "2030-05-06", "end": "2030-05-05", "all_day": true}"#.to_string(), "must not be before"),
            (r#"{"summary": "A", "start": "2030-05-06", "duration_minutes": 60, "all_day": true}"#.to_string(), "not used for all-day"),
            (r#"{"summary": "A", "start": "2100-12-31 23:30", "duration_minutes": 120}"#.to_string(), "beyond the year 2100"),
            (r#"{"summary": "A", "start": "2030-05-06 09:00", "attendees": ["a@example.org"]}"#.to_string(), "Events with attendees are not supported."),
            (r#"{"summary": "A", "start": "2030-05-06 09:00", "recurrence": "FREQ=DAILY"}"#.to_string(), "Recurring events are not supported."),
            (r#"{"summary": "A", "start": "2030-05-06 09:00", "alarm": 5}"#.to_string(), "Alerts and links are not supported."),
            (r#"{"summary": "A", "start": "2030-05-06 09:00", "calendar": "Demo Familie"}"#.to_string(), "Unknown calendar 'Demo Familie'"),
            (r#"{"summary": "A", "start": "2030-05-06 09:00", "bcc": 1}"#.to_string(), "Unknown parameter(s): bcc."),
        ] {
            let result = create_event(&ctx, &args(&input)?);
            assert!(
                matches!(&result, Err(ToolError::Msg(m)) if m.contains(needle)),
                "{input}: {result:?}"
            );
        }
        assert!(!std::path::Path::new(&ctx.settings.changes_path()).exists());
        Ok(())
    }

    #[test]
    fn titles_and_places_are_one_clean_line_and_notes_keep_their_lines() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = privat(&dir, "yes");
        create_event(
            &ctx,
            &args(
                r#"{"summary": "Eins\nZwei\u202e\u200b", "start": "2030-05-06 09:00", "location": "Ort\r\nZwei", "notes": "a\r\nb\u202ec"}"#,
            )?,
        )?;
        let detail = found(&ctx, "fake-new-1")?;
        assert_eq!(detail.event.summary, "Eins Zwei");
        assert_eq!(detail.event.location, "Ort Zwei");
        assert_eq!(detail.notes, "a\nbc");
        Ok(())
    }

    #[test]
    fn update_changes_only_the_given_fields_and_keeps_the_length_when_moving() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = privat(&dir, "yes");
        let before = EventFields::of(&found(&ctx, "demo-dentist")?);
        let text = update_event(
            &ctx,
            &args(
                r#"{"id": "demo-dentist", "calendar": "Demo Privat", "summary": "Zahnarzt Kontrolle", "start": "2030-05-06 14:15"}"#,
            )?,
        )?;
        assert!(text.starts_with("Changed: the user approved"), "{text}");
        assert!(text.contains("Title: Zahnarzt Kontrolle"));
        assert!(text.contains("When: 2030-05-06 14:15-15:15 (local time)"));
        let after = EventFields::of(&found(&ctx, "demo-dentist")?);
        assert_eq!(after.summary, "Zahnarzt Kontrolle");
        assert_eq!(after.location, before.location);
        assert_eq!(after.notes, before.notes);
        let log = entries(&ctx)?;
        assert_eq!(log[0].before.as_ref(), Some(&before));
        assert_eq!(log[1].after.as_ref(), Some(&after));
        assert_eq!(log[1].uid.as_deref(), Some("demo-dentist"));
        update_event(
            &ctx,
            &args(r#"{"id": "demo-dentist", "location": "", "notes": "Zahnseide"}"#)?,
        )?;
        let cleared = EventFields::of(&found(&ctx, "demo-dentist")?);
        assert_eq!(
            (cleared.location.as_str(), cleared.notes.as_str()),
            ("", "Zahnseide")
        );
        update_event(
            &ctx,
            &args(r#"{"id": "demo-dentist", "end": "2030-05-06 16:00"}"#)?,
        )?;
        assert_eq!(
            found(&ctx, "demo-dentist")?.event.end.to_string(),
            "2030-05-06 16:00"
        );
        update_event(
            &ctx,
            &args(r#"{"id": "demo-dentist", "duration_minutes": 20}"#)?,
        )?;
        assert_eq!(
            found(&ctx, "demo-dentist")?.event.end.to_string(),
            "2030-05-06 14:35"
        );
        Ok(())
    }

    #[test]
    fn all_day_events_move_by_date_and_keep_their_number_of_days() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = privat(&dir, "yes");
        create_event(
            &ctx,
            &args(
                r#"{"summary": "Urlaub", "start": "2030-07-01", "end": "2030-07-03", "all_day": true}"#,
            )?,
        )?;
        update_event(
            &ctx,
            &args(r#"{"id": "fake-new-1", "start": "2030-07-10"}"#)?,
        )?;
        let moved = EventFields::of(&found(&ctx, "fake-new-1")?);
        assert_eq!(
            (moved.start.to_string(), moved.end.to_string()),
            (
                "2030-07-10 00:00".to_string(),
                "2030-07-13 00:00".to_string()
            )
        );
        update_event(&ctx, &args(r#"{"id": "fake-new-1", "end": "2030-07-20"}"#)?)?;
        assert_eq!(
            found(&ctx, "fake-new-1")?.event.end.to_string(),
            "2030-07-21 00:00"
        );
        update_event(&ctx, &args(r#"{"id": "fake-new-1", "summary": "Ferien"}"#)?)?;
        assert_eq!(
            found(&ctx, "fake-new-1")?.event.end.to_string(),
            "2030-07-21 00:00"
        );
        for (input, needle) in [
            (
                r#"{"id": "fake-new-1", "start": "2030-07-10 10:00"}"#,
                "must be a date",
            ),
            (
                r#"{"id": "fake-new-1", "duration_minutes": 30}"#,
                "not used for all-day",
            ),
            (
                r#"{"id": "fake-new-1", "end": "2030-07-01"}"#,
                "must not be before",
            ),
        ] {
            let result = update_event(&ctx, &args(input)?);
            assert!(
                matches!(&result, Err(ToolError::Msg(m)) if m.contains(needle)),
                "{input}: {result:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn updates_without_a_real_change_or_with_bad_values_are_refused() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = privat(&dir, "yes");
        let long = format!(
            r#"{{"id": "demo-dentist", "location": "{}"}}"#,
            "o".repeat(201)
        );
        for (input, needle) in [
            (r#"{"id": "demo-dentist"}"#.to_string(), "Nothing to change"),
            (
                r#"{"id": "demo-dentist", "summary": "Zahnarzt"}"#.to_string(),
                "Nothing to change",
            ),
            (
                r#"{"id": "demo-dentist", "summary": ""}"#.to_string(),
                "must not be empty",
            ),
            (
                r#"{"id": "demo-dentist", "start": "2030-05-06"}"#.to_string(),
                "Parameter 'start' must look like",
            ),
            (
                r#"{"id": "demo-dentist", "end": "2000-01-01 00:00"}"#.to_string(),
                "must be after 'start'",
            ),
            (
                r#"{"id": "demo-dentist", "end": "2030-05-06 10:00", "duration_minutes": 5}"#
                    .to_string(),
                "either 'end' or 'duration_minutes'",
            ),
            (long, "Parameter 'location' is too long"),
            (
                r#"{"summary": "A"}"#.to_string(),
                "Parameter 'id' is required",
            ),
            (r#"{"id": ""}"#.to_string(), "Parameter 'id' is required"),
            (r#"{"id": 7}"#.to_string(), "Parameter 'id' is required"),
            (
                r#"{"id": "a\u202eb", "summary": "A"}"#.to_string(),
                "Parameter 'id' is required",
            ),
            (
                r#"{"id": "demo-dentist", "all_day": true}"#.to_string(),
                "all-day setting of an existing event cannot be changed",
            ),
            (
                r#"{"id": "demo-dentist", "attendees": []}"#.to_string(),
                "Events with attendees are not supported.",
            ),
        ] {
            let result = update_event(&ctx, &args(&input)?);
            assert!(
                matches!(&result, Err(ToolError::Msg(m)) if m.contains(needle)),
                "{input}: {result:?}"
            );
        }
        assert!(!std::path::Path::new(&ctx.settings.changes_path()).exists());
        Ok(())
    }

    #[test]
    fn events_with_attendees_series_and_unknown_ids_cannot_be_changed_or_deleted() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = writer(
            &dir,
            "FAKE-CAL-PRIVAT,FAKE-CAL-FAMILIE",
            "yes",
            FakeOptions::default(),
        );
        for (input, needle) in [
            (
                r#"{"id": "demo-parents", "calendar": "Demo Privat", "summary": "X"}"#,
                "has attendees",
            ),
            (
                r#"{"id": "demo-dinner", "calendar": "Demo Familie", "summary": "X"}"#,
                "recurring event",
            ),
            (
                r#"{"id": "nope", "calendar": "Demo Privat", "summary": "X"}"#,
                "Event not found",
            ),
            (
                r#"{"id": "demo-dentist", "calendar": "Demo Familie", "summary": "X"}"#,
                "Event not found",
            ),
        ] {
            let changed = update_event(&ctx, &args(input)?);
            assert!(
                matches!(&changed, Err(ToolError::Msg(m)) if m.contains(needle)),
                "{input}: {changed:?}"
            );
            let deleted = delete_event(&ctx, &args(&input.replace(r#", "summary": "X""#, ""))?);
            assert!(
                matches!(&deleted, Err(ToolError::Msg(m)) if m.contains(needle)),
                "{input}: {deleted:?}"
            );
        }
        assert!(!std::path::Path::new(&ctx.settings.changes_path()).exists());
        assert!(found(&ctx, "demo-parents").is_ok());
        Ok(())
    }

    #[test]
    fn a_calendar_that_is_read_only_in_calendar_refuses_changes_and_logs_the_error() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = writer(&dir, "FAKE-CAL-FEIERTAGE", "yes", FakeOptions::default());
        let result = create_event(&ctx, &args(NEW)?);
        assert!(
            matches!(&result, Err(ToolError::Msg(m)) if m.contains("read-only in Calendar")),
            "{result:?}"
        );
        let log = entries(&ctx)?;
        assert_eq!(
            log.iter().map(|e| e.outcome).collect::<Vec<_>>(),
            [Outcome::Pending, Outcome::Error]
        );
        Ok(())
    }

    #[test]
    fn a_delete_removes_the_event_and_keeps_its_fields_in_the_log() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = privat(&dir, "yes");
        let before = EventFields::of(&found(&ctx, "demo-birthday")?);
        let text = delete_event(&ctx, &args(r#"{"id": "demo-birthday"}"#)?)?;
        assert!(
            text.starts_with("Deleted: the user approved and Calendar removed the event. It was:"),
            "{text}"
        );
        assert!(text.contains("Title: Geburtstag Anna") && text.contains(UNTRUSTED));
        assert!(found(&ctx, "demo-birthday").is_err());
        let log = entries(&ctx)?;
        assert_eq!(log[1].outcome, Outcome::Done);
        assert_eq!(log[1].before.as_ref(), Some(&before));
        assert!(log[1].after.is_none());
        assert!(before.notes.contains("WICHTIGE ANWEISUNG"));
        Ok(())
    }

    #[test]
    fn declined_updates_and_deletes_leave_the_event_alone() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = privat(&dir, "no");
        let before = EventFields::of(&found(&ctx, "demo-dentist")?);
        let text = update_event(
            &ctx,
            &args(r#"{"id": "demo-dentist", "summary": "Anders"}"#)?,
        )?;
        assert!(text.contains("did NOT approve"));
        assert_eq!(EventFields::of(&found(&ctx, "demo-dentist")?), before);
        let other = TempDir::new()?;
        let ctx = privat(&other, "no");
        let text = delete_event(&ctx, &args(r#"{"id": "demo-dentist"}"#)?)?;
        assert!(text.contains("did NOT approve"));
        assert_eq!(EventFields::of(&found(&ctx, "demo-dentist")?), before);
        Ok(())
    }

    #[test]
    fn without_the_write_level_nothing_is_changed_or_logged() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = writer(&dir, "", "yes", FakeOptions::default());
        for result in [
            create_event(&ctx, &args(NEW)?),
            update_event(&ctx, &args(r#"{"id": "demo-dentist", "summary": "X"}"#)?),
            delete_event(&ctx, &args(r#"{"id": "demo-dentist"}"#)?),
        ] {
            assert!(
                matches!(&result, Err(ToolError::Msg(m)) if m.contains("Changing calendars is not enabled")),
                "{result:?}"
            );
        }
        assert!(!std::path::Path::new(&ctx.settings.changes_path()).exists());
        Ok(())
    }

    #[test]
    fn several_writable_calendars_need_the_calendar_parameter() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = writer(
            &dir,
            "FAKE-CAL-PRIVAT,FAKE-CAL-FAMILIE",
            "yes",
            FakeOptions::default(),
        );
        let result = create_event(&ctx, &args(NEW)?);
        assert!(
            matches!(&result, Err(ToolError::Msg(m)) if m.contains("Parameter 'calendar' is required. Calendars that may be changed: Demo Privat, Demo Familie")),
            "{result:?}"
        );
        let with = create_event(
            &ctx,
            &args(r#"{"summary": "A", "start": "2030-05-06 09:00", "calendar": "demo familie"}"#)?,
        )?;
        assert!(with.contains("Calendar: Demo Familie"), "{with}");
        Ok(())
    }

    #[test]
    fn the_hourly_limit_blocks_before_anything_is_asked() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = privat(&dir, "yes");
        changelog::ensure_writable(&ctx.settings)?;
        let now = journal::now_seconds();
        for index in 0..10 {
            let entry = ChangeEntry {
                ts: now - 1000 - index,
                id: format!("old{index}"),
                action: crate::calendar::Action::Create,
                outcome: Outcome::Done,
                calendar_id: "FAKE-CAL-PRIVAT".into(),
                calendar: "Demo Privat".into(),
                uid: Some(format!("u{index}")),
                before: None,
                after: None,
                undoes: None,
            };
            changelog::append(&ctx.settings, &entry, true)?;
        }
        let result = create_event(&ctx, &args(NEW)?);
        assert!(
            matches!(&result, Err(ToolError::Msg(m)) if m.starts_with("Change limit reached (10 per hour)")),
            "{result:?}"
        );
        assert!(found(&ctx, "fake-new-1").is_err());
        assert_eq!(entries(&ctx)?.len(), 10);
        Ok(())
    }

    #[test]
    fn an_unreadable_or_unwritable_log_blocks_every_change() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = privat(&dir, "yes");
        fs::write(ctx.settings.changes_path(), "garbage\n")?;
        let result = update_event(&ctx, &args(r#"{"id": "demo-dentist", "summary": "X"}"#)?);
        assert!(
            matches!(&result, Err(ToolError::Msg(m)) if m.contains("cannot be read")),
            "{result:?}"
        );
        let blocked = TempDir::new()?;
        let ctx = privat(&blocked, "yes");
        fs::create_dir_all(ctx.settings.changes_path())?;
        let result = delete_event(&ctx, &args(r#"{"id": "demo-dentist"}"#)?);
        assert!(
            matches!(&result, Err(ToolError::Msg(m)) if m.contains("change log is not writable")),
            "{result:?}"
        );
        assert!(found(&ctx, "demo-dentist").is_ok());
        Ok(())
    }

    #[test]
    fn an_unclear_outcome_stays_pending_and_asks_the_user_to_check_calendar() -> TestResult {
        let dir = TempDir::new()?;
        let options = FakeOptions {
            uncertain: true,
            ..FakeOptions::default()
        };
        let ctx = writer(&dir, "FAKE-CAL-PRIVAT", "yes", options);
        let result = create_event(&ctx, &args(NEW)?);
        assert!(
            matches!(&result, Err(ToolError::Uncertain(m)) if m.contains("may or may not")),
            "{result:?}"
        );
        let log = entries(&ctx)?;
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].outcome, Outcome::Pending);
        let crash = TempDir::new()?;
        let options = FakeOptions {
            crash: true,
            ..FakeOptions::default()
        };
        let ctx = writer(&crash, "FAKE-CAL-PRIVAT", "yes", options);
        assert!(matches!(
            create_event(&ctx, &args(NEW)?),
            Err(ToolError::Internal(_))
        ));
        assert_eq!(entries(&ctx)?.len(), 1);
        Ok(())
    }

    #[test]
    fn a_stale_snapshot_is_refused_by_the_backend_and_logged_as_an_error() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = privat(&dir, "yes");
        let stale = EventFields::of(&found(&ctx, "demo-dentist")?);
        update_event(&ctx, &args(r#"{"id": "demo-dentist", "summary": "Neu"}"#)?)?;
        let change = Change::Delete {
            uid: "demo-dentist".into(),
            before: stale,
        };
        let result = perform(&ctx, &calendar(), &change, Origin::Human, None);
        assert!(
            matches!(&result, Err(ToolError::Msg(m)) if m.contains("changed in Calendar")),
            "{:?}",
            result.err()
        );
        assert!(found(&ctx, "demo-dentist").is_ok());
        let log = entries(&ctx)?;
        assert_eq!(log.last().map(|e| e.outcome), Some(Outcome::Error));
        Ok(())
    }

    #[test]
    fn human_changes_ignore_the_model_limits_but_not_the_log() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = privat(&dir, "yes");
        changelog::ensure_writable(&ctx.settings)?;
        let now = journal::now_seconds();
        for index in 0..10 {
            let entry = ChangeEntry {
                ts: now - 1000 - index,
                id: format!("old{index}"),
                action: crate::calendar::Action::Create,
                outcome: Outcome::Done,
                calendar_id: "FAKE-CAL-PRIVAT".into(),
                calendar: "Demo Privat".into(),
                uid: None,
                before: None,
                after: None,
                undoes: None,
            };
            changelog::append(&ctx.settings, &entry, true)?;
        }
        let before = EventFields::of(&found(&ctx, "demo-dentist")?);
        let change = Change::Delete {
            uid: "demo-dentist".into(),
            before,
        };
        let done = perform(&ctx, &calendar(), &change, Origin::Human, Some("old0"))?;
        assert_eq!(done.decision, Decision::Applied);
        assert!(found(&ctx, "demo-dentist").is_err());
        let last = entries(&ctx)?.pop().ok_or("entry")?;
        assert_eq!(last.undoes.as_deref(), Some("old0"));
        Ok(())
    }
}
