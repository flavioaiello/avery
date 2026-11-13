mod common;

use std::error::Error;

use common::{run_cli, Sandbox, Server, TestResult};

const MAIL: &str = r#"{"account": "Demo Gmail", "to": ["anna@example.org"], "subject": "Hallo", "body": "Kurzer Text."}"#;
const READ_TOOLS: [&str; 5] = [
    "list_accounts",
    "list_mailboxes",
    "list_messages",
    "search_messages",
    "read_message",
];

fn account(id: &str, name: &str, level: &str) -> String {
    format!(
        r#"{{"id": "{id}", "name": "{name}", "type": "imap", "addresses": ["me@gmail.example"], "level": "{level}"}}"#
    )
}

fn mail_config(level: &str) -> String {
    format!(
        r#"{{"version": 3, "mail": {{"accounts": [{}]}}}}"#,
        account("ACC-1", "Privat Gmail", level)
    )
}

fn start_writer(
    sandbox: &Sandbox,
    accounts: &str,
    approve: &str,
    extra: &[(&str, &str)],
) -> Result<Server, Box<dyn Error>> {
    let outbox = sandbox.path("outbox.jsonl").to_string_lossy().into_owned();
    let mut env = vec![
        ("MAIL_MCP_FAKE", "1"),
        ("MAIL_MCP_FAKE_WRITE", accounts),
        ("MAIL_MCP_FAKE_APPROVE", approve),
        ("MAIL_MCP_FAKE_OUTBOX", outbox.as_str()),
    ];
    env.extend_from_slice(extra);
    Server::start(sandbox, &env)
}

fn lines(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .collect()
}

#[test]
fn the_server_greets_lists_its_tools_and_stops_at_the_end_of_input() -> TestResult {
    let sandbox = Sandbox::new()?;
    let mut server = Server::start(&sandbox, &[("MAIL_MCP_FAKE", "1")])?;
    let hello = server.rpc("initialize", r#"{"protocolVersion": "2024-11-05"}"#)?;
    assert!(hello.contains("\"name\": \"avery-mail\""), "{hello}");
    assert!(hello.contains(&format!("\"version\": \"{}\"", avery_mail::VERSION)));
    let names = server.tool_names()?;
    assert_eq!(names[..5], READ_TOOLS);
    assert_eq!(names[5..], ["list_calendars", "list_events", "get_event"]);
    let (error, text) = server.call("list_accounts", "{}")?;
    assert!(!error && text.contains("Demo Gmail") && !text.contains("Demo Firma"));
    let (error, text) = server.call("list_calendars", "{}")?;
    assert!(!error && text.contains("Demo Privat") && !text.contains("Demo Firma"));
    let (error, text) = server.call("list_events", r#"{"days": 14}"#)?;
    assert!(
        !error && text.contains("Zahnarzt") && !text.contains("Quartalsplanung"),
        "{text}"
    );
    assert_eq!(server.finish()?, Some(0));
    Ok(())
}

#[test]
fn writing_is_off_by_default() -> TestResult {
    let sandbox = Sandbox::new()?;
    let mut server = Server::start(&sandbox, &[("MAIL_MCP_FAKE", "1")])?;
    for name in ["send_message", "create_draft"] {
        let (error, text) = server.call(name, MAIL)?;
        assert!(error && text.contains("not enabled"), "{text}");
    }
    assert!(!sandbox.path("sendlog.jsonl").exists());
    Ok(())
}

#[test]
fn a_declined_send_sends_nothing_and_is_logged_before_and_after() -> TestResult {
    let sandbox = Sandbox::new()?;
    let mut server = start_writer(&sandbox, "FAKE-GMAIL", "no", &[])?;
    assert_eq!(server.tool_names()?.len(), 10);
    let (error, text) = server.call("send_message", MAIL)?;
    assert!(!error && text.contains("did NOT approve"), "{text}");
    assert_eq!(sandbox.read("outbox.jsonl"), "");
    let log = sandbox.read("sendlog.jsonl");
    let entries = lines(&log);
    assert_eq!(entries.len(), 2);
    assert!(entries[0].contains("\"v\": 1") && entries[0].contains("\"outcome\": \"pending\""));
    assert!(entries[1].contains("\"outcome\": \"declined\""));
    Ok(())
}

#[test]
fn an_approved_send_goes_out_and_the_log_has_no_mail_text() -> TestResult {
    let sandbox = Sandbox::new()?;
    let mut server = start_writer(&sandbox, "FAKE-GMAIL", "yes", &[])?;
    let (error, text) = server.call("send_message", MAIL)?;
    assert!(!error && text.starts_with("Sent:"), "{text}");
    let outbox = sandbox.read("outbox.jsonl");
    assert!(outbox.contains("\"status\": \"sent\"") && outbox.contains("anna@example.org"));
    let log = sandbox.read("sendlog.jsonl");
    assert_eq!(lines(&log).len(), 2);
    assert!(log.contains("\"outcome\": \"sent\"") && !log.contains("Kurzer Text"));
    let (error, text) = server.call("create_draft", MAIL)?;
    assert!(!error && text.contains("NOTHING was sent"));
    Ok(())
}

#[test]
fn the_hourly_limit_survives_a_restart() -> TestResult {
    let sandbox = Sandbox::new()?;
    let mut first = start_writer(&sandbox, "FAKE-GMAIL", "yes", &[])?;
    for number in 0..5 {
        let mail = MAIL.replace("Hallo", &format!("Nr {number}"));
        let (error, text) = first.call("send_message", &mail)?;
        assert!(!error && text.starts_with("Sent:"), "{text}");
    }
    assert_eq!(first.finish()?, Some(0));
    let mut second = start_writer(&sandbox, "FAKE-GMAIL", "yes", &[])?;
    let (error, text) = second.call("send_message", MAIL)?;
    assert!(error && text.contains("Send limit reached"), "{text}");
    Ok(())
}

#[test]
fn a_declined_send_blocks_a_retry_even_after_a_restart() -> TestResult {
    let sandbox = Sandbox::new()?;
    let mut first = start_writer(&sandbox, "FAKE-GMAIL", "no", &[])?;
    first.call("send_message", MAIL)?;
    assert_eq!(first.finish()?, Some(0));
    let mut second = start_writer(&sandbox, "FAKE-GMAIL", "yes", &[])?;
    let (error, text) = second.call("send_message", MAIL)?;
    assert!(error && text.contains("just declined"), "{text}");
    assert_eq!(sandbox.read("outbox.jsonl"), "");
    Ok(())
}

#[test]
fn a_crash_is_reported_without_details_and_the_server_goes_on() -> TestResult {
    let sandbox = Sandbox::new()?;
    let mut server = start_writer(
        &sandbox,
        "FAKE-GMAIL",
        "yes",
        &[("MAIL_MCP_FAKE_CRASH", "1")],
    )?;
    let (error, text) = server.call("send_message", MAIL)?;
    assert!(error);
    assert_eq!(text, "Internal error (RuntimeError). Please retry.");
    let (error, _) = server.call("list_accounts", "{}")?;
    assert!(!error);
    let log = sandbox.read("sendlog.jsonl");
    assert_eq!(lines(&log).len(), 1);
    assert!(log.contains("\"outcome\": \"pending\""));
    Ok(())
}

#[test]
fn an_uncertain_outcome_asks_the_user_to_check_mail_and_stays_pending() -> TestResult {
    let sandbox = Sandbox::new()?;
    let mut server = start_writer(
        &sandbox,
        "FAKE-GMAIL",
        "yes",
        &[("MAIL_MCP_FAKE_UNCERTAIN", "1")],
    )?;
    let (error, text) = server.call("send_message", MAIL)?;
    assert!(error && text.contains("may or may not"), "{text}");
    assert_eq!(lines(&sandbox.read("sendlog.jsonl")).len(), 1);
    Ok(())
}

#[test]
fn real_mode_reads_the_configuration_file_and_never_guesses() -> TestResult {
    let calendars = r#"{"version": 3, "calendar": {"calendars": [{"id": "C", "name": "K", "level": "write"}]}}"#;
    for (config, tools) in [
        (mail_config("read"), 5),
        (mail_config("write"), 7),
        (calendars.to_string(), 6),
        (r#"{"version": 3}"#.to_string(), 0),
        (r#"{"version": 2, "allowed_accounts": []}"#.to_string(), 8),
        (
            r#"{"version": 3, "mail": {"accounts": [{"id": "A", "level": "root"}]}}"#.to_string(),
            8,
        ),
        ("not json".to_string(), 8),
    ] {
        let sandbox = Sandbox::new()?;
        sandbox.write("config.json", &config)?;
        let mut server = Server::start(&sandbox, &[])?;
        assert_eq!(server.tool_names()?.len(), tools, "{config}");
    }
    let sandbox = Sandbox::new()?;
    sandbox.write("config.json", r#"{"version": 2, "allowed_accounts": []}"#)?;
    let mut server = Server::start(&sandbox, &[])?;
    assert_eq!(server.tool_names()?[..5], READ_TOOLS);
    let (error, text) = server.call("list_accounts", "{}")?;
    assert!(
        error && text.contains("unsupported format version"),
        "{text}"
    );
    Ok(())
}

#[test]
fn avery_home_decides_where_the_configuration_lives() -> TestResult {
    let sandbox = Sandbox::new()?;
    std::fs::create_dir_all(sandbox.path("home/mail"))?;
    sandbox.write("home/mail/config.json", &mail_config("write"))?;
    let home = sandbox.path("home").to_string_lossy().into_owned();
    let mut server = Server::start(
        &sandbox,
        &[("MAIL_MCP_CONFIG", ""), ("AVERY_HOME", home.as_str())],
    )?;
    assert_eq!(server.tool_names()?.len(), 7);
    Ok(())
}

#[test]
fn garbage_and_oversized_lines_do_not_stop_the_server() -> TestResult {
    let sandbox = Sandbox::new()?;
    let mut server = Server::start(&sandbox, &[("MAIL_MCP_FAKE", "1")])?;
    server.send_raw(b"\n   \nnot json\n")?;
    server.send_raw(&[0xff, 0xfe, b'\n'])?;
    let mut huge = vec![b'x'; 2 * 1024 * 1024];
    huge.push(b'\n');
    server.send_raw(&huge)?;
    server.send_raw(b"{\"jsonrpc\": \"2.0\", \"method\": \"notifications/initialized\"}\n")?;
    let answer = server.rpc("ping", "{}")?;
    assert_eq!(answer, r#"{"jsonrpc": "2.0", "id": 1, "result": {}}"#);
    Ok(())
}

#[test]
fn status_prints_the_configuration_and_exits_with_zero() -> TestResult {
    let sandbox = Sandbox::new()?;
    sandbox.write("config.json", &mail_config("write"))?;
    let output = run_cli(&sandbox, &[], &["status"], "")?;
    assert_eq!(output.code, Some(0));
    assert!(output.stdout.contains("Mail: Privat Gmail (schreiben)"));
    assert!(output.stdout.contains("Kalender: keine"));
    Ok(())
}

#[test]
fn help_version_and_unknown_commands_use_their_own_exit_codes() -> TestResult {
    let sandbox = Sandbox::new()?;
    let help = run_cli(&sandbox, &[], &["help"], "")?;
    assert_eq!((help.code, help.stdout.contains("Usage")), (Some(0), true));
    let version = run_cli(&sandbox, &[], &["--version"], "")?;
    assert_eq!(version.stdout.trim(), avery_mail::VERSION);
    let typo = run_cli(&sandbox, &[], &["selftst"], "")?;
    assert_eq!(typo.code, Some(2));
    assert!(typo.stdout.contains("Meintest du: avery mail selftest"));
    assert!(!typo.stdout.contains("Usage"));
    Ok(())
}

#[test]
fn demo_mode_selftest_and_accounts_work_without_mail() -> TestResult {
    let sandbox = Sandbox::new()?;
    let env = [("MAIL_MCP_FAKE", "1")];
    let selftest = run_cli(&sandbox, &env, &["selftest."], "")?;
    assert_eq!(selftest.code, Some(0), "{}", selftest.stdout);
    assert!(selftest.stdout.contains("PASS") && !selftest.stdout.contains("FAIL"));
    assert!(!selftest.stdout.contains("Paket"));
    let accounts = run_cli(&sandbox, &env, &["accounts,"], "")?;
    assert_eq!(accounts.code, Some(0));
    assert!(accounts.stdout.contains("Demo Gmail") && accounts.stdout.contains("Demo Firma"));
    Ok(())
}

#[test]
fn setup_reads_its_answers_from_standard_input() -> TestResult {
    let sandbox = Sandbox::new()?;
    let env = [("MAIL_MCP_FAKE", "1")];
    let output = run_cli(&sandbox, &env, &["setup"], "2\n1\n\n1\n\n\n\nj\n")?;
    assert_eq!(output.code, Some(0), "{}", output.stdout);
    let saved = sandbox.read("config.json");
    assert!(
        saved.contains("\"level\": \"write\"") && saved.contains("FAKE-CAL-PRIVAT"),
        "{saved}"
    );
    let status = run_cli(&sandbox, &[], &["status"], "")?;
    assert!(status
        .stdout
        .contains("Mail: Demo Gmail (schreiben), Demo Outlook (lesen)"));
    assert!(status.stdout.contains("Kalender: Demo Privat (lesen)"));
    let revoked = run_cli(&sandbox, &env, &["revoke"], "")?;
    assert_eq!(revoked.code, Some(0));
    assert!(!sandbox.read("config.json").contains("level"));
    Ok(())
}

#[test]
fn an_unsupported_configuration_is_never_overwritten() -> TestResult {
    let sandbox = Sandbox::new()?;
    let originals = [
        r#"{"version": 2, "allowed_accounts": [], "write": {"accounts": []}}"#,
        r#"{"version": 4, "areas": {"calendar": "write"}}"#,
        "not json",
    ];
    for original in originals {
        sandbox.write("config.json", original)?;
        for (args, input) in [
            (vec!["setup"], "1\nj\n"),
            (vec!["setup", "mail"], "1\nj\n"),
            (vec!["setup", "calendar"], "1\nj\n"),
            (vec!["revoke"], ""),
        ] {
            let output = run_cli(&sandbox, &[("MAIL_MCP_FAKE", "1")], &args, input)?;
            assert_eq!(output.code, Some(1), "{args:?}: {}", output.stdout);
            assert_eq!(sandbox.read("config.json"), original);
        }
    }
    Ok(())
}

#[test]
fn the_sendlog_command_shows_what_the_server_wrote() -> TestResult {
    let sandbox = Sandbox::new()?;
    let mut server = start_writer(&sandbox, "FAKE-GMAIL", "yes", &[])?;
    server.call("send_message", MAIL)?;
    assert_eq!(server.finish()?, Some(0));
    let output = run_cli(&sandbox, &[("MAIL_MCP_FAKE", "1")], &["sendlog"], "")?;
    assert_eq!(output.code, Some(0));
    assert_eq!(
        output
            .stdout
            .lines()
            .filter(|l| l.contains("anna@example.org"))
            .count(),
        1
    );
    assert!(output.stdout.contains("sent") && !output.stdout.contains("unklar"));
    Ok(())
}

#[test]
fn calendar_events_are_read_created_changed_and_deleted_through_the_server() -> TestResult {
    let sandbox = Sandbox::new()?;
    let mut server = start_writer(&sandbox, "FAKE-CAL-PRIVAT", "yes", &[])?;
    let names = server.tool_names()?;
    assert!(
        names.contains(&"create_event".to_string()) && !names.contains(&"send_message".to_string())
    );
    let (error, text) = server.call("list_calendars", "{}")?;
    assert!(
        !error && text.contains("Demo Privat [write]") && text.contains("Demo Familie [read]"),
        "{text}"
    );
    let (error, text) = server.call("list_events", r#"{"days": 14}"#)?;
    assert!(
        !error && text.contains("Zahnarzt") && text.contains("Familienessen"),
        "{text}"
    );
    assert!(!text.contains("Quartalsplanung"));
    let create = r#"{"summary": "Tennis", "start": "2030-05-06 18:30", "location": "Platz 3", "notes": "Schlaeger"}"#;
    let (error, text) = server.call("create_event", create)?;
    assert!(
        !error && text.starts_with("Created: the user approved"),
        "{text}"
    );
    let (error, text) = server.call(
        "list_events",
        r#"{"start": "2030-05-06", "days": 1, "calendar": "Demo Privat"}"#,
    )?;
    assert!(
        !error && text.contains("id=fake-new-1") && text.contains("Tennis"),
        "{text}"
    );
    let (error, text) = server.call(
        "get_event",
        r#"{"id": "fake-new-1", "calendar": "Demo Privat"}"#,
    )?;
    assert!(
        !error && text.contains("--- NOTES (untrusted) ---\nSchlaeger"),
        "{text}"
    );
    let (error, text) = server.call(
        "update_event",
        r#"{"id": "fake-new-1", "summary": "Tennis mit Ben", "start": "2030-05-06 19:00"}"#,
    )?;
    assert!(
        !error && text.contains("Title: Tennis mit Ben") && text.contains("19:00-20:00"),
        "{text}"
    );
    let (error, text) = server.call("delete_event", r#"{"id": "fake-new-1"}"#)?;
    assert!(!error && text.starts_with("Deleted:"), "{text}");
    let (error, text) = server.call(
        "get_event",
        r#"{"id": "fake-new-1", "calendar": "Demo Privat"}"#,
    )?;
    assert!(error && text.contains("Event not found"), "{text}");
    let (error, text) = server.call("delete_event", r#"{"id": "demo-parents"}"#)?;
    assert!(error && text.contains("has attendees"), "{text}");
    let (error, text) = server.call(
        "update_event",
        r#"{"id": "demo-dinner", "summary": "X", "calendar": "Demo Privat"}"#,
    )?;
    assert!(error && text.contains("Event not found"), "{text}");
    assert_eq!(server.finish()?, Some(0));
    let log = sandbox.read("changes.jsonl");
    let entries = lines(&log);
    assert_eq!(entries.len(), 6, "{log}");
    assert!(entries.iter().all(|line| line.contains("\"v\": 1")));
    assert!(
        entries[0].contains("\"action\": \"create\"")
            && entries[0].contains("\"outcome\": \"pending\"")
    );
    assert!(
        entries[5].contains("\"action\": \"delete\"")
            && entries[5].contains("\"outcome\": \"done\"")
    );
    assert!(!sandbox.path("sendlog.jsonl").exists());
    Ok(())
}

#[test]
fn a_declined_calendar_change_changes_nothing_and_the_log_command_shows_it() -> TestResult {
    let sandbox = Sandbox::new()?;
    let mut server = start_writer(&sandbox, "FAKE-CAL-PRIVAT", "no", &[])?;
    let (error, text) = server.call("delete_event", r#"{"id": "demo-dentist"}"#)?;
    assert!(
        !error && text.contains("did NOT approve") && text.contains("NOTHING was changed"),
        "{text}"
    );
    let (error, text) = server.call(
        "get_event",
        r#"{"id": "demo-dentist", "calendar": "Demo Privat"}"#,
    )?;
    assert!(!error && text.contains("Title: Zahnarzt"), "{text}");
    let (error, text) = server.call("delete_event", r#"{"id": "demo-dentist"}"#)?;
    assert!(error && text.contains("just declined or ignored"), "{text}");
    assert_eq!(server.finish()?, Some(0));
    let output = run_cli(&sandbox, &[("MAIL_MCP_FAKE", "1")], &["changes"], "")?;
    assert_eq!(output.code, Some(0));
    let rows: Vec<&str> = output
        .stdout
        .lines()
        .filter(|l| l.contains("Zahnarzt"))
        .collect();
    assert_eq!(rows.len(), 1, "{}", output.stdout);
    assert!(rows[0].contains("declined") && !rows[0].contains("rueckgaengig moeglich"));
    let undo = run_cli(&sandbox, &[("MAIL_MCP_FAKE", "1")], &["undo"], "")?;
    assert_eq!(undo.code, Some(0));
    assert!(undo.stdout.contains("Nichts zum Rueckgaengigmachen."));
    Ok(())
}

#[test]
fn calendar_changes_are_refused_without_the_write_level_and_leave_no_log() -> TestResult {
    let sandbox = Sandbox::new()?;
    let mut server = Server::start(&sandbox, &[("MAIL_MCP_FAKE", "1")])?;
    for (name, arguments) in [
        (
            "create_event",
            r#"{"summary": "A", "start": "2030-05-06 09:00"}"#,
        ),
        ("update_event", r#"{"id": "demo-dentist", "summary": "A"}"#),
        ("delete_event", r#"{"id": "demo-dentist"}"#),
    ] {
        let (error, text) = server.call(name, arguments)?;
        assert!(
            error && text.contains("Changing calendars is not enabled"),
            "{text}"
        );
    }
    assert!(!sandbox.path("changes.jsonl").exists());
    Ok(())
}

#[test]
fn an_undo_in_a_fresh_process_reports_an_event_that_is_gone() -> TestResult {
    let sandbox = Sandbox::new()?;
    let mut server = start_writer(&sandbox, "FAKE-CAL-PRIVAT", "yes", &[])?;
    let (error, _) = server.call(
        "create_event",
        r#"{"summary": "Tennis", "start": "2030-05-06 18:30"}"#,
    )?;
    assert!(!error);
    assert_eq!(server.finish()?, Some(0));
    let env = [("MAIL_MCP_FAKE", "1"), ("MAIL_MCP_FAKE_APPROVE", "yes")];
    let changes = run_cli(&sandbox, &env, &["changes"], "")?;
    assert!(
        changes.stdout.contains("[rueckgaengig moeglich]"),
        "{}",
        changes.stdout
    );
    let read_only = run_cli(&sandbox, &env, &["undo"], "1\nj\n")?;
    assert_eq!(read_only.code, Some(1), "{}", read_only.stdout);
    assert!(
        read_only
            .stdout
            .contains("nicht mehr zum Schreiben freigegeben"),
        "{}",
        read_only.stdout
    );
    let writable = [
        ("MAIL_MCP_FAKE", "1"),
        ("MAIL_MCP_FAKE_APPROVE", "yes"),
        ("MAIL_MCP_FAKE_WRITE", "FAKE-CAL-PRIVAT"),
    ];
    let undo = run_cli(&sandbox, &writable, &["undo"], "1\nj\n")?;
    assert_eq!(undo.code, Some(1), "{}", undo.stdout);
    assert!(undo.stdout.contains("Event not found"), "{}", undo.stdout);
    Ok(())
}
