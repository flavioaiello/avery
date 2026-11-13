use crate::calendar::{
    Approval, Calendar, Change, ChangeResult, Decision, EventFields, MAX_KEPT_LOCATION,
    MAX_KEPT_NOTES, MAX_KEPT_SUMMARY,
};
use crate::civil::DateTime;
use crate::error::{ToolError, ToolResult};
use crate::html;
use crate::http::Method;
use crate::json::{Json, Object};
use crate::osascript::Host;
use crate::provider::dialog::{approve_change, Verdict};
use crate::text::char_len;

use super::{calendar_gone, event_gone, segment, unexpected, usable_id, wire, Target, BASE};

const MAX_NEW_NOTES: usize = 1200;

enum Plan {
    Create {
        url: String,
        body: Json,
    },
    Update {
        uid: String,
        url: String,
        body: Json,
    },
    Delete {
        uid: String,
        url: String,
    },
}

fn read_only() -> ToolError {
    ToolError::msg("This calendar is read-only, so NOTHING was changed.")
}

fn links_unsupported() -> ToolError {
    ToolError::msg("Links are not supported for this kind of calendar, so NOTHING was changed.")
}

fn notes_too_long() -> ToolError {
    ToolError::msg("The notes are too long, so NOTHING was changed.")
}

fn text_too_long() -> ToolError {
    ToolError::msg("The title or place is too long, so NOTHING was changed.")
}

fn all_day_fixed() -> ToolError {
    ToolError::msg("The all-day setting of an event cannot be changed, so NOTHING was changed.")
}

fn nothing_to_change() -> ToolError {
    ToolError::msg("Nothing to change: the event already has these values, or no field was given.")
}

fn recurring() -> ToolError {
    ToolError::msg(concat!(
        "This is a recurring event; only single events can be changed or deleted here. ",
        "NOTHING was changed. The user has to do it in the calendar."
    ))
}

fn has_attendees() -> ToolError {
    ToolError::msg(concat!(
        "This event has attendees; changing or deleting it would notify other people, so ",
        "NOTHING was changed. The user has to do it in the calendar."
    ))
}

fn special_kind() -> ToolError {
    ToolError::msg(concat!(
        "This kind of event (out of office, focus time, working location, birthday, from Gmail ",
        "or locked) cannot be changed here, so NOTHING was changed. ",
        "The user has to do it in the calendar."
    ))
}

fn changed() -> ToolError {
    ToolError::msg(concat!(
        "The event changed since you read it or while the approval dialog was open, ",
        "so NOTHING was changed. Read it again with get_event."
    ))
}

fn created_unsure() -> ToolError {
    ToolError::Uncertain(
        concat!(
            "The calendar answered, but not with the new event. The event may or may not have ",
            "been created: ask the user to check the calendar before you try again."
        )
        .to_string(),
    )
}

fn gone_unsure() -> ToolError {
    ToolError::Uncertain(
        concat!(
            "The calendar did not confirm that the event is gone. It may or may not have been ",
            "deleted: ask the user to check the calendar before you try again."
        )
        .to_string(),
    )
}

fn ensure_writable(target: &Target) -> ToolResult<()> {
    let id = segment(target.calendar).ok_or_else(calendar_gone)?;
    let url = format!("{BASE}/users/me/calendarList/{id}");
    let entry = target.api.get_optional(&url)?.ok_or_else(calendar_gone)?;
    let role = entry
        .as_object()
        .ok_or_else(unexpected)?
        .get("accessRole")
        .and_then(Json::as_str);
    match role {
        Some("owner" | "writer") => Ok(()),
        _ => Err(read_only()),
    }
}

fn moment(at: DateTime, all_day: bool, zone: &str) -> Json {
    if all_day {
        Json::object(vec![("date", Json::from(at.date().to_string()))])
    } else {
        Json::object(vec![
            (
                "dateTime",
                Json::from(format!("{}T{}:00", at.date(), at.clock())),
            ),
            ("timeZone", Json::from(zone)),
        ])
    }
}

fn create_body(after: &EventFields, zone: &str) -> Json {
    let mut body = Object::new();
    body.insert("summary", after.summary.as_str());
    if !after.location.is_empty() {
        body.insert("location", after.location.as_str());
    }
    if !after.notes.is_empty() {
        body.insert("description", html::escape(&after.notes));
    }
    body.insert("start", moment(after.start, after.all_day, zone));
    body.insert("end", moment(after.end, after.all_day, zone));
    Json::Obj(body)
}

fn update_body(before: &EventFields, after: &EventFields, zone: &str) -> ToolResult<Json> {
    if before.all_day != after.all_day {
        return Err(all_day_fixed());
    }
    let mut body = Object::new();
    if before.summary != after.summary {
        body.insert("summary", after.summary.as_str());
    }
    if before.location != after.location {
        body.insert("location", after.location.as_str());
    }
    if before.notes != after.notes {
        body.insert("description", html::escape(&after.notes));
    }
    if before.start != after.start || before.end != after.end {
        body.insert("start", moment(after.start, after.all_day, zone));
        body.insert("end", moment(after.end, after.all_day, zone));
    }
    if body.is_empty() {
        Err(nothing_to_change())
    } else {
        Ok(Json::Obj(body))
    }
}

fn check_fields(after: &EventFields, notes_limit: usize) -> ToolResult<()> {
    if !after.url.is_empty() {
        return Err(links_unsupported());
    }
    if char_len(&after.summary) > MAX_KEPT_SUMMARY || char_len(&after.location) > MAX_KEPT_LOCATION
    {
        return Err(text_too_long());
    }
    if char_len(&after.notes) > notes_limit {
        return Err(notes_too_long());
    }
    Ok(())
}

fn plan(target: &Target, change: &Change, approval: &Approval) -> ToolResult<Plan> {
    match change {
        Change::Create { after } => {
            let limit = if approval.undo {
                MAX_KEPT_NOTES
            } else {
                MAX_NEW_NOTES
            };
            check_fields(after, limit)?;
            Ok(Plan::Create {
                url: target.write_url(None)?,
                body: create_body(after, target.zone),
            })
        }
        Change::Update { uid, before, after } => {
            check_fields(after, MAX_KEPT_NOTES)?;
            let body = update_body(before, after, target.zone)?;
            Ok(Plan::Update {
                uid: uid.clone(),
                url: target.write_url(Some(uid))?,
                body,
            })
        }
        Change::Delete { uid, .. } => Ok(Plan::Delete {
            uid: uid.clone(),
            url: target.write_url(Some(uid))?,
        }),
    }
}

fn verified(target: &Target, uid: &str, before: &EventFields) -> ToolResult<()> {
    let object = target.lookup(uid)?.ok_or_else(event_gone)?;
    let detail = wire::detail(&object)?;
    if !detail.event.recurrence.is_empty() {
        return Err(recurring());
    }
    if detail.event.attendees != Some(0) {
        return Err(has_attendees());
    }
    if wire::special(&object) {
        return Err(special_kind());
    }
    if EventFields::of(&detail) != *before {
        return Err(changed());
    }
    Ok(())
}

fn recheck(target: &Target, change: &Change) -> ToolResult<()> {
    match change {
        Change::Create { .. } => Ok(()),
        Change::Update { uid, before, .. } | Change::Delete { uid, before } => {
            verified(target, uid, before)
        }
    }
}

fn applied(uid: String, after: Option<EventFields>) -> ChangeResult {
    ChangeResult {
        decision: Decision::Applied,
        uid: Some(uid),
        after,
    }
}

fn stopped(decision: Decision) -> ChangeResult {
    ChangeResult {
        decision,
        uid: None,
        after: None,
    }
}

fn created(answer: &Json) -> ToolResult<ChangeResult> {
    let object = answer.as_object().ok_or_else(created_unsure)?;
    let uid = object
        .get("id")
        .and_then(Json::as_str)
        .filter(|id| usable_id(id))
        .ok_or_else(created_unsure)?;
    let after = wire::detail(object)
        .ok()
        .map(|detail| EventFields::of(&detail));
    Ok(applied(uid.to_string(), after))
}

fn read_back(target: &Target, uid: &str) -> Option<EventFields> {
    let object = target.lookup(uid).ok()??;
    wire::detail(&object)
        .ok()
        .map(|detail| EventFields::of(&detail))
}

fn confirm_gone(target: &Target, uid: &str) -> ToolResult<()> {
    match target.lookup(uid) {
        Ok(None) => Ok(()),
        Ok(Some(_)) | Err(_) => Err(gone_unsure()),
    }
}

fn apply(target: &Target, plan: Plan) -> ToolResult<ChangeResult> {
    match plan {
        Plan::Create { url, body } => created(&target.api.send(Method::Post, &url, Some(&body))?),
        Plan::Update { uid, url, body } => {
            target.api.send(Method::Patch, &url, Some(&body))?;
            let after = read_back(target, &uid);
            Ok(applied(uid, after))
        }
        Plan::Delete { uid, url } => {
            target.api.send(Method::Delete, &url, None)?;
            confirm_gone(target, &uid)?;
            Ok(applied(uid, None))
        }
    }
}

pub(super) fn run(
    target: &Target,
    host: &dyn Host,
    calendar: &Calendar,
    change: &Change,
    approval: &Approval,
) -> ToolResult<ChangeResult> {
    ensure_writable(target)?;
    let plan = plan(target, change, approval)?;
    recheck(target, change)?;
    match approve_change(host, &calendar.label, change, approval)? {
        Verdict::Approved => {}
        Verdict::Declined => return Ok(stopped(Decision::Declined)),
        Verdict::TimedOut => return Ok(stopped(Decision::TimedOut)),
    }
    recheck(target, change)?;
    apply(target, plan)
}
