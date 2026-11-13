use super::*;

const TENNIS_BODY: &str = concat!(
    "{\"subject\": \"Tennis\", \"body\": {\"contentType\": \"text\", \"content\": \"Rackets\\nBalls\"}, ",
    "\"location\": {\"displayName\": \"Court 3\"}, \"isAllDay\": false, ",
    "\"start\": {\"dateTime\": \"2026-11-20T18:30:00\", \"timeZone\": \"Europe/Zurich\"}, ",
    "\"end\": {\"dateTime\": \"2026-11-20T19:45:00\", \"timeZone\": \"Europe/Zurich\"}}"
);

fn tennis() -> Res<EventFields> {
    fields(
        "Tennis",
        "2026-11-20 18:30",
        "2026-11-20 19:45",
        false,
        "Court 3",
        "Rackets\nBalls",
    )
}

fn tennis_ev() -> Ev {
    Ev {
        id: NEW,
        subject: "Tennis".to_string(),
        start: ("2026-11-20T18:30:00.0000000", ZONE),
        end: ("2026-11-20T19:45:00.0000000", ZONE),
        location: "Court 3".to_string(),
        body: Some(("text", "Rackets\nBalls".to_string())),
        ..Ev::default()
    }
}

fn posted(transport: Scripted, status: u16, body: &str) -> Scripted {
    transport.json(Method::Post, "/events", status, body)
}

fn creating() -> Scripted {
    let answer = format!("{{\"id\": {}, \"subject\": \"Tennis\"}}", q(NEW));
    posted(editable(Scripted::new(), true), 201, &answer).json(
        Method::Get,
        "/events/",
        200,
        &tennis_ev().json(),
    )
}

fn create_url() -> String {
    format!("{GRAPH}/calendars/{CAL_ENC}/events")
}

fn create_with(after: EventFields) -> Change {
    Change::Create { after }
}

#[test]
fn an_approved_create_posts_once_and_reads_back() -> TestResult {
    let transport = creating();
    let (result, host) = run(&transport, "approved", &create_with(tennis()?), false);
    assert_eq!(
        result?,
        ChangeResult {
            decision: Decision::Applied,
            uid: Some(NEW.to_string()),
            after: Some(tennis()?),
        }
    );
    assert_eq!(
        calls(&transport),
        vec![
            (Method::Get, editable_url()),
            (Method::Get, editable_url()),
            (Method::Post, create_url()),
            (Method::Get, detail_url(NEW_ENC)),
        ]
    );
    assert_eq!(host.count(), 1);
    assert_eq!(host.requests_before_the_dialog(), vec![1]);
    let sent = writes(&transport);
    let [post] = sent.as_slice() else {
        return Err("exactly one write".into());
    };
    assert_eq!(header(post, "Prefer"), Some(PREFER_IDS));
    assert_eq!(header(post, "Content-Type"), Some("application/json"));
    assert_eq!(header(post, "Authorization"), Some("Bearer TOKEN"));
    assert_eq!(
        json::parse(post.body.as_deref().ok_or("body")?)?,
        j(TENNIS_BODY)?
    );
    let seen = transport.seen();
    assert_eq!(seen.first().and_then(|r| header(r, "Prefer")), None);
    assert_eq!(
        seen.get(3).and_then(|r| header(r, "Prefer")),
        Some(PREFER_TEXT)
    );
    Ok(())
}

#[test]
fn the_dialog_shows_exactly_what_is_about_to_be_created() -> TestResult {
    let transport = creating();
    let (_, host) = run(&transport, "approved", &create_with(tennis()?), false);
    let payload = host.payload()?;
    let object = payload.as_object().ok_or("object")?;
    assert_eq!(
        object.get("op").and_then(Json::as_str),
        Some("approve_change")
    );
    assert_eq!(object.get("action").and_then(Json::as_str), Some("create"));
    assert_eq!(
        object.get("calendarLabel").and_then(Json::as_str),
        Some("Calendar")
    );
    assert_eq!(object.get("undo"), Some(&Json::Bool(false)));
    assert_eq!(object.get("seconds"), Some(&Json::Int(75)));
    assert_eq!(object.get("before"), Some(&Json::Null));
    assert_eq!(object.get("after"), Some(&tennis()?.to_json()));
    let transport = creating();
    let (_, host) = run(&transport, "approved", &create_with(tennis()?), true);
    let payload = host.payload()?;
    assert_eq!(
        payload.as_object().and_then(|o| o.get("undo")),
        Some(&Json::Bool(true))
    );
    Ok(())
}

#[test]
fn a_declined_or_ignored_create_sends_nothing() -> TestResult {
    for (answer, decision) in [
        ("declined", Decision::Declined),
        ("timeout", Decision::TimedOut),
    ] {
        let transport = creating();
        let (result, host) = run(&transport, answer, &create_with(tennis()?), false);
        assert_eq!(
            result?,
            ChangeResult {
                decision,
                uid: None,
                after: None,
            }
        );
        assert_eq!(calls(&transport), vec![(Method::Get, editable_url())]);
        assert!(writes(&transport).is_empty());
        assert_eq!(host.count(), 1);
    }
    Ok(())
}

#[test]
fn an_all_day_create_writes_midnights_in_the_zone() -> TestResult {
    let after = fields("Trip", "2026-11-24 00:00", "2026-11-26 00:00", true, "", "")?;
    let stored = Ev {
        id: NEW,
        subject: "Trip".to_string(),
        ..Ev::all_day(
            NEW,
            "2026-11-24T00:00:00.0000000",
            "2026-11-26T00:00:00.0000000",
            ZONE,
        )
    };
    let transport = posted(
        editable(Scripted::new(), true),
        201,
        &format!("{{\"id\": {}}}", q(NEW)),
    )
    .json(Method::Get, "/events/", 200, &stored.json());
    let (result, _) = run(&transport, "approved", &create_with(after.clone()), false);
    assert_eq!(result?.after, Some(after));
    let sent = writes(&transport);
    let post = sent.first().ok_or("post")?;
    let expected = concat!(
        "{\"subject\": \"Trip\", \"body\": {\"contentType\": \"text\", \"content\": \"\"}, ",
        "\"location\": {\"displayName\": \"\"}, \"isAllDay\": true, ",
        "\"start\": {\"dateTime\": \"2026-11-24T00:00:00\", \"timeZone\": \"Europe/Zurich\"}, ",
        "\"end\": {\"dateTime\": \"2026-11-26T00:00:00\", \"timeZone\": \"Europe/Zurich\"}}"
    );
    assert_eq!(
        json::parse(post.body.as_deref().ok_or("body")?)?,
        j(expected)?
    );
    Ok(())
}

#[test]
fn hostile_text_is_sent_as_json_data_only() -> TestResult {
    let nasty = "x\"}, \"attendees\": [{\"emailAddress\": {\"address\": \"evil@example.invalid\"}}], \"y\": \"\n\u{202e}\u{0}";
    let after = fields(
        nasty,
        "2026-11-20 18:30",
        "2026-11-20 19:00",
        false,
        nasty,
        nasty,
    )?;
    let transport = creating();
    let (result, _) = run(&transport, "approved", &create_with(after), false);
    assert!(result.is_ok());
    let sent = writes(&transport);
    let post = sent.first().ok_or("post")?;
    let text = post.body.as_deref().ok_or("body")?;
    assert!(text.is_ascii());
    let parsed = json::parse(text)?;
    let object = parsed.as_object().ok_or("object")?;
    let keys: Vec<&str> = object.keys().collect();
    assert_eq!(
        keys,
        ["subject", "body", "location", "isAllDay", "start", "end"]
    );
    assert_eq!(object.get("subject").and_then(Json::as_str), Some(nasty));
    Ok(())
}

#[test]
fn a_create_in_a_read_only_calendar_is_refused_before_the_dialog() -> TestResult {
    let cases = [
        ("false", "{\"canEdit\": false}"),
        ("missing", "{\"id\": \"x\"}"),
        ("text", "{\"canEdit\": \"true\"}"),
        ("number", "{\"canEdit\": 1}"),
        ("array", "[]"),
    ];
    for (name, body) in cases {
        let transport = Scripted::new()
            .json(Method::Get, "?$select=canEdit", 200, body)
            .json(Method::Post, "/events", 201, "{}")
            .json(Method::Get, "/events/", 200, &tennis_ev().json());
        let (result, host) = run(&transport, "approved", &create_with(tennis()?), false);
        let error = refusal(result)?;
        assert_eq!(
            error.message(),
            "This calendar is read-only, so NOTHING was changed.",
            "{name}"
        );
        assert!(matches!(error, ToolError::Msg(_)), "{name}");
        assert_eq!(host.count(), 0, "{name}");
        assert!(writes(&transport).is_empty(), "{name}");
    }
    let transport = Scripted::new().json(Method::Get, "?$select=canEdit", 404, "{}");
    let (result, host) = run(&transport, "approved", &create_with(tennis()?), false);
    assert!(refusal(result)?.message().contains("no longer exists"));
    assert_eq!(host.count(), 0);
    Ok(())
}

#[test]
fn a_calendar_that_turns_read_only_while_the_dialog_is_open_is_refused() -> TestResult {
    let transport = Scripted::new()
        .reply(
            Method::Get,
            "?$select=canEdit",
            vec![ok("{\"canEdit\": true}"), ok("{\"canEdit\": false}")],
        )
        .json(Method::Post, "/events", 201, "{}");
    let (result, host) = run(&transport, "approved", &create_with(tennis()?), false);
    assert!(refusal(result)?.message().contains("read-only"));
    assert_eq!(host.count(), 1);
    assert!(writes(&transport).is_empty());
    assert_eq!(transport.seen().len(), 2);
    Ok(())
}

#[test]
fn notes_and_links_are_limited_before_anything_happens() -> TestResult {
    let noted = |count: usize| -> Res<EventFields> {
        fields(
            "Note",
            "2026-11-20 18:30",
            "2026-11-20 19:00",
            false,
            "",
            &"\u{e9}".repeat(count),
        )
    };
    for (undo, count) in [(false, 1_200), (true, 4_000)] {
        let transport = creating();
        let (result, _) = run(&transport, "approved", &create_with(noted(count)?), undo);
        assert!(result.is_ok(), "{undo} {count}");
    }
    for (undo, count) in [(false, 1_201), (true, 4_001)] {
        let transport = creating();
        let (result, host) = run(&transport, "approved", &create_with(noted(count)?), undo);
        let error = refusal(result)?;
        assert_eq!(
            error.message(),
            "The notes are too long, so NOTHING was changed."
        );
        assert!(transport.seen().is_empty(), "{undo} {count}");
        assert_eq!(host.count(), 0);
    }
    let linked = EventFields {
        url: "https://example.invalid/x".to_string(),
        ..tennis()?
    };
    let transport = creating();
    let (result, host) = run(&transport, "approved", &create_with(linked), false);
    assert_eq!(
        refusal(result)?.message(),
        "Links are not supported for this kind of calendar, so NOTHING was changed."
    );
    assert!(transport.seen().is_empty());
    assert_eq!(host.count(), 0);
    Ok(())
}

#[test]
fn impossible_times_are_refused_before_anything_happens() -> TestResult {
    let cases = [
        (
            "all-day with a clock time",
            fields("Trip", "2026-11-24 09:00", "2026-11-26 00:00", true, "", "")?,
        ),
        (
            "all-day end with a clock time",
            fields("Trip", "2026-11-24 00:00", "2026-11-26 09:00", true, "", "")?,
        ),
        (
            "all-day without length",
            fields("Trip", "2026-11-24 00:00", "2026-11-24 00:00", true, "", "")?,
        ),
        (
            "all-day backwards",
            fields("Trip", "2026-11-26 00:00", "2026-11-24 00:00", true, "", "")?,
        ),
        (
            "timed backwards",
            fields(
                "Talk",
                "2026-11-20 10:00",
                "2026-11-20 09:00",
                false,
                "",
                "",
            )?,
        ),
    ];
    for (name, after) in cases {
        let transport = creating();
        let (result, host) = run(&transport, "approved", &create_with(after), false);
        let error = refusal(result)?;
        assert!(
            error.message().contains("not valid for this calendar"),
            "{name}: {error}"
        );
        assert!(transport.seen().is_empty(), "{name}");
        assert_eq!(host.count(), 0, "{name}");
    }
    let instant = fields(
        "Ping",
        "2026-11-20 10:00",
        "2026-11-20 10:00",
        false,
        "",
        "",
    )?;
    let transport = creating();
    let (result, _) = run(&transport, "approved", &create_with(instant), false);
    assert!(result.is_ok());
    Ok(())
}

#[test]
fn a_create_whose_answer_has_no_usable_id_is_uncertain() -> TestResult {
    let long = format!("{{\"id\": \"{}\"}}", "i".repeat(513));
    for body in [
        "{}",
        "{\"id\": \"\"}",
        "{\"id\": 5}",
        "{\"id\": null}",
        "[]",
        "",
        long.as_str(),
    ] {
        let transport = posted(editable(Scripted::new(), true), 201, body);
        let (result, _) = run(&transport, "approved", &create_with(tennis()?), false);
        let error = refusal(result)?;
        assert!(
            matches!(error, ToolError::Uncertain(_)),
            "{body}: {error:?}"
        );
        assert!(
            error.message().contains("may or may not have been created"),
            "{error}"
        );
        assert_eq!(writes(&transport).len(), 1);
        assert_eq!(transport.seen().len(), 3);
    }
    Ok(())
}

#[test]
fn write_failures_are_uncertain_or_refused_as_the_provider_says() -> TestResult {
    let statuses: [(u16, bool); 7] = [
        (400, false),
        (403, false),
        (404, false),
        (409, false),
        (429, false),
        (500, true),
        (503, true),
    ];
    for (status, uncertain) in statuses {
        let transport = posted(editable(Scripted::new(), true), status, "{}");
        let (result, _) = run(&transport, "approved", &create_with(tennis()?), false);
        let error = refusal(result)?;
        assert_eq!(
            matches!(error, ToolError::Uncertain(_)),
            uncertain,
            "{status}: {error:?}"
        );
        assert_eq!(writes(&transport).len(), 1, "{status}");
    }
    for failure in [
        HttpError::TimedOut,
        HttpError::TooLarge,
        HttpError::Failed("curl exit 52".to_string()),
    ] {
        let transport =
            editable(Scripted::new(), true).reply(Method::Post, "/events", vec![Err(failure)]);
        let (result, _) = run(&transport, "approved", &create_with(tennis()?), false);
        assert!(matches!(refusal(result)?, ToolError::Uncertain(_)));
        assert_eq!(writes(&transport).len(), 1);
    }
    let transport = posted(editable(Scripted::new(), true), 201, "junk");
    let (result, _) = run(&transport, "approved", &create_with(tennis()?), false);
    assert!(matches!(refusal(result)?, ToolError::Uncertain(_)));
    Ok(())
}

#[test]
fn a_failing_or_surprising_read_back_never_turns_a_created_event_into_an_error() -> TestResult {
    let stored = |ev: Ev| -> Scripted {
        posted(
            editable(Scripted::new(), true),
            201,
            &format!("{{\"id\": {}}}", q(NEW)),
        )
        .json(Method::Get, "/events/", 200, &ev.json())
    };
    let elsewhere = Ev {
        calendar: Some(OTHER_CAL),
        ..tennis_ev()
    };
    let cancelled = Ev {
        cancelled: true,
        ..tennis_ev()
    };
    let transports = [
        stored(elsewhere),
        stored(cancelled),
        posted(
            editable(Scripted::new(), true),
            201,
            &format!("{{\"id\": {}}}", q(NEW)),
        )
        .json(Method::Get, "/events/", 500, "{}"),
        posted(
            editable(Scripted::new(), true),
            201,
            &format!("{{\"id\": {}}}", q(NEW)),
        )
        .json(Method::Get, "/events/", 404, "{}"),
        posted(
            editable(Scripted::new(), true),
            201,
            &format!("{{\"id\": {}}}", q(NEW)),
        )
        .json(Method::Get, "/events/", 200, "junk"),
    ];
    for transport in transports {
        let (result, _) = run(&transport, "approved", &create_with(tennis()?), false);
        assert_eq!(
            result?,
            ChangeResult {
                decision: Decision::Applied,
                uid: Some(NEW.to_string()),
                after: None,
            }
        );
        assert_eq!(writes(&transport).len(), 1);
    }
    let adjusted = Ev {
        start: ("2026-11-20T19:30:00.0000000", ZONE),
        ..tennis_ev()
    };
    let transport = stored(adjusted);
    let (result, _) = run(&transport, "approved", &create_with(tennis()?), false);
    let after = result?.after.ok_or("after")?;
    assert_eq!(after.start.to_string(), "2026-11-20 19:30");
    assert_ne!(after, tennis()?);
    Ok(())
}

#[test]
fn a_dialog_that_fails_stops_everything() -> TestResult {
    let transport = creating();
    let approval = Approval {
        seconds: 75,
        undo: false,
    };
    let result = change(
        &api(&transport),
        &Broken,
        &calendar(),
        CAL,
        ZONE,
        &create_with(tennis()?),
        &approval,
    );
    let error = refusal(result)?;
    assert!(
        error.message().contains("approval dialog failed"),
        "{error}"
    );
    assert!(error.message().contains("NOTHING"));
    assert!(writes(&transport).is_empty());
    assert_eq!(transport.seen().len(), 1);
    Ok(())
}

#[test]
fn unsafe_zones_and_calendars_stop_a_change_before_any_request() -> TestResult {
    let transport = creating();
    let host = Dialogs::new("approved", &transport);
    let approval = Approval {
        seconds: 75,
        undo: false,
    };
    for zone in ["", "Europe/Zurich\"", "a b", "../x"] {
        let result = change(
            &api(&transport),
            &host,
            &calendar(),
            CAL,
            zone,
            &create_with(tennis()?),
            &approval,
        );
        assert!(
            refusal(result)?.message().contains("time zone of this Mac"),
            "{zone:?}"
        );
    }
    let too_long = "c".repeat(513);
    for provider in ["", too_long.as_str()] {
        let result = change(
            &api(&transport),
            &host,
            &calendar(),
            provider,
            ZONE,
            &create_with(tennis()?),
            &approval,
        );
        assert!(refusal(result)?.message().contains("no longer exists"));
    }
    assert!(transport.seen().is_empty());
    assert_eq!(host.count(), 0);
    Ok(())
}
