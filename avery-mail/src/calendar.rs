mod fake;
mod read;
mod real;
mod write;

use crate::civil::{Date, DateTime};
use crate::error::ToolResult;
use crate::journal;
use crate::json::{Json, Object};
use crate::localtime;
use crate::source::Source;
use crate::text::text_of;

pub(crate) use crate::journal::Origin;
pub(crate) use fake::FakeCalendar;
pub(crate) use read::{get_event, list_calendars, list_events};
pub(crate) use real::RealCalendar;
pub(crate) use write::{create_event, delete_event, perform, update_event};

pub(crate) const MAX_SCANNED_EVENTS: i64 = 20_000;
pub(crate) const MAX_SELECTED_EVENTS: i64 = 500;

pub(crate) const MAX_SUMMARY: usize = 200;
pub(crate) const MAX_LOCATION: usize = 200;
pub(crate) const MAX_NOTES: usize = 1000;
pub(crate) const MAX_KEPT_SUMMARY: usize = 500;
pub(crate) const MAX_KEPT_LOCATION: usize = 500;
pub(crate) const MAX_KEPT_NOTES: usize = 4000;
pub(crate) const MAX_KEPT_URL: usize = 500;

pub(crate) trait CalendarBackend {
    fn calendars(&self, source: Source, address: &str) -> ToolResult<Vec<CalendarInfo>>;
    fn events(&self, calendar: &Calendar, window: Window) -> ToolResult<Vec<RawEvent>>;
    fn event(&self, calendar: &Calendar, uid: &str) -> ToolResult<EventDetail>;
    fn change(
        &self,
        calendar: &Calendar,
        change: &Change,
        approval: &Approval,
    ) -> ToolResult<ChangeResult>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Approval {
    pub(crate) seconds: u64,
    pub(crate) undo: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Create,
    Update,
    Delete,
}

impl Action {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Action::Create => "create",
            Action::Update => "update",
            Action::Delete => "delete",
        }
    }

    pub(crate) fn parse(text: &str) -> Option<Action> {
        match text {
            "create" => Some(Action::Create),
            "update" => Some(Action::Update),
            "delete" => Some(Action::Delete),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EventFields {
    pub(crate) summary: String,
    pub(crate) start: DateTime,
    pub(crate) end: DateTime,
    pub(crate) all_day: bool,
    pub(crate) location: String,
    pub(crate) notes: String,
    pub(crate) url: String,
}

impl EventFields {
    pub(crate) fn of(detail: &EventDetail) -> EventFields {
        EventFields {
            summary: detail.event.summary.clone(),
            start: detail.event.start,
            end: detail.event.end,
            all_day: detail.event.all_day,
            location: detail.event.location.clone(),
            notes: detail.notes.clone(),
            url: detail.url.clone(),
        }
    }

    pub(crate) fn fits_the_backup(&self) -> bool {
        self.summary.chars().count() <= MAX_KEPT_SUMMARY
            && self.location.chars().count() <= MAX_KEPT_LOCATION
            && self.notes.chars().count() <= MAX_KEPT_NOTES
            && self.url.chars().count() <= MAX_KEPT_URL
    }

    pub(crate) fn to_json(&self) -> Json {
        Json::object(vec![
            ("summary", Json::from(&self.summary)),
            ("start", Json::from(self.start.to_string())),
            ("end", Json::from(self.end.to_string())),
            ("allDay", Json::Bool(self.all_day)),
            ("location", Json::from(&self.location)),
            ("notes", Json::from(&self.notes)),
            ("url", Json::from(&self.url)),
        ])
    }

    pub(crate) fn from_json(value: &Json) -> Option<EventFields> {
        let object = value.as_object()?;
        let text = |key: &str| object.get(key).and_then(Json::as_str).map(str::to_string);
        Some(EventFields {
            summary: text("summary")?,
            start: DateTime::parse(object.get("start")?.as_str()?)?,
            end: DateTime::parse(object.get("end")?.as_str()?)?,
            all_day: object.get("allDay")?.as_bool()?,
            location: text("location")?,
            notes: text("notes")?,
            url: text("url")?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    Create {
        after: EventFields,
    },
    Update {
        uid: String,
        before: EventFields,
        after: EventFields,
    },
    Delete {
        uid: String,
        before: EventFields,
    },
}

impl Change {
    pub(crate) fn action(&self) -> Action {
        match self {
            Change::Create { .. } => Action::Create,
            Change::Update { .. } => Action::Update,
            Change::Delete { .. } => Action::Delete,
        }
    }

    pub(crate) fn uid(&self) -> Option<&str> {
        match self {
            Change::Create { .. } => None,
            Change::Update { uid, .. } | Change::Delete { uid, .. } => Some(uid),
        }
    }

    pub(crate) fn before(&self) -> Option<&EventFields> {
        match self {
            Change::Create { .. } => None,
            Change::Update { before, .. } | Change::Delete { before, .. } => Some(before),
        }
    }

    pub(crate) fn after(&self) -> Option<&EventFields> {
        match self {
            Change::Create { after } | Change::Update { after, .. } => Some(after),
            Change::Delete { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    Applied,
    Declined,
    TimedOut,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChangeResult {
    pub(crate) decision: Decision,
    pub(crate) uid: Option<String>,
    pub(crate) after: Option<EventFields>,
}

impl ChangeResult {
    pub(crate) fn from_json(value: &Json) -> Option<ChangeResult> {
        let object = value.as_object()?;
        let decision = match object.get("status")?.as_str()? {
            "applied" => Decision::Applied,
            "declined" => Decision::Declined,
            "timeout" => Decision::TimedOut,
            _ => return None,
        };
        Some(ChangeResult {
            decision,
            uid: object
                .get("uid")
                .and_then(Json::as_str)
                .filter(|uid| !uid.is_empty())
                .map(str::to_string),
            after: object.get("after").and_then(EventFields::from_json),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Window {
    pub(crate) from: DateTime,
    pub(crate) to: DateTime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Attendee {
    pub(crate) name: String,
    pub(crate) email: String,
    pub(crate) status: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RawEvent {
    pub(crate) uid: String,
    pub(crate) summary: String,
    pub(crate) start: DateTime,
    pub(crate) end: DateTime,
    pub(crate) all_day: bool,
    pub(crate) location: String,
    pub(crate) recurrence: String,
    pub(crate) excluded: Vec<Date>,
    pub(crate) utc_offset: i64,
    pub(crate) attendees: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EventDetail {
    pub(crate) event: RawEvent,
    pub(crate) notes: String,
    pub(crate) url: String,
    pub(crate) people: Vec<Attendee>,
}

impl RawEvent {
    pub(crate) fn from_json(value: &Json) -> Option<RawEvent> {
        let object = value.as_object()?;
        let uid = text_of(object.get("uid"));
        if uid.is_empty() {
            return None;
        }
        let excluded = match object.get("excluded") {
            Some(Json::Arr(items)) => items
                .iter()
                .filter_map(Json::as_str)
                .filter_map(DateTime::parse)
                .map(DateTime::date)
                .collect(),
            _ => Vec::new(),
        };
        let utc_offset = match object.get("utcOffset") {
            Some(Json::Int(minutes)) if (-1440..=1440).contains(minutes) => *minutes,
            _ => 0,
        };
        let attendees = match object.get("attendees") {
            Some(Json::Int(count)) => usize::try_from(*count).ok(),
            _ => None,
        };
        Some(RawEvent {
            uid,
            summary: text_of(object.get("summary")),
            start: DateTime::parse(object.get("start")?.as_str()?)?,
            end: DateTime::parse(object.get("end")?.as_str()?)?,
            all_day: object.get("allDay") == Some(&Json::Bool(true)),
            location: text_of(object.get("location")),
            recurrence: text_of(object.get("recurrence")),
            excluded,
            utc_offset,
            attendees,
        })
    }
}

impl EventDetail {
    pub(crate) fn from_json(value: &Json) -> Option<EventDetail> {
        let object = value.as_object()?;
        let people = match object.get("people") {
            Some(Json::Arr(items)) => items
                .iter()
                .filter_map(Json::as_object)
                .map(|entry: &Object| Attendee {
                    name: text_of(entry.get("name")),
                    email: text_of(entry.get("email")),
                    status: text_of(entry.get("status")),
                })
                .collect(),
            _ => Vec::new(),
        };
        Some(EventDetail {
            event: RawEvent::from_json(value)?,
            notes: text_of(object.get("notes")),
            url: text_of(object.get("url")),
            people,
        })
    }
}

pub(crate) fn local_today() -> Option<Date> {
    DateTime::parse(&localtime::minute(journal::now_seconds()))
        .map(DateTime::date)
        .or_else(|| Date::from_days(journal::now_seconds().div_euclid(86_400)))
}

pub(crate) fn last_day(start: DateTime, end: DateTime) -> Date {
    let last = if end.minute_of_day() == 0 && end.date() > start.date() {
        end.date().add_days(-1).unwrap_or(end.date())
    } else {
        end.date()
    };
    last.max(start.date())
}

pub(crate) fn selected(event: &RawEvent, window: Window) -> bool {
    if event.start >= window.to {
        return false;
    }
    !event.recurrence.is_empty()
        || event.end > window.from
        || (event.start == event.end && event.start >= window.from)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CalendarInfo {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) writable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CalendarEntry {
    pub(crate) id: String,
    pub(crate) name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Calendar {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) label: String,
}

impl Calendar {
    pub(crate) fn is_named(&self, wanted: &str) -> bool {
        self.label.to_lowercase() == wanted || self.name.to_lowercase() == wanted
    }
}

pub(crate) fn demo_entries() -> Vec<CalendarEntry> {
    fake::calendars()
        .into_iter()
        .map(|info| CalendarEntry {
            id: info.id,
            name: info.name,
        })
        .collect()
}

pub(crate) fn with_labels(entries: Vec<CalendarEntry>) -> Vec<Calendar> {
    let names: Vec<String> = entries.iter().map(|e| e.name.to_lowercase()).collect();
    entries
        .into_iter()
        .map(|entry| {
            let own = entry.name.to_lowercase();
            let duplicate = names.iter().filter(|name| **name == own).count() > 1;
            let label = if duplicate {
                let short: String = entry.id.chars().take(8).collect();
                format!("{} <{short}>", entry.name)
            } else {
                entry.name.clone()
            };
            Calendar {
                id: entry.id,
                name: entry.name,
                label,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, name: &str) -> CalendarEntry {
        CalendarEntry {
            id: id.into(),
            name: name.into(),
        }
    }

    #[test]
    fn labels_are_names_unless_two_calendars_share_one() {
        let calendars = with_labels(vec![entry("A1", "Privat"), entry("B2", "Familie")]);
        assert_eq!(calendars[0].label, "Privat");
        assert_eq!(calendars[1].label, "Familie");
        let twins = with_labels(vec![
            entry("0123456789ABC", "Kalender"),
            entry("ZZ", "kalender"),
            entry("C3", "Other"),
        ]);
        assert_eq!(twins[0].label, "Kalender <01234567>");
        assert_eq!(twins[1].label, "kalender <ZZ>");
        assert_eq!(twins[2].label, "Other");
    }

    #[test]
    fn calendars_are_found_by_label_or_name_ignoring_case() {
        let calendar = with_labels(vec![entry("A1", "Demo Privat")]).remove(0);
        assert!(calendar.is_named("demo privat"));
        assert!(!calendar.is_named("privat"));
    }
}
