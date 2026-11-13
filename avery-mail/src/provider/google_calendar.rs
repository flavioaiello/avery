mod wire;
mod write;

#[cfg(test)]
mod tests;

use super::Api;
use crate::calendar::{
    selected, Approval, Calendar, CalendarInfo, Change, ChangeResult, EventDetail, RawEvent,
    Window, MAX_SELECTED_EVENTS,
};
use crate::civil::DateTime;
use crate::error::{ToolError, ToolResult};
use crate::http::{encode_component, query, Method};
use crate::json::{Json, Object};
use crate::osascript::Host;

const BASE: &str = "https://www.googleapis.com/calendar/v3";
const LIST_FIELDS: &str =
    "nextPageToken,items(id,status,summary,location,start,end,attendees(email),attendeesOmitted)";
const PAGE_SIZE: usize = 250;
const MAX_CALENDAR_PAGES: usize = 3;
const MAX_CALENDARS: usize = 100;
const MAX_EVENT_PAGES: usize = 4;
const MAX_ID: usize = 512;
const MAX_PAGE_TOKEN: usize = 512;
const DAY_MINUTES: i64 = 1440;

fn unexpected() -> ToolError {
    ToolError::msg("The provider gave an unexpected answer.")
}

fn calendar_gone() -> ToolError {
    ToolError::msg(
        "The calendar no longer exists in Google Calendar. Ask the user to run `avery mail setup` again.",
    )
}

fn event_gone() -> ToolError {
    ToolError::msg("Event not found. Use an id from list_events together with the same calendar.")
}

fn too_many() -> ToolError {
    ToolError::msg(concat!(
        "The calendar has too many events in this range to read safely. ",
        "Use a shorter range (days) or a more specific calendar."
    ))
}

fn invalid_range() -> ToolError {
    ToolError::msg("The range is not valid.")
}

fn usable_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID && id != "." && id != ".."
}

fn segment(id: &str) -> Option<String> {
    usable_id(id).then(|| encode_component(id))
}

struct Target<'a, 'b> {
    api: &'a Api<'b>,
    calendar: &'a str,
    zone: &'a str,
}

impl Target<'_, '_> {
    fn calendar_path(&self) -> ToolResult<String> {
        let id = segment(self.calendar).ok_or_else(calendar_gone)?;
        Ok(format!("{BASE}/calendars/{id}"))
    }

    fn event_path(&self, uid: &str) -> ToolResult<String> {
        let id = segment(uid).ok_or_else(event_gone)?;
        Ok(format!("{}/events/{id}", self.calendar_path()?))
    }

    fn read_url(&self, uid: &str) -> ToolResult<String> {
        Ok(format!(
            "{}?{}",
            self.event_path(uid)?,
            query(&[("timeZone", self.zone)])
        ))
    }

    fn write_url(&self, uid: Option<&str>) -> ToolResult<String> {
        let path = match uid {
            Some(uid) => self.event_path(uid)?,
            None => format!("{}/events", self.calendar_path()?),
        };
        Ok(format!("{path}?{}", query(&[("sendUpdates", "none")])))
    }

    fn lookup(&self, uid: &str) -> ToolResult<Option<Object>> {
        let reply = self.api.call(Method::Get, &self.read_url(uid)?, None)?;
        match reply.status {
            200..=299 => {}
            404 | 410 => return Ok(None),
            _ => return Err(self.api.failed(&reply, false)),
        }
        match reply.json {
            Json::Obj(object) if wire::cancelled(&object) => Ok(None),
            Json::Obj(object) => Ok(Some(object)),
            _ => Err(unexpected()),
        }
    }
}

fn fetch_page(api: &Api, url: &str, missing: fn() -> ToolError) -> ToolResult<Object> {
    match api.get_optional(url)?.ok_or_else(missing)? {
        Json::Obj(object) => Ok(object),
        _ => Err(unexpected()),
    }
}

fn page_items(page: &Object, overflow: fn() -> ToolError) -> ToolResult<&[Json]> {
    match page.get("items") {
        None | Some(Json::Null) => Ok(&[]),
        Some(Json::Arr(items)) if items.len() <= PAGE_SIZE => Ok(items.as_slice()),
        Some(Json::Arr(_)) => Err(overflow()),
        Some(_) => Err(unexpected()),
    }
}

fn next_token(page: &Object) -> ToolResult<Option<String>> {
    match page.get("nextPageToken") {
        None | Some(Json::Null) => Ok(None),
        Some(Json::Str(token)) if token.is_empty() => Ok(None),
        Some(Json::Str(token)) if token.len() <= MAX_PAGE_TOKEN => Ok(Some(token.clone())),
        Some(_) => Err(unexpected()),
    }
}

fn paged(url: &str, token: Option<&str>) -> String {
    match token {
        Some(token) => format!("{url}&pageToken={}", encode_component(token)),
        None => url.to_string(),
    }
}

fn utc_stamp(moment: DateTime) -> String {
    format!("{}T{}:00Z", moment.date(), moment.clock())
}

fn events_url(target: &Target, window: Window) -> ToolResult<String> {
    let from = window
        .from
        .add_minutes(-DAY_MINUTES)
        .ok_or_else(invalid_range)?;
    let to = window
        .to
        .add_minutes(DAY_MINUTES)
        .ok_or_else(invalid_range)?;
    let (min, max, size) = (utc_stamp(from), utc_stamp(to), PAGE_SIZE.to_string());
    Ok(format!(
        "{}/events?{}",
        target.calendar_path()?,
        query(&[
            ("singleEvents", "true"),
            ("orderBy", "startTime"),
            ("timeMin", &min),
            ("timeMax", &max),
            ("timeZone", target.zone),
            ("maxResults", &size),
            ("fields", LIST_FIELDS),
        ])
    ))
}

fn too_many_selected(count: usize) -> bool {
    i64::try_from(count).map_or(true, |count| count > MAX_SELECTED_EVENTS)
}

pub(crate) fn calendars(api: &Api) -> ToolResult<Vec<CalendarInfo>> {
    let size = PAGE_SIZE.to_string();
    let url = format!(
        "{BASE}/users/me/calendarList?{}",
        query(&[("minAccessRole", "reader"), ("maxResults", &size)])
    );
    let mut found: Vec<CalendarInfo> = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..MAX_CALENDAR_PAGES {
        let page = fetch_page(api, &paged(&url, token.as_deref()), unexpected)?;
        for item in page_items(&page, unexpected)? {
            let Some(info) = item.as_object().and_then(wire::calendar) else {
                continue;
            };
            if found.iter().any(|known| known.id == info.id) {
                continue;
            }
            found.push(info);
            if found.len() >= MAX_CALENDARS {
                return Ok(found);
            }
        }
        match next_token(&page)? {
            Some(next) => token = Some(next),
            None => break,
        }
    }
    Ok(found)
}

pub(crate) fn events(
    api: &Api,
    calendar: &str,
    window: Window,
    zone: &str,
) -> ToolResult<Vec<RawEvent>> {
    let target = Target {
        api,
        calendar,
        zone,
    };
    let url = events_url(&target, window)?;
    let mut found: Vec<RawEvent> = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..MAX_EVENT_PAGES {
        let page = fetch_page(api, &paged(&url, token.as_deref()), calendar_gone)?;
        for item in page_items(&page, too_many)? {
            let Some(event) = item.as_object().and_then(wire::listed) else {
                continue;
            };
            if !selected(&event, window) {
                continue;
            }
            found.push(event);
            if too_many_selected(found.len()) {
                return Err(too_many());
            }
        }
        match next_token(&page)? {
            Some(next) => token = Some(next),
            None => return Ok(found),
        }
    }
    Err(too_many())
}

pub(crate) fn event(api: &Api, calendar: &str, uid: &str, zone: &str) -> ToolResult<EventDetail> {
    let target = Target {
        api,
        calendar,
        zone,
    };
    match target.lookup(uid)? {
        Some(object) => wire::detail(&object),
        None => Err(event_gone()),
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
    let target = Target {
        api,
        calendar: provider_calendar,
        zone,
    };
    write::run(&target, host, calendar, change, approval)
}
