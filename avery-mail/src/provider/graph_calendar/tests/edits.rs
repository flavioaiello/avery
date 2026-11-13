use super::*;

fn event_url() -> String {
    format!("{GRAPH}/events/{EV_ENC}")
}

fn update_of(before: EventFields, after: EventFields) -> Change {
    Change::Update {
        uid: EV.to_string(),
        before,
        after,
    }
}

fn delete_of(before: EventFields) -> Change {
    Change::Delete {
        uid: EV.to_string(),
        before,
    }
}

fn updating(first: &Ev, read_back: &Ev) -> Scripted {
    editable(Scripted::new(), true)
        .json(Method::Patch, "/events/", 200, &read_back.json())
        .reply(
            Method::Get,
            "/events/",
            vec![ok(&first.json()), ok(&first.json()), ok(&read_back.json())],
        )
}

fn deleting(
    presence: Result<Response, HttpError>,
    removal: Result<Response, HttpError>,
) -> Scripted {
    editable(Scripted::new(), true)
        .reply(Method::Get, "?$select=id,isCancelled", vec![presence])
        .reply(Method::Delete, "/events/", vec![removal])
        .json(Method::Get, "/events/", 200, &Ev::default().json())
}

fn gone() -> Scripted {
    deleting(answer(404, "{}"), answer(204, ""))
}

fn all_day_ev() -> Ev {
    Ev::all_day(
        EV,
        "2026-11-24T00:00:00.0000000",
        "2026-11-25T00:00:00.0000000",
        WINDOWS,
    )
}

fn holiday() -> Res<EventFields> {
    fields(
        "Holiday",
        "2026-11-24 00:00",
        "2026-11-25 00:00",
        true,
        "",
        "",
    )
}

#[test]
fn an_update_patches_only_the_changed_fields() -> TestResult {
    let before = dentist()?;
    let zone = "\"timeZone\": \"W. Europe Standard Time\"";
    let at = |first: &str, second: &str| {
        format!(
            "\"isAllDay\": false, \"start\": {{\"dateTime\": \"{first}\", {zone}}}, \"end\": {{\"dateTime\": \"{second}\", {zone}}}"
        )
    };
    let cases: Vec<(&str, EventFields, String)> = vec![
        (
            "title",
            EventFields {
                summary: "Dentist (moved)".to_string(),
                ..before.clone()
            },
            "{\"subject\": \"Dentist (moved)\"}".to_string(),
        ),
        (
            "place",
            EventFields {
                location: "Praxis Nord".to_string(),
                ..before.clone()
            },
            "{\"location\": {\"displayName\": \"Praxis Nord\"}}".to_string(),
        ),
        (
            "place cleared",
            EventFields {
                location: String::new(),
                ..before.clone()
            },
            "{\"location\": {\"displayName\": \"\"}}".to_string(),
        ),
        (
            "notes",
            EventFields {
                notes: "Other\nnotes".to_string(),
                ..before.clone()
            },
            "{\"body\": {\"contentType\": \"text\", \"content\": \"Other\\nnotes\"}}".to_string(),
        ),
        (
            "notes cleared",
            EventFields {
                notes: String::new(),
                ..before.clone()
            },
            "{\"body\": {\"contentType\": \"text\", \"content\": \"\"}}".to_string(),
        ),
        (
            "later",
            EventFields {
                start: dt("2026-11-20 10:00")?,
                end: dt("2026-11-20 11:00")?,
                ..before.clone()
            },
            format!("{{{}}}", at("2026-11-20T10:00:00", "2026-11-20T11:00:00")),
        ),
        (
            "longer",
            EventFields {
                end: dt("2026-11-20 11:00")?,
                ..before.clone()
            },
            format!("{{{}}}", at("2026-11-20T09:00:00", "2026-11-20T11:00:00")),
        ),
        (
            "earlier start",
            EventFields {
                start: dt("2026-11-20 08:30")?,
                ..before.clone()
            },
            format!("{{{}}}", at("2026-11-20T08:30:00", "2026-11-20T10:00:00")),
        ),
        (
            "everything",
            fields(
                "Other",
                "2026-11-21 14:00",
                "2026-11-21 15:15",
                false,
                "Elsewhere",
                "Text",
            )?,
            format!(
                concat!(
                    "{{\"subject\": \"Other\", \"location\": {{\"displayName\": \"Elsewhere\"}}, ",
                    "\"body\": {{\"contentType\": \"text\", \"content\": \"Text\"}}, {}}}"
                ),
                at("2026-11-21T14:00:00", "2026-11-21T15:15:00")
            ),
        ),
    ];
    for (name, after, expected) in cases {
        let transport = updating(&Ev::default(), &Ev::default());
        let (result, _) = run(
            &transport,
            "approved",
            &update_of(before.clone(), after),
            false,
        );
        assert!(result.is_ok(), "{name}");
        let sent = writes(&transport);
        let [patch] = sent.as_slice() else {
            return Err(format!("{name}: exactly one write").into());
        };
        assert_eq!(patch.method, Method::Patch, "{name}");
        assert_eq!(patch.url, event_url(), "{name}");
        assert_eq!(
            json::parse(patch.body.as_deref().ok_or("body")?)?,
            j(&expected)?,
            "{name}"
        );
    }
    Ok(())
}

#[test]
fn an_all_day_move_patches_both_dates_in_the_zone() -> TestResult {
    let after = fields(
        "Holiday",
        "2026-11-25 00:00",
        "2026-11-27 00:00",
        true,
        "",
        "",
    )?;
    let moved = Ev::all_day(
        EV,
        "2026-11-25T00:00:00.0000000",
        "2026-11-27T00:00:00.0000000",
        WINDOWS,
    );
    let transport = updating(&all_day_ev(), &moved);
    let (result, _) = run(
        &transport,
        "approved",
        &update_of(holiday()?, after.clone()),
        false,
    );
    assert_eq!(result?.after, Some(after));
    let sent = writes(&transport);
    let patch = sent.first().ok_or("patch")?;
    let expected = concat!(
        "{\"isAllDay\": true, ",
        "\"start\": {\"dateTime\": \"2026-11-25T00:00:00\", \"timeZone\": \"W. Europe Standard Time\"}, ",
        "\"end\": {\"dateTime\": \"2026-11-27T00:00:00\", \"timeZone\": \"W. Europe Standard Time\"}}"
    );
    assert_eq!(
        json::parse(patch.body.as_deref().ok_or("body")?)?,
        j(expected)?
    );
    Ok(())
}

#[test]
fn an_update_reads_twice_around_the_dialog_and_writes_once() -> TestResult {
    let before = dentist()?;
    let after = EventFields {
        summary: "Dentist (moved)".to_string(),
        ..before.clone()
    };
    let stored = Ev {
        subject: "Dentist (moved)".to_string(),
        ..Ev::default()
    };
    let transport = updating(&Ev::default(), &stored);
    let (result, host) = run(
        &transport,
        "approved",
        &update_of(before.clone(), after.clone()),
        false,
    );
    assert_eq!(
        result?,
        ChangeResult {
            decision: Decision::Applied,
            uid: Some(EV.to_string()),
            after: Some(after.clone()),
        }
    );
    assert_eq!(
        calls(&transport),
        vec![
            (Method::Get, editable_url()),
            (Method::Get, detail_url(EV_ENC)),
            (Method::Get, editable_url()),
            (Method::Get, detail_url(EV_ENC)),
            (Method::Patch, event_url()),
            (Method::Get, detail_url(EV_ENC)),
        ]
    );
    assert_eq!(host.count(), 1);
    assert_eq!(host.requests_before_the_dialog(), vec![2]);
    let seen = transport.seen();
    let patch = seen.get(4).ok_or("patch")?;
    assert_eq!(header(patch, "Prefer"), Some(PREFER_IDS));
    assert_eq!(header(patch, "Content-Type"), Some("application/json"));
    assert_eq!(header(patch, "Authorization"), Some("Bearer TOKEN"));
    assert_eq!(
        json::parse(patch.body.as_deref().ok_or("body")?)?,
        j("{\"subject\": \"Dentist (moved)\"}")?
    );
    for index in [1, 3, 5] {
        assert_eq!(
            seen.get(index).and_then(|r| header(r, "Prefer")),
            Some(PREFER_TEXT)
        );
    }
    let payload = host.payload()?;
    let object = payload.as_object().ok_or("object")?;
    assert_eq!(object.get("action").and_then(Json::as_str), Some("update"));
    assert_eq!(object.get("before"), Some(&before.to_json()));
    assert_eq!(object.get("after"), Some(&after.to_json()));
    Ok(())
}

#[test]
fn a_declined_or_ignored_update_sends_nothing() -> TestResult {
    let before = dentist()?;
    let after = EventFields {
        summary: "Other".to_string(),
        ..before.clone()
    };
    for (answer, decision) in [
        ("declined", Decision::Declined),
        ("timeout", Decision::TimedOut),
    ] {
        let transport = updating(&Ev::default(), &Ev::default());
        let (result, host) = run(
            &transport,
            answer,
            &update_of(before.clone(), after.clone()),
            false,
        );
        assert_eq!(
            result?,
            ChangeResult {
                decision,
                uid: None,
                after: None,
            }
        );
        assert_eq!(
            calls(&transport),
            vec![
                (Method::Get, editable_url()),
                (Method::Get, detail_url(EV_ENC)),
            ]
        );
        assert!(writes(&transport).is_empty());
        assert_eq!(host.count(), 1);
    }
    Ok(())
}

#[test]
fn a_stale_before_is_refused_before_the_dialog() -> TestResult {
    let before = dentist()?;
    let after = EventFields {
        summary: "Other".to_string(),
        ..before.clone()
    };
    let cases = [
        (
            "title",
            Ev {
                subject: "Dentist!".to_string(),
                ..Ev::default()
            },
        ),
        (
            "start",
            Ev {
                start: ("2026-11-20T09:30:00.0000000", WINDOWS),
                ..Ev::default()
            },
        ),
        (
            "end",
            Ev {
                end: ("2026-11-20T10:30:00.0000000", WINDOWS),
                ..Ev::default()
            },
        ),
        (
            "place",
            Ev {
                location: "Elsewhere".to_string(),
                ..Ev::default()
            },
        ),
        (
            "notes",
            Ev {
                body: Some(("text", "Other".to_string())),
                ..Ev::default()
            },
        ),
        (
            "no notes",
            Ev {
                body: None,
                ..Ev::default()
            },
        ),
        (
            "all-day",
            Ev::all_day(
                EV,
                "2026-11-20T00:00:00.0000000",
                "2026-11-21T00:00:00.0000000",
                WINDOWS,
            ),
        ),
        (
            "untitled",
            Ev {
                subject: String::new(),
                ..Ev::default()
            },
        ),
    ];
    for (name, current) in cases {
        let transport =
            editable(Scripted::new(), true).json(Method::Get, "/events/", 200, &current.json());
        let (result, host) = run(
            &transport,
            "approved",
            &update_of(before.clone(), after.clone()),
            false,
        );
        let error = refusal(result)?;
        assert!(
            error.message().contains("changed since you read it"),
            "{name}: {error}"
        );
        assert!(error.message().contains("NOTHING was changed"), "{name}");
        assert!(matches!(error, ToolError::Msg(_)), "{name}");
        assert_eq!(host.count(), 0, "{name}");
        assert!(writes(&transport).is_empty(), "{name}");
        assert_eq!(transport.seen().len(), 2, "{name}");
    }
    Ok(())
}

#[test]
fn a_change_while_the_dialog_is_open_is_refused_after_it() -> TestResult {
    let before = dentist()?;
    let after = EventFields {
        summary: "Other".to_string(),
        ..before.clone()
    };
    let changed = Ev {
        location: "Moved while you looked".to_string(),
        ..Ev::default()
    };
    let transport = editable(Scripted::new(), true)
        .json(Method::Patch, "/events/", 200, "{}")
        .reply(
            Method::Get,
            "/events/",
            vec![ok(&Ev::default().json()), ok(&changed.json())],
        );
    let (result, host) = run(&transport, "approved", &update_of(before, after), false);
    let error = refusal(result)?;
    assert!(error
        .message()
        .contains("changed since you read it or while the approval dialog was open"));
    assert_eq!(host.count(), 1);
    assert!(writes(&transport).is_empty());
    assert_eq!(transport.seen().len(), 4);
    Ok(())
}

#[test]
fn what_makes_an_event_unsafe_is_refused_before_the_dialog() -> TestResult {
    let before = dentist()?;
    let after = EventFields {
        summary: "Other".to_string(),
        ..before.clone()
    };
    let base = Ev::default();
    let unmarked = Ev::default().json().replace("\"isOrganizer\": true, ", "");
    let cases: Vec<(&str, String, &str)> = vec![
        (
            "master",
            Ev {
                kind: "seriesMaster",
                recurrence: true,
                ..base.clone()
            }
            .json(),
            "recurring event",
        ),
        (
            "occurrence",
            Ev {
                kind: "occurrence",
                series: Some("m"),
                ..base.clone()
            }
            .json(),
            "recurring event",
        ),
        (
            "exception",
            Ev {
                kind: "exception",
                series: Some("m"),
                ..base.clone()
            }
            .json(),
            "recurring event",
        ),
        (
            "rule",
            Ev {
                recurrence: true,
                ..base.clone()
            }
            .json(),
            "recurring event",
        ),
        (
            "one guest",
            Ev {
                attendees: Some(1),
                ..base.clone()
            }
            .json(),
            "has attendees",
        ),
        (
            "many guests",
            Ev {
                attendees: Some(60),
                ..base.clone()
            }
            .json(),
            "has attendees",
        ),
        (
            "unknown guests",
            Ev {
                attendees: None,
                ..base.clone()
            }
            .json(),
            "has attendees",
        ),
        (
            "organized elsewhere",
            Ev {
                organizer: false,
                ..base.clone()
            }
            .json(),
            "someone else's invitation",
        ),
        ("organizer unknown", unmarked, "someone else's invitation"),
        (
            "cancelled",
            Ev {
                cancelled: true,
                ..base.clone()
            }
            .json(),
            "is cancelled",
        ),
        (
            "other calendar",
            Ev {
                calendar: Some(OTHER_CAL),
                ..base.clone()
            }
            .json(),
            "Event not found",
        ),
        (
            "no calendar",
            Ev {
                calendar: None,
                ..base.clone()
            }
            .json(),
            "did not say which calendar",
        ),
        (
            "recurring and stale",
            Ev {
                kind: "occurrence",
                series: Some("m"),
                subject: "Changed".to_string(),
                ..base.clone()
            }
            .json(),
            "recurring event",
        ),
        (
            "guests and stale",
            Ev {
                attendees: Some(2),
                subject: "Changed".to_string(),
                ..base.clone()
            }
            .json(),
            "has attendees",
        ),
    ];
    for (name, body, wanted) in cases {
        for edit in [
            update_of(before.clone(), after.clone()),
            delete_of(before.clone()),
        ] {
            let transport =
                editable(Scripted::new(), true).json(Method::Get, "/events/", 200, &body);
            let (result, host) = run(&transport, "approved", &edit, false);
            let error = refusal(result)?;
            assert!(error.message().contains(wanted), "{name}: {error}");
            assert!(matches!(error, ToolError::Msg(_)), "{name}");
            assert_eq!(host.count(), 0, "{name}");
            assert!(writes(&transport).is_empty(), "{name}");
        }
    }
    Ok(())
}

#[test]
fn a_missing_event_cannot_be_changed() -> TestResult {
    let before = dentist()?;
    let after = EventFields {
        summary: "Other".to_string(),
        ..before.clone()
    };
    for edit in [update_of(before.clone(), after), delete_of(before)] {
        let transport = editable(Scripted::new(), true).json(Method::Get, "/events/", 404, "{}");
        let (result, host) = run(&transport, "approved", &edit, false);
        let error = refusal(result)?;
        assert!(error.message().starts_with("Event not found."), "{error}");
        assert_eq!(host.count(), 0);
        assert!(writes(&transport).is_empty());
    }
    Ok(())
}

#[test]
fn an_event_that_disappears_during_the_dialog_is_refused() -> TestResult {
    let before = dentist()?;
    let transport = editable(Scripted::new(), true)
        .json(Method::Delete, "/events/", 204, "")
        .reply(
            Method::Get,
            "/events/",
            vec![ok(&Ev::default().json()), answer(404, "{}")],
        );
    let (result, host) = run(&transport, "approved", &delete_of(before), false);
    assert!(refusal(result)?.message().starts_with("Event not found."));
    assert_eq!(host.count(), 1);
    assert!(writes(&transport).is_empty());
    Ok(())
}

#[test]
fn updates_never_switch_between_timed_and_all_day() -> TestResult {
    let before = dentist()?;
    let after = fields(
        "Dentist",
        "2026-11-20 00:00",
        "2026-11-21 00:00",
        true,
        "Praxis",
        "Bring the card",
    )?;
    let transport = updating(&Ev::default(), &Ev::default());
    let (result, host) = run(&transport, "approved", &update_of(before, after), false);
    assert_eq!(
        refusal(result)?.message(),
        "The all-day setting of an event cannot be changed here, so NOTHING was changed."
    );
    assert!(transport.seen().is_empty());
    assert_eq!(host.count(), 0);
    Ok(())
}

#[test]
fn an_update_without_a_difference_is_refused() -> TestResult {
    let before = dentist()?;
    let transport = updating(&Ev::default(), &Ev::default());
    let (result, host) = run(
        &transport,
        "approved",
        &update_of(before.clone(), before),
        false,
    );
    assert!(refusal(result)?.message().starts_with("Nothing to change"));
    assert!(transport.seen().is_empty());
    assert_eq!(host.count(), 0);
    Ok(())
}

#[test]
fn updates_are_limited_like_creates() -> TestResult {
    let before = dentist()?;
    let noted = |count: usize| EventFields {
        notes: "\u{e9}".repeat(count),
        ..before.clone()
    };
    for (undo, count) in [(false, 1_200), (true, 4_000)] {
        let transport = updating(&Ev::default(), &Ev::default());
        let (result, _) = run(
            &transport,
            "approved",
            &update_of(before.clone(), noted(count)),
            undo,
        );
        assert!(result.is_ok(), "{undo} {count}");
    }
    for (undo, count) in [(false, 1_201), (true, 4_001)] {
        let transport = updating(&Ev::default(), &Ev::default());
        let (result, host) = run(
            &transport,
            "approved",
            &update_of(before.clone(), noted(count)),
            undo,
        );
        assert_eq!(
            refusal(result)?.message(),
            "The notes are too long, so NOTHING was changed."
        );
        assert!(transport.seen().is_empty());
        assert_eq!(host.count(), 0);
    }
    let linked = EventFields {
        url: "https://example.invalid/x".to_string(),
        ..before.clone()
    };
    let backwards = EventFields {
        end: dt("2026-11-20 08:00")?,
        ..before.clone()
    };
    for (after, wanted) in [
        (linked, "Links are not supported"),
        (backwards, "not valid for this calendar"),
    ] {
        let transport = updating(&Ev::default(), &Ev::default());
        let (result, _) = run(
            &transport,
            "approved",
            &update_of(before.clone(), after),
            false,
        );
        assert!(refusal(result)?.message().contains(wanted));
        assert!(transport.seen().is_empty());
    }
    Ok(())
}

#[test]
fn a_calendar_that_turns_read_only_is_refused_for_updates_and_deletes() -> TestResult {
    let before = dentist()?;
    let after = EventFields {
        summary: "Other".to_string(),
        ..before.clone()
    };
    for edit in [update_of(before.clone(), after), delete_of(before)] {
        let transport = Scripted::new()
            .reply(
                Method::Get,
                "?$select=canEdit",
                vec![ok("{\"canEdit\": true}"), ok("{\"canEdit\": false}")],
            )
            .json(Method::Get, "/events/", 200, &Ev::default().json());
        let (result, host) = run(&transport, "approved", &edit, false);
        assert!(refusal(result)?.message().contains("read-only"));
        assert_eq!(host.count(), 1);
        assert!(writes(&transport).is_empty());
        let read_only = Scripted::new()
            .json(Method::Get, "?$select=canEdit", 200, "{\"canEdit\": false}")
            .json(Method::Get, "/events/", 200, &Ev::default().json());
        let (result, host) = run(&read_only, "approved", &edit, false);
        assert!(refusal(result)?.message().contains("read-only"));
        assert_eq!(host.count(), 0);
        assert_eq!(read_only.seen().len(), 1);
    }
    Ok(())
}

#[test]
fn a_failed_patch_is_uncertain_or_refused_as_the_provider_says() -> TestResult {
    let before = dentist()?;
    let after = EventFields {
        summary: "Other".to_string(),
        ..before.clone()
    };
    let statuses: [(u16, bool); 5] = [
        (400, false),
        (403, false),
        (412, false),
        (500, true),
        (502, true),
    ];
    for (status, uncertain) in statuses {
        let transport = editable(Scripted::new(), true)
            .json(Method::Patch, "/events/", status, "{}")
            .json(Method::Get, "/events/", 200, &Ev::default().json());
        let (result, _) = run(
            &transport,
            "approved",
            &update_of(before.clone(), after.clone()),
            false,
        );
        let error = refusal(result)?;
        assert_eq!(
            matches!(error, ToolError::Uncertain(_)),
            uncertain,
            "{status}: {error:?}"
        );
        assert_eq!(writes(&transport).len(), 1, "{status}");
        assert_eq!(transport.seen().len(), 5, "{status}");
    }
    for failure in [
        HttpError::TimedOut,
        HttpError::Failed("curl exit 52".to_string()),
    ] {
        let transport = editable(Scripted::new(), true)
            .reply(Method::Patch, "/events/", vec![Err(failure)])
            .json(Method::Get, "/events/", 200, &Ev::default().json());
        let (result, _) = run(
            &transport,
            "approved",
            &update_of(before.clone(), after.clone()),
            false,
        );
        assert!(matches!(refusal(result)?, ToolError::Uncertain(_)));
        assert_eq!(writes(&transport).len(), 1);
    }
    Ok(())
}

#[test]
fn a_failing_read_back_does_not_undo_an_update() -> TestResult {
    let before = dentist()?;
    let after = EventFields {
        summary: "Other".to_string(),
        ..before.clone()
    };
    let transport = editable(Scripted::new(), true)
        .json(Method::Patch, "/events/", 200, "{}")
        .reply(
            Method::Get,
            "/events/",
            vec![
                ok(&Ev::default().json()),
                ok(&Ev::default().json()),
                answer(500, "{}"),
            ],
        );
    let (result, _) = run(&transport, "approved", &update_of(before, after), false);
    assert_eq!(
        result?,
        ChangeResult {
            decision: Decision::Applied,
            uid: Some(EV.to_string()),
            after: None,
        }
    );
    Ok(())
}

#[test]
fn unusable_ids_never_reach_a_request_and_odd_ones_are_encoded() -> TestResult {
    let before = dentist()?;
    let after = EventFields {
        summary: "Other".to_string(),
        ..before.clone()
    };
    let long = "e".repeat(513);
    for uid in ["", long.as_str()] {
        for edit in [
            Change::Update {
                uid: uid.to_string(),
                before: before.clone(),
                after: after.clone(),
            },
            Change::Delete {
                uid: uid.to_string(),
                before: before.clone(),
            },
        ] {
            let transport = updating(&Ev::default(), &Ev::default());
            let (result, host) = run(&transport, "approved", &edit, false);
            assert!(refusal(result)?.message().starts_with("Event not found."));
            assert!(transport.seen().is_empty());
            assert_eq!(host.count(), 0);
        }
    }
    let transport = editable(Scripted::new(), true).json(Method::Get, "/events/", 404, "{}");
    let edit = Change::Update {
        uid: "a/b?c#d e\"f%g".to_string(),
        before,
        after,
    };
    let (result, _) = run(&transport, "approved", &edit, false);
    assert!(refusal(result)?.message().starts_with("Event not found."));
    let seen = transport.seen();
    let request = seen.get(1).ok_or("event request")?;
    assert!(request.url.starts_with(&format!(
        "{GRAPH}/events/a%2Fb%3Fc%23d%20e%22f%25g?$select="
    )));
    Ok(())
}

#[test]
fn a_delete_removes_the_event_and_checks_that_it_is_gone() -> TestResult {
    let transport = gone();
    let (result, host) = run(&transport, "approved", &delete_of(dentist()?), false);
    assert_eq!(
        result?,
        ChangeResult {
            decision: Decision::Applied,
            uid: Some(EV.to_string()),
            after: None,
        }
    );
    assert_eq!(
        calls(&transport),
        vec![
            (Method::Get, editable_url()),
            (Method::Get, detail_url(EV_ENC)),
            (Method::Get, editable_url()),
            (Method::Get, detail_url(EV_ENC)),
            (Method::Delete, event_url()),
            (Method::Get, presence_url(EV_ENC)),
        ]
    );
    assert_eq!(host.count(), 1);
    assert_eq!(host.requests_before_the_dialog(), vec![2]);
    let seen = transport.seen();
    let delete = seen.get(4).ok_or("delete")?;
    assert_eq!(header(delete, "Prefer"), Some(PREFER_IDS));
    assert_eq!(header(delete, "Authorization"), Some("Bearer TOKEN"));
    assert_eq!(header(delete, "Content-Type"), None);
    assert_eq!(delete.body, None);
    assert_eq!(
        seen.get(5).and_then(|r| header(r, "Prefer")),
        Some(PREFER_IDS)
    );
    assert_eq!(writes(&transport).len(), 1);
    let payload = host.payload()?;
    let object = payload.as_object().ok_or("object")?;
    assert_eq!(object.get("action").and_then(Json::as_str), Some("delete"));
    assert_eq!(object.get("before"), Some(&dentist()?.to_json()));
    assert_eq!(object.get("after"), Some(&Json::Null));
    Ok(())
}

#[test]
fn a_declined_or_ignored_delete_sends_nothing() -> TestResult {
    for (answer, decision) in [
        ("declined", Decision::Declined),
        ("timeout", Decision::TimedOut),
    ] {
        let transport = gone();
        let (result, host) = run(&transport, answer, &delete_of(dentist()?), false);
        assert_eq!(
            result?,
            ChangeResult {
                decision,
                uid: None,
                after: None,
            }
        );
        assert!(writes(&transport).is_empty());
        assert_eq!(transport.seen().len(), 2);
        assert_eq!(host.count(), 1);
    }
    Ok(())
}

#[test]
fn a_delete_that_leaves_the_event_in_the_calendar_is_uncertain() -> TestResult {
    let still_there = format!(
        "{{\"id\": {}, \"isCancelled\": false, \"calendar\": {{\"id\": {}}}}}",
        q(EV),
        q(CAL)
    );
    let transport = deleting(answer(200, &still_there), answer(204, ""));
    let (result, _) = run(&transport, "approved", &delete_of(dentist()?), false);
    let error = refusal(result)?;
    assert!(matches!(error, ToolError::Uncertain(_)), "{error:?}");
    assert!(error.message().contains("still in the calendar"), "{error}");
    assert_eq!(writes(&transport).len(), 1);
    Ok(())
}

#[test]
fn a_delete_counts_when_the_event_is_cancelled_or_has_left_the_calendar() -> TestResult {
    let bodies = [
        format!(
            "{{\"id\": {}, \"isCancelled\": true, \"calendar\": {{\"id\": {}}}}}",
            q(EV),
            q(CAL)
        ),
        format!(
            "{{\"id\": {}, \"isCancelled\": false, \"calendar\": {{\"id\": {}}}}}",
            q(EV),
            q(OTHER_CAL)
        ),
        format!(
            "{{\"id\": {}, \"isCancelled\": false, \"calendar\": null}}",
            q(EV)
        ),
        format!("{{\"id\": {}, \"isCancelled\": false}}", q(EV)),
    ];
    for body in bodies {
        let transport = deleting(answer(200, &body), answer(204, ""));
        let (result, _) = run(&transport, "approved", &delete_of(dentist()?), false);
        assert_eq!(result?.decision, Decision::Applied, "{body}");
    }
    Ok(())
}

#[test]
fn a_delete_whose_check_cannot_be_read_is_uncertain() -> TestResult {
    let broken = [
        answer(200, "[]"),
        answer(200, "junk"),
        answer(500, "{}"),
        answer(401, "{}"),
        answer(403, "{}"),
        Err(HttpError::TimedOut),
        Err(HttpError::Failed("curl exit 6".to_string())),
    ];
    for presence in broken {
        let transport = deleting(presence.clone(), answer(204, ""));
        let (result, _) = run(&transport, "approved", &delete_of(dentist()?), false);
        let error = refusal(result)?;
        assert!(
            matches!(error, ToolError::Uncertain(_)),
            "{presence:?}: {error:?}"
        );
        assert!(error.message().contains("did not confirm"), "{error}");
        assert_eq!(writes(&transport).len(), 1);
    }
    Ok(())
}

#[test]
fn a_failed_delete_request_is_uncertain_or_refused_as_the_provider_says() -> TestResult {
    let statuses: [(u16, bool); 5] = [
        (403, false),
        (404, false),
        (409, false),
        (500, true),
        (503, true),
    ];
    for (status, uncertain) in statuses {
        let transport = deleting(answer(404, "{}"), answer(status, "{}"));
        let (result, _) = run(&transport, "approved", &delete_of(dentist()?), false);
        let error = refusal(result)?;
        assert_eq!(
            matches!(error, ToolError::Uncertain(_)),
            uncertain,
            "{status}: {error:?}"
        );
        assert_eq!(writes(&transport).len(), 1, "{status}");
        assert_eq!(transport.seen().len(), 5, "{status}");
    }
    for failure in [
        HttpError::TimedOut,
        HttpError::TooLarge,
        HttpError::Failed("x".to_string()),
    ] {
        let transport = deleting(answer(404, "{}"), Err(failure));
        let (result, _) = run(&transport, "approved", &delete_of(dentist()?), false);
        assert!(matches!(refusal(result)?, ToolError::Uncertain(_)));
        assert_eq!(transport.seen().len(), 5);
    }
    Ok(())
}
