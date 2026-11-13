use super::*;

const CALENDAR_BODY: &str = concat!(
    "{\"@odata.context\": \"https://graph.microsoft.com/v1.0/$metadata#users('00000000-0000-0000-",
    "0000-000000000000')/calendars(id,name,canEdit)\", \"value\": [",
    "{\"id\": \"AQMkADAwATM3ZmYAZS0wYzBhLTQ4ADgxLTAwAi0wMAoARgAAA-cal==\", \"name\": \"Calendar\", ",
    "\"canEdit\": true}, ",
    "{\"id\": \"AQMkADAwATM3ZmYAZS0wYzBhLTQ4ADgxLTAwAi0wMAoARgAAA-bday==\", \"name\": \"Birthdays\", ",
    "\"canEdit\": false}, ",
    "{\"id\": \"AQMkADAwATM3ZmYAZS0wYzBhLTQ4ADgxLTAwAi0wMAoARgAAA-fam==\", \"name\": \"Family\", ",
    "\"canEdit\": true}]}"
);

fn info(id: &str, name: &str, writable: bool) -> CalendarInfo {
    CalendarInfo {
        id: id.to_string(),
        name: name.to_string(),
        writable,
    }
}

fn single(id: &'static str, number: usize) -> String {
    Ev {
        id,
        subject: format!("Event {number}"),
        ..Ev::default()
    }
    .json()
}

fn chain(count: usize, last_has_next: bool) -> Scripted {
    let link = |number: usize| format!("{}&$skiptoken=P{number}", list_url());
    let body = |number: usize| {
        let items = [single(EV, number).replace(EV, &format!("P{number}"))];
        if number < count || last_has_next {
            page_with_next(&items, &link(number + 1))
        } else {
            page(&items)
        }
    };
    let mut transport = Scripted::new();
    for number in (2..=count).rev() {
        transport = transport.json(
            Method::Get,
            &format!("$skiptoken=P{number}"),
            200,
            &body(number),
        );
    }
    transport.json(Method::Get, "/calendarView?", 200, &body(1))
}

#[test]
fn calendars_are_listed_with_their_write_flag() -> TestResult {
    let transport = Scripted::new().json(Method::Get, "/calendars?", 200, CALENDAR_BODY);
    let found = calendars(&api(&transport))?;
    assert_eq!(
        found,
        vec![
            info(CAL, "Calendar", true),
            info(
                "AQMkADAwATM3ZmYAZS0wYzBhLTQ4ADgxLTAwAi0wMAoARgAAA-bday==",
                "Birthdays",
                false
            ),
            info(
                "AQMkADAwATM3ZmYAZS0wYzBhLTQ4ADgxLTAwAi0wMAoARgAAA-fam==",
                "Family",
                true
            ),
        ]
    );
    let seen = transport.seen();
    assert_eq!(seen.len(), 1);
    let request = seen.first().ok_or("request")?;
    assert_eq!(request.method, Method::Get);
    assert_eq!(
        request.url,
        format!("{GRAPH}/calendars?$top=100&$select=id,name,canEdit")
    );
    assert_eq!(header(request, "Authorization"), Some("Bearer TOKEN"));
    assert_eq!(header(request, "Prefer"), None);
    assert_eq!(request.body, None);
    Ok(())
}

#[test]
fn an_empty_calendar_list_is_fine() -> TestResult {
    let transport = Scripted::new().json(Method::Get, "/calendars?", 200, "{\"value\": []}");
    assert_eq!(calendars(&api(&transport))?, Vec::new());
    Ok(())
}

#[test]
fn odd_calendar_entries_are_skipped_or_made_safe() -> TestResult {
    let body = concat!(
        "{\"value\": [",
        "{\"id\": \"A\", \"name\": \"Plain\", \"canEdit\": true}, ",
        "{\"id\": \"B\", \"name\": \"No flag\"}, ",
        "{\"id\": \"C\", \"name\": \"Text flag\", \"canEdit\": \"true\"}, ",
        "{\"name\": \"No id\", \"canEdit\": true}, ",
        "{\"id\": \"\", \"name\": \"Empty id\", \"canEdit\": true}, ",
        "{\"id\": 5, \"name\": \"Numeric id\", \"canEdit\": true}, ",
        "\"not an object\", null, [], ",
        "{\"id\": \"D\", \"name\": \"  Messy\\n\\tname\\u202e with\\u200b hidden  \", \"canEdit\": true}, ",
        "{\"id\": \"E\", \"canEdit\": true}, ",
        "{\"id\": \"F\", \"name\": \"   \", \"canEdit\": false}]}"
    );
    let transport = Scripted::new().json(Method::Get, "/calendars?", 200, body);
    assert_eq!(
        calendars(&api(&transport))?,
        vec![
            info("A", "Plain", true),
            info("B", "No flag", false),
            info("C", "Text flag", false),
            info("D", "Messy name with hidden", true),
            info("E", "(no name)", true),
            info("F", "(no name)", false),
        ]
    );
    Ok(())
}

#[test]
fn calendar_names_and_ids_are_bounded() -> TestResult {
    let long_name = "n".repeat(300);
    let longest_id = "i".repeat(512);
    let too_long_id = "i".repeat(513);
    let body = format!(
        concat!(
            "{{\"value\": [{{\"id\": \"{}\", \"name\": \"{}\", \"canEdit\": true}}, ",
            "{{\"id\": \"{}\", \"name\": \"Too long id\", \"canEdit\": true}}]}}"
        ),
        longest_id, long_name, too_long_id
    );
    let transport = Scripted::new().json(Method::Get, "/calendars?", 200, &body);
    let found = calendars(&api(&transport))?;
    assert_eq!(found.len(), 1);
    let first = found.first().ok_or("calendar")?;
    assert_eq!(first.id, longest_id);
    assert!(first.name.starts_with(&"n".repeat(100)));
    assert!(first.name.ends_with("[...]"));
    assert!(first.name.chars().count() <= 106);
    Ok(())
}

#[test]
fn at_most_one_hundred_calendars_are_kept() -> TestResult {
    let items: Vec<String> = (0..150)
        .map(|number| {
            format!("{{\"id\": \"C{number}\", \"name\": \"Calendar {number}\", \"canEdit\": true}}")
        })
        .collect();
    let body = page_with_next(&items, &format!("{GRAPH}/calendars?$skiptoken=more"));
    let transport = Scripted::new().json(Method::Get, "/calendars?", 200, &body);
    let found = calendars(&api(&transport))?;
    assert_eq!(found.len(), 100);
    assert_eq!(found.first().map(|c| c.id.as_str()), Some("C0"));
    assert_eq!(found.last().map(|c| c.id.as_str()), Some("C99"));
    assert_eq!(transport.seen().len(), 1);
    Ok(())
}

#[test]
fn calendar_list_failures_tell_the_model_what_happened() -> TestResult {
    let cases: [(u16, &str, &str); 6] = [
        (401, "{}", "no longer accepts the sign-in"),
        (
            403,
            "{\"error\": {\"message\": \"Access is denied.\"}}",
            "(403): Access is denied.",
        ),
        (429, "{}", "429"),
        (503, "<html>", "temporary problem (503)"),
        (200, "{}", "unexpected answer"),
        (200, "[]", "unexpected answer"),
    ];
    for (status, body, wanted) in cases {
        let transport = Scripted::new().json(Method::Get, "/calendars?", status, body);
        let error = refusal(calendars(&api(&transport)))?;
        assert!(error.message().contains(wanted), "{status}: {error}");
        assert!(matches!(error, ToolError::Msg(_)), "{status}");
    }
    let transport =
        Scripted::new().reply(Method::Get, "/calendars?", vec![Err(HttpError::TimedOut)]);
    let error = refusal(calendars(&api(&transport)))?;
    assert!(error.message().contains("did not answer in time"));
    Ok(())
}

#[test]
fn the_calendar_view_request_is_exact() -> TestResult {
    let transport = Scripted::new().json(Method::Get, "/calendarView?", 200, &page(&[]));
    let found = events(&api(&transport), CAL, week()?, ZONE)?;
    assert!(found.is_empty());
    let seen = transport.seen();
    assert_eq!(seen.len(), 1);
    let request = seen.first().ok_or("request")?;
    assert_eq!(request.method, Method::Get);
    assert_eq!(request.url, list_url());
    assert_eq!(header(request, "Prefer"), Some(PREFER));
    assert_eq!(header(request, "Authorization"), Some("Bearer TOKEN"));
    assert_eq!(header(request, "Content-Type"), None);
    assert_eq!(request.body, None);
    Ok(())
}

#[test]
fn headers_of_the_handle_survive_next_to_the_preference() -> TestResult {
    let transport = Scripted::new().json(Method::Get, "/calendarView?", 200, &page(&[]));
    let handle = api(&transport).with_header("X-Test", "1");
    events(&handle, CAL, week()?, ZONE)?;
    let seen = transport.seen();
    let request = seen.first().ok_or("request")?;
    assert_eq!(header(request, "X-Test"), Some("1"));
    assert_eq!(header(request, "Prefer"), Some(PREFER));
    assert_eq!(header(request, "Authorization"), Some("Bearer TOKEN"));
    Ok(())
}

#[test]
fn the_window_is_widened_by_one_day_in_utc_digits() -> TestResult {
    let transport = Scripted::new().json(Method::Get, "/calendarView?", 200, &page(&[]));
    let window = Window {
        from: dt("2026-03-29 02:30")?,
        to: dt("2026-03-30 13:45")?,
    };
    events(&api(&transport), CAL, window, ZONE)?;
    let seen = transport.seen();
    let request = seen.first().ok_or("request")?;
    assert!(
        request.url.contains(
            "startDateTime=2026-03-28T02%3A30%3A00Z&endDateTime=2026-03-31T13%3A45%3A00Z&$top=100"
        ),
        "{}",
        request.url
    );
    Ok(())
}

#[test]
fn a_window_at_the_edge_of_the_calendar_is_refused_before_any_request() -> TestResult {
    let transport = Scripted::new();
    let early = Window {
        from: dt("0001-01-01 00:00")?,
        to: dt("0001-01-02 00:00")?,
    };
    let late = Window {
        from: dt("9999-12-30 00:00")?,
        to: dt("9999-12-31 23:59")?,
    };
    for window in [early, late] {
        let error = refusal(events(&api(&transport), CAL, window, ZONE))?;
        assert!(error.message().contains("range of days cannot be used"));
    }
    assert!(transport.seen().is_empty());
    Ok(())
}

#[test]
fn events_are_filtered_by_the_local_window() -> TestResult {
    let at = |id: &'static str, start: &'static str, end: &'static str| Ev {
        id,
        start: (start, ZONE),
        end: (end, ZONE),
        ..Ev::default()
    };
    let items = [
        at(
            "before",
            "2026-11-18T09:00:00.0000000",
            "2026-11-18T10:00:00.0000000",
        ),
        at(
            "into",
            "2026-11-19T23:00:00.0000000",
            "2026-11-20T01:00:00.0000000",
        ),
        at(
            "inside",
            "2026-11-22T09:00:00.0000000",
            "2026-11-22T10:00:00.0000000",
        ),
        at(
            "out",
            "2026-11-26T23:00:00.0000000",
            "2026-11-27T02:00:00.0000000",
        ),
        at(
            "after",
            "2026-11-27T00:00:00.0000000",
            "2026-11-27T01:00:00.0000000",
        ),
        at(
            "touching",
            "2026-11-19T23:00:00.0000000",
            "2026-11-20T00:00:00.0000000",
        ),
        at(
            "instant",
            "2026-11-20T00:00:00.0000000",
            "2026-11-20T00:00:00.0000000",
        ),
        at(
            "early instant",
            "2026-11-19T12:00:00.0000000",
            "2026-11-19T12:00:00.0000000",
        ),
    ]
    .map(|ev| ev.json());
    let transport = Scripted::new().json(Method::Get, "/calendarView?", 200, &page(&items));
    let found = events(&api(&transport), CAL, week()?, ZONE)?;
    let ids: Vec<&str> = found.iter().map(|event| event.uid.as_str()).collect();
    assert_eq!(ids, ["into", "inside", "out", "instant"]);
    Ok(())
}

#[test]
fn events_are_mapped_field_by_field() -> TestResult {
    let plain = Ev {
        id: "plain",
        ..Ev::default()
    };
    let busy = Ev {
        id: "busy",
        subject: String::new(),
        location: String::new(),
        attendees: Some(2),
        ..Ev::default()
    };
    let blank = Ev {
        id: "blank",
        subject: "   \t ".to_string(),
        attendees: None,
        ..Ev::default()
    };
    let nameless = Ev {
        id: "nameless",
        ..Ev::default()
    }
    .json()
    .replace("\"subject\": \"Dentist\"", "\"subject\": null")
    .replace(
        "\"location\": {\"displayName\": \"Praxis\", \"locationType\": \"default\"}, ",
        "",
    );
    let items = [plain.json(), busy.json(), blank.json(), nameless];
    let transport = Scripted::new().json(Method::Get, "/calendarView?", 200, &page(&items));
    let found = events(&api(&transport), CAL, week()?, ZONE)?;
    assert_eq!(
        found,
        vec![
            raw(
                "plain",
                "Dentist",
                "2026-11-20 09:00",
                "2026-11-20 10:00",
                false,
                "Praxis",
                Some(0)
            )?,
            raw(
                "busy",
                "(no title)",
                "2026-11-20 09:00",
                "2026-11-20 10:00",
                false,
                "",
                Some(2)
            )?,
            raw(
                "blank",
                "(no title)",
                "2026-11-20 09:00",
                "2026-11-20 10:00",
                false,
                "Praxis",
                None
            )?,
            raw(
                "nameless",
                "(no title)",
                "2026-11-20 09:00",
                "2026-11-20 10:00",
                false,
                "",
                Some(0)
            )?,
        ]
    );
    Ok(())
}

#[test]
fn cancelled_events_are_skipped_and_series_instances_are_listed_like_any_other() -> TestResult {
    let cancelled = Ev {
        id: "cancelled",
        cancelled: true,
        ..Ev::default()
    };
    let occurrence = Ev {
        id: "occurrence",
        kind: "occurrence",
        series: Some("master"),
        ..Ev::default()
    };
    let exception = Ev {
        id: "exception",
        kind: "exception",
        series: Some("master"),
        ..Ev::default()
    };
    let unmarked = Ev {
        id: "unmarked",
        ..Ev::default()
    }
    .json()
    .replace("\"isCancelled\": false, ", "");
    let items = [
        cancelled.json(),
        "{\"id\": \"gone\", \"isCancelled\": true, \"start\": 5}".to_string(),
        occurrence.json(),
        exception.json(),
        unmarked,
    ];
    let transport = Scripted::new().json(Method::Get, "/calendarView?", 200, &page(&items));
    let found = events(&api(&transport), CAL, week()?, ZONE)?;
    let ids: Vec<&str> = found.iter().map(|event| event.uid.as_str()).collect();
    assert_eq!(ids, ["occurrence", "exception", "unmarked"]);
    for event in &found {
        assert_eq!(event.recurrence, "");
        assert!(event.excluded.is_empty());
        assert_eq!(event.utc_offset, 0);
        assert_eq!(event.attendees, Some(0));
    }
    Ok(())
}

#[test]
fn hostile_text_is_kept_whole_when_short_and_bounded_when_long() -> TestResult {
    let nasty = concat!(
        "Dentist \"quoted\" \\ back\nnewline\ttab \u{202e}rtl \u{200b}zw \u{0} nul ",
        "<script>alert(1)</script> {\"json\": true} \u{1f600}"
    );
    let long = "x".repeat(5_000);
    let hostile = Ev {
        id: "hostile",
        subject: nasty.to_string(),
        location: long.clone(),
        ..Ev::default()
    };
    let longer = Ev {
        id: "longer",
        subject: long.clone(),
        location: nasty.to_string(),
        ..Ev::default()
    };
    let edge = Ev {
        id: "edge",
        subject: "y".repeat(MAX_SUMMARY_READ),
        location: "z".repeat(MAX_LOCATION_READ + 1),
        ..Ev::default()
    };
    let items = [hostile.json(), longer.json(), edge.json()];
    let transport = Scripted::new().json(Method::Get, "/calendarView?", 200, &page(&items));
    let found = events(&api(&transport), CAL, week()?, ZONE)?;
    let [first, second, third] = found.as_slice() else {
        return Err("three events".into());
    };
    assert_eq!(first.summary, nasty);
    assert_eq!(
        first.location,
        format!("{} [...]", "x".repeat(MAX_LOCATION_READ))
    );
    assert_eq!(
        second.summary,
        format!("{} [...]", "x".repeat(MAX_SUMMARY_READ))
    );
    assert_eq!(second.location, nasty);
    assert_eq!(third.summary, "y".repeat(MAX_SUMMARY_READ));
    assert_eq!(
        third.location,
        format!("{} [...]", "z".repeat(MAX_LOCATION_READ))
    );
    for event in [first, second] {
        let detail = EventDetail {
            event: event.clone(),
            notes: String::new(),
            url: String::new(),
            people: Vec::new(),
        };
        assert!(!EventFields::of(&detail).fits_the_backup());
    }
    Ok(())
}

#[test]
fn paging_follows_next_links_with_the_same_preference() -> TestResult {
    let transport = chain(2, false);
    let found = events(&api(&transport), CAL, week()?, ZONE)?;
    let ids: Vec<&str> = found.iter().map(|event| event.uid.as_str()).collect();
    assert_eq!(ids, ["P1", "P2"]);
    assert_eq!(
        calls(&transport),
        vec![
            (Method::Get, list_url()),
            (Method::Get, format!("{}&$skiptoken=P2", list_url())),
        ]
    );
    for request in transport.seen() {
        assert_eq!(header(&request, "Prefer"), Some(PREFER));
    }
    Ok(())
}

#[test]
fn graph_style_next_links_are_followed() -> TestResult {
    let link = concat!(
        "https://graph.microsoft.com/v1.0/users('00000000-0000-0000-0000-000000000000')",
        "/calendars('AQMkADAw')/calendarView?startDateTime=2026-11-19T00%3A00%3A00Z",
        "&endDateTime=2026-11-28T00%3A00%3A00Z&%24top=100&%24skip=100"
    );
    let first = page_with_next(&[single("first", 1)], link);
    let second = page(&[single("second", 2)]);
    let transport = Scripted::new()
        .json(Method::Get, "%24skip=100", 200, &second)
        .json(Method::Get, "/calendarView?", 200, &first);
    let found = events(&api(&transport), CAL, week()?, ZONE)?;
    let ids: Vec<&str> = found.iter().map(|event| event.uid.as_str()).collect();
    assert_eq!(ids, ["first", "second"]);
    assert_eq!(
        calls(&transport).get(1).map(|call| call.1.as_str()),
        Some(link)
    );
    Ok(())
}

#[test]
fn next_links_that_leave_graph_are_refused() -> TestResult {
    let long = format!("{ORIGIN}{}", "a".repeat(5_000));
    let links = [
        "https://evil.example/v1.0/me/calendars/x/calendarView?a=1",
        "http://graph.microsoft.com/v1.0/me/x",
        "https://graph.microsoft.com.evil.example/v1.0/me/x",
        "https://graph.microsoft.com@evil.example/v1.0/me/x",
        "https://graph.microsoft.com/beta/me/x",
        "https://graph.microsoft.com/v1.0",
        "https://login.microsoftonline.com/v1.0/x",
        "/v1.0/me/x",
        "https://graph.microsoft.com/v1.0/me/calendars/x/calendarView?$skip=[1-9999]",
        "https://graph.microsoft.com/v1.0/me/calendars/x/calendarView?$skip={1,2}",
        "https://graph.microsoft.com/v1.0/me/calendars/../users",
        "https://graph.microsoft.com/v1.0/./me/calendars",
        "https://graph.microsoft.com/v1.0/me/calendars/%2E%2e/users",
        "",
        long.as_str(),
    ];
    for link in links {
        let body = page_with_next(&[single("first", 1)], link);
        let transport = Scripted::new().json(Method::Get, "/calendarView?", 200, &body);
        let error = refusal(events(&api(&transport), CAL, week()?, ZONE))?;
        assert!(
            error.message().contains("unexpected answer"),
            "{link}: {error}"
        );
        assert_eq!(transport.seen().len(), 1, "{link}");
    }
    let numeric = "{\"value\": [], \"@odata.nextLink\": 5}";
    let transport = Scripted::new().json(Method::Get, "/calendarView?", 200, numeric);
    assert!(refusal(events(&api(&transport), CAL, week()?, ZONE)).is_ok());
    let nothing = "{\"value\": [], \"@odata.nextLink\": null}";
    let transport = Scripted::new().json(Method::Get, "/calendarView?", 200, nothing);
    assert!(events(&api(&transport), CAL, week()?, ZONE)?.is_empty());
    Ok(())
}

#[test]
fn at_most_four_pages_are_read() -> TestResult {
    let transport = chain(4, false);
    let found = events(&api(&transport), CAL, week()?, ZONE)?;
    assert_eq!(found.len(), 4);
    assert_eq!(transport.seen().len(), 4);
    let transport = chain(4, true);
    let error = refusal(events(&api(&transport), CAL, week()?, ZONE))?;
    assert!(error.message().contains("too many events"), "{error}");
    assert!(matches!(error, ToolError::Msg(_)));
    assert_eq!(transport.seen().len(), 4);
    Ok(())
}

#[test]
fn too_many_events_are_refused() -> TestResult {
    let numbered = |count: usize| -> Vec<String> {
        (0..count)
            .map(|number| single(EV, number).replace(EV, &format!("E{number}")))
            .collect()
    };
    let transport = Scripted::new().json(Method::Get, "/calendarView?", 200, &page(&numbered(500)));
    assert_eq!(events(&api(&transport), CAL, week()?, ZONE)?.len(), 500);
    let transport = Scripted::new().json(Method::Get, "/calendarView?", 200, &page(&numbered(501)));
    let error = refusal(events(&api(&transport), CAL, week()?, ZONE))?;
    assert!(error.message().contains("too many events"), "{error}");
    let crowd = vec!["{}".to_string(); MAX_PAGE_ITEMS + 1];
    let transport = Scripted::new().json(Method::Get, "/calendarView?", 200, &page(&crowd));
    let error = refusal(events(&api(&transport), CAL, week()?, ZONE))?;
    assert!(error.message().contains("too many events"), "{error}");
    assert_eq!(
        i64::try_from(MAX_EVENTS)?,
        crate::calendar::MAX_SELECTED_EVENTS
    );
    Ok(())
}

#[test]
fn events_outside_the_window_do_not_count_against_the_limit() -> TestResult {
    let old = Ev {
        start: ("2026-11-10T09:00:00.0000000", ZONE),
        end: ("2026-11-10T10:00:00.0000000", ZONE),
        ..Ev::default()
    }
    .json();
    let mut items = vec![old; 600];
    items.push(single("kept", 1));
    let transport = Scripted::new().json(Method::Get, "/calendarView?", 200, &page(&items));
    let found = events(&api(&transport), CAL, week()?, ZONE)?;
    assert_eq!(found.len(), 1);
    Ok(())
}

#[test]
fn a_calendar_that_does_not_exist_is_reported() -> TestResult {
    let transport = Scripted::new().json(Method::Get, "/calendarView?", 404, "{}");
    let error = refusal(events(&api(&transport), CAL, week()?, ZONE))?;
    assert!(error.message().contains("no longer exists"), "{error}");
    let later = Scripted::new()
        .json(Method::Get, "$skiptoken=P2", 404, "{}")
        .json(
            Method::Get,
            "/calendarView?",
            200,
            &page_with_next(
                &[single("first", 1)],
                &format!("{}&$skiptoken=P2", list_url()),
            ),
        );
    assert!(refusal(events(&api(&later), CAL, week()?, ZONE)).is_ok());
    Ok(())
}

#[test]
fn malformed_events_fail_closed() -> TestResult {
    let base = Ev::default().json();
    let start = "{\"dateTime\": \"2026-11-20T09:00:00.0000000\", \"timeZone\": \"Europe/Zurich\"}";
    let end = "{\"dateTime\": \"2026-11-20T10:00:00.0000000\", \"timeZone\": \"Europe/Zurich\"}";
    let cases: Vec<(&str, String)> = vec![
        ("string", "\"x\"".to_string()),
        ("number", "5".to_string()),
        ("null", "null".to_string()),
        ("array", "[]".to_string()),
        ("no id", base.replace(&format!("\"id\": {}, ", q(EV)), "")),
        ("empty id", base.replace(&q(EV), "\"\"")),
        ("numeric id", base.replace(&q(EV), "5")),
        ("long id", base.replace(&q(EV), &q(&"i".repeat(513)))),
        ("no flag", base.replace("\"isAllDay\": false, ", "")),
        (
            "text flag",
            base.replace("\"isAllDay\": false", "\"isAllDay\": \"false\""),
        ),
        (
            "null flag",
            base.replace("\"isAllDay\": false", "\"isAllDay\": null"),
        ),
        (
            "no start",
            base.replace(&format!("\"start\": {start}, "), ""),
        ),
        ("no end", base.replace(&format!("\"end\": {end}, "), "")),
        ("start text", base.replace(start, "\"2026-11-20T09:00\"")),
        ("end text", base.replace(end, "[]")),
        (
            "start without time",
            base.replace(start, "{\"timeZone\": \"Europe/Zurich\"}"),
        ),
        (
            "start without zone",
            base.replace(start, "{\"dateTime\": \"2026-11-20T09:00:00.0000000\"}"),
        ),
        (
            "start garbage",
            base.replace("2026-11-20T09:00:00.0000000", "yesterday"),
        ),
        (
            "start with Z",
            base.replace("2026-11-20T09:00:00.0000000", "2026-11-20T09:00:00Z"),
        ),
        (
            "start with offset",
            base.replace("2026-11-20T09:00:00.0000000", "2026-11-20T09:00:00+01:00"),
        ),
        (
            "end garbage",
            base.replace("2026-11-20T10:00:00.0000000", "tomorrow"),
        ),
        (
            "impossible date",
            base.replace("2026-11-20T09:00:00.0000000", "2026-02-30T09:00:00.0000000"),
        ),
    ];
    for (name, item) in cases {
        let transport = Scripted::new().json(Method::Get, "/calendarView?", 200, &page(&[item]));
        let error = refusal(events(&api(&transport), CAL, week()?, ZONE))?;
        assert!(
            error.message().contains("cannot be read safely"),
            "{name}: {error}"
        );
        assert!(matches!(error, ToolError::Msg(_)), "{name}");
    }
    Ok(())
}

#[test]
fn a_malformed_page_fails_closed() -> TestResult {
    for body in [
        "[]",
        "\"x\"",
        "null",
        "",
        "{}",
        "{\"value\": {}}",
        "{\"value\": \"x\"}",
        "{\"value\": null}",
        "not json at all",
    ] {
        let transport = Scripted::new().json(Method::Get, "/calendarView?", 200, body);
        let error = refusal(events(&api(&transport), CAL, week()?, ZONE))?;
        assert!(
            error.message().contains("unexpected answer"),
            "{body}: {error}"
        );
    }
    Ok(())
}

#[test]
fn times_in_another_zone_are_refused() -> TestResult {
    let utc = Ev {
        start: ("2026-11-20T08:00:00.0000000", "UTC"),
        end: ("2026-11-20T09:00:00.0000000", "UTC"),
        ..Ev::default()
    };
    let windows = Ev {
        start: ("2026-11-20T09:00:00.0000000", "W. Europe Standard Time"),
        end: ("2026-11-20T10:00:00.0000000", "W. Europe Standard Time"),
        ..Ev::default()
    };
    let mixed = Ev {
        end: ("2026-11-20T09:00:00.0000000", "UTC"),
        ..Ev::default()
    };
    for ev in [utc, windows, mixed] {
        let transport =
            Scripted::new().json(Method::Get, "/calendarView?", 200, &page(&[ev.json()]));
        let error = refusal(events(&api(&transport), CAL, week()?, ZONE))?;
        assert!(error.message().contains("time zone of this Mac"), "{error}");
        assert!(matches!(error, ToolError::Msg(_)));
    }
    Ok(())
}

#[test]
fn zone_labels_are_compared_without_regard_to_case_and_utc_spellings() -> TestResult {
    let lower = Ev {
        start: ("2026-11-20T09:00:00.0000000", "europe/zurich"),
        end: ("2026-11-20T10:00:00.0000000", "EUROPE/ZURICH"),
        ..Ev::default()
    };
    let transport =
        Scripted::new().json(Method::Get, "/calendarView?", 200, &page(&[lower.json()]));
    assert_eq!(events(&api(&transport), CAL, week()?, ZONE)?.len(), 1);
    let utc = Ev {
        start: ("2026-11-20T09:00:00.0000000", "UTC"),
        end: ("2026-11-20T10:00:00.0000000", "Etc/UTC"),
        ..Ev::default()
    };
    let transport = Scripted::new().json(Method::Get, "/calendarView?", 200, &page(&[utc.json()]));
    let found = events(&api(&transport), CAL, week()?, "UTC")?;
    assert_eq!(found.len(), 1);
    let seen = transport.seen();
    let request = seen.first().ok_or("request")?;
    assert_eq!(header(request, "Prefer"), Some(PREFER_UTC));
    Ok(())
}

#[test]
fn unsafe_zones_and_calendar_ids_never_reach_a_request() -> TestResult {
    let transport = Scripted::new().json(Method::Get, "/calendarView?", 200, &page(&[]));
    let long = "z".repeat(65);
    for zone in [
        "",
        "Europe/Zurich\"",
        "Europe/Zurich, IdType=\"x\"",
        "Europe/Zurich\r\nX-Evil: 1",
        "../etc",
        "/etc/localtime",
        "Europe/Z\u{fc}rich",
        long.as_str(),
    ] {
        let error = refusal(events(&api(&transport), CAL, week()?, zone))?;
        assert!(
            error.message().contains("time zone of this Mac"),
            "{zone:?}: {error}"
        );
    }
    let too_long = "c".repeat(513);
    for calendar in ["", too_long.as_str()] {
        let error = refusal(events(&api(&transport), calendar, week()?, ZONE))?;
        assert!(error.message().contains("no longer exists"), "{error}");
    }
    assert!(transport.seen().is_empty());
    Ok(())
}

#[test]
fn calendar_ids_are_percent_encoded_in_the_path() -> TestResult {
    let transport = Scripted::new().json(Method::Get, "/calendarView?", 200, &page(&[]));
    events(&api(&transport), "a/b?c#d e\"f%g", week()?, ZONE)?;
    let seen = transport.seen();
    let request = seen.first().ok_or("request")?;
    assert!(
        request.url.starts_with(&format!(
            "{GRAPH}/calendars/a%2Fb%3Fc%23d%20e%22f%25g/calendarView?"
        )),
        "{}",
        request.url
    );
    Ok(())
}

#[test]
fn list_failures_tell_the_model_what_happened() -> TestResult {
    let statuses: [(u16, &str); 4] = [
        (401, "no longer accepts the sign-in"),
        (403, "refused the request (403)"),
        (429, "limits requests"),
        (500, "temporary problem (500)"),
    ];
    for (status, wanted) in statuses {
        let transport = Scripted::new().json(Method::Get, "/calendarView?", status, "{}");
        let error = refusal(events(&api(&transport), CAL, week()?, ZONE))?;
        assert!(error.message().contains(wanted), "{status}: {error}");
        assert!(matches!(error, ToolError::Msg(_)), "{status}");
    }
    let transports: [(HttpError, &str); 3] = [
        (HttpError::TimedOut, "did not answer in time"),
        (HttpError::TooLarge, "too large"),
        (
            HttpError::Failed("curl exit 6".into()),
            "could not be reached",
        ),
    ];
    for (failure, wanted) in transports {
        let transport = Scripted::new().reply(Method::Get, "/calendarView?", vec![Err(failure)]);
        let error = refusal(events(&api(&transport), CAL, week()?, ZONE))?;
        assert!(error.message().contains(wanted), "{error}");
        assert!(matches!(error, ToolError::Msg(_)));
    }
    Ok(())
}
