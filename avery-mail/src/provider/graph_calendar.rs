#[cfg(test)]
mod tests;
mod times;
mod zones;

use std::collections::HashMap;

use super::dialog::{approve_change, Verdict};
use super::Api;
use crate::calendar::{
    selected, Approval, Attendee, Calendar, CalendarInfo, Change, ChangeResult, Decision,
    EventDetail, EventFields, RawEvent, Window, MAX_KEPT_LOCATION, MAX_KEPT_NOTES,
    MAX_KEPT_SUMMARY,
};
use crate::civil::{Date, DateTime};
use crate::error::{ToolError, ToolResult};
use crate::html;
use crate::http::{encode_component, query, url_allowed, Method};
use crate::json::{Json, Object};
use crate::osascript::Host;
use crate::text::{char_len, one_line_str, take_chars};

const BASE: &str = "https://graph.microsoft.com/v1.0/me";
const ORIGIN: &str = "https://graph.microsoft.com/v1.0/";
const MAX_LINK: usize = 4096;
const PAGE_SIZE: usize = 100;
const MAX_PAGES: usize = 4;
const MAX_PAGE_ITEMS: usize = 1_000;
const MAX_EVENTS: usize = 500;
const MAX_CALENDARS: usize = 100;
const MAX_PEOPLE: usize = 50;
const MAX_ID: usize = 512;
const MAX_NAME: usize = 100;
const MAX_ZONE: usize = 64;
const MAX_PERSON: usize = 200;
const MAX_SUMMARY_READ: usize = 2 * MAX_KEPT_SUMMARY;
const MAX_LOCATION_READ: usize = 2 * MAX_KEPT_LOCATION;
const MAX_NOTES_READ: usize = 5 * MAX_KEPT_NOTES;
const CREATE_NOTES: usize = 1_200;
const MINUTES_PER_DAY: i64 = 1_440;
const NO_TITLE: &str = "(no title)";
const NO_NAME: &str = "(no name)";
const RECURRING: &str = "RECURRING-SERIES";
const CALENDAR_FIELDS: &str = "id,name,canEdit";
const LIST_FIELDS: &str = "id,subject,start,end,isAllDay,location,attendees,isCancelled";
const UTC_FIELDS: &str = "id,start,end,isAllDay";
const SHIFT_FIELDS: &str = "id,start,end";
const PRESENCE_FIELDS: &str = "id,isCancelled";
const DETAIL_FIELDS: &str = concat!(
    "id,subject,start,end,isAllDay,location,body,type,seriesMasterId,recurrence,",
    "attendees,isCancelled,isOrganizer"
);
const WITH_CALENDAR: &str = "$expand=calendar($select=id)";
const IDS: &str = "IdType=\"ImmutableId\"";

fn not_found() -> ToolError {
    ToolError::msg("Event not found. Use an id from list_events together with the same calendar.")
}

fn calendar_missing() -> ToolError {
    ToolError::msg("The calendar no longer exists. Ask the user to run `avery mail setup` again.")
}

fn read_only() -> ToolError {
    ToolError::msg("This calendar is read-only, so NOTHING was changed.")
}

fn recurring_refused() -> ToolError {
    ToolError::msg(concat!(
        "This is a recurring event; only single events can be changed or deleted here. ",
        "NOTHING was changed. The user has to do it in the calendar."
    ))
}

fn attendees_refused() -> ToolError {
    ToolError::msg(concat!(
        "This event has attendees; changing or deleting it would notify other people, so ",
        "NOTHING was changed. The user has to do it in the calendar."
    ))
}

fn not_organizer() -> ToolError {
    ToolError::msg(concat!(
        "This event belongs to someone else's invitation; changing or deleting it could ",
        "notify other people, so NOTHING was changed. The user has to do it in the calendar."
    ))
}

fn cancelled_refused() -> ToolError {
    ToolError::msg("This event is cancelled, so NOTHING was changed.")
}

fn cancelled_read() -> ToolError {
    ToolError::msg(
        "This event is cancelled. Use an id from list_events together with the same calendar.",
    )
}

fn changed() -> ToolError {
    ToolError::msg(concat!(
        "The event changed since you read it or while the approval dialog was open, so ",
        "NOTHING was changed. Read it again with get_event."
    ))
}

fn notes_too_long() -> ToolError {
    ToolError::msg("The notes are too long, so NOTHING was changed.")
}

fn no_links() -> ToolError {
    ToolError::msg("Links are not supported for this kind of calendar, so NOTHING was changed.")
}

fn bad_times() -> ToolError {
    ToolError::msg(
        "That time is not valid for this calendar, so NOTHING was changed. Choose another time.",
    )
}

fn no_all_day_switch() -> ToolError {
    ToolError::msg(
        "The all-day setting of an event cannot be changed here, so NOTHING was changed.",
    )
}

fn nothing_to_change() -> ToolError {
    ToolError::msg("Nothing to change: the event already has these values, so NOTHING was changed.")
}

fn too_many() -> ToolError {
    ToolError::msg(concat!(
        "The calendar has too many events in this range to read safely. ",
        "Use a shorter range (days) or a more specific calendar."
    ))
}

fn unreadable() -> ToolError {
    ToolError::msg(concat!(
        "The calendar gave an event that cannot be read safely, so it was not read. ",
        "Retry later or ask the user to look at the calendar."
    ))
}

fn unexpected() -> ToolError {
    ToolError::msg("The calendar gave an unexpected answer. Retry later.")
}

fn zone_refused() -> ToolError {
    ToolError::msg(concat!(
        "The calendar did not report the times in the time zone of this Mac, so it was not read. ",
        "Ask the user to check the time zone in System Settings."
    ))
}

fn bad_zone(zone: &str) -> ToolError {
    ToolError::msg(format!(
        "The time zone of this Mac ({}) is not known for Outlook.com calendars, so NOTHING was read or changed. Ask the user to choose another time zone in System Settings.",
        one_line_str(zone, MAX_ZONE)
    ))
}

fn bad_range() -> ToolError {
    ToolError::msg("That range of days cannot be used. Choose another start.")
}

fn changed_while_reading() -> ToolError {
    ToolError::msg("The calendar changed while it was read. Retry.")
}

fn unverifiable() -> ToolError {
    ToolError::msg(
        "The calendar did not say which calendar this event belongs to, so it was not opened.",
    )
}

fn unknown_id() -> ToolError {
    ToolError::Uncertain(
        concat!(
            "The calendar accepted the new event but did not say which one it is. It may or may ",
            "not have been created: ask the user to check the calendar before you try again."
        )
        .to_string(),
    )
}

fn unconfirmed() -> ToolError {
    ToolError::Uncertain(
        concat!(
            "The calendar did not confirm that the event is gone. It may or may not have been ",
            "deleted: ask the user to check the calendar before you try again."
        )
        .to_string(),
    )
}

fn still_there() -> ToolError {
    ToolError::Uncertain(
        concat!(
            "The event is still in the calendar after the delete request. It may or may not ",
            "disappear: ask the user to check the calendar before you try again."
        )
        .to_string(),
    )
}

fn id_ok(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID && !id.contains("..")
}

fn checked_zone(zone: &str) -> ToolResult<&'static str> {
    zones::windows_name(zone).ok_or_else(|| bad_zone(zone))
}

fn checked_calendar(calendar: &str) -> ToolResult<&str> {
    if id_ok(calendar) {
        Ok(calendar)
    } else {
        Err(calendar_missing())
    }
}

fn prefer(zone: &str, text_body: bool) -> String {
    let mut value = format!("outlook.timezone=\"{zone}\", {IDS}");
    if text_body {
        value.push_str(", outlook.body-content-type=\"text\"");
    }
    value
}

fn with_prefer<'a>(api: &Api<'a>, value: &str) -> Api<'a> {
    let mut derived = Api::new(api.transport, api.token.clone(), api.provider);
    for (name, existing) in &api.headers {
        derived = derived.with_header(name, existing);
    }
    derived.with_header("Prefer", value)
}

fn calendar_url(calendar: &str) -> String {
    format!("{BASE}/calendars/{}", encode_component(calendar))
}

fn event_url(uid: &str) -> String {
    format!("{BASE}/events/{}", encode_component(uid))
}

fn view_url(calendar: &str, window: Window, fields: &str) -> ToolResult<String> {
    let (from, to) = window
        .from
        .add_minutes(-MINUTES_PER_DAY)
        .zip(window.to.add_minutes(MINUTES_PER_DAY))
        .ok_or_else(bad_range)?;
    Ok(format!(
        "{}/calendarView?{}&$top={PAGE_SIZE}&$select={fields}",
        calendar_url(calendar),
        query(&[
            ("startDateTime", &times::instant(from)),
            ("endDateTime", &times::instant(to)),
        ]),
    ))
}

fn bounded(text: &str, limit: usize) -> String {
    if text.chars().nth(limit).is_none() {
        text.to_string()
    } else {
        format!("{} [...]", take_chars(text, limit))
    }
}

fn walk(
    api: &Api,
    first: String,
    mut visit: impl FnMut(&Json) -> ToolResult<()>,
) -> ToolResult<()> {
    let mut url = first;
    for page in 1..=MAX_PAGES {
        let reply = api.get_optional(&url)?.ok_or_else(calendar_missing)?;
        let object = reply.as_object().ok_or_else(unexpected)?;
        let items = object
            .get("value")
            .and_then(Json::as_array)
            .ok_or_else(unexpected)?;
        if items.len() > MAX_PAGE_ITEMS {
            return Err(too_many());
        }
        for item in items {
            visit(item)?;
        }
        match object.get("@odata.nextLink") {
            None | Some(Json::Null) => return Ok(()),
            Some(Json::Str(_)) if page == MAX_PAGES => return Err(too_many()),
            Some(Json::Str(next))
                if next.starts_with(ORIGIN) && next.len() <= MAX_LINK && url_allowed(next) =>
            {
                url = next.clone();
            }
            Some(_) => return Err(unexpected()),
        }
    }
    Err(too_many())
}

fn calendar_info(item: &Json) -> Option<CalendarInfo> {
    let object = item.as_object()?;
    let id = object
        .get("id")
        .and_then(Json::as_str)
        .filter(|id| id_ok(id))?;
    let name = one_line_str(
        object
            .get("name")
            .and_then(Json::as_str)
            .unwrap_or_default(),
        MAX_NAME,
    );
    Some(CalendarInfo {
        id: id.to_string(),
        name: if name.is_empty() {
            NO_NAME.to_string()
        } else {
            name
        },
        writable: object.get("canEdit") == Some(&Json::Bool(true)),
    })
}

pub(crate) fn calendars(api: &Api) -> ToolResult<Vec<CalendarInfo>> {
    let url = format!("{BASE}/calendars?$top={MAX_CALENDARS}&$select={CALENDAR_FIELDS}");
    let reply = api.get(&url)?;
    let items = reply
        .as_object()
        .and_then(|object| object.get("value"))
        .and_then(Json::as_array)
        .ok_or_else(unexpected)?;
    Ok(items
        .iter()
        .take(MAX_CALENDARS)
        .filter_map(calendar_info)
        .collect())
}

fn summary_of(object: &Object) -> String {
    let subject = object
        .get("subject")
        .and_then(Json::as_str)
        .unwrap_or_default();
    if subject.trim().is_empty() {
        NO_TITLE.to_string()
    } else {
        bounded(subject, MAX_SUMMARY_READ)
    }
}

fn location_of(object: &Object) -> String {
    object
        .get("location")
        .and_then(Json::as_object)
        .and_then(|location| location.get("displayName"))
        .and_then(Json::as_str)
        .map(|name| bounded(name, MAX_LOCATION_READ))
        .unwrap_or_default()
}

fn attendee_count(object: &Object) -> Option<usize> {
    match object.get("attendees") {
        Some(Json::Arr(items)) => Some(items.len()),
        _ => None,
    }
}

fn span(first: Date, last: Date) -> Option<(DateTime, DateTime)> {
    let end = if last > first {
        last
    } else {
        first.add_days(1)?
    };
    Some((DateTime::midnight(first), DateTime::midnight(end)))
}

struct Row {
    event: RawEvent,
    pending: bool,
}

fn parse_event(object: &Object, zone: &str) -> ToolResult<Row> {
    let uid = object
        .get("id")
        .and_then(Json::as_str)
        .filter(|id| id_ok(id))
        .ok_or_else(unreadable)?;
    let all_day = object
        .get("isAllDay")
        .and_then(Json::as_bool)
        .ok_or_else(unreadable)?;
    let start = times::stamp(object.get("start"), zone).ok_or_else(unreadable)?;
    let end = times::stamp(object.get("end"), zone).ok_or_else(unreadable)?;
    let (first, last, pending) = if all_day {
        match (start.day(), end.day()) {
            (Some(first), Some(last)) => {
                let (first, last) = span(first, last).ok_or_else(unreadable)?;
                (first, last, false)
            }
            _ => (start.at, end.at, true),
        }
    } else {
        match (start.timed(), end.timed()) {
            (Some(first), Some(last)) => (first, last, false),
            _ => return Err(zone_refused()),
        }
    };
    Ok(Row {
        event: RawEvent {
            uid: uid.to_string(),
            summary: summary_of(object),
            start: first,
            end: last,
            all_day,
            location: location_of(object),
            recurrence: String::new(),
            excluded: Vec::new(),
            utc_offset: 0,
            attendees: attendee_count(object),
        },
        pending,
    })
}

fn utc_days(
    api: &Api,
    calendar: &str,
    window: Window,
) -> ToolResult<HashMap<String, (Date, Date)>> {
    let mut found: HashMap<String, (Date, Date)> = HashMap::new();
    walk(api, view_url(calendar, window, UTC_FIELDS)?, |item| {
        let object = item.as_object().ok_or_else(unreadable)?;
        if object.get("isAllDay") != Some(&Json::Bool(true)) {
            return Ok(());
        }
        let uid = object
            .get("id")
            .and_then(Json::as_str)
            .filter(|id| id_ok(id))
            .ok_or_else(unreadable)?;
        let first = times::stamp(object.get("start"), times::UTC)
            .and_then(times::Stamp::utc_day)
            .ok_or_else(zone_refused)?;
        let last = times::stamp(object.get("end"), times::UTC)
            .and_then(times::Stamp::utc_day)
            .ok_or_else(zone_refused)?;
        if found.len() >= MAX_EVENTS {
            return Err(too_many());
        }
        found.insert(uid.to_string(), (first, last));
        Ok(())
    })?;
    Ok(found)
}

pub(crate) fn events(
    api: &Api,
    calendar: &str,
    window: Window,
    zone: &str,
) -> ToolResult<Vec<RawEvent>> {
    let zone = checked_zone(zone)?;
    let calendar = checked_calendar(calendar)?;
    let reading = with_prefer(api, &prefer(zone, false));
    let mut rows: Vec<Row> = Vec::new();
    walk(&reading, view_url(calendar, window, LIST_FIELDS)?, |item| {
        let object = item.as_object().ok_or_else(unreadable)?;
        if object.get("isCancelled") == Some(&Json::Bool(true)) {
            return Ok(());
        }
        let row = parse_event(object, zone)?;
        if row.pending || selected(&row.event, window) {
            if rows.len() >= MAX_EVENTS {
                return Err(too_many());
            }
            rows.push(row);
        }
        Ok(())
    })?;
    if rows.iter().any(|row| row.pending) {
        let shifted = with_prefer(api, &prefer(times::UTC, false));
        let days = utc_days(&shifted, calendar, window)?;
        for row in rows.iter_mut().filter(|row| row.pending) {
            let (first, last) = days.get(&row.event.uid).ok_or_else(changed_while_reading)?;
            let (start, end) = span(*first, *last).ok_or_else(unreadable)?;
            row.event.start = start;
            row.event.end = end;
            row.pending = false;
        }
        rows.retain(|row| selected(&row.event, window));
    }
    Ok(rows.into_iter().map(|row| row.event).collect())
}

struct Current {
    detail: EventDetail,
    cancelled: bool,
    organizer: bool,
}

fn owner_of(object: &Object) -> Option<&str> {
    object
        .get("calendar")
        .and_then(Json::as_object)
        .and_then(|owner| owner.get("id"))
        .and_then(Json::as_str)
}

fn belongs_to(object: &Object, calendar: &str) -> ToolResult<()> {
    match owner_of(object) {
        Some(id) if id == calendar => Ok(()),
        Some(_) => Err(not_found()),
        None => Err(unverifiable()),
    }
}

fn recurring(object: &Object) -> bool {
    let single = object.get("type").and_then(Json::as_str) == Some("singleInstance");
    let series = object
        .get("seriesMasterId")
        .and_then(Json::as_str)
        .is_some_and(|id| !id.is_empty());
    let rule = matches!(object.get("recurrence"), Some(Json::Obj(_)));
    !single || series || rule
}

fn plain(content: &str) -> String {
    take_chars(content, 2 * MAX_NOTES_READ)
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim()
        .to_string()
}

fn notes_of(object: &Object) -> String {
    let Some(body) = object.get("body").and_then(Json::as_object) else {
        return String::new();
    };
    let content = body
        .get("content")
        .and_then(Json::as_str)
        .unwrap_or_default();
    let is_html = body
        .get("contentType")
        .and_then(Json::as_str)
        .is_some_and(|kind| kind.eq_ignore_ascii_case("html"));
    let text = if is_html {
        html::to_text(content)
    } else {
        plain(content)
    };
    bounded(&text, MAX_NOTES_READ)
}

fn people_of(object: &Object) -> Vec<Attendee> {
    let Some(Json::Arr(items)) = object.get("attendees") else {
        return Vec::new();
    };
    items
        .iter()
        .take(MAX_PEOPLE)
        .filter_map(Json::as_object)
        .map(|entry| {
            let address = entry.get("emailAddress").and_then(Json::as_object);
            let part = |key: &str| {
                address
                    .and_then(|address| address.get(key))
                    .and_then(Json::as_str)
                    .unwrap_or_default()
            };
            let status = entry
                .get("status")
                .and_then(Json::as_object)
                .and_then(|status| status.get("response"))
                .and_then(Json::as_str)
                .unwrap_or_default();
            Attendee {
                name: bounded(part("name"), MAX_PERSON),
                email: bounded(part("address"), MAX_PERSON),
                status: bounded(status, MAX_PERSON),
            }
        })
        .collect()
}

fn shifted_days(api: &Api, uid: &str) -> ToolResult<(Date, Date)> {
    let utc = with_prefer(api, &prefer(times::UTC, false));
    let url = format!("{}?$select={SHIFT_FIELDS}", event_url(uid));
    let reply = utc.get_optional(&url)?.ok_or_else(not_found)?;
    let object = reply.as_object().ok_or_else(unexpected)?;
    let day = |key: &str| {
        times::stamp(object.get(key), times::UTC)
            .and_then(times::Stamp::utc_day)
            .ok_or_else(zone_refused)
    };
    Ok((day("start")?, day("end")?))
}

fn read_event(api: &Api, calendar: &str, uid: &str, zone: &str) -> ToolResult<Current> {
    if !id_ok(uid) {
        return Err(not_found());
    }
    let reading = with_prefer(api, &prefer(zone, true));
    let url = format!("{}?$select={DETAIL_FIELDS}&{WITH_CALENDAR}", event_url(uid));
    let reply = reading.get_optional(&url)?.ok_or_else(not_found)?;
    let object = reply.as_object().ok_or_else(unexpected)?;
    belongs_to(object, calendar)?;
    let mut row = parse_event(object, zone)?;
    if row.pending {
        let (first, last) = shifted_days(api, uid)?;
        let (start, end) = span(first, last).ok_or_else(unreadable)?;
        row.event.start = start;
        row.event.end = end;
    }
    if recurring(object) {
        row.event.recurrence = RECURRING.to_string();
    }
    Ok(Current {
        detail: EventDetail {
            event: row.event,
            notes: notes_of(object),
            url: String::new(),
            people: people_of(object),
        },
        cancelled: object.get("isCancelled") == Some(&Json::Bool(true)),
        organizer: object.get("isOrganizer") == Some(&Json::Bool(true)),
    })
}

pub(crate) fn event(api: &Api, calendar: &str, uid: &str, zone: &str) -> ToolResult<EventDetail> {
    let zone = checked_zone(zone)?;
    let calendar = checked_calendar(calendar)?;
    let current = read_event(api, calendar, uid, zone)?;
    if current.cancelled {
        return Err(cancelled_read());
    }
    Ok(current.detail)
}

fn ensure_writable(api: &Api, calendar: &str) -> ToolResult<()> {
    let url = format!("{}?$select=canEdit", calendar_url(calendar));
    let reply = api.get_optional(&url)?.ok_or_else(calendar_missing)?;
    if reply.as_object().and_then(|object| object.get("canEdit")) == Some(&Json::Bool(true)) {
        Ok(())
    } else {
        Err(read_only())
    }
}

fn times_ok(fields: &EventFields) -> bool {
    if fields.all_day {
        fields.start.minute_of_day() == 0
            && fields.end.minute_of_day() == 0
            && fields.end > fields.start
    } else {
        fields.end >= fields.start
    }
}

fn checked_fields(after: &EventFields, approval: &Approval) -> ToolResult<()> {
    if !after.url.is_empty() {
        return Err(no_links());
    }
    let limit = if approval.undo {
        MAX_KEPT_NOTES
    } else {
        CREATE_NOTES
    };
    if char_len(&after.notes) > limit {
        return Err(notes_too_long());
    }
    if !times_ok(after) {
        return Err(bad_times());
    }
    Ok(())
}

fn moment_json(at: DateTime, zone: &str) -> Json {
    Json::object(vec![
        ("dateTime", Json::from(times::local_stamp(at))),
        ("timeZone", Json::from(zone)),
    ])
}

fn text_body(notes: &str) -> Json {
    Json::object(vec![
        ("contentType", Json::from("text")),
        ("content", Json::from(notes)),
    ])
}

fn place(location: &str) -> Json {
    Json::object(vec![("displayName", Json::from(location))])
}

fn create_body(after: &EventFields, zone: &str) -> Json {
    Json::object(vec![
        ("subject", Json::from(&after.summary)),
        ("body", text_body(&after.notes)),
        ("location", place(&after.location)),
        ("isAllDay", Json::Bool(after.all_day)),
        ("start", moment_json(after.start, zone)),
        ("end", moment_json(after.end, zone)),
    ])
}

fn patch_body(before: &EventFields, after: &EventFields, zone: &str) -> Object {
    let mut patch = Object::new();
    if before.summary != after.summary {
        patch.insert("subject", after.summary.as_str());
    }
    if before.location != after.location {
        patch.insert("location", place(&after.location));
    }
    if before.notes != after.notes {
        patch.insert("body", text_body(&after.notes));
    }
    if before.start != after.start || before.end != after.end {
        patch.insert("isAllDay", Json::Bool(after.all_day));
        patch.insert("start", moment_json(after.start, zone));
        patch.insert("end", moment_json(after.end, zone));
    }
    patch
}

enum Plan {
    Create(Json),
    Update { uid: String, patch: Json },
    Delete { uid: String },
}

fn checked_uid(uid: &str) -> ToolResult<()> {
    if id_ok(uid) {
        Ok(())
    } else {
        Err(not_found())
    }
}

fn plan(change: &Change, approval: &Approval, zone: &str) -> ToolResult<Plan> {
    match change {
        Change::Create { after } => {
            checked_fields(after, approval)?;
            Ok(Plan::Create(create_body(after, zone)))
        }
        Change::Update { uid, before, after } => {
            checked_uid(uid)?;
            checked_fields(after, approval)?;
            if before.all_day != after.all_day {
                return Err(no_all_day_switch());
            }
            let patch = patch_body(before, after, zone);
            if patch.is_empty() {
                return Err(nothing_to_change());
            }
            Ok(Plan::Update {
                uid: uid.clone(),
                patch: Json::Obj(patch),
            })
        }
        Change::Delete { uid, .. } => {
            checked_uid(uid)?;
            Ok(Plan::Delete { uid: uid.clone() })
        }
    }
}

fn check_current(api: &Api, calendar: &str, zone: &str, change: &Change) -> ToolResult<()> {
    let (Some(uid), Some(before)) = (change.uid(), change.before()) else {
        return Ok(());
    };
    let current = read_event(api, calendar, uid, zone)?;
    if current.cancelled {
        return Err(cancelled_refused());
    }
    if !current.detail.event.recurrence.is_empty() {
        return Err(recurring_refused());
    }
    if current.detail.event.attendees != Some(0) {
        return Err(attendees_refused());
    }
    if !current.organizer {
        return Err(not_organizer());
    }
    if EventFields::of(&current.detail) != *before {
        return Err(changed());
    }
    Ok(())
}

fn unapplied(decision: Decision) -> ChangeResult {
    ChangeResult {
        decision,
        uid: None,
        after: None,
    }
}

fn read_back(api: &Api, calendar: &str, uid: &str, zone: &str) -> Option<EventFields> {
    read_event(api, calendar, uid, zone)
        .ok()
        .filter(|current| !current.cancelled)
        .map(|current| EventFields::of(&current.detail))
}

fn still_in(object: &Object, calendar: &str) -> bool {
    object.get("isCancelled") != Some(&Json::Bool(true)) && owner_of(object) == Some(calendar)
}

fn confirm_gone(api: &Api, calendar: &str, uid: &str) -> ToolResult<()> {
    let reading = with_prefer(api, IDS);
    let url = format!(
        "{}?$select={PRESENCE_FIELDS}&{WITH_CALENDAR}",
        event_url(uid)
    );
    match reading.get_optional(&url) {
        Ok(None) => Ok(()),
        Ok(Some(reply)) => match reply.as_object() {
            Some(object) if still_in(object, calendar) => Err(still_there()),
            Some(_) => Ok(()),
            None => Err(unconfirmed()),
        },
        Err(_) => Err(unconfirmed()),
    }
}

fn apply(api: &Api, calendar: &str, zone: &str, plan: &Plan) -> ToolResult<ChangeResult> {
    let writing = with_prefer(api, IDS);
    match plan {
        Plan::Create(body) => {
            let url = format!("{}/events", calendar_url(calendar));
            let answer = writing.send(Method::Post, &url, Some(body))?;
            let uid = answer
                .as_object()
                .and_then(|object| object.get("id"))
                .and_then(Json::as_str)
                .filter(|id| id_ok(id))
                .ok_or_else(unknown_id)?;
            Ok(ChangeResult {
                decision: Decision::Applied,
                uid: Some(uid.to_string()),
                after: read_back(api, calendar, uid, zone),
            })
        }
        Plan::Update { uid, patch } => {
            writing.send(Method::Patch, &event_url(uid), Some(patch))?;
            Ok(ChangeResult {
                decision: Decision::Applied,
                uid: Some(uid.clone()),
                after: read_back(api, calendar, uid, zone),
            })
        }
        Plan::Delete { uid } => {
            writing.send(Method::Delete, &event_url(uid), None)?;
            confirm_gone(api, calendar, uid)?;
            Ok(ChangeResult {
                decision: Decision::Applied,
                uid: Some(uid.clone()),
                after: None,
            })
        }
    }
}

pub(crate) fn change(
    api: &Api,
    host: &dyn Host,
    calendar: &Calendar,
    provider_calendar: &str,
    zone: &str,
    change: &Change,
    approval: &Approval,
) -> ToolResult<ChangeResult> {
    let zone = checked_zone(zone)?;
    let provider = checked_calendar(provider_calendar)?;
    let plan = plan(change, approval, zone)?;
    ensure_writable(api, provider)?;
    check_current(api, provider, zone, change)?;
    match approve_change(host, &calendar.label, change, approval)? {
        Verdict::Approved => {}
        Verdict::Declined => return Ok(unapplied(Decision::Declined)),
        Verdict::TimedOut => return Ok(unapplied(Decision::TimedOut)),
    }
    ensure_writable(api, provider)?;
    check_current(api, provider, zone, change)?;
    apply(api, provider, zone, &plan)
}
