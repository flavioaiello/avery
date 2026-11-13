use std::ops::RangeInclusive;

use crate::calendar::{
    Attendee, CalendarInfo, EventDetail, RawEvent, MAX_KEPT_LOCATION, MAX_KEPT_NOTES,
    MAX_KEPT_SUMMARY,
};
use crate::civil::{Date, DateTime};
use crate::error::{ToolError, ToolResult};
use crate::json::{Json, Object};
use crate::text::{one_line_str, take_chars};

use super::{html, unexpected, usable_id};

const SUMMARY_LIMIT: usize = MAX_KEPT_SUMMARY + 1;
const LOCATION_LIMIT: usize = MAX_KEPT_LOCATION + 1;
const NOTES_LIMIT: usize = MAX_KEPT_NOTES + 1;
const MAX_DESCRIPTION_BYTES: usize = 1 << 20;
const MAX_PEOPLE: usize = 50;
const MAX_NAME: usize = 200;
const MAX_EMAIL: usize = 254;
const MAX_STATUS: usize = 40;
const MAX_RULE: usize = 200;
const MAX_CALENDAR_NAME: usize = 80;
const SERIES: &str = "RECURRING-SERIES";
const NO_TITLE: &str = "(no title)";

enum Moment {
    Day(Date),
    At(DateTime),
}

fn text(object: &Object, key: &str) -> String {
    object
        .get(key)
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_string()
}

fn set(object: &Object, key: &str) -> bool {
    !matches!(
        object.get(key),
        None | Some(Json::Null) | Some(Json::Bool(false))
    )
}

fn two_digits(text: &str, allowed: RangeInclusive<u32>) -> bool {
    text.len() == 2
        && text.bytes().all(|byte| byte.is_ascii_digit())
        && text
            .parse::<u32>()
            .is_ok_and(|number| allowed.contains(&number))
}

fn zone_offset(text: &str) -> bool {
    if text == "Z" || text == "z" {
        return true;
    }
    let Some(rest) = text.strip_prefix(['+', '-']) else {
        return false;
    };
    match (rest.get(..2), rest.get(2..3), rest.get(3..)) {
        (Some(hours), Some(":"), Some(minutes)) => {
            two_digits(hours, 0..=23) && two_digits(minutes, 0..=59)
        }
        _ => false,
    }
}

fn clock_tail(tail: &str) -> bool {
    let Some(rest) = tail.strip_prefix(':') else {
        return false;
    };
    let (Some(seconds), Some(rest)) = (rest.get(..2), rest.get(2..)) else {
        return false;
    };
    if !two_digits(seconds, 0..=60) {
        return false;
    }
    match rest.strip_prefix('.') {
        Some(fraction) => {
            let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
            (1..=9).contains(&digits) && fraction.get(digits..).is_some_and(zone_offset)
        }
        None => zone_offset(rest),
    }
}

fn instant(text: &str) -> Option<DateTime> {
    let stamp = text
        .get(..16)
        .filter(|head| head.as_bytes().get(10) == Some(&b'T'))?;
    let parsed = DateTime::parse(stamp)?;
    clock_tail(text.get(16..)?).then_some(parsed)
}

fn moment(value: Option<&Json>) -> Option<Moment> {
    let object = value?.as_object()?;
    match (object.get("date"), object.get("dateTime")) {
        (Some(Json::Str(day)), None | Some(Json::Null)) => Date::parse(day).map(Moment::Day),
        (None | Some(Json::Null), Some(Json::Str(stamp))) => instant(stamp).map(Moment::At),
        _ => None,
    }
}

fn span(object: &Object) -> Option<(DateTime, DateTime, bool)> {
    let (start, end, all_day) = match (moment(object.get("start"))?, moment(object.get("end"))?) {
        (Moment::Day(first), Moment::Day(last)) => {
            (DateTime::midnight(first), DateTime::midnight(last), true)
        }
        (Moment::At(first), Moment::At(last)) => (first, last, false),
        _ => return None,
    };
    (end >= start).then_some((start, end, all_day))
}

fn attendee_count(object: &Object) -> Option<usize> {
    if set(object, "attendeesOmitted") {
        return None;
    }
    match object.get("attendees") {
        None | Some(Json::Null) => Some(0),
        Some(Json::Arr(items)) => Some(items.len()),
        Some(_) => None,
    }
}

fn people(object: &Object) -> Vec<Attendee> {
    let Some(Json::Arr(items)) = object.get("attendees") else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(Json::as_object)
        .take(MAX_PEOPLE)
        .map(|entry| Attendee {
            name: take_chars(&text(entry, "displayName"), MAX_NAME),
            email: take_chars(&text(entry, "email"), MAX_EMAIL),
            status: take_chars(&text(entry, "responseStatus"), MAX_STATUS),
        })
        .collect()
}

fn series(object: &Object) -> bool {
    let listed = match object.get("recurrence") {
        None | Some(Json::Null) => false,
        Some(Json::Arr(lines)) => !lines.is_empty(),
        Some(_) => true,
    };
    let instance = ["recurringEventId", "originalStartTime"]
        .iter()
        .any(|key| !matches!(object.get(key), None | Some(Json::Null)));
    listed || instance
}

fn recurrence(object: &Object) -> String {
    if !series(object) {
        return String::new();
    }
    let rule = object
        .get("recurrence")
        .and_then(Json::as_array)
        .and_then(|lines| {
            lines
                .iter()
                .filter_map(Json::as_str)
                .find(|line| line.starts_with("RRULE:") && line.chars().count() <= MAX_RULE)
        });
    rule.map_or_else(|| SERIES.to_string(), str::to_string)
}

pub(super) fn cancelled(object: &Object) -> bool {
    object.get("status").and_then(Json::as_str) == Some("cancelled")
}

pub(super) fn special(object: &Object) -> bool {
    let kind = match object.get("eventType") {
        None | Some(Json::Null) => false,
        Some(Json::Str(kind)) => kind != "default",
        Some(_) => true,
    };
    kind || set(object, "locked")
}

fn base(object: &Object) -> Option<RawEvent> {
    let uid = object
        .get("id")
        .and_then(Json::as_str)
        .filter(|id| usable_id(id))?;
    let (start, end, all_day) = span(object)?;
    Some(RawEvent {
        uid: uid.to_string(),
        summary: take_chars(&text(object, "summary"), SUMMARY_LIMIT),
        start,
        end,
        all_day,
        location: take_chars(&text(object, "location"), LOCATION_LIMIT),
        recurrence: String::new(),
        excluded: Vec::new(),
        utc_offset: 0,
        attendees: attendee_count(object),
    })
}

pub(super) fn listed(object: &Object) -> Option<RawEvent> {
    if cancelled(object) {
        return None;
    }
    let mut event = base(object)?;
    if event.summary.trim().is_empty() {
        event.summary = NO_TITLE.to_string();
    }
    Some(event)
}

fn notes(object: &Object) -> ToolResult<String> {
    let description = object
        .get("description")
        .and_then(Json::as_str)
        .unwrap_or_default();
    if description.len() > MAX_DESCRIPTION_BYTES {
        return Err(ToolError::msg(
            "The description of this event is too large to read safely.",
        ));
    }
    Ok(html::to_text(description, NOTES_LIMIT))
}

pub(super) fn detail(object: &Object) -> ToolResult<EventDetail> {
    let mut event = base(object).ok_or_else(unexpected)?;
    event.recurrence = recurrence(object);
    Ok(EventDetail {
        event,
        notes: notes(object)?,
        url: String::new(),
        people: people(object),
    })
}

pub(super) fn calendar(object: &Object) -> Option<CalendarInfo> {
    let id = object
        .get("id")
        .and_then(Json::as_str)
        .filter(|id| usable_id(id))?;
    if set(object, "hidden") || set(object, "deleted") {
        return None;
    }
    let writable = match object.get("accessRole").and_then(Json::as_str)? {
        "owner" | "writer" => true,
        "reader" | "writerWithoutPrivateAccess" => false,
        _ => return None,
    };
    let name = ["summaryOverride", "summary"]
        .iter()
        .map(|key| one_line_str(&text(object, key), MAX_CALENDAR_NAME))
        .find(|name| !name.is_empty())
        .unwrap_or_else(|| one_line_str(id, MAX_CALENDAR_NAME));
    Some(CalendarInfo {
        id: id.to_string(),
        name,
        writable,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{j, TestResult};

    fn object(text: &str) -> Result<Object, Box<dyn std::error::Error>> {
        match j(text)? {
            Json::Obj(object) => Ok(object),
            _ => Err("object".into()),
        }
    }

    fn at(text: &str) -> Result<DateTime, Box<dyn std::error::Error>> {
        DateTime::parse(text).ok_or_else(|| "time".into())
    }

    #[test]
    fn instants_take_the_wall_clock_digits_and_ignore_the_offset() -> TestResult {
        for (text, wanted) in [
            ("2030-05-06T09:00:00+02:00", "2030-05-06 09:00"),
            ("2030-05-06T09:00:00-07:30", "2030-05-06 09:00"),
            ("2030-05-06T23:59:59Z", "2030-05-06 23:59"),
            ("2030-05-06T09:00:00z", "2030-05-06 09:00"),
            ("2030-05-06T09:00:00.123456789+00:00", "2030-05-06 09:00"),
            ("2030-05-06T09:00:60Z", "2030-05-06 09:00"),
            ("2030-12-31T00:00:00+14:00", "2030-12-31 00:00"),
        ] {
            assert_eq!(instant(text), Some(at(wanted)?), "{text}");
        }
        for text in [
            "",
            "2030-05-06",
            "2030-05-06T09:00",
            "2030-05-06 09:00:00+02:00",
            "2030-05-06t09:00:00Z",
            "2030-05-06T09:00:00",
            "2030-05-06T09:00:00+0200",
            "2030-05-06T09:00:00+02",
            "2030-05-06T09:00:00+24:00",
            "2030-05-06T09:00:00+02:60",
            "2030-05-06T09:00:61Z",
            "2030-05-06T09:00:0Z",
            "2030-05-06T09:00:00.Z",
            "2030-05-06T09:00:00.1234567890Z",
            "2030-05-06T09:00:00Zjunk",
            "2030-05-06T24:00:00Z",
            "2030-02-30T09:00:00Z",
            "2030-13-06T09:00:00Z",
            "0000-05-06T09:00:00Z",
            "2030-05-06T09:00:00\u{ff0b}02:00",
            "\u{e4}030-05-06T09:00:00Z",
            "2030-05-06T09:00:00+0\u{e4}:00",
        ] {
            assert_eq!(instant(text), None, "{text:?}");
        }
        Ok(())
    }

    #[test]
    fn spans_are_timed_or_all_day_and_never_mixed_or_reversed() -> TestResult {
        let timed = object(
            r#"{"start": {"dateTime": "2030-05-06T09:00:00+02:00", "timeZone": "Europe/Zurich"},
                "end": {"dateTime": "2030-05-06T10:30:00+02:00"}}"#,
        )?;
        assert_eq!(
            span(&timed),
            Some((at("2030-05-06 09:00")?, at("2030-05-06 10:30")?, false))
        );
        let whole = object(r#"{"start": {"date": "2030-05-06"}, "end": {"date": "2030-05-08"}}"#)?;
        assert_eq!(
            span(&whole),
            Some((at("2030-05-06 00:00")?, at("2030-05-08 00:00")?, true))
        );
        let empty = object(
            r#"{"start": {"dateTime": "2030-05-06T09:00:00Z"}, "end": {"dateTime": "2030-05-06T09:00:00Z"}}"#,
        )?;
        assert_eq!(
            span(&empty),
            Some((at("2030-05-06 09:00")?, at("2030-05-06 09:00")?, false))
        );
        for broken in [
            r#"{}"#,
            r#"{"start": {"date": "2030-05-06"}}"#,
            r#"{"start": {"date": "2030-05-06"}, "end": {"dateTime": "2030-05-06T09:00:00Z"}}"#,
            r#"{"start": {"dateTime": "2030-05-06T09:00:00Z"}, "end": {"date": "2030-05-06"}}"#,
            r#"{"start": {"date": "2030-05-06", "dateTime": "2030-05-06T09:00:00Z"}, "end": {"date": "2030-05-07"}}"#,
            r#"{"start": {"date": "2030-05-08"}, "end": {"date": "2030-05-06"}}"#,
            r#"{"start": {"dateTime": "2030-05-06T10:00:00Z"}, "end": {"dateTime": "2030-05-06T09:00:00Z"}}"#,
            r#"{"start": {"date": "30-05-06"}, "end": {"date": "2030-05-07"}}"#,
            r#"{"start": {"date": 20300506}, "end": {"date": "2030-05-07"}}"#,
            r#"{"start": "2030-05-06", "end": "2030-05-07"}"#,
            r#"{"start": {"date": null, "dateTime": null}, "end": {"date": null}}"#,
        ] {
            assert_eq!(span(&object(broken)?), None, "{broken}");
        }
        Ok(())
    }

    #[test]
    fn attendees_are_counted_and_unknown_lists_are_unknown() -> TestResult {
        for (text, wanted) in [
            (r#"{}"#, Some(0)),
            (r#"{"attendees": null}"#, Some(0)),
            (r#"{"attendees": []}"#, Some(0)),
            (
                r#"{"attendees": [{"email": "a@x.example"}, {"email": "b@x.example", "self": true}]}"#,
                Some(2),
            ),
            (r#"{"attendees": [1, "x"]}"#, Some(2)),
            (r#"{"attendeesOmitted": false}"#, Some(0)),
            (r#"{"attendeesOmitted": null, "attendees": []}"#, Some(0)),
            (r#"{"attendeesOmitted": true}"#, None),
            (
                r#"{"attendeesOmitted": true, "attendees": [{"email": "a@x.example"}]}"#,
                None,
            ),
            (r#"{"attendeesOmitted": "yes"}"#, None),
            (r#"{"attendees": "a@x.example"}"#, None),
            (r#"{"attendees": {"email": "a@x.example"}}"#, None),
        ] {
            assert_eq!(attendee_count(&object(text)?), wanted, "{text}");
        }
        Ok(())
    }

    #[test]
    fn series_are_recognised_by_rule_instance_id_or_original_start() -> TestResult {
        for (text, wanted) in [
            (r#"{}"#, ""),
            (r#"{"recurrence": []}"#, ""),
            (r#"{"recurrence": null, "recurringEventId": null}"#, ""),
            (
                r#"{"recurrence": ["EXDATE;TZID=Europe/Zurich:20300513T090000", "RRULE:FREQ=WEEKLY;BYDAY=MO"]}"#,
                "RRULE:FREQ=WEEKLY;BYDAY=MO",
            ),
            (
                r#"{"recurrence": ["RDATE;VALUE=DATE:20300601"]}"#,
                "RECURRING-SERIES",
            ),
            (r#"{"recurrence": "RRULE:FREQ=DAILY"}"#, "RECURRING-SERIES"),
            (r#"{"recurringEventId": "abc"}"#, "RECURRING-SERIES"),
            (r#"{"recurringEventId": ""}"#, "RECURRING-SERIES"),
            (
                r#"{"originalStartTime": {"dateTime": "2030-05-06T09:00:00Z"}}"#,
                "RECURRING-SERIES",
            ),
        ] {
            assert_eq!(recurrence(&object(text)?), wanted, "{text}");
        }
        let long = format!(
            r#"{{"recurrence": ["RRULE:FREQ=WEEKLY;BYDAY={}"]}}"#,
            "MO,".repeat(100)
        );
        assert_eq!(recurrence(&object(&long)?), "RECURRING-SERIES");
        Ok(())
    }

    #[test]
    fn special_kinds_and_locked_copies_are_flagged() -> TestResult {
        for (text, wanted) in [
            (r#"{}"#, false),
            (r#"{"eventType": "default"}"#, false),
            (r#"{"eventType": null, "locked": false}"#, false),
            (r#"{"eventType": "outOfOffice"}"#, true),
            (r#"{"eventType": "focusTime"}"#, true),
            (r#"{"eventType": "workingLocation"}"#, true),
            (r#"{"eventType": "birthday"}"#, true),
            (r#"{"eventType": "fromGmail"}"#, true),
            (r#"{"eventType": ""}"#, true),
            (r#"{"eventType": 5}"#, true),
            (r#"{"locked": true}"#, true),
            (r#"{"locked": "yes"}"#, true),
        ] {
            assert_eq!(special(&object(text)?), wanted, "{text}");
        }
        assert!(cancelled(&object(r#"{"status": "cancelled"}"#)?));
        assert!(!cancelled(&object(r#"{"status": "confirmed"}"#)?));
        assert!(!cancelled(&object(r#"{"status": 3}"#)?));
        assert!(!cancelled(&object(r#"{}"#)?));
        Ok(())
    }

    fn event(extra: &str) -> Result<Object, Box<dyn std::error::Error>> {
        object(&format!(
            r#"{{"id": "ev1", "status": "confirmed", "summary": "Zahnarzt",
                "start": {{"dateTime": "2030-05-06T09:00:00+02:00"}},
                "end": {{"dateTime": "2030-05-06T10:00:00+02:00"}}{extra}}}"#
        ))
    }

    #[test]
    fn listed_events_are_plain_and_titled() -> TestResult {
        let found = listed(&event(
            r#", "location": "Praxis", "attendees": [{"email": "a@x.example"}]"#,
        )?)
        .ok_or("event")?;
        assert_eq!(found.uid, "ev1");
        assert_eq!(found.summary, "Zahnarzt");
        assert_eq!(found.location, "Praxis");
        assert_eq!(
            (found.start, found.end),
            (at("2030-05-06 09:00")?, at("2030-05-06 10:00")?)
        );
        assert!(!found.all_day);
        assert_eq!(found.recurrence, "");
        assert_eq!(found.excluded, Vec::new());
        assert_eq!(found.utc_offset, 0);
        assert_eq!(found.attendees, Some(1));
        let series = listed(&event(r#", "recurringEventId": "root""#)?).ok_or("series")?;
        assert_eq!(series.recurrence, "");
        let untitled = object(
            r#"{"id": "ev2", "start": {"date": "2030-05-06"}, "end": {"date": "2030-05-07"}, "summary": " \n "}"#,
        )?;
        assert_eq!(listed(&untitled).ok_or("untitled")?.summary, "(no title)");
        let missing = object(
            r#"{"id": "ev3", "start": {"date": "2030-05-06"}, "end": {"date": "2030-05-07"}}"#,
        )?;
        let found = listed(&missing).ok_or("missing")?;
        assert_eq!(
            (found.summary.as_str(), found.all_day),
            ("(no title)", true)
        );
        assert_eq!(found.end, at("2030-05-07 00:00")?);
        Ok(())
    }

    #[test]
    fn unusable_events_are_not_listed() -> TestResult {
        assert!(listed(&event(r#", "status": "cancelled""#)?).is_none());
        let cases = [
            r#"{"summary": "no id", "start": {"date": "2030-05-06"}, "end": {"date": "2030-05-07"}}"#,
            r#"{"id": "", "start": {"date": "2030-05-06"}, "end": {"date": "2030-05-07"}}"#,
            r#"{"id": "..", "start": {"date": "2030-05-06"}, "end": {"date": "2030-05-07"}}"#,
            r#"{"id": 5, "start": {"date": "2030-05-06"}, "end": {"date": "2030-05-07"}}"#,
            r#"{"id": "x", "start": {"date": "bad"}, "end": {"date": "2030-05-07"}}"#,
            r#"{"id": "x"}"#,
            r#"{"id": "x", "status": "cancelled"}"#,
        ];
        for case in cases {
            assert!(listed(&object(case)?).is_none(), "{case}");
        }
        let long = format!(
            r#"{{"id": "{}", "start": {{"date": "2030-05-06"}}, "end": {{"date": "2030-05-07"}}}}"#,
            "a".repeat(600)
        );
        assert!(listed(&object(&long)?).is_none());
        Ok(())
    }

    #[test]
    fn text_is_kept_as_it_is_but_bounded_one_character_above_the_backup_limit() -> TestResult {
        let summary = format!("{}\u{200b}\n\"q\"", "x".repeat(10));
        let found = listed(&event(&format!(
            r#", "summary": {}, "location": "{}""#,
            crate::json::dumps(&Json::from(summary.as_str()), true),
            "o".repeat(900)
        ))?)
        .ok_or("event")?;
        assert_eq!(found.summary, summary);
        assert_eq!(found.location.chars().count(), MAX_KEPT_LOCATION + 1);
        let long =
            listed(&event(&format!(r#", "summary": "{}""#, "t".repeat(900)))?).ok_or("event")?;
        assert_eq!(long.summary.chars().count(), MAX_KEPT_SUMMARY + 1);
        let exact = listed(&event(&format!(
            r#", "summary": "{}""#,
            "t".repeat(MAX_KEPT_SUMMARY)
        ))?)
        .ok_or("event")?;
        assert_eq!(exact.summary.chars().count(), MAX_KEPT_SUMMARY);
        Ok(())
    }

    #[test]
    fn details_carry_plain_notes_people_and_the_series_marker() -> TestResult {
        let found = detail(&event(
            r#", "description": "Bring <b>Kuchen</b><br>und&nbsp;Tee &amp; mehr",
                "location": "Praxis", "recurringEventId": "root",
                "attendees": [
                    {"email": "muster@schule.example", "displayName": "Frau Muster", "responseStatus": "accepted"},
                    {"email": "me@gmail.example", "self": true, "responseStatus": "needsAction"},
                    7,
                    {"displayName": "ohne Mail"}
                ]"#,
        )?)?;
        assert_eq!(found.notes, "Bring Kuchen\nund Tee & mehr");
        assert_eq!(found.url, "");
        assert_eq!(found.event.recurrence, "RECURRING-SERIES");
        assert_eq!(found.event.attendees, Some(4));
        assert_eq!(found.people.len(), 3);
        assert_eq!(
            found.people.first(),
            Some(&Attendee {
                name: "Frau Muster".into(),
                email: "muster@schule.example".into(),
                status: "accepted".into(),
            })
        );
        assert_eq!(
            found.people.get(2),
            Some(&Attendee {
                name: "ohne Mail".into(),
                email: String::new(),
                status: String::new(),
            })
        );
        let plain = detail(&event("")?)?;
        assert_eq!(plain.notes, "");
        assert_eq!(plain.event.summary, "Zahnarzt");
        assert_eq!(plain.event.recurrence, "");
        assert_eq!(plain.event.attendees, Some(0));
        assert!(plain.people.is_empty());
        let untitled = detail(&object(
            r#"{"id": "u", "start": {"date": "2030-05-06"}, "end": {"date": "2030-05-07"}}"#,
        )?)?;
        assert_eq!(untitled.event.summary, "");
        assert!(untitled.event.all_day);
        Ok(())
    }

    #[test]
    fn the_people_list_and_the_notes_are_bounded() -> TestResult {
        let attendees: Vec<String> = (0..80)
            .map(|n| {
                format!(
                    r#"{{"email": "p{n}@x.example", "displayName": "{}"}}"#,
                    "N".repeat(400)
                )
            })
            .collect();
        let found = detail(&event(&format!(
            r#", "attendees": [{}]"#,
            attendees.join(", ")
        ))?)?;
        assert_eq!(found.people.len(), MAX_PEOPLE);
        assert_eq!(found.event.attendees, Some(80));
        assert!(found
            .people
            .iter()
            .all(|p| p.name.chars().count() == MAX_NAME));
        let notes = "n".repeat(5000);
        let found = detail(&event(&format!(r#", "description": "{notes}""#))?)?;
        assert_eq!(found.notes.chars().count(), MAX_KEPT_NOTES + 1);
        let exact = "n".repeat(MAX_KEPT_NOTES);
        let found = detail(&event(&format!(r#", "description": "{exact}""#))?)?;
        assert_eq!(found.notes, exact);
        let huge = "n".repeat(MAX_DESCRIPTION_BYTES + 1);
        let Err(error) = detail(&event(&format!(r#", "description": "{huge}""#))?) else {
            return Err("expected an error".into());
        };
        assert!(error.message().contains("too large"), "{error}");
        Ok(())
    }

    #[test]
    fn details_of_malformed_events_are_unexpected_answers() -> TestResult {
        for text in [
            r#"{"id": "x", "summary": "no times"}"#,
            r#"{"start": {"date": "2030-05-06"}, "end": {"date": "2030-05-07"}}"#,
            r#"{"id": "x", "start": {"date": "2030-05-06"}, "end": {"date": "garbage"}}"#,
        ] {
            let Err(error) = detail(&object(text)?) else {
                return Err("expected an error".into());
            };
            assert_eq!(
                error.message(),
                "The provider gave an unexpected answer.",
                "{text}"
            );
        }
        Ok(())
    }

    #[test]
    fn calendars_are_mapped_by_role_and_name() -> TestResult {
        let info = |text: &str| -> Result<Option<CalendarInfo>, Box<dyn std::error::Error>> {
            Ok(calendar(&object(text)?))
        };
        assert_eq!(
            info(
                r#"{"id": "alice@gmail.example", "summary": "alice@gmail.example", "accessRole": "owner", "primary": true}"#
            )?,
            Some(CalendarInfo {
                id: "alice@gmail.example".into(),
                name: "alice@gmail.example".into(),
                writable: true,
            })
        );
        assert_eq!(
            info(
                r#"{"id": "fam", "summary": "Familie", "summaryOverride": "Wir", "accessRole": "writer"}"#
            )?,
            Some(CalendarInfo {
                id: "fam".into(),
                name: "Wir".into(),
                writable: true
            })
        );
        assert_eq!(
            info(
                r#"{"id": "fam", "summary": "Familie", "summaryOverride": "  ", "accessRole": "reader"}"#
            )?,
            Some(CalendarInfo {
                id: "fam".into(),
                name: "Familie".into(),
                writable: false
            })
        );
        assert_eq!(
            info(
                r#"{"id": "h#holiday@group.v.calendar.google.com", "accessRole": "writerWithoutPrivateAccess"}"#
            )?,
            Some(CalendarInfo {
                id: "h#holiday@group.v.calendar.google.com".into(),
                name: "h#holiday@group.v.calendar.google.com".into(),
                writable: false,
            })
        );
        let hostile = info(&format!(
            r#"{{"id": "x", "summary": "A\nB\u200b{}", "accessRole": "reader"}}"#,
            "c".repeat(200)
        ))?
        .ok_or("calendar")?;
        assert!(hostile.name.starts_with("A Bc"), "{}", hostile.name);
        assert!(!hostile.name.contains('\n') && !hostile.name.contains('\u{200b}'));
        assert!(hostile.name.chars().count() <= MAX_CALENDAR_NAME + 6);
        for skipped in [
            r#"{"id": "x", "summary": "S", "accessRole": "reader", "hidden": true}"#,
            r#"{"id": "x", "summary": "S", "accessRole": "reader", "deleted": true}"#,
            r#"{"id": "x", "summary": "S", "accessRole": "reader", "hidden": "yes"}"#,
            r#"{"id": "x", "summary": "S", "accessRole": "freeBusyReader"}"#,
            r#"{"id": "x", "summary": "S", "accessRole": "none"}"#,
            r#"{"id": "x", "summary": "S", "accessRole": "superuser"}"#,
            r#"{"id": "x", "summary": "S"}"#,
            r#"{"id": "x", "summary": "S", "accessRole": 4}"#,
            r#"{"summary": "S", "accessRole": "owner"}"#,
            r#"{"id": "", "summary": "S", "accessRole": "owner"}"#,
            r#"{"id": ".", "summary": "S", "accessRole": "owner"}"#,
            r#"{"id": 12, "summary": "S", "accessRole": "owner"}"#,
        ] {
            assert_eq!(info(skipped)?, None, "{skipped}");
        }
        assert_eq!(
            info(r#"{"id": "x", "hidden": false, "deleted": null, "accessRole": "reader"}"#)?,
            Some(CalendarInfo {
                id: "x".into(),
                name: "x".into(),
                writable: false
            })
        );
        Ok(())
    }
}
