mod all_day;
mod changes;
mod detail;
mod edits;
mod listing;

use super::*;
use crate::http::scripted::{ok, Scripted};
use crate::http::{HttpError, Request, Response};
use crate::json;
use crate::osascript::{App, Finished};
use crate::source::Source;
use crate::testutil::{j, TestResult};
use std::sync::Mutex;
use std::time::Duration;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

const ZONE: &str = "Europe/Zurich";
const CAL: &str = "AQMkADAwATM3ZmYAZS0wYzBhLTQ4ADgxLTAwAi0wMAoARgAAA-cal==";
const CAL_ENC: &str = "AQMkADAwATM3ZmYAZS0wYzBhLTQ4ADgxLTAwAi0wMAoARgAAA-cal%3D%3D";
const OTHER_CAL: &str = "AQMkADAwATM3ZmYAZS0wYzBhLTQ4ADgxLTAwAi0wMAoARgAAA-other==";
const EV: &str = "AAkALgAAAAAAHYQDEapmEc2byACqAC-EWg0AqGpjeuZLQESC3qXtNlA-AgAAAAAAAQAAA=";
const EV_ENC: &str = "AAkALgAAAAAAHYQDEapmEc2byACqAC-EWg0AqGpjeuZLQESC3qXtNlA-AgAAAAAAAQAAA%3D";
const NEW: &str = "AAkALgAAAAAAHYQDEapmEc2byACqAC-EWg0AqGpjeuZLQESC3qXtNlA-AgAAAAAAAQAAB=";
const NEW_ENC: &str = "AAkALgAAAAAAHYQDEapmEc2byACqAC-EWg0AqGpjeuZLQESC3qXtNlA-AgAAAAAAAQAAB%3D";
const GRAPH: &str = "https://graph.microsoft.com/v1.0/me";
const PREFER: &str = "outlook.timezone=\"Europe/Zurich\", IdType=\"ImmutableId\"";
const PREFER_TEXT: &str = concat!(
    "outlook.timezone=\"Europe/Zurich\", IdType=\"ImmutableId\", ",
    "outlook.body-content-type=\"text\""
);
const PREFER_UTC: &str = "outlook.timezone=\"UTC\", IdType=\"ImmutableId\"";
const PREFER_IDS: &str = "IdType=\"ImmutableId\"";
const RECURRENCE: &str = concat!(
    "{\"pattern\": {\"type\": \"weekly\", \"interval\": 1, \"daysOfWeek\": [\"friday\"], ",
    "\"firstDayOfWeek\": \"sunday\", \"index\": \"first\"}, \"range\": {\"type\": \"noEnd\", ",
    "\"startDate\": \"2026-01-02\", \"recurrenceTimeZone\": \"W. Europe Standard Time\"}}"
);

fn q(text: &str) -> String {
    json::dumps(&Json::from(text), true)
}

fn api(transport: &Scripted) -> Api<'_> {
    Api::new(transport, "TOKEN".to_string(), Source::Microsoft)
}

fn dt(text: &str) -> Res<DateTime> {
    DateTime::parse(text).ok_or_else(|| format!("bad time {text}").into())
}

fn week() -> Res<Window> {
    Ok(Window {
        from: dt("2026-11-20 00:00")?,
        to: dt("2026-11-27 00:00")?,
    })
}

fn refusal<T: std::fmt::Debug>(result: ToolResult<T>) -> Res<ToolError> {
    match result {
        Err(error) => Ok(error),
        Ok(value) => Err(format!("expected an error, got {value:?}").into()),
    }
}

fn header<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

fn calls(transport: &Scripted) -> Vec<(Method, String)> {
    transport
        .seen()
        .into_iter()
        .map(|request| (request.method, request.url))
        .collect()
}

fn writes(transport: &Scripted) -> Vec<Request> {
    transport
        .seen()
        .into_iter()
        .filter(|request| request.method != Method::Get)
        .collect()
}

fn answer(status: u16, body: &str) -> Result<Response, HttpError> {
    Ok(Response {
        status,
        body: body.to_string(),
    })
}

fn list_url() -> String {
    format!(
        concat!(
            "{}/calendars/{}/calendarView?startDateTime=2026-11-19T00%3A00%3A00Z",
            "&endDateTime=2026-11-28T00%3A00%3A00Z&$top=100",
            "&$select=id,subject,start,end,isAllDay,location,attendees,isCancelled"
        ),
        GRAPH, CAL_ENC
    )
}

fn utc_url() -> String {
    format!(
        concat!(
            "{}/calendars/{}/calendarView?startDateTime=2026-11-19T00%3A00%3A00Z",
            "&endDateTime=2026-11-28T00%3A00%3A00Z&$top=100&$select=id,start,end,isAllDay"
        ),
        GRAPH, CAL_ENC
    )
}

fn detail_url(id: &str) -> String {
    format!(
        concat!(
            "{}/events/{}?$select=id,subject,start,end,isAllDay,location,body,type,",
            "seriesMasterId,recurrence,attendees,isCancelled,isOrganizer",
            "&$expand=calendar($select=id)"
        ),
        GRAPH, id
    )
}

fn shift_url(id: &str) -> String {
    format!("{GRAPH}/events/{id}?$select=id,start,end")
}

fn presence_url(id: &str) -> String {
    format!("{GRAPH}/events/{id}?$select=id,isCancelled&$expand=calendar($select=id)")
}

fn editable_url() -> String {
    format!("{GRAPH}/calendars/{CAL_ENC}?$select=canEdit")
}

#[derive(Clone)]
struct Ev {
    id: &'static str,
    subject: String,
    start: (&'static str, &'static str),
    end: (&'static str, &'static str),
    all_day: bool,
    location: String,
    body: Option<(&'static str, String)>,
    kind: &'static str,
    series: Option<&'static str>,
    recurrence: bool,
    attendees: Option<usize>,
    cancelled: bool,
    organizer: bool,
    calendar: Option<&'static str>,
}

impl Default for Ev {
    fn default() -> Ev {
        Ev {
            id: EV,
            subject: "Dentist".to_string(),
            start: ("2026-11-20T09:00:00.0000000", ZONE),
            end: ("2026-11-20T10:00:00.0000000", ZONE),
            all_day: false,
            location: "Praxis".to_string(),
            body: Some(("text", "Bring the card".to_string())),
            kind: "singleInstance",
            series: None,
            recurrence: false,
            attendees: Some(0),
            cancelled: false,
            organizer: true,
            calendar: Some(CAL),
        }
    }
}

fn person(number: usize) -> String {
    format!(
        concat!(
            "{{\"type\": \"required\", \"status\": {{\"response\": \"accepted\", ",
            "\"time\": \"2026-11-01T10:00:00Z\"}}, \"emailAddress\": ",
            "{{\"name\": \"Person {}\", \"address\": \"p{}@example.invalid\"}}}}"
        ),
        number, number
    )
}

impl Ev {
    fn json(&self) -> String {
        let mut parts = vec![
            format!("\"id\": {}", q(self.id)),
            format!("\"subject\": {}", q(&self.subject)),
            format!("\"isAllDay\": {}", self.all_day),
            format!("\"isCancelled\": {}", self.cancelled),
            format!("\"isOrganizer\": {}", self.organizer),
            format!("\"type\": {}", q(self.kind)),
            format!(
                "\"seriesMasterId\": {}",
                self.series.map_or("null".to_string(), q)
            ),
            format!(
                "\"recurrence\": {}",
                if self.recurrence { RECURRENCE } else { "null" }
            ),
            format!(
                "\"start\": {{\"dateTime\": {}, \"timeZone\": {}}}",
                q(self.start.0),
                q(self.start.1)
            ),
            format!(
                "\"end\": {{\"dateTime\": {}, \"timeZone\": {}}}",
                q(self.end.0),
                q(self.end.1)
            ),
            format!(
                "\"location\": {{\"displayName\": {}, \"locationType\": \"default\"}}",
                q(&self.location)
            ),
        ];
        if let Some((kind, content)) = &self.body {
            parts.push(format!(
                "\"body\": {{\"contentType\": {}, \"content\": {}}}",
                q(kind),
                q(content)
            ));
        }
        if let Some(count) = self.attendees {
            let people: Vec<String> = (0..count).map(person).collect();
            parts.push(format!("\"attendees\": [{}]", people.join(", ")));
        }
        if let Some(id) = self.calendar {
            parts.push(format!("\"calendar\": {{\"id\": {}}}", q(id)));
        }
        format!("{{{}}}", parts.join(", "))
    }

    fn all_day(
        id: &'static str,
        first: &'static str,
        after: &'static str,
        label: &'static str,
    ) -> Ev {
        Ev {
            id,
            subject: "Holiday".to_string(),
            start: (first, label),
            end: (after, label),
            all_day: true,
            location: String::new(),
            body: None,
            ..Ev::default()
        }
    }
}

fn utc_item(id: &str, all_day: bool, start: &str, end: &str) -> String {
    format!(
        concat!(
            "{{\"id\": {}, \"isAllDay\": {}, \"start\": {{\"dateTime\": {}, \"timeZone\": \"UTC\"}}, ",
            "\"end\": {{\"dateTime\": {}, \"timeZone\": \"UTC\"}}}}"
        ),
        q(id),
        all_day,
        q(start),
        q(end)
    )
}

fn page(items: &[String]) -> String {
    format!("{{\"value\": [{}]}}", items.join(", "))
}

fn page_with_next(items: &[String], next: &str) -> String {
    format!(
        "{{\"value\": [{}], \"@odata.nextLink\": {}}}",
        items.join(", "),
        q(next)
    )
}

fn editable(transport: Scripted, can_edit: bool) -> Scripted {
    transport.json(
        Method::Get,
        "?$select=canEdit",
        200,
        &format!("{{\"@odata.context\": \"x\", \"canEdit\": {can_edit}}}"),
    )
}

fn calendar() -> Calendar {
    Calendar {
        id: format!("microsoft:me@outlook.example:{CAL}"),
        name: "Calendar".to_string(),
        label: "Calendar".to_string(),
    }
}

struct Dialogs<'a> {
    decision: &'static str,
    transport: &'a Scripted,
    shown: Mutex<Vec<String>>,
    before: Mutex<Vec<usize>>,
}

impl<'a> Dialogs<'a> {
    fn new(decision: &'static str, transport: &'a Scripted) -> Dialogs<'a> {
        Dialogs {
            decision,
            transport,
            shown: Mutex::new(Vec::new()),
            before: Mutex::new(Vec::new()),
        }
    }

    fn count(&self) -> usize {
        self.shown.lock().map(|shown| shown.len()).unwrap_or(0)
    }

    fn requests_before_the_dialog(&self) -> Vec<usize> {
        self.before
            .lock()
            .map(|before| before.clone())
            .unwrap_or_default()
    }

    fn payload(&self) -> Res<Json> {
        let shown = self.shown.lock().map_err(|_| "lock")?;
        let first = shown.first().ok_or("no dialog was shown")?;
        Ok(json::parse(first)?)
    }
}

impl Host for Dialogs<'_> {
    fn ensure_running(&self, _app: App) {}

    fn run(&self, _app: App, payload: &str, _timeout: Duration) -> Finished {
        if let Ok(mut shown) = self.shown.lock() {
            shown.push(payload.to_string());
        }
        if let Ok(mut before) = self.before.lock() {
            before.push(self.transport.seen().len());
        }
        Finished::Done {
            code: Some(0),
            stdout: format!(
                "{{\"ok\": true, \"data\": {{\"decision\": \"{}\"}}}}\n",
                self.decision
            ),
            stderr: String::new(),
        }
    }
}

struct Broken;

impl Host for Broken {
    fn ensure_running(&self, _app: App) {}

    fn run(&self, _app: App, _payload: &str, _timeout: Duration) -> Finished {
        Finished::NotStarted
    }
}

fn run<'a>(
    transport: &'a Scripted,
    decision: &'static str,
    edit: &Change,
    undo: bool,
) -> (ToolResult<ChangeResult>, Dialogs<'a>) {
    let host = Dialogs::new(decision, transport);
    let approval = Approval { seconds: 75, undo };
    let result = change(
        &api(transport),
        &host,
        &calendar(),
        CAL,
        ZONE,
        edit,
        &approval,
    );
    (result, host)
}

fn fields(
    summary: &str,
    start: &str,
    end: &str,
    all_day: bool,
    location: &str,
    notes: &str,
) -> Res<EventFields> {
    Ok(EventFields {
        summary: summary.to_string(),
        start: dt(start)?,
        end: dt(end)?,
        all_day,
        location: location.to_string(),
        notes: notes.to_string(),
        url: String::new(),
    })
}

fn raw(
    uid: &str,
    summary: &str,
    start: &str,
    end: &str,
    all_day: bool,
    location: &str,
    attendees: Option<usize>,
) -> Res<RawEvent> {
    Ok(RawEvent {
        uid: uid.to_string(),
        summary: summary.to_string(),
        start: dt(start)?,
        end: dt(end)?,
        all_day,
        location: location.to_string(),
        recurrence: String::new(),
        excluded: Vec::new(),
        utc_offset: 0,
        attendees,
    })
}

fn dentist() -> Res<EventFields> {
    fields(
        "Dentist",
        "2026-11-20 09:00",
        "2026-11-20 10:00",
        false,
        "Praxis",
        "Bring the card",
    )
}

#[test]
fn the_fixture_event_parses_as_the_fixture_fields() -> TestResult {
    let value = j(&Ev::default().json())?;
    let object = value.as_object().ok_or("object")?;
    assert_eq!(object.get("id").and_then(Json::as_str), Some(EV));
    let row = parse_event(object, ZONE)?;
    assert_eq!(row.event.summary, "Dentist");
    assert!(!row.pending);
    assert_eq!(row.event.attendees, Some(0));
    Ok(())
}
