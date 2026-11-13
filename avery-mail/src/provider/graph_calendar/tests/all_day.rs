use super::*;

fn prefer(zone: &str) -> String {
    format!("outlook.timezone=\"{zone}\", IdType=\"ImmutableId\"")
}

fn view(
    zone: &'static str,
    shifted: (&'static str, &'static str),
    utc: (&str, &str),
) -> Res<(Vec<RawEvent>, Scripted)> {
    let second = page(&[utc_item("AD1", true, utc.0, utc.1)]);
    let first = page(&[Ev::all_day("AD1", shifted.0, shifted.1, zone).json()]);
    let transport = Scripted::new()
        .json(Method::Get, "$select=id,start,end,isAllDay", 200, &second)
        .json(Method::Get, "/calendarView?", 200, &first);
    let found = events(&api(&transport), CAL, week()?, zone)?;
    Ok((found, transport))
}

fn holiday(first: &str, after: &str) -> Res<RawEvent> {
    raw("AD1", "Holiday", first, after, true, "", Some(0))
}

#[test]
fn all_day_events_created_in_the_mac_zone_come_out_as_whole_dates() -> TestResult {
    let single = Ev::all_day(
        "single",
        "2026-11-21T00:00:00.0000000",
        "2026-11-22T00:00:00.0000000",
        ZONE,
    );
    let long = Ev::all_day(
        "long",
        "2026-11-23T00:00:00.0000000",
        "2026-11-26T00:00:00.0000000",
        ZONE,
    );
    let items = [single.json(), long.json()];
    let transport = Scripted::new().json(Method::Get, "/calendarView?", 200, &page(&items));
    let found = events(&api(&transport), CAL, week()?, ZONE)?;
    assert_eq!(
        found,
        vec![
            raw(
                "single",
                "Holiday",
                "2026-11-21 00:00",
                "2026-11-22 00:00",
                true,
                "",
                Some(0)
            )?,
            raw(
                "long",
                "Holiday",
                "2026-11-23 00:00",
                "2026-11-26 00:00",
                true,
                "",
                Some(0)
            )?,
        ]
    );
    assert_eq!(transport.seen().len(), 1);
    Ok(())
}

#[test]
fn all_day_events_at_midnight_are_dates_whatever_label_the_provider_gives() -> TestResult {
    for label in [
        "UTC",
        "utc",
        "Etc/UTC",
        "W. Europe Standard Time",
        "Pacific Standard Time",
    ] {
        let ev = Ev::all_day(
            "AD1",
            "2026-11-20T00:00:00.0000000",
            "2026-11-21T00:00:00.0000000",
            label,
        );
        let transport =
            Scripted::new().json(Method::Get, "/calendarView?", 200, &page(&[ev.json()]));
        let found = events(&api(&transport), CAL, week()?, ZONE)?;
        assert_eq!(
            found,
            vec![holiday("2026-11-20 00:00", "2026-11-21 00:00")?],
            "{label}"
        );
        assert_eq!(transport.seen().len(), 1, "{label}");
    }
    Ok(())
}

#[test]
fn shifted_all_day_events_east_of_utc_are_resolved_from_the_utc_view() -> TestResult {
    let (found, transport) = view(
        ZONE,
        ("2026-11-20T01:00:00.0000000", "2026-11-21T01:00:00.0000000"),
        ("2026-11-20T00:00:00.0000000", "2026-11-21T00:00:00.0000000"),
    )?;
    assert_eq!(
        found,
        vec![holiday("2026-11-20 00:00", "2026-11-21 00:00")?]
    );
    assert_eq!(
        calls(&transport),
        vec![(Method::Get, list_url()), (Method::Get, utc_url())]
    );
    let seen = transport.seen();
    assert_eq!(seen.first().and_then(|r| header(r, "Prefer")), Some(PREFER));
    assert_eq!(
        seen.get(1).and_then(|r| header(r, "Prefer")),
        Some(PREFER_UTC)
    );
    assert_eq!(
        seen.get(1).and_then(|r| header(r, "Authorization")),
        Some("Bearer TOKEN")
    );
    Ok(())
}

#[test]
fn shifted_all_day_events_west_of_utc_are_resolved_from_the_utc_view() -> TestResult {
    let (found, transport) = view(
        "America/Los_Angeles",
        ("2026-11-19T16:00:00.0000000", "2026-11-20T16:00:00.0000000"),
        ("2026-11-20T00:00:00.0000000", "2026-11-21T00:00:00.0000000"),
    )?;
    assert_eq!(
        found,
        vec![holiday("2026-11-20 00:00", "2026-11-21 00:00")?]
    );
    let seen = transport.seen();
    assert_eq!(seen.len(), 2);
    assert_eq!(
        seen.first().and_then(|r| header(r, "Prefer")),
        Some(prefer("America/Los_Angeles").as_str())
    );
    assert_eq!(
        seen.get(1).and_then(|r| header(r, "Prefer")),
        Some(PREFER_UTC)
    );
    Ok(())
}

#[test]
fn shifted_all_day_events_far_east_of_utc_are_resolved_from_the_utc_view() -> TestResult {
    let (found, transport) = view(
        "Pacific/Auckland",
        ("2026-11-20T13:00:00.0000000", "2026-11-21T13:00:00.0000000"),
        ("2026-11-20T00:00:00.0000000", "2026-11-21T00:00:00.0000000"),
    )?;
    assert_eq!(
        found,
        vec![holiday("2026-11-20 00:00", "2026-11-21 00:00")?]
    );
    assert_eq!(transport.seen().len(), 2);
    Ok(())
}

#[test]
fn an_all_day_event_made_in_a_third_zone_is_resolved_by_its_utc_time() -> TestResult {
    let (found, _) = view(
        "America/Los_Angeles",
        ("2026-11-19T15:00:00.0000000", "2026-11-20T15:00:00.0000000"),
        ("2026-11-19T23:00:00.0000000", "2026-11-20T23:00:00.0000000"),
    )?;
    assert_eq!(
        found,
        vec![holiday("2026-11-20 00:00", "2026-11-21 00:00")?]
    );
    let (found, _) = view(
        ZONE,
        ("2026-11-20T11:00:00.0000000", "2026-11-21T11:00:00.0000000"),
        ("2026-11-20T10:00:00.0000000", "2026-11-21T10:00:00.0000000"),
    )?;
    assert_eq!(
        found,
        vec![holiday("2026-11-20 00:00", "2026-11-21 00:00")?]
    );
    Ok(())
}

#[test]
fn a_shifted_multi_day_event_keeps_its_length() -> TestResult {
    let (found, _) = view(
        ZONE,
        ("2026-11-23T01:00:00.0000000", "2026-11-26T01:00:00.0000000"),
        ("2026-11-23T00:00:00.0000000", "2026-11-26T00:00:00.0000000"),
    )?;
    assert_eq!(
        found,
        vec![holiday("2026-11-23 00:00", "2026-11-26 00:00")?]
    );
    Ok(())
}

#[test]
fn a_utc_mac_resolves_shifted_all_day_events_without_a_second_request() -> TestResult {
    let ev = Ev::all_day(
        "AD1",
        "2026-11-19T23:00:00.0000000",
        "2026-11-20T23:00:00.0000000",
        "UTC",
    );
    let transport = Scripted::new().json(Method::Get, "/calendarView?", 200, &page(&[ev.json()]));
    let found = events(&api(&transport), CAL, week()?, "UTC")?;
    assert_eq!(
        found,
        vec![holiday("2026-11-20 00:00", "2026-11-21 00:00")?]
    );
    assert_eq!(transport.seen().len(), 1);
    Ok(())
}

#[test]
fn the_utc_view_is_only_requested_when_an_event_needs_it() -> TestResult {
    let timed = Ev {
        id: "timed",
        ..Ev::default()
    };
    let aligned = Ev::all_day(
        "aligned",
        "2026-11-24T00:00:00.0000000",
        "2026-11-25T00:00:00.0000000",
        ZONE,
    );
    let shifted = Ev::all_day(
        "shifted",
        "2026-11-21T01:00:00.0000000",
        "2026-11-22T01:00:00.0000000",
        ZONE,
    );
    let items = [timed.json(), shifted.json(), aligned.json()];
    let second = page(&[
        utc_item(
            "timed",
            false,
            "2026-11-20T08:00:00.0000000",
            "2026-11-20T09:00:00.0000000",
        ),
        utc_item(
            "aligned",
            true,
            "2026-11-24T00:00:00.0000000",
            "2026-11-25T00:00:00.0000000",
        ),
        utc_item(
            "shifted",
            true,
            "2026-11-21T00:00:00.0000000",
            "2026-11-22T00:00:00.0000000",
        ),
        "{\"id\": \"foreign\", \"isAllDay\": false, \"start\": 5}".to_string(),
    ]);
    let transport = Scripted::new()
        .json(Method::Get, "$select=id,start,end,isAllDay", 200, &second)
        .json(Method::Get, "/calendarView?", 200, &page(&items));
    let found = events(&api(&transport), CAL, week()?, ZONE)?;
    let ids: Vec<&str> = found.iter().map(|event| event.uid.as_str()).collect();
    assert_eq!(ids, ["timed", "shifted", "aligned"]);
    let shifted = found.get(1).ok_or("shifted")?;
    assert_eq!(shifted.start.to_string(), "2026-11-21 00:00");
    assert_eq!(shifted.end.to_string(), "2026-11-22 00:00");
    let aligned = found.get(2).ok_or("aligned")?;
    assert_eq!(aligned.start.to_string(), "2026-11-24 00:00");
    assert_eq!(transport.seen().len(), 2);
    Ok(())
}

#[test]
fn a_resolved_all_day_event_outside_the_window_is_dropped() -> TestResult {
    let (found, _) = view(
        ZONE,
        ("2026-11-27T01:00:00.0000000", "2026-11-28T01:00:00.0000000"),
        ("2026-11-27T00:00:00.0000000", "2026-11-28T00:00:00.0000000"),
    )?;
    assert!(found.is_empty());
    let (found, _) = view(
        ZONE,
        ("2026-11-19T01:00:00.0000000", "2026-11-20T01:00:00.0000000"),
        ("2026-11-19T00:00:00.0000000", "2026-11-20T00:00:00.0000000"),
    )?;
    assert!(found.is_empty());
    Ok(())
}

#[test]
fn an_all_day_event_that_vanished_between_the_two_views_is_an_error() -> TestResult {
    let ev = Ev::all_day(
        "AD1",
        "2026-11-20T01:00:00.0000000",
        "2026-11-21T01:00:00.0000000",
        ZONE,
    );
    let second = page(&[utc_item(
        "other",
        true,
        "2026-11-20T00:00:00.0000000",
        "2026-11-21T00:00:00.0000000",
    )]);
    let transport = Scripted::new()
        .json(Method::Get, "$select=id,start,end,isAllDay", 200, &second)
        .json(Method::Get, "/calendarView?", 200, &page(&[ev.json()]));
    let error = refusal(events(&api(&transport), CAL, week()?, ZONE))?;
    assert!(
        error.message().contains("changed while it was read"),
        "{error}"
    );
    assert!(matches!(error, ToolError::Msg(_)));
    Ok(())
}

#[test]
fn the_utc_view_must_really_be_in_utc_and_readable() -> TestResult {
    let ev = Ev::all_day(
        "AD1",
        "2026-11-20T01:00:00.0000000",
        "2026-11-21T01:00:00.0000000",
        ZONE,
    );
    let wrong = concat!(
        "{\"id\": \"AD1\", \"isAllDay\": true, ",
        "\"start\": {\"dateTime\": \"2026-11-20T01:00:00.0000000\", \"timeZone\": \"Europe/Zurich\"}, ",
        "\"end\": {\"dateTime\": \"2026-11-21T01:00:00.0000000\", \"timeZone\": \"Europe/Zurich\"}}"
    )
    .to_string();
    let broken = [
        (page(&[wrong]), "time zone of this Mac"),
        (page(&["\"x\"".to_string()]), "cannot be read safely"),
        (
            page(&["{\"id\": \"AD1\", \"isAllDay\": true}".to_string()]),
            "time zone of this Mac",
        ),
        (
            page(&["{\"isAllDay\": true, \"start\": 1, \"end\": 2}".to_string()]),
            "cannot be read safely",
        ),
        ("{}".to_string(), "unexpected answer"),
    ];
    for (second, wanted) in broken {
        let transport = Scripted::new()
            .json(Method::Get, "$select=id,start,end,isAllDay", 200, &second)
            .json(Method::Get, "/calendarView?", 200, &page(&[ev.json()]));
        let error = refusal(events(&api(&transport), CAL, week()?, ZONE))?;
        assert!(error.message().contains(wanted), "{wanted}: {error}");
    }
    let transport = Scripted::new()
        .json(Method::Get, "$select=id,start,end,isAllDay", 500, "{}")
        .json(Method::Get, "/calendarView?", 200, &page(&[ev.json()]));
    let error = refusal(events(&api(&transport), CAL, week()?, ZONE))?;
    assert!(error.message().contains("temporary problem"), "{error}");
    Ok(())
}

#[test]
fn the_utc_view_is_paged_with_the_same_bounds() -> TestResult {
    let ev = Ev::all_day(
        "AD1",
        "2026-11-20T01:00:00.0000000",
        "2026-11-21T01:00:00.0000000",
        ZONE,
    );
    let link = |number: usize| format!("{}&$skiptoken=U{number}", utc_url());
    let mut transport = Scripted::new();
    for number in (2..=4).rev() {
        let items = [utc_item(
            &format!("x{number}"),
            false,
            "2026-11-20T08:00:00",
            "2026-11-20T09:00:00",
        )];
        let body = page_with_next(&items, &link(number + 1));
        transport = transport.json(Method::Get, &format!("$skiptoken=U{number}"), 200, &body);
    }
    let first = page_with_next(
        &[utc_item(
            "x1",
            false,
            "2026-11-20T08:00:00",
            "2026-11-20T09:00:00",
        )],
        &link(2),
    );
    let transport = transport
        .json(Method::Get, "$select=id,start,end,isAllDay", 200, &first)
        .json(Method::Get, "/calendarView?", 200, &page(&[ev.json()]));
    let error = refusal(events(&api(&transport), CAL, week()?, ZONE))?;
    assert!(error.message().contains("too many events"), "{error}");
    assert_eq!(transport.seen().len(), 5);
    Ok(())
}

#[test]
fn nonsense_all_day_spans_are_repaired_to_a_single_day() -> TestResult {
    let empty = Ev::all_day(
        "empty",
        "2026-11-21T00:00:00.0000000",
        "2026-11-21T00:00:00.0000000",
        ZONE,
    );
    let backwards = Ev::all_day(
        "backwards",
        "2026-11-24T00:00:00.0000000",
        "2026-11-23T00:00:00.0000000",
        ZONE,
    );
    let transport = Scripted::new().json(
        Method::Get,
        "/calendarView?",
        200,
        &page(&[empty.json(), backwards.json()]),
    );
    let found = events(&api(&transport), CAL, week()?, ZONE)?;
    assert_eq!(
        found,
        vec![
            raw(
                "empty",
                "Holiday",
                "2026-11-21 00:00",
                "2026-11-22 00:00",
                true,
                "",
                Some(0)
            )?,
            raw(
                "backwards",
                "Holiday",
                "2026-11-24 00:00",
                "2026-11-25 00:00",
                true,
                "",
                Some(0)
            )?,
        ]
    );
    Ok(())
}
