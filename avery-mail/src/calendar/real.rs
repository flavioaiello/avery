use crate::calendar::{
    Approval, Calendar, CalendarBackend, CalendarInfo, Change, ChangeResult, EventDetail,
    EventFields, RawEvent, Window, MAX_SCANNED_EVENTS, MAX_SELECTED_EVENTS,
};
use crate::error::{ToolError, ToolResult};
use crate::json::{Json, Object};
use crate::jxa::exchange;
use crate::osascript::{App, Host, OsascriptHost};
use crate::text::{one_line_str, text_of};
use crate::texts::JXA_TIMEOUT;

pub(crate) fn map_error(text: &str, op: Option<&str>) -> String {
    let low = text.to_lowercase();
    let has = |needle: &str| low.contains(needle);
    if has("-1743") || has("not authorized") || has("nicht berechtigt") {
        return concat!(
            "macOS blocks access to Calendar. Ask the user to allow it in System Settings > ",
            "Privacy & Security > Automation (the app that runs this tool, e.g. Goose, ",
            "needs Calendar enabled), then retry."
        )
        .to_string();
    }
    if has("approval_dialog_failed") {
        return concat!(
            "The approval dialog could not be shown, so NOTHING was changed. The user must be logged in ",
            "at the Mac with an unlocked screen."
        )
        .to_string();
    }
    if has("calendar_read_only") {
        return "This calendar is read-only in Calendar, so NOTHING was changed.".to_string();
    }
    if has("recurring_event") {
        return concat!(
            "This is a recurring event; only single events can be changed or deleted here. ",
            "NOTHING was changed. The user has to do it in Calendar."
        )
        .to_string();
    }
    if has("has_attendees") {
        return concat!(
            "This event has attendees; changing or deleting it would notify other people, so ",
            "NOTHING was changed. The user has to do it in Calendar."
        )
        .to_string();
    }
    if has("event_changed") {
        return concat!(
            "The event changed in Calendar since you read it or while the approval dialog was open, ",
            "so NOTHING was changed. Read it again with get_event."
        )
        .to_string();
    }
    if has("notes_too_long") {
        return "The notes are too long, so NOTHING was changed.".to_string();
    }
    if has("change_state_unknown") {
        return "Calendar reported an error while changing the event.".to_string();
    }
    if has("calendar_not_found") {
        return "The calendar no longer exists in Calendar. Ask the user to run `avery mail setup` again."
            .to_string();
    }
    if has("event_not_found") || (op == Some("event") && (has("can't get") || has("invalid index")))
    {
        return "Event not found. Use an id from list_events together with the same calendar."
            .to_string();
    }
    if has("ambiguous_event") {
        return concat!(
            "More than one event has this id (a series with changed occurrences?). ",
            "The user has to look at it in Calendar."
        )
        .to_string();
    }
    if has("calendar_too_large") {
        return concat!(
            "This calendar has too many events to be read safely, whatever the range. ",
            "The user has to tidy it up or to choose another calendar."
        )
        .to_string();
    }
    if has("too_many_events") {
        return concat!(
            "The calendar has too many events in this range to read safely. ",
            "Use a shorter range (days) or a more specific calendar."
        )
        .to_string();
    }
    if has("invalid_local_time") || has("bad_time") {
        return "That local time does not exist (daylight saving change) or is not valid. Choose another time."
            .to_string();
    }
    if has("-1712") || has("timed out") || has("timeout") {
        return "Calendar did not answer in time. Retry with a shorter range (days).".to_string();
    }
    format!("Calendar error: {}", one_line_str(text, 200))
}

pub(crate) fn failure(text: &str, op: Option<&str>, uncertain: bool) -> ToolError {
    let low = text.to_lowercase();
    let timed_out = low.contains("-1712")
        || low.contains("timed out")
        || low.contains("timeout")
        || low.contains("change_state_unknown");
    if op == Some("change") && (uncertain || timed_out) {
        let mut message = map_error(text, op);
        if !message.contains("may or may not") {
            message.push_str(
                " The change may or may not have been applied: ask the user to check Calendar before you try again.",
            );
        }
        return ToolError::Uncertain(message);
    }
    ToolError::Msg(map_error(text, op))
}

pub(crate) struct RealCalendar {
    host: Box<dyn Host>,
}

impl RealCalendar {
    pub(crate) fn new(host: Box<dyn Host>) -> RealCalendar {
        RealCalendar { host }
    }

    pub(crate) fn system() -> RealCalendar {
        RealCalendar::new(Box::new(OsascriptHost))
    }

    fn call(&self, request: Object, timeout_seconds: u64) -> ToolResult<Json> {
        self.host.ensure_running(App::Calendar);
        exchange(
            self.host.as_ref(),
            App::Calendar,
            request,
            timeout_seconds,
            failure,
        )
    }

    fn calendar_request(op: &str, calendar: &Calendar) -> Object {
        let mut request = Object::new();
        request.insert("op", op);
        request.insert("calendarId", calendar.id.as_str());
        request
    }

    fn unexpected(op: &str) -> ToolError {
        failure("unexpected answer from osascript", Some(op), true)
    }
}

impl CalendarBackend for RealCalendar {
    fn calendars(&self) -> ToolResult<Vec<CalendarInfo>> {
        let mut request = Object::new();
        request.insert("op", "calendars");
        let data = self.call(request, JXA_TIMEOUT)?;
        let items = data
            .as_array()
            .ok_or_else(|| Self::unexpected("calendars"))?;
        Ok(items
            .iter()
            .filter_map(Json::as_object)
            .map(|entry| CalendarInfo {
                id: text_of(entry.get("id")),
                name: text_of(entry.get("name")),
                writable: entry.get("writable") == Some(&Json::Bool(true)),
            })
            .collect())
    }

    fn events(&self, calendar: &Calendar, window: Window) -> ToolResult<Vec<RawEvent>> {
        let mut request = Self::calendar_request("events", calendar);
        request.insert("from", window.from.to_string());
        request.insert("to", window.to.to_string());
        request.insert("maxScan", MAX_SCANNED_EVENTS);
        request.insert("maxItems", MAX_SELECTED_EVENTS);
        let data = self.call(request, JXA_TIMEOUT)?;
        let items = data.as_array().ok_or_else(|| Self::unexpected("events"))?;
        Ok(items.iter().filter_map(RawEvent::from_json).collect())
    }

    fn event(&self, calendar: &Calendar, uid: &str) -> ToolResult<EventDetail> {
        let mut request = Self::calendar_request("event", calendar);
        request.insert("uid", uid);
        let data = self.call(request, JXA_TIMEOUT)?;
        EventDetail::from_json(&data).ok_or_else(|| Self::unexpected("event"))
    }

    fn change(
        &self,
        calendar: &Calendar,
        change: &Change,
        approval: &Approval,
    ) -> ToolResult<ChangeResult> {
        let mut request = Self::calendar_request("change", calendar);
        request.insert("calendarLabel", calendar.label.as_str());
        request.insert("action", change.action().as_str());
        request.insert("uid", change.uid().map_or(Json::Null, Json::from));
        request.insert(
            "before",
            change.before().map_or(Json::Null, EventFields::to_json),
        );
        request.insert(
            "after",
            change.after().map_or(Json::Null, EventFields::to_json),
        );
        request.insert(
            "seconds",
            i64::try_from(approval.seconds).unwrap_or(i64::MAX),
        );
        request.insert("undo", Json::Bool(approval.undo));
        let data = self.call(request, approval.seconds + 60)?;
        ChangeResult::from_json(&data).ok_or_else(|| Self::unexpected("change"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::with_labels;
    use crate::calendar::CalendarEntry;
    use crate::civil::DateTime;
    use crate::osascript::Finished;
    use crate::testutil::{j, TestResult};
    use std::cell::RefCell;
    use std::collections::VecDeque;
    #[cfg(target_os = "macos")]
    use std::process::Command;
    use std::rc::Rc;
    use std::time::Duration;

    #[derive(Default)]
    struct Script {
        answers: VecDeque<Finished>,
        payloads: Vec<String>,
        checks: usize,
    }

    struct Scripted(Rc<RefCell<Script>>);

    impl Host for Scripted {
        fn ensure_running(&self, app: App) {
            assert_eq!(app, App::Calendar);
            self.0.borrow_mut().checks += 1;
        }

        fn run(&self, app: App, payload: &str, _timeout: Duration) -> Finished {
            assert_eq!(app, App::Calendar);
            let mut script = self.0.borrow_mut();
            script.payloads.push(payload.to_string());
            script.answers.pop_front().unwrap_or(Finished::NotStarted)
        }
    }

    fn backend(answers: Vec<Finished>) -> (RealCalendar, Rc<RefCell<Script>>) {
        let script = Rc::new(RefCell::new(Script {
            answers: answers.into(),
            ..Script::default()
        }));
        (
            RealCalendar::new(Box::new(Scripted(Rc::clone(&script)))),
            script,
        )
    }

    fn ok(data: &str) -> Finished {
        Finished::Done {
            code: Some(0),
            stdout: format!("{{\"ok\": true, \"data\": {data}}}\n"),
            stderr: String::new(),
        }
    }

    fn failed(error: &str) -> Finished {
        Finished::Done {
            code: Some(0),
            stdout: format!("{{\"ok\": false, \"error\": \"{error}\"}}"),
            stderr: String::new(),
        }
    }

    fn privat() -> Calendar {
        with_labels(vec![CalendarEntry {
            id: "CAL-1".into(),
            name: "Privat".into(),
        }])
        .remove(0)
    }

    fn window() -> Result<Window, &'static str> {
        Ok(Window {
            from: DateTime::parse("2026-11-13 00:00").ok_or("from")?,
            to: DateTime::parse("2026-11-20 00:00").ok_or("to")?,
        })
    }

    #[test]
    fn calendars_are_decoded_and_the_app_is_started_first() -> TestResult {
        let (backend, script) = backend(vec![ok(
            r#"[{"id": "A", "name": "N", "writable": true}, {"id": "B", "name": "M", "writable": false}, 5]"#,
        )]);
        let calendars = backend.calendars()?;
        assert_eq!(calendars.len(), 2);
        assert!(calendars[0].writable && !calendars[1].writable);
        assert_eq!(script.borrow().checks, 1);
        let request = j(&script.borrow().payloads[0])?;
        assert_eq!(request, j(r#"{"op": "calendars"}"#)?);
        Ok(())
    }

    #[test]
    fn events_are_requested_with_the_window_and_the_bounds_and_decoded() -> TestResult {
        let (backend, script) = backend(vec![ok(
            r#"[{"uid": "U1", "summary": "S", "start": "2026-11-14 09:00", "end": "2026-11-14 10:00",
                 "allDay": false, "location": "L", "recurrence": "", "excluded": [], "utcOffset": 60, "attendees": 0},
                {"uid": "U2", "summary": "R", "start": "2026-10-04 18:00", "end": "2026-10-04 20:00",
                 "allDay": false, "location": "", "recurrence": "FREQ=WEEKLY;BYDAY=SU",
                 "excluded": ["2026-10-11 18:00", "kaputt", 5], "utcOffset": 120, "attendees": -1},
                {"uid": "", "summary": "no id"}, {"uid": "U3", "start": "bad", "end": "x"}, 7]"#,
        )]);
        let events = backend.events(&privat(), window()?)?;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].attendees, Some(0));
        assert_eq!(events[1].attendees, None);
        assert_eq!(events[1].excluded.len(), 1);
        assert_eq!(events[1].utc_offset, 120);
        let request = j(&script.borrow().payloads[0])?;
        assert_eq!(
            request,
            j(
                r#"{"op": "events", "calendarId": "CAL-1", "from": "2026-11-13 00:00",
                    "to": "2026-11-20 00:00", "maxScan": 20000, "maxItems": 500}"#
            )?
        );
        Ok(())
    }

    #[test]
    fn one_event_is_decoded_with_notes_and_attendees() -> TestResult {
        let (backend, script) = backend(vec![ok(
            r#"{"uid": "U1", "summary": "S", "start": "2026-11-14 09:00", "end": "2026-11-14 10:00",
                "allDay": false, "location": "", "recurrence": "", "excluded": [], "utcOffset": 60,
                "attendees": 1, "notes": "N", "url": "http://example.org",
                "people": [{"name": "A", "email": "a@example.org", "status": "accepted"}, 5]}"#,
        )]);
        let detail = backend.event(&privat(), "U1")?;
        assert_eq!(detail.notes, "N");
        assert_eq!(detail.people.len(), 1);
        assert_eq!(
            j(&script.borrow().payloads[0])?,
            j(r#"{"op": "event", "calendarId": "CAL-1", "uid": "U1"}"#)?
        );
        let (broken, _) = backend_with(vec![ok("5")]);
        assert!(broken.event(&privat(), "U1").is_err());
        Ok(())
    }

    fn backend_with(answers: Vec<Finished>) -> (RealCalendar, Rc<RefCell<Script>>) {
        backend(answers)
    }

    #[test]
    fn failures_are_explained_for_the_model() {
        for (input, needle) in [
            (
                "execution error: Not authorized to send Apple events to Calendar. (-1743)",
                "Automation",
            ),
            ("calendar_not_found", "no longer exists"),
            ("event_not_found", "Event not found"),
            ("ambiguous_event", "More than one event"),
            ("too_many_events:25000", "in this range"),
            ("calendar_too_large:25000", "whatever the range"),
            ("invalid_local_time", "does not exist"),
            ("bad_time", "does not exist"),
            ("Apple event timed out. (-1712)", "did not answer in time"),
            (
                "something odd\nwith lines",
                "Calendar error: something odd with lines",
            ),
        ] {
            let message = map_error(input, Some("events"));
            if input == "calendar_not_found" {
                assert!(message.contains("`avery mail setup` again"), "{message}");
                assert!(!message.contains("setup apple"), "{message}");
            }
            assert!(message.contains(needle), "{input}: {message}");
        }
        assert!(map_error("Can't get event 3. (-1728)", Some("event")).contains("Event not found"));
        assert!(map_error("Can't get event 3.", Some("events")).starts_with("Calendar error"));
    }

    #[test]
    fn script_errors_garbled_answers_and_timeouts_become_tool_errors() {
        let (backend, _) = backend(vec![
            failed("calendar_not_found"),
            Finished::TimedOut,
            Finished::NotStarted,
            Finished::TooLarge,
            Finished::Done {
                code: Some(1),
                stdout: "garbage".into(),
                stderr: String::new(),
            },
        ]);
        for _ in 0..5 {
            let result = backend.calendars();
            assert!(matches!(result, Err(ToolError::Msg(_))), "{result:?}");
        }
    }

    #[cfg(target_os = "macos")]
    const MOCK_CALENDAR: &str = r#"
var LOG = {dialog: null, title: null, button: null, created: 0, otherCreated: 0, sets: [], deleted: 0};
var DECISION = 'approve';
var WRITABLE = true;
var SCENARIO = {};
function MOCKAPP() {
  return {
    includeStandardAdditions: false,
    activate: function () {},
    displayDialog: function (text, opts) {
      LOG.dialog = text; LOG.title = opts.withTitle; LOG.button = opts.buttons[1];
      if (SCENARIO.afterDialog) { SCENARIO.afterDialog(); }
      if (DECISION === 'timeout') { return {gaveUp: true, buttonReturned: ''}; }
      if (DECISION === 'approve') { return {gaveUp: false, buttonReturned: opts.buttons[1]}; }
      throw new Error('User canceled. (-128)');
    }
  };
}
function ev(o) {
  var self = {
    alldayEvent: function () { return !!o.allDay; },
    url: function () { return o.url === undefined ? null : o.url; },
    recurrence: function () { return o.rec === undefined ? null : o.rec; },
    uid: function () { return o.uid; },
    excludedDates: function () { return o.excluded || null; },
    attendees: function () {
      return (o.people || []).map(function (p) {
        return {displayName: function () { return p[0]; }, email: function () { return p[1]; },
                participationStatus: function () { return p[2]; }};
      });
    }
  };
  [['summary', 'summary'], ['location', 'location'], ['description', 'notes'],
   ['startDate', 'start'], ['endDate', 'end']].forEach(function (pair) {
    Object.defineProperty(self, pair[0], {
      get: function () { return function () { var v = o[pair[1]]; return v === undefined ? null : v; }; },
      set: function (v) { LOG.sets.push([pair[0], v instanceof Date ? v.getTime() : v]); o[pair[1]] = v; }
    });
  });
  self.__o = o;
  return self;
}
var LIST = [];
function column(name) { return function () { return LIST.map(function (e) { return e[name](); }); }; }
var events = {startDate: column('startDate'), endDate: column('endDate'), recurrence: column('recurrence'),
              uid: column('uid'), summary: column('summary'), location: column('location'),
              alldayEvent: column('alldayEvent')};
function sync() {
  Object.keys(events).forEach(function (k) { if (/^\d+$/.test(k)) { delete events[k]; } });
  LIST.forEach(function (e, i) { events[i] = e; });
}
events.push = function (e) { LOG.created++; LIST.push(e); sync(); };
events.whose = function (query) {
  var hits = LIST.filter(function (e) { return e.uid() === query.uid; });
  var sel = {uid: function () { return hits.map(function (e) { return e.uid(); }); }};
  hits.forEach(function (e, i) { sel[i] = e; });
  return sel;
};
var CAL = {events: events, writable: function () { return WRITABLE; }, name: function () { return 'Privat'; },
           calendarIdentifier: function () { return 'CAL-1'; }};
var OTHER = {events: {push: function (e) { LOG.otherCreated++; }}, writable: function () { return true; },
             name: function () { return 'Anderer'; }, calendarIdentifier: function () { return 'CAL-2'; }};
var ORDER = [CAL, OTHER];
function lazy(i) {
  return {get events() { return ORDER[i].events; }, writable: function () { return ORDER[i].writable(); },
          name: function () { return ORDER[i].name(); },
          calendarIdentifier: function () { return ORDER[i].calendarIdentifier(); }};
}
var calendars = function () { return ORDER.map(function (c, i) { return lazy(i); }); };
calendars.calendarIdentifier = function () { return ORDER.map(function (c) { return c.calendarIdentifier(); }); };
calendars[0] = lazy(0);
calendars[1] = lazy(1);
var fakeCalendar = {
  calendars: calendars,
  Event: function (props) {
    return ev({uid: 'new-uid', summary: props.summary, start: props.startDate, end: props.endDate,
               allDay: props.alldayEvent, location: props.location, notes: props.description, url: props.url});
  },
  'delete': function (e) {
    if (SCENARIO.deleteThrows) { throw new Error(SCENARIO.deleteThrows); }
    LOG.deleted++; LIST.splice(LIST.indexOf(e), 1); sync();
  }
};
function calendarApp() { return fakeCalendar; }
function run(argv) {
  var input = JSON.parse(argv[0]), out;
  DECISION = input.decision || 'approve';
  WRITABLE = input.writable !== false;
  SCENARIO = {};
  LIST = input.events.map(function (e) {
    e.start = new Date(e.start); e.end = new Date(e.end);
    if (e.excluded) { e.excluded = e.excluded.map(function (d) { return new Date(d); }); }
    return ev(e);
  });
  sync();
  if (input.mutate) { SCENARIO.afterDialog = function () { LIST[0].__o.summary = 'Hinterrücks geändert'; }; }
  if (input.reorder) { SCENARIO.afterDialog = function () { ORDER = [OTHER, CAL]; }; }
  if (input.deleteThrows) { SCENARIO.deleteThrows = input.deleteThrows; }
  try { out = {ok: true, data: dispatch(input.req)}; }
  catch (e) { out = {ok: false, error: String((e && e.message) || e)}; }
  out.log = LOG;
  return JSON.stringify(out);
}
"#;

    #[cfg(target_os = "macos")]
    fn probe(events: &str, req: &str) -> Result<Json, Box<dyn std::error::Error>> {
        probe_with(events, req, "")
    }

    #[cfg(target_os = "macos")]
    fn probe_with(
        events: &str,
        req: &str,
        extra: &str,
    ) -> Result<Json, Box<dyn std::error::Error>> {
        let script = format!(
            "{}\n{MOCK_CALENDAR}",
            crate::osascript::calendar_script()
                .replace("Application.currentApplication()", "MOCKAPP()")
        );
        let separator = if extra.is_empty() { "" } else { ", " };
        let payload = format!(r#"{{"events": {events}, "req": {req}{separator}{extra}}}"#);
        let output = Command::new(crate::osascript::OSASCRIPT)
            .args(["-l", "JavaScript", "-e", &script, &payload])
            .output()?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
        }
        Ok(j(String::from_utf8_lossy(&output.stdout).trim())?)
    }

    #[cfg(target_os = "macos")]
    fn local(text: &str) -> String {
        let (date, clock) = text.split_once(' ').unwrap_or((text, "00:00"));
        format!("{date}T{clock}:00")
    }

    #[cfg(target_os = "macos")]
    fn events_request(from: &str, to: &str, max_scan: i64, max_items: i64) -> String {
        format!(
            r#"{{"op": "events", "calendarId": "CAL-1", "from": "{from}", "to": "{to}", "maxScan": {max_scan}, "maxItems": {max_items}}}"#
        )
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_script_selects_overlapping_events_series_and_zero_length_events() -> TestResult {
        let events = format!(
            r#"[
            {{"uid": "old", "summary": "Old", "start": "{}", "end": "{}"}},
            {{"uid": "inside", "summary": "Inside", "start": "{}", "end": "{}", "location": "L"}},
            {{"uid": "late", "summary": "Late", "start": "{}", "end": "{}"}},
            {{"uid": "series", "summary": "Series", "start": "{}", "end": "{}", "rec": "FREQ=WEEKLY", "excluded": ["{}"]}},
            {{"uid": "zero", "summary": "Zero", "start": "{}", "end": "{}"}},
            {{"uid": "straddle", "summary": "Straddle", "start": "{}", "end": "{}", "allDay": true, "people": [["A", "a@example.org", "accepted"]]}},
            {{"uid": "ended", "summary": "Ended", "start": "{}", "end": "{}", "rec": "FREQ=DAILY;UNTIL=20250301T000000Z"}},
            {{"uid": "ending", "summary": "Ending", "start": "{}", "end": "{}", "rec": "FREQ=DAILY;UNTIL=20261112T090000Z"}},
            {{"uid": "counted", "summary": "Counted", "start": "{}", "end": "{}", "rec": "FREQ=DAILY;COUNT=5"}},
            {{"uid": "newseries", "summary": "Future series", "start": "{}", "end": "{}", "rec": "FREQ=DAILY"}}
            ]"#,
            local("2026-11-01 09:00"),
            local("2026-11-01 10:00"),
            local("2026-11-14 09:00"),
            local("2026-11-14 10:00"),
            local("2026-11-20 00:00"),
            local("2026-11-20 01:00"),
            local("2026-09-01 18:00"),
            local("2026-09-01 19:00"),
            local("2026-09-08 18:00"),
            local("2026-11-13 00:00"),
            local("2026-11-13 00:00"),
            local("2026-11-12 00:00"),
            local("2026-11-14 00:00"),
            local("2025-01-01 09:00"),
            local("2025-01-01 10:00"),
            local("2026-01-01 09:00"),
            local("2026-01-01 10:00"),
            local("2024-01-01 09:00"),
            local("2024-01-01 10:00"),
            local("2026-12-01 09:00"),
            local("2026-12-01 10:00"),
        );
        let answer = probe(
            &events,
            &events_request("2026-11-13 00:00", "2026-11-20 00:00", 1000, 100),
        )?;
        let object = answer.as_object().ok_or("object")?;
        assert_eq!(object.get("ok"), Some(&Json::Bool(true)), "{answer:?}");
        let items = object.get("data").and_then(Json::as_array).ok_or("data")?;
        let uids: Vec<String> = items
            .iter()
            .filter_map(Json::as_object)
            .map(|item| text_of(item.get("uid")))
            .collect();
        assert_eq!(
            uids,
            ["inside", "series", "zero", "straddle", "ending", "counted"]
        );
        let inside = items[0].as_object().ok_or("inside")?;
        assert_eq!(text_of(inside.get("start")), "2026-11-14 09:00");
        assert_eq!(text_of(inside.get("location")), "L");
        assert_eq!(inside.get("allDay"), Some(&Json::Bool(false)));
        assert_eq!(inside.get("recurrence"), Some(&Json::from("")));
        assert_eq!(inside.get("attendees"), Some(&Json::Int(0)));
        let series = items[1].as_object().ok_or("series")?;
        assert_eq!(text_of(series.get("recurrence")), "FREQ=WEEKLY");
        assert_eq!(
            series.get("excluded"),
            Some(&Json::Arr(vec![Json::from("2026-09-08 18:00")]))
        );
        let straddle = items[3].as_object().ok_or("straddle")?;
        assert_eq!(straddle.get("allDay"), Some(&Json::Bool(true)));
        assert_eq!(straddle.get("attendees"), Some(&Json::Int(1)));
        let decoded: Vec<RawEvent> = items.iter().filter_map(RawEvent::from_json).collect();
        assert_eq!(decoded.len(), 6);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_script_refuses_oversized_scans_bad_times_and_unknown_calendars() -> TestResult {
        let events = format!(
            r#"[{{"uid": "a", "summary": "A", "start": "{}", "end": "{}"}},
                {{"uid": "b", "summary": "B", "start": "{}", "end": "{}"}}]"#,
            local("2026-11-14 09:00"),
            local("2026-11-14 10:00"),
            local("2026-11-15 09:00"),
            local("2026-11-15 10:00"),
        );
        let error_of = |req: &str| -> Result<String, Box<dyn std::error::Error>> {
            let answer = probe(&events, req)?;
            let object = answer.as_object().ok_or("object")?;
            assert_eq!(object.get("ok"), Some(&Json::Bool(false)), "{answer:?}");
            Ok(text_of(object.get("error")))
        };
        assert_eq!(
            error_of(&events_request(
                "2026-11-13 00:00",
                "2026-11-20 00:00",
                1,
                100
            ))?,
            "calendar_too_large:2"
        );
        assert_eq!(
            error_of(&events_request(
                "2026-11-13 00:00",
                "2026-11-20 00:00",
                100,
                1
            ))?,
            "too_many_events:2"
        );
        assert_eq!(
            error_of(&events_request("tomorrow", "2026-11-20 00:00", 100, 100))?,
            "bad_time"
        );
        assert_eq!(
            error_of(&events_request(
                "2026-02-30 00:00",
                "2026-11-20 00:00",
                100,
                100
            ))?,
            "invalid_local_time"
        );
        assert_eq!(
            error_of(
                r#"{"op": "events", "calendarId": "NOPE", "from": "2026-11-13 00:00", "to": "2026-11-20 00:00", "maxScan": 5, "maxItems": 5}"#
            )?,
            "calendar_not_found"
        );
        assert_eq!(
            error_of(r#"{"op": "wipe", "calendarId": "CAL-1"}"#)?,
            "unknown_op"
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_calendar_script_refuses_every_operation_of_the_setup_dialogs() -> TestResult {
        for op in [
            "ask_buttons",
            "choose_list",
            "notify",
            "ask_text",
            "open_url",
            "approve_send",
            "approve_change",
            "approval_test",
            "accounts",
            "compose",
        ] {
            let request = format!(
                r#"{{"op": "{op}", "calendarId": "CAL-1", "title": "T", "text": "x", "buttons": ["OK"], "items": ["a"], "seconds": 5}}"#
            );
            let answer = probe("[]", &request)?;
            let object = answer.as_object().ok_or("object")?;
            assert_eq!(object.get("ok"), Some(&Json::Bool(false)), "{op}");
            assert_eq!(text_of(object.get("error")), "unknown_op", "{op}");
            let log = object.get("log").and_then(Json::as_object).ok_or("log")?;
            assert_eq!(log.get("dialog"), Some(&Json::Null), "{op}");
            assert_eq!(log.get("created"), Some(&Json::Int(0)), "{op}");
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_script_reads_one_event_and_refuses_unknown_and_ambiguous_ids() -> TestResult {
        let events = format!(
            r#"[{{"uid": "one", "summary": "Eins", "start": "{}", "end": "{}", "notes": "Notiz",
                  "rec": "FREQ=DAILY", "excluded": ["{}"], "people": [["Anna", "anna@example.org", "accepted"]]}},
                {{"uid": "twin", "summary": "A", "start": "{}", "end": "{}"}},
                {{"uid": "twin", "summary": "B", "start": "{}", "end": "{}"}}]"#,
            local("2026-11-14 09:00"),
            local("2026-11-14 10:00"),
            local("2026-11-15 09:00"),
            local("2026-11-14 09:00"),
            local("2026-11-14 10:00"),
            local("2026-11-16 09:00"),
            local("2026-11-16 10:00"),
        );
        let ask = |uid: &str| -> Result<Json, Box<dyn std::error::Error>> {
            probe(
                &events,
                &format!(r#"{{"op": "event", "calendarId": "CAL-1", "uid": "{uid}"}}"#),
            )
        };
        let answer = ask("one")?;
        let data = answer
            .as_object()
            .and_then(|o| o.get("data"))
            .ok_or("data")?;
        let detail = EventDetail::from_json(data).ok_or("detail")?;
        assert_eq!(detail.event.summary, "Eins");
        assert_eq!(detail.notes, "Notiz");
        assert_eq!(detail.url, "");
        assert_eq!(detail.people[0].email, "anna@example.org");
        assert_eq!(detail.event.recurrence, "FREQ=DAILY");
        assert_eq!(detail.event.excluded.len(), 1);
        assert_eq!(detail.event.attendees, Some(1));
        let missing = ask("nope")?;
        assert_eq!(
            missing.as_object().map(|o| text_of(o.get("error"))),
            Some("event_not_found".to_string())
        );
        let twin = ask("twin")?;
        assert_eq!(
            twin.as_object().map(|o| text_of(o.get("error"))),
            Some("ambiguous_event".to_string())
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn fields_json(summary: &str, start: &str, end: &str, location: &str, notes: &str) -> String {
        format!(
            r#"{{"summary": "{summary}", "start": "{start}", "end": "{end}", "allDay": false, "location": "{location}", "notes": "{notes}", "url": ""}}"#
        )
    }

    #[cfg(target_os = "macos")]
    fn existing_event(extra: &str) -> String {
        format!(
            r#"[{{"uid": "one", "summary": "Zahnarzt", "start": "{}", "end": "{}", "location": "Praxis", "notes": "Alt"{extra}}}]"#,
            local("2030-05-06 09:00"),
            local("2030-05-06 10:00"),
        )
    }

    #[cfg(target_os = "macos")]
    fn change_request(action: &str, uid: &str, before: &str, after: &str, undo: bool) -> String {
        let uid = if uid.is_empty() {
            "null".to_string()
        } else {
            format!("\"{uid}\"")
        };
        format!(
            r#"{{"op": "change", "calendarId": "CAL-1", "calendarLabel": "Privat", "action": "{action}", "uid": {uid}, "before": {before}, "after": {after}, "seconds": 75, "undo": {undo}}}"#
        )
    }

    #[cfg(target_os = "macos")]
    struct Outcome {
        ok: bool,
        status: String,
        error: String,
        uid: String,
        after: Option<EventFields>,
        dialog: Option<String>,
        button: String,
        created: i64,
        other_created: i64,
        deleted: i64,
        sets: Vec<String>,
    }

    #[cfg(target_os = "macos")]
    fn change(
        events: &str,
        request: &str,
        extra: &str,
    ) -> Result<Outcome, Box<dyn std::error::Error>> {
        let answer = probe_with(events, request, extra)?;
        let object = answer.as_object().ok_or("object")?;
        let log = object.get("log").and_then(Json::as_object).ok_or("log")?;
        let data = object.get("data").and_then(Json::as_object);
        let count = |key: &str| match log.get(key) {
            Some(Json::Int(number)) => *number,
            _ => -1,
        };
        Ok(Outcome {
            ok: object.get("ok") == Some(&Json::Bool(true)),
            status: data.map_or_else(String::new, |d| text_of(d.get("status"))),
            error: text_of(object.get("error")),
            uid: data.map_or_else(String::new, |d| text_of(d.get("uid"))),
            after: data
                .and_then(|d| d.get("after"))
                .and_then(EventFields::from_json),
            dialog: log.get("dialog").and_then(Json::as_str).map(str::to_string),
            button: text_of(log.get("button")),
            created: count("created"),
            other_created: count("otherCreated"),
            deleted: count("deleted"),
            sets: log
                .get("sets")
                .and_then(Json::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(Json::as_array)
                        .filter_map(|pair| pair.first().and_then(Json::as_str).map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    #[cfg(target_os = "macos")]
    const OLD: fn() -> String = || {
        fields_json(
            "Zahnarzt",
            "2030-05-06 09:00",
            "2030-05-06 10:00",
            "Praxis",
            "Alt",
        )
    };

    #[cfg(target_os = "macos")]
    #[test]
    fn a_calendar_that_moves_in_the_list_while_the_dialog_is_open_still_gets_the_event(
    ) -> TestResult {
        let after = fields_json("Tennis", "2030-05-07 18:30", "2030-05-07 19:45", "", "");
        let done = change(
            "[]",
            &change_request("create", "", "null", &after, false),
            r#""reorder": true"#,
        )?;
        assert!(done.ok, "{}", done.error);
        assert_eq!((done.created, done.other_created), (1, 0));
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_approved_create_shows_the_details_and_adds_the_event() -> TestResult {
        let after = fields_json(
            "Tennis",
            "2030-05-07 18:30",
            "2030-05-07 19:45",
            "Platz 3",
            "Schlaeger\\nBaelle",
        );
        let done = change(
            "[]",
            &change_request("create", "", "null", &after, false),
            "",
        )?;
        assert!(done.ok, "{}", done.error);
        assert_eq!(
            (done.status.as_str(), done.uid.as_str()),
            ("applied", "new-uid")
        );
        assert_eq!((done.created, done.deleted), (1, 0));
        let stored = done.after.ok_or("after")?;
        assert_eq!(stored.summary, "Tennis");
        assert_eq!(stored.start.to_string(), "2030-05-07 18:30");
        assert_eq!(stored.end.to_string(), "2030-05-07 19:45");
        let dialog = done.dialog.ok_or("dialog")?;
        assert!(
            dialog.starts_with("Goose m\u{f6}chte einen Termin ANLEGEN."),
            "{dialog}"
        );
        for line in [
            "Kalender:   Privat",
            "Titel:      Tennis",
            "Wann:       2030-05-07 18:30 bis 2030-05-07 19:45",
            "Ort:        Platz 3",
            "Notizen:\n--------------------------------\n\u{2502} \u{200e}Schlaeger\n\u{2502} \u{200e}Baelle\n--------------------------------",
        ] {
            assert!(dialog.contains(line), "{line}\n{dialog}");
        }
        assert_eq!(done.button, "Anlegen");
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_declined_or_ignored_dialog_changes_nothing() -> TestResult {
        let after = fields_json("Tennis", "2030-05-07 18:30", "2030-05-07 19:45", "", "");
        for (decision, status) in [("decline", "declined"), ("timeout", "timeout")] {
            let result = change(
                "[]",
                &change_request("create", "", "null", &after, false),
                &format!(r#""decision": "{decision}""#),
            )?;
            assert!(result.ok && result.status == status, "{}", result.error);
            assert_eq!((result.created, result.deleted), (0, 0));
            assert!(result.sets.is_empty());
        }
        let declined = change(
            &existing_event(""),
            &change_request("delete", "one", &OLD(), "null", false),
            r#""decision": "decline""#,
        )?;
        assert!(declined.ok && declined.status == "declined" && declined.deleted == 0);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_update_shows_before_and_after_and_sets_only_what_changed() -> TestResult {
        let after = fields_json(
            "Zahnarzt Kontrolle",
            "2030-05-06 14:00",
            "2030-05-06 15:00",
            "Praxis",
            "Alt",
        );
        let done = change(
            &existing_event(""),
            &change_request("update", "one", &OLD(), &after, false),
            "",
        )?;
        assert!(done.ok, "{}", done.error);
        assert_eq!(done.status, "applied");
        assert_eq!(done.sets, ["summary", "endDate", "startDate"]);
        assert_eq!(done.created, 0);
        let dialog = done.dialog.ok_or("dialog")?;
        assert!(
            dialog.starts_with("Goose m\u{f6}chte einen Termin \u{c4}NDERN."),
            "{dialog}"
        );
        for line in [
            "Titel:      Zahnarzt  \u{2192}  Zahnarzt Kontrolle",
            "Wann:       2030-05-06 09:00 bis 2030-05-06 10:00  \u{2192}  2030-05-06 14:00 bis 2030-05-06 15:00",
            "Ort:        Praxis  (unver\u{e4}ndert)",
        ] {
            assert!(dialog.contains(line), "{line}\n{dialog}");
        }
        assert!(!dialog.contains("Notizen"));
        assert_eq!(done.button, "\u{c4}ndern");
        let earlier = fields_json(
            "Zahnarzt",
            "2030-05-06 08:00",
            "2030-05-06 09:30",
            "Praxis",
            "Neu",
        );
        let moved = change(
            &existing_event(""),
            &change_request("update", "one", &OLD(), &earlier, false),
            "",
        )?;
        assert_eq!(moved.sets, ["description", "startDate", "endDate"]);
        let dialog = moved.dialog.ok_or("dialog")?;
        assert!(
            dialog.contains(concat!(
                "Notizen nachher:\n--------------------------------\n\u{2502} \u{200e}Neu\n--------------------------------\n",
                "Notizen vorher:\n--------------------------------\n\u{2502} \u{200e}Alt\n--------------------------------"
            )),
            "{dialog}"
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_delete_shows_the_event_and_checks_that_it_is_gone() -> TestResult {
        let done = change(
            &existing_event(""),
            &change_request("delete", "one", &OLD(), "null", false),
            "",
        )?;
        assert!(done.ok, "{}", done.error);
        assert_eq!((done.status.as_str(), done.deleted), ("applied", 1));
        assert!(done.after.is_none());
        let dialog = done.dialog.ok_or("dialog")?;
        assert!(
            dialog.starts_with("Goose m\u{f6}chte einen Termin L\u{d6}SCHEN."),
            "{dialog}"
        );
        assert!(dialog.contains("Titel:      Zahnarzt") && dialog.contains("Ort:        Praxis"));
        assert_eq!(done.button, "L\u{f6}schen");
        let undo = change(
            &existing_event(""),
            &change_request("delete", "one", &OLD(), "null", true),
            "",
        )?;
        let dialog = undo.dialog.ok_or("dialog")?;
        assert!(dialog.starts_with("Goose m\u{f6}chte eine \u{c4}nderung R\u{dc}CKG\u{c4}NGIG machen: Termin L\u{d6}SCHEN."), "{dialog}");
        Ok(())
    }

    #[cfg(target_os = "macos")]
    const MARK: &str = "\u{2502} \u{200e}";

    #[cfg(target_os = "macos")]
    const RULE: &str = "--------------------------------";

    #[cfg(target_os = "macos")]
    fn event_fields(notes: &str) -> String {
        crate::json::dumps(
            &Json::object(vec![
                ("summary", Json::from("Zahnarzt")),
                ("start", Json::from("2030-05-06 09:00")),
                ("end", Json::from("2030-05-06 10:00")),
                ("allDay", Json::Bool(false)),
                ("location", Json::from("Praxis")),
                ("notes", Json::from(notes)),
                ("url", Json::from("")),
            ]),
            true,
        )
    }

    #[cfg(target_os = "macos")]
    fn event_with_notes(notes: &str) -> String {
        format!(
            "[{}]",
            crate::json::dumps(
                &Json::object(vec![
                    ("uid", Json::from("one")),
                    ("summary", Json::from("Zahnarzt")),
                    ("start", Json::from(local("2030-05-06 09:00"))),
                    ("end", Json::from(local("2030-05-06 10:00"))),
                    ("location", Json::from("Praxis")),
                    ("notes", Json::from(notes)),
                ]),
                true
            )
        )
    }

    #[cfg(target_os = "macos")]
    fn dialog_of(
        action: &str,
        before: Option<&str>,
        after: Option<&str>,
        undo: bool,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let events = before.map_or_else(|| "[]".to_string(), event_with_notes);
        let done = change(
            &events,
            &change_request(
                action,
                if before.is_some() { "one" } else { "" },
                &before.map_or_else(|| "null".to_string(), event_fields),
                &after.map_or_else(|| "null".to_string(), event_fields),
                undo,
            ),
            "",
        )?;
        assert!(done.ok, "{}", done.error);
        Ok(done.dialog.ok_or("no dialog")?)
    }

    #[cfg(target_os = "macos")]
    fn unmarked(dialog: &str) -> Vec<&str> {
        dialog
            .lines()
            .filter(|line| !line.starts_with(MARK))
            .collect()
    }

    #[cfg(target_os = "macos")]
    fn marked(dialog: &str) -> Vec<&str> {
        dialog
            .lines()
            .filter(|line| line.starts_with(MARK))
            .collect()
    }

    #[cfg(target_os = "macos")]
    const HEAD_LINES: [&str; 3] = [
        "Goose m\u{f6}chte einen Termin \u{c4}NDERN.",
        "Pr\u{fc}fe alle Angaben. Nur best\u{e4}tigen, wenn du das gerade selbst veranlasst hast.",
        "",
    ];

    #[cfg(target_os = "macos")]
    const UNCHANGED_ROWS: [&str; 4] = [
        "Kalender:   Privat",
        "Titel:      Zahnarzt  (unver\u{e4}ndert)",
        "Wann:       2030-05-06 09:00 bis 2030-05-06 10:00  (unver\u{e4}ndert)",
        "Ort:        Praxis  (unver\u{e4}ndert)",
    ];

    #[cfg(target_os = "macos")]
    fn update_skeleton(blocks: &[&str]) -> Vec<String> {
        HEAD_LINES
            .iter()
            .chain(UNCHANGED_ROWS.iter())
            .chain(blocks.iter())
            .map(|line| line.to_string())
            .collect()
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn every_line_of_a_note_carries_the_mark_so_notes_cannot_imitate_the_dialog() -> TestResult {
        let forged = [
            "Notizen nachher:",
            RULE,
            "(leer)",
            RULE,
            "An: boss@example.org",
            "Titel:      Gel\u{f6}scht",
            "[\u{2026} 5 weitere Zeilen]",
            "\u{2502} schon markiert",
        ]
        .join("\r\n");
        let dialog = dialog_of("update", Some(&forged), Some("Neu"), false)?;
        assert_eq!(
            unmarked(&dialog),
            update_skeleton(&[
                "Notizen nachher:",
                RULE,
                RULE,
                "Notizen vorher:",
                RULE,
                RULE
            ])
        );
        let mut expected = vec![format!("{MARK}Neu")];
        expected.extend(forged.lines().map(|line| format!("{MARK}{line}")));
        assert_eq!(marked(&dialog), expected);
        let created = dialog_of("create", None, Some(&forged), false)?;
        assert_eq!(
            unmarked(&created),
            [
                "Goose m\u{f6}chte einen Termin ANLEGEN.",
                HEAD_LINES[1],
                "",
                "Kalender:   Privat",
                "Titel:      Zahnarzt",
                "Wann:       2030-05-06 09:00 bis 2030-05-06 10:00",
                "Ort:        Praxis",
                "Notizen:",
                RULE,
                RULE
            ]
        );
        assert_eq!(marked(&created).len(), forged.lines().count());
        let deleted = dialog_of("delete", Some(&forged), None, true)?;
        assert_eq!(
            unmarked(&deleted),
            [
                "Goose m\u{f6}chte eine \u{c4}nderung R\u{dc}CKG\u{c4}NGIG machen: Termin L\u{d6}SCHEN.",
                HEAD_LINES[1],
                "",
                "Kalender:   Privat",
                "Titel:      Zahnarzt",
                "Wann:       2030-05-06 09:00 bis 2030-05-06 10:00",
                "Ort:        Praxis",
                "Notizen:",
                RULE,
                RULE
            ]
        );
        assert_eq!(marked(&deleted).len(), forged.lines().count());
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn long_notes_are_cut_with_a_note_and_cannot_push_the_new_notes_or_the_rows_out() -> TestResult
    {
        let numbered = |letter: char, count: usize| -> String {
            (0..count)
                .map(|n| format!("{letter}{n:03}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let benign = dialog_of("update", Some("Alt"), Some("Neu"), false)?;
        let position = |dialog: &str, line: &str| dialog.lines().position(|found| found == line);
        for (before, after) in [
            (numbered('n', 300), "Neu".to_string()),
            ("Alt".to_string(), numbered('m', 300)),
            (numbered('n', 300), numbered('m', 300)),
            ("\n".repeat(300) + "Alt", "Neu".to_string()),
        ] {
            let dialog = dialog_of("update", Some(&before), Some(&after), false)?;
            assert!(dialog.lines().count() <= 60, "{}", dialog.lines().count());
            assert_eq!(
                position(&dialog, "Notizen nachher:"),
                position(&benign, "Notizen nachher:")
            );
            assert_eq!(
                dialog.lines().take(7).collect::<Vec<_>>(),
                benign.lines().take(7).collect::<Vec<_>>()
            );
            assert_eq!(
                dialog
                    .lines()
                    .filter(|line| *line == "Notizen nachher:" || *line == "Notizen vorher:")
                    .count(),
                2
            );
        }
        let dialog = dialog_of(
            "update",
            Some(&numbered('n', 300)),
            Some(&numbered('m', 300)),
            false,
        )?;
        assert_eq!(
            unmarked(&dialog),
            update_skeleton(&[
                "Notizen nachher:",
                RULE,
                "[\u{2026} 270 weitere Zeilen]",
                RULE,
                "Notizen vorher:",
                RULE,
                "[\u{2026} 292 weitere Zeilen]",
                RULE
            ])
        );
        let shown = marked(&dialog);
        assert_eq!(shown.len(), 38);
        assert_eq!(shown[0], format!("{MARK}m000"));
        assert_eq!(shown[29], format!("{MARK}m029"));
        assert_eq!(shown[30], format!("{MARK}n000"));
        assert_eq!(shown[37], format!("{MARK}n007"));
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn one_endless_line_is_cut_by_characters_and_blank_runs_are_squeezed() -> TestResult {
        let dialog = dialog_of(
            "update",
            Some(&"x".repeat(5000)),
            Some(&"y".repeat(5000)),
            false,
        )?;
        assert_eq!(
            marked(&dialog),
            [
                format!("{MARK}{}\u{2026}", "y".repeat(1200)),
                format!("{MARK}{}\u{2026}", "x".repeat(300))
            ]
        );
        assert!(dialog.contains("\n[\u{2026} 3800 weitere Zeichen]\n"));
        assert!(dialog.contains("\n[\u{2026} 4700 weitere Zeichen]\n"));
        let padded = format!("erste\n{}\t zweite  \r\n", "\n \r".repeat(300));
        let squeezed = dialog_of("update", Some(&padded), Some("Neu"), false)?;
        assert_eq!(
            marked(&squeezed),
            [
                format!("{MARK}Neu"),
                format!("{MARK}erste"),
                MARK.to_string(),
                format!("{MARK}  zweite")
            ]
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn lines_that_use_up_the_character_budget_end_the_block_with_a_note() -> TestResult {
        let lines = |width: usize| -> String {
            (0..10)
                .map(|n| format!("{n}{}", "x".repeat(width - 1)))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let exact = dialog_of("update", Some(&lines(49)), Some("Neu"), false)?;
        let shown = marked(&exact);
        assert_eq!(shown.len(), 1 + 6);
        assert_eq!(shown[6], format!("{MARK}5{}", "x".repeat(48)));
        assert!(exact.contains("\n[\u{2026} 4 weitere Zeilen]\n"), "{exact}");
        let partial = dialog_of("update", Some(&lines(50)), Some("Neu"), false)?;
        let shown = marked(&partial);
        assert_eq!(shown.len(), 1 + 6);
        assert_eq!(shown[5], format!("{MARK}4{}", "x".repeat(49)));
        assert_eq!(shown[6], format!("{MARK}5{}\u{2026}", "x".repeat(44)));
        assert!(
            partial.contains("\n[\u{2026} 4 weitere Zeilen]\n"),
            "{partial}"
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn empty_notes_are_the_dialogs_own_word_and_a_note_saying_it_is_not() -> TestResult {
        let dialog = dialog_of("update", Some("\n\r\n \t \n"), Some("(leer)"), false)?;
        assert_eq!(
            unmarked(&dialog),
            update_skeleton(&[
                "Notizen nachher:",
                RULE,
                RULE,
                "Notizen vorher:",
                RULE,
                "(leer)",
                RULE
            ])
        );
        assert_eq!(marked(&dialog), [format!("{MARK}(leer)")]);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn hidden_and_direction_characters_of_notes_are_removed_and_separators_become_spaces(
    ) -> TestResult {
        let hostile = "a\u{202e}b\u{85}c\u{2028}d\u{2029}e\u{2066}f\u{200b}g\u{b}h\u{c}i\u{feff}j\u{e0041}k\u{200f}";
        let dialog = dialog_of("update", Some(hostile), Some(hostile), false)?;
        assert!(unmarked(&dialog).contains(&UNCHANGED_ROWS[1]));
        let none = dialog_of("update", Some(hostile), Some("Neu"), false)?;
        assert_eq!(
            marked(&none),
            [format!("{MARK}Neu"), format!("{MARK}ab c d efghijk")]
        );
        for forbidden in [
            '\u{202e}',
            '\u{85}',
            '\u{2028}',
            '\u{2029}',
            '\u{2066}',
            '\u{200b}',
            '\u{200f}',
            '\u{b}',
            '\u{c}',
            '\u{feff}',
            '\u{e0041}',
        ] {
            assert!(!none.contains(forbidden), "{:x}", u32::from(forbidden));
        }
        let right_to_left = dialog_of(
            "update",
            Some("\u{5e9}\u{5dc}\u{5d5}\u{5dd} Notizen nachher:"),
            Some("Neu"),
            false,
        )?;
        assert_eq!(
            marked(&right_to_left)[1],
            format!("{MARK}\u{5e9}\u{5dc}\u{5d5}\u{5dd} Notizen nachher:")
        );
        assert!(MARK.ends_with('\u{200e}'));
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_failure_after_the_click_is_reported_as_an_unknown_state() -> TestResult {
        let failed = change(
            &existing_event(""),
            &change_request("delete", "one", &OLD(), "null", false),
            r#""deleteThrows": "Apple event timed out""#,
        )?;
        assert!(!failed.ok);
        assert!(
            failed.error.starts_with("change_state_unknown: "),
            "{}",
            failed.error
        );
        let error = failure(&failed.error, Some("change"), false);
        assert!(matches!(error, ToolError::Uncertain(m) if m.contains("may or may not")));
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_event_that_changed_meanwhile_is_never_changed_or_deleted() -> TestResult {
        let after = fields_json(
            "Neu",
            "2030-05-06 09:00",
            "2030-05-06 10:00",
            "Praxis",
            "Alt",
        );
        let during = change(
            &existing_event(""),
            &change_request("update", "one", &OLD(), &after, false),
            r#""mutate": true"#,
        )?;
        assert!(
            !during.ok && during.error == "event_changed",
            "{}",
            during.error
        );
        assert!(during.dialog.is_some());
        assert!(during.sets.is_empty());
        let stale = fields_json(
            "Anders",
            "2030-05-06 09:00",
            "2030-05-06 10:00",
            "Praxis",
            "Alt",
        );
        let before = change(
            &existing_event(""),
            &change_request("delete", "one", &stale, "null", false),
            "",
        )?;
        assert!(!before.ok && before.error == "event_changed");
        assert!(before.dialog.is_none() && before.deleted == 0);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn events_with_attendees_series_and_read_only_calendars_are_refused_before_any_dialog(
    ) -> TestResult {
        let after = fields_json(
            "Neu",
            "2030-05-06 09:00",
            "2030-05-06 10:00",
            "Praxis",
            "Alt",
        );
        let cases = [
            (
                r#", "people": [["A", "a@example.org", "accepted"]]"#,
                true,
                "has_attendees",
            ),
            (r#", "rec": "FREQ=WEEKLY""#, true, "recurring_event"),
            ("", false, "calendar_read_only"),
        ];
        for (extra, writable, error) in cases {
            for action in ["update", "delete"] {
                let request = change_request(action, "one", &OLD(), &after, false);
                let result = change(
                    &existing_event(extra),
                    &request,
                    &format!(r#""writable": {writable}"#),
                )?;
                assert!(
                    !result.ok && result.error == error,
                    "{action} {error}: {}",
                    result.error
                );
                assert!(result.dialog.is_none() && result.deleted == 0 && result.sets.is_empty());
            }
        }
        let unknown = change(
            &existing_event(""),
            &change_request("delete", "nope", &OLD(), "null", false),
            "",
        )?;
        assert_eq!(unknown.error, "event_not_found");
        let created = change(
            "[]",
            &change_request("create", "", "null", &after, false),
            r#""writable": false"#,
        )?;
        assert_eq!(created.error, "calendar_read_only");
        assert_eq!(created.created, 0);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn text_from_the_calendar_cannot_forge_lines_in_the_dialog() -> TestResult {
        let nasty = r#"[{"uid": "one", "summary": "Echt\nKalender:   Fremd\u202eX", "start": "2030-05-06T09:00:00", "end": "2030-05-06T10:00:00", "location": "Ort\r\nTitel: Gefaelscht", "notes": "Zeile1\u202e\u200b\nZeile2"}]"#;
        let before = fields_json(
            "Echt\\nKalender:   Fremd\u{202e}X",
            "2030-05-06 09:00",
            "2030-05-06 10:00",
            "Ort\\r\\nTitel: Gefaelscht",
            "Zeile1\u{202e}\u{200b}\\nZeile2",
        );
        let done = change(
            nasty,
            &change_request("delete", "one", &before, "null", false),
            "",
        )?;
        assert!(done.ok, "{}", done.error);
        let dialog = done.dialog.ok_or("dialog")?;
        assert_eq!(dialog.matches("Kalender:   ").count(), 1, "{dialog}");
        assert!(dialog.contains("Titel:      Echt Kalender: FremdX"));
        assert!(dialog.contains("Ort:        Ort Titel: Gefaelscht"));
        assert!(!dialog.contains('\u{202e}') && !dialog.contains('\u{200b}'));
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn invalid_times_and_oversized_notes_are_refused_before_the_dialog() -> TestResult {
        let long = "n".repeat(1300);
        let big = fields_json("A", "2030-05-06 09:00", "2030-05-06 10:00", "", &long);
        let result = change("[]", &change_request("create", "", "null", &big, false), "")?;
        assert_eq!(result.error, "notes_too_long");
        assert!(result.dialog.is_none());
        let bad = fields_json("A", "2030-02-30 09:00", "2030-05-06 10:00", "", "");
        let result = change("[]", &change_request("create", "", "null", &bad, false), "")?;
        assert_eq!(result.error, "invalid_local_time");
        assert!(result.dialog.is_none() && result.created == 0);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_undo_may_recreate_an_event_with_long_notes_but_a_new_event_may_not() -> TestResult {
        let notes = "n".repeat(3000);
        let kept = fields_json("A", "2030-05-06 09:00", "2030-05-06 10:00", "", &notes);
        let undone = change("[]", &change_request("create", "", "null", &kept, true), "")?;
        assert!(undone.ok, "{}", undone.error);
        assert_eq!((undone.status.as_str(), undone.created), ("applied", 1));
        let new = change(
            "[]",
            &change_request("create", "", "null", &kept, false),
            "",
        )?;
        assert_eq!(new.error, "notes_too_long");
        let huge = fields_json(
            "A",
            "2030-05-06 09:00",
            "2030-05-06 10:00",
            "",
            &"n".repeat(4100),
        );
        let refused = change("[]", &change_request("create", "", "null", &huge, true), "")?;
        assert_eq!(refused.error, "notes_too_long");
        Ok(())
    }

    #[test]
    fn a_change_request_carries_the_snapshots_and_a_longer_deadline() -> TestResult {
        let (backend, script) = backend(vec![ok(
            r#"{"status": "applied", "uid": "U9", "after": null}"#,
        )]);
        let before = crate::calendar::EventFields {
            summary: "Alt".into(),
            start: DateTime::parse("2030-05-06 09:00").ok_or("start")?,
            end: DateTime::parse("2030-05-06 10:00").ok_or("end")?,
            all_day: false,
            location: String::new(),
            notes: String::new(),
            url: String::new(),
        };
        let change = Change::Delete {
            uid: "U9".into(),
            before: before.clone(),
        };
        let result = backend.change(
            &privat(),
            &change,
            &Approval {
                seconds: 75,
                undo: true,
            },
        )?;
        assert_eq!(result.decision, crate::calendar::Decision::Applied);
        assert_eq!(result.uid.as_deref(), Some("U9"));
        let request = j(&script.borrow().payloads[0])?;
        let object = request.as_object().ok_or("object")?;
        assert_eq!(text_of(object.get("op")), "change");
        assert_eq!(text_of(object.get("action")), "delete");
        assert_eq!(text_of(object.get("uid")), "U9");
        assert_eq!(text_of(object.get("calendarLabel")), "Privat");
        assert_eq!(object.get("undo"), Some(&Json::Bool(true)));
        assert_eq!(object.get("seconds"), Some(&Json::Int(75)));
        assert_eq!(object.get("after"), Some(&Json::Null));
        assert_eq!(
            EventFields::from_json(object.get("before").ok_or("before")?),
            Some(before)
        );
        Ok(())
    }

    #[test]
    fn unclear_or_garbled_answers_to_a_change_are_uncertain_but_refusals_are_not() -> TestResult {
        let (backend, _) = backend(vec![
            Finished::TimedOut,
            Finished::NotStarted,
            ok("5"),
            failed("has_attendees"),
            failed("change_state_unknown: boom"),
        ]);
        let change = Change::Create {
            after: crate::calendar::EventFields {
                summary: "A".into(),
                start: DateTime::parse("2030-01-01 00:00").ok_or("start")?,
                end: DateTime::parse("2030-01-02 00:00").ok_or("end")?,
                all_day: true,
                location: String::new(),
                notes: String::new(),
                url: String::new(),
            },
        };
        let approval = Approval {
            seconds: 5,
            undo: false,
        };
        let results: Vec<ToolResult<ChangeResult>> = (0..5)
            .map(|_| backend.change(&privat(), &change, &approval))
            .collect();
        assert!(matches!(results[0], Err(ToolError::Uncertain(_))));
        assert!(matches!(results[1], Err(ToolError::Uncertain(_))));
        assert!(matches!(results[2], Err(ToolError::Uncertain(_))));
        assert!(matches!(&results[3], Err(ToolError::Msg(m)) if m.contains("attendees")));
        assert!(matches!(results[4], Err(ToolError::Uncertain(_))));
        Ok(())
    }
}
