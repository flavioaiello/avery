use std::error::Error;
use std::fs;
use std::io::Cursor;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::*;
use crate::account::{Account, MailAccount};
use crate::backend::{
    Backend, ComposeRequest, Mailbox, MessageBody, MessageId, MessageList, MessageQuery, Preview,
};
use crate::calendar::{
    Approval, Calendar, CalendarBackend, CalendarInfo, Change, ChangeResult, EventDetail, RawEvent,
    Window,
};
use crate::cli::dispatch;
use crate::console::PRIVATE_ONLY;
use crate::error::{ToolError, ToolResult};
use crate::fake::FakeOptions;
use crate::json;
use crate::rights::Level;
use crate::testutil::{dark, settings_at, Listing, Reply, Script, TempDir, TestResult};

type Step = (&'static str, Reply);

fn chars(text: &str) -> usize {
    text.chars().count()
}

#[derive(Clone)]
struct Orchard {
    listing: Listing,
    touched: Arc<AtomicUsize>,
    broken: bool,
}

impl Orchard {
    fn new(accounts: Vec<MailAccount>, calendars: Vec<CalendarInfo>) -> Orchard {
        Orchard {
            listing: Listing {
                accounts,
                calendars,
            },
            touched: Arc::new(AtomicUsize::new(0)),
            broken: false,
        }
    }

    fn empty() -> Orchard {
        Orchard::new(Vec::new(), Vec::new())
    }

    fn broken() -> Orchard {
        Orchard {
            broken: true,
            ..Orchard::empty()
        }
    }

    fn touch(&self) {
        self.touched.fetch_add(1, Ordering::SeqCst);
    }
}

impl Backend for Orchard {
    fn all_accounts(&self) -> ToolResult<Vec<MailAccount>> {
        self.touch();
        if self.broken {
            return Err(ToolError::msg("Mail laeuft nicht"));
        }
        self.listing.all_accounts()
    }

    fn account_info(&self, account: &Account) -> ToolResult<MailAccount> {
        self.touch();
        self.listing.account_info(account)
    }

    fn mailboxes(&self, account: &Account) -> ToolResult<Vec<Mailbox>> {
        self.touch();
        self.listing.mailboxes(account)
    }

    fn messages(&self, account: &Account, query: &MessageQuery) -> ToolResult<MessageList> {
        self.touch();
        self.listing.messages(account, query)
    }

    fn read(
        &self,
        account: &Account,
        mailbox: Option<&str>,
        id: &MessageId,
        max_chars: usize,
    ) -> ToolResult<MessageBody> {
        self.touch();
        self.listing.read(account, mailbox, id, max_chars)
    }

    fn compose(&self, account: &Account, request: &ComposeRequest) -> ToolResult<Preview> {
        self.touch();
        self.listing.compose(account, request)
    }

    fn approval_test(&self, seconds: u64) -> ToolResult<String> {
        self.touch();
        self.listing.approval_test(seconds)
    }
}

impl CalendarBackend for Orchard {
    fn calendars(&self) -> ToolResult<Vec<CalendarInfo>> {
        self.touch();
        self.listing.calendars()
    }

    fn events(&self, calendar: &Calendar, window: Window) -> ToolResult<Vec<RawEvent>> {
        self.touch();
        self.listing.events(calendar, window)
    }

    fn event(&self, calendar: &Calendar, uid: &str) -> ToolResult<EventDetail> {
        self.touch();
        self.listing.event(calendar, uid)
    }

    fn change(
        &self,
        calendar: &Calendar,
        change: &Change,
        approval: &Approval,
    ) -> ToolResult<ChangeResult> {
        self.touch();
        self.listing.change(calendar, change, approval)
    }
}

struct Station {
    _dir: TempDir,
    ctx: Context,
    apple: Arc<AtomicUsize>,
}

fn prepared(config: Option<&str>) -> Result<(TempDir, crate::config::Settings), Box<dyn Error>> {
    let dir = TempDir::new()?;
    let settings = settings_at(&dir.path("mail/config.json"));
    if let Some(text) = config {
        fs::create_dir_all(dir.path("mail"))?;
        fs::write(&settings.config_path, text)?;
    }
    Ok((dir, settings))
}

fn station(config: Option<&str>, orchard: Orchard) -> Result<Station, Box<dyn Error>> {
    let (dir, settings) = prepared(config)?;
    let apple = Arc::clone(&orchard.touched);
    Ok(Station {
        _dir: dir,
        ctx: Context::new(settings, Box::new(orchard.clone()), Box::new(orchard)),
        apple,
    })
}

fn demo_station() -> Result<Station, Box<dyn Error>> {
    let (dir, settings) = prepared(None)?;
    Ok(Station {
        _dir: dir,
        ctx: Context::demo(settings, FakeOptions::default()),
        apple: Arc::new(AtomicUsize::new(0)),
    })
}

impl Station {
    fn config(&self) -> String {
        fs::read_to_string(&self.ctx.settings.config_path).unwrap_or_default()
    }

    fn rights(&self) -> Result<Rights, String> {
        self.ctx
            .settings
            .stored_rights()
            .map_err(|error| error.message())
    }

    fn marker(&self) -> String {
        self.ctx.settings.onboard_marker_path()
    }

    fn apple_calls(&self) -> usize {
        self.apple.load(Ordering::SeqCst)
    }
}

fn apple_orchard() -> Orchard {
    Orchard::new(
        vec![MailAccount {
            id: "ACC-1".to_string(),
            name: "iCloud".to_string(),
            kind: "imap".to_string(),
            addresses: vec!["me@icloud.example".to_string()],
        }],
        vec![
            CalendarInfo {
                id: "CAL-1".to_string(),
                name: "Privat".to_string(),
                writable: true,
            },
            CalendarInfo {
                id: "CAL-2".to_string(),
                name: "Feiertage".to_string(),
                writable: false,
            },
        ],
    )
}

fn run_with(station: &Station, script: &Script, gui: fn() -> bool, args: &[&str]) -> (u8, String) {
    let screen = Screen {
        host: script,
        gui: &gui,
    };
    let args: Vec<String> = args.iter().map(|argument| argument.to_string()).collect();
    let mut out = Vec::new();
    let code = dispatch(
        &args,
        &station.ctx,
        &screen,
        &mut Cursor::new(Vec::new()),
        &mut out,
    );
    (code, String::from_utf8_lossy(&out).into_owned())
}

fn onboard(station: &Station, script: &Script) -> (u8, String) {
    run_with(station, script, || true, &["onboard"])
}

fn scripted(steps: &[Vec<Step>]) -> Script {
    let mut script = Script::new();
    for (op, reply) in steps.concat() {
        script = script.then(op, reply);
    }
    script
}

fn welcome() -> Vec<Step> {
    vec![("ask_buttons", Reply::Button(2))]
}

fn levels(read: &[usize], write: &[usize]) -> Vec<Step> {
    vec![
        ("ask_buttons", Reply::Button(1)),
        ("choose_list", Reply::List(read.to_vec())),
        ("choose_list", Reply::List(write.to_vec())),
        ("ask_buttons", Reply::Button(1)),
    ]
}

fn summary_later() -> Vec<Step> {
    vec![("ask_buttons", Reply::Cancel), ("notify", Reply::Done)]
}

fn happy() -> Vec<Vec<Step>> {
    vec![welcome(), levels(&[0, 1, 2], &[1]), summary_later()]
}

fn assert_clean(script: &Script) {
    assert!(script.mismatches().is_empty(), "{:?}", script.mismatches());
    assert_eq!(script.left(), 0);
    for seen in script.seen() {
        assert!(seen.payload.len() < 64 * 1024, "{}", seen.payload.len());
        assert!(seen.wait <= Duration::from_secs(700), "{:?}", seen.wait);
    }
}

fn mode(path: &str) -> Result<u32, Box<dyn Error>> {
    Ok(fs::metadata(path)?.permissions().mode() & 0o777)
}

#[test]
fn the_arguments_of_the_command_are_none_or_the_one_flag() {
    let words = |args: &[&str]| -> Vec<String> { args.iter().map(|a| a.to_string()).collect() };
    assert_eq!(mode_of(&words(&[])), Some(Mode::Always));
    assert_eq!(mode_of(&words(&["--if-needed"])), Some(Mode::IfNeeded));
    for bad in [
        vec!["--if-needed", "--if-needed"],
        vec!["--if-needed", "x"],
        vec!["x"],
        vec!["--if-needed "],
        vec!["--IF-NEEDED"],
        vec!["if-needed"],
        vec![""],
    ] {
        assert_eq!(mode_of(&words(&bad)), None, "{bad:?}");
    }
}

#[test]
fn a_dismissal_is_a_private_regular_file_beside_the_config_and_only_a_regular_file_counts(
) -> TestResult {
    let station = station(None, Orchard::empty())?;
    let settings = &station.ctx.settings;
    assert_eq!(
        settings.onboard_marker_path(),
        format!("{}/onboard.dismissed", dirname(&settings.config_path))
    );
    assert!(!dismissed(settings));
    dismiss(settings)?;
    assert!(dismissed(settings));
    assert_eq!(mode(&station.marker())?, 0o600);
    assert_eq!(mode(&dirname(&settings.config_path))?, 0o700);
    assert_eq!(fs::read_to_string(station.marker())?, "");
    dismiss(settings)?;
    assert!(dismissed(settings));
    fs::remove_file(station.marker())?;
    assert!(!dismissed(settings));
    fs::create_dir(station.marker())?;
    assert!(!dismissed(settings));
    assert!(dismiss(settings).is_err());
    fs::remove_dir(station.marker())?;
    let target = dirname(&settings.config_path) + "/elsewhere";
    fs::write(&target, "x")?;
    std::os::unix::fs::symlink(&target, station.marker())?;
    assert!(!dismissed(settings));
    assert!(dismiss(settings).is_err());
    assert_eq!(fs::read_to_string(&target)?, "x");
    Ok(())
}

#[test]
fn nothing_is_asked_when_the_setup_is_complete_dismissed_switched_off_demo_or_headless(
) -> TestResult {
    let cases: [(&str, Option<&str>, bool); 6] = [
        (
            "a mail account is enabled",
            Some(r#"{"version": 4, "mail": [{"id": "A1", "name": "iCloud", "level": "read"}]}"#),
            true,
        ),
        (
            "a calendar is enabled",
            Some(
                r#"{"version": 4, "calendars": [{"id": "C1", "name": "Privat", "level": "read"}]}"#,
            ),
            true,
        ),
        ("the dialogs were dismissed", None, true),
        ("the variable is set", None, true),
        ("demo mode", None, true),
        ("there is no graphical session", None, false),
    ];
    for (name, config, gui) in cases {
        let mut station = station(config, Orchard::empty())?;
        match name {
            "the dialogs were dismissed" => dismiss(&station.ctx.settings)?,
            "the variable is set" => station.ctx.settings.no_onboard = true,
            "demo mode" => station.ctx.settings.fake = true,
            _ => {}
        }
        let script = Script::new();
        let screen: fn() -> bool = if gui { || true } else { || false };
        let (code, out) = run_with(&station, &script, screen, &["onboard", "--if-needed"]);
        assert_eq!((code, out.as_str()), (0, ""), "{name}");
        assert!(script.seen().is_empty(), "{name}");
        assert_eq!(station.apple_calls(), 0, "{name}");
    }
    Ok(())
}

#[test]
fn the_flag_starts_the_dialogs_only_for_an_empty_setup_in_a_graphical_session() -> TestResult {
    for config in [None, Some(r#"{"version": 4}"#)] {
        let station = station(config, Orchard::empty())?;
        let script = Script::new().then("ask_buttons", Reply::Cancel);
        let (code, out) = run_with(&station, &script, || true, &["onboard", "--if-needed"]);
        assert_eq!(code, 0, "{out}");
        assert_eq!(script.ops(), ["ask_buttons"], "{config:?}");
        assert!(script.mismatches().is_empty());
    }
    Ok(())
}

#[test]
fn an_unreadable_config_is_reported_and_never_gets_a_dialog() -> TestResult {
    for config in [
        "not json",
        "{\"version\": 2}",
        "{\"version\": 3, \"apple\": {\"mail\": [{\"id\": \"A\", \"level\": \"read\"}]}}",
        "[]",
        "{\"version\": 4, \"mail\": 5}",
    ] {
        let station = station(Some(config), Orchard::empty())?;
        let script = Script::new().then("notify", Reply::Done);
        let (code, out) = run_with(&station, &script, || true, &["onboard", "--if-needed"]);
        assert_eq!(code, 1, "{config}: {out}");
        assert!(out.starts_with("Fehler: "), "{config}: {out}");
        assert_eq!(script.ops(), ["notify"], "{config}");
        assert!(script.mismatches().is_empty(), "{config}");
        let seen = script.seen();
        assert_eq!(seen[0].text("title"), FAILED_TITLE);
        assert_eq!(seen[0].text("text"), UNREADABLE_NOTICE);
        assert!(seen[0].text("text").contains("nicht lesbar"));
        assert!(seen[0].text("text").contains("avery mail status"));
        assert_eq!(station.config(), config);
        let dark = Script::new();
        let (code, out) = run_with(&station, &dark, || false, &["onboard", "--if-needed"]);
        assert_eq!(code, 1, "{config}: {out}");
        assert!(out.starts_with("Fehler: "), "{config}: {out}");
        assert!(dark.seen().is_empty(), "{config}");
        assert_eq!(station.config(), config);
    }
    Ok(())
}

#[test]
fn unknown_arguments_are_a_usage_error_before_anything_happens() -> TestResult {
    let station = station(None, Orchard::empty())?;
    for args in [
        vec!["onboard", "--now"],
        vec!["onboard", "--if-needed", "x"],
        vec!["onboard", "x"],
    ] {
        let script = Script::new();
        let (code, out) = run_with(&station, &script, || true, &args);
        assert_eq!(code, 2, "{args:?}: {out}");
        assert!(
            out.contains("Benutzung: avery mail onboard [--if-needed]"),
            "{out}"
        );
        assert!(script.seen().is_empty());
    }
    Ok(())
}

#[test]
fn onboarding_refuses_in_demo_mode_before_any_dialog_and_touches_nothing() -> TestResult {
    let mut station = station(None, Orchard::empty())?;
    station.ctx.settings.fake = true;
    let script = Script::new();
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("Im Demo-Modus (MAIL_MCP_FAKE=1) nicht moeglich"),
        "{out}"
    );
    assert!(script.seen().is_empty());
    assert!(!Path::new(&station.ctx.settings.config_path).exists());
    assert!(!Path::new(&station.marker()).exists());
    assert!(!Path::new(&station.ctx.settings.lock_path()).exists());
    assert_eq!(station.apple_calls(), 0);
    Ok(())
}

#[test]
fn a_dismissal_does_not_stop_the_plain_command() -> TestResult {
    let station = station(None, Orchard::empty())?;
    dismiss(&station.ctx.settings)?;
    let script = Script::new().then("ask_buttons", Reply::Cancel);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_eq!(script.ops(), ["ask_buttons"]);
    Ok(())
}

#[test]
fn the_overview_counts_the_enabled_accounts_and_calendars() -> TestResult {
    let config = r#"{"version": 4,
        "mail": [{"id": "A1", "name": "iCloud", "level": "read"}, {"id": "A2", "name": "Privat", "level": "write"}],
        "calendars": [{"id": "C1", "name": "Privat", "level": "read"}]}"#;
    let station = station(Some(config), Orchard::empty())?;
    assert_eq!(
        overview(&station.rights()?),
        "Apple Mail und Kalender: 2 Konten, 1 Kalender"
    );
    assert!(station.rights()?.enabled());
    assert!(!Rights::empty().enabled());
    Ok(())
}

#[test]
fn a_stop_becomes_the_matching_ending() {
    assert_eq!(ending_of(Stop::Cancelled), Ending::Cancelled);
    assert_eq!(ending_of(Stop::TimedOut), Ending::TimedOut);
    assert_eq!(
        ending_of(Stop::Failed("x".to_string())),
        Ending::Failed("x".to_string())
    );
}

#[test]
fn a_setup_starts_with_the_explanation_and_saves_the_chosen_levels() -> TestResult {
    let station = station(None, apple_orchard())?;
    let script = scripted(&happy());
    assert_eq!(station.apple_calls(), 0);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    assert_eq!(
        script.ops(),
        [
            "ask_buttons",
            "ask_buttons",
            "choose_list",
            "choose_list",
            "ask_buttons",
            "ask_buttons",
            "notify"
        ]
    );
    assert!(out.contains("Einrichtung abgeschlossen."), "{out}");
    assert!(out.contains(RESTART_GOOSE), "{out}");
    let rights = station.rights()?;
    assert_eq!(rights.mail.len(), 1);
    assert_eq!(rights.mail_level("ACC-1"), Some(Level::Read));
    assert_eq!(rights.calendar_level("CAL-1"), Some(Level::Write));
    assert_eq!(rights.calendar_level("CAL-2"), Some(Level::Read));
    assert!(station.apple_calls() >= 2);
    assert!(!Path::new(&station.marker()).exists());
    let seen = script.seen();
    assert_eq!(seen[0].text("title"), "Willkommen bei Avery");
    assert_eq!(seen[1].text("title"), "Apple Mail und Kalender");
    assert!(seen[1].text("text").contains("Automation"));
    assert!(seen[1].text("text").contains(PRIVATE_ONLY));
    assert!(seen[1]
        .text("text")
        .starts_with("Avery startet jetzt Mail und Kalender"));
    assert!(
        seen[4].text("text").contains("Neue Freigaben:"),
        "{}",
        seen[4].text("text")
    );
    assert!(seen[5].text("text").contains("Das ist eingerichtet:"));
    assert!(seen[5]
        .text("text")
        .contains("Apple Mail und Kalender: 1 Konten, 2 Kalender"));
    assert!(seen[5].text("text").contains(RESTART_GOOSE));
    assert!(seen[6].text("text").contains("Einrichtung abgeschlossen"));
    assert!(seen[6].text("text").contains("Goose danach neu starten"));
    let again = scripted(&[]);
    let (code, out) = run_with(&station, &again, || true, &["onboard", "--if-needed"]);
    assert_eq!((code, out.as_str()), (0, ""));
    assert!(again.seen().is_empty());
    Ok(())
}

#[test]
fn nothing_is_touched_until_the_explanation_is_confirmed() -> TestResult {
    let station = station(None, apple_orchard())?;
    let script = scripted(&[
        welcome(),
        vec![("ask_buttons", Reply::Cancel), ("notify", Reply::Done)],
    ]);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    assert_eq!(station.apple_calls(), 0);
    assert!(!Path::new(&station.ctx.settings.config_path).exists());
    let later = scripted(&[vec![("ask_buttons", Reply::Cancel)]]);
    let (code, out) = onboard(&station, &later);
    assert_eq!(code, 0, "{out}");
    assert_eq!(station.apple_calls(), 0);
    Ok(())
}

#[test]
fn the_offer_to_check_runs_the_same_read_check_and_shows_the_result() -> TestResult {
    let station = demo_station()?;
    let script = scripted(&[
        welcome(),
        levels(&[0, 3], &[]),
        vec![
            ("ask_buttons", Reply::Button(1)),
            ("notify", Reply::Done),
            ("ask_buttons", Reply::Button(0)),
            ("notify", Reply::Done),
        ],
    ]);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    let seen = script.seen();
    assert_eq!(seen[6].op(), "notify");
    assert!(seen[6].text("text").contains("Lesetest"));
    let result = seen[7].text("text");
    assert!(result.starts_with("Lesetest bestanden.\n\n"), "{result}");
    assert!(
        result.contains("PASS  Konto 1  Konto erreichbar"),
        "{result}"
    );
    assert!(
        result.contains("PASS  Kalender 1  Termine lesen"),
        "{result}"
    );
    assert!(result.contains("Alle Pruefungen bestanden."), "{result}");
    assert_eq!(seen[7].text("icon"), "note");
    let failing = station_with_failing_checks()?;
    let script = scripted(&[
        welcome(),
        levels(&[0, 1], &[]),
        vec![
            ("ask_buttons", Reply::Button(1)),
            ("notify", Reply::Done),
            ("ask_buttons", Reply::Button(0)),
            ("notify", Reply::Done),
        ],
    ]);
    let (code, out) = onboard(&failing, &script);
    assert_eq!(code, 0, "{out}");
    let seen = script.seen();
    assert!(
        seen[7].text("text").starts_with("Lesetest fehlgeschlagen."),
        "{}",
        seen[7].text("text")
    );
    assert!(seen[7].text("text").contains("FAIL"));
    assert_eq!(seen[7].text("icon"), "caution");
    Ok(())
}

fn station_with_failing_checks() -> Result<Station, Box<dyn Error>> {
    station(None, apple_orchard())
}

fn trailing(index: usize) -> Vec<Step> {
    match index {
        0 => Vec::new(),
        _ => vec![("notify", Reply::Done)],
    }
}

#[test]
fn cancelling_at_every_step_stops_cleanly_and_saves_nothing() -> TestResult {
    let steps = happy().concat();
    for index in 0..5usize {
        let (op, _) = steps[index];
        let mut tried: Vec<Step> = steps[..index].to_vec();
        tried.push((op, Reply::Cancel));
        tried.extend(trailing(index));
        let mut script = Script::new();
        for (op, reply) in tried {
            script = script.then(op, reply);
        }
        let station = station(None, apple_orchard())?;
        let (code, out) = onboard(&station, &script);
        assert_eq!(code, 0, "step {index}: {out}");
        assert_clean(&script);
        assert!(!Path::new(&station.marker()).exists(), "step {index}");
        assert!(
            !Path::new(&station.ctx.settings.config_path).exists(),
            "step {index}"
        );
        let expected = if index == 0 {
            "spaeter verschoben"
        } else {
            "Abgebrochen."
        };
        assert!(out.contains(expected), "step {index}: {out}");
        if index == 0 {
            assert_eq!(station.apple_calls(), 0);
        }
    }
    Ok(())
}

#[test]
fn a_timeout_ends_the_run_and_keeps_only_what_was_saved_before() -> TestResult {
    let station = station(None, apple_orchard())?;
    let script = scripted(&[
        welcome(),
        vec![("ask_buttons", Reply::Timeout), ("notify", Reply::Done)],
    ]);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    assert!(out.contains("Keine Antwort im Dialog"), "{out}");
    assert_eq!(station.apple_calls(), 0);
    assert!(!Path::new(&station.ctx.settings.config_path).exists());

    let config = r#"{"version": 4, "mail": [{"id": "A1", "name": "iCloud", "level": "read"}],
        "calendars": [{"id": "C1", "name": "Privat", "level": "read"}]}"#;
    for (way_out, lead) in [
        (Reply::Cancel, "Abgebrochen. Das ist schon gespeichert und bleibt:"),
        (
            Reply::Timeout,
            "Keine Antwort im Dialog, die Einrichtung wurde beendet. Das ist schon gespeichert und bleibt:",
        ),
    ] {
        let station = station_with(config)?;
        let script = scripted(&[
            welcome(),
            vec![
                ("ask_buttons", way_out),
                ("ask_buttons", Reply::Button(0)),
            ],
        ]);
        let (code, out) = onboard(&station, &script);
        assert_eq!(code, 0, "{out}");
        assert_clean(&script);
        let kept = script
            .seen()
            .last()
            .map(|seen| seen.text("text"))
            .unwrap_or_default();
        assert!(kept.contains(lead), "{kept}");
        assert!(
            kept.contains("Apple Mail und Kalender: 1 Konten, 1 Kalender"),
            "{kept}"
        );
        assert!(kept.contains(RESTART_GOOSE), "{kept}");
        assert_eq!(station.config(), config);
    }
    Ok(())
}

fn station_with(config: &str) -> Result<Station, Box<dyn Error>> {
    station(Some(config), apple_orchard())
}

#[test]
fn the_finished_notice_and_the_hint_of_the_terminal_say_the_same_about_goose() {
    assert!(FINISHED_NOTICE.contains(RESTART_GOOSE));
    assert!(FINISHED_NOTICE.contains("Goose danach neu starten"));
    assert!(FINISHED_NOTICE.chars().count() < 240);
}

#[test]
fn the_welcome_can_postpone_or_dismiss_for_good() -> TestResult {
    let station = station(None, Orchard::empty())?;
    let script = Script::new().then("ask_buttons", Reply::Cancel);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert!(!Path::new(&station.marker()).exists());
    assert_eq!(script.seen().len(), 1);
    let (code, out) = run_with(
        &station,
        &Script::new().then("ask_buttons", Reply::Timeout),
        || true,
        &["onboard", "--if-needed"],
    );
    assert_eq!(code, 0, "{out}");
    assert!(!Path::new(&station.marker()).exists());

    let script = Script::new()
        .then("ask_buttons", Reply::Button(1))
        .then("notify", Reply::Done);
    let (code, out) = run_with(&station, &script, || true, &["onboard", "--if-needed"]);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    assert!(Path::new(&station.marker()).is_file());
    assert_eq!(mode(&station.marker())?, 0o600);
    assert!(out.contains("onboard.dismissed"), "{out}");
    assert!(!Path::new(&station.ctx.settings.config_path).exists());
    let silent = Script::new();
    let (code, out) = run_with(&station, &silent, || true, &["onboard", "--if-needed"]);
    assert_eq!((code, out.as_str()), (0, ""));
    assert!(silent.seen().is_empty());
    Ok(())
}

#[test]
fn a_dismissal_that_cannot_be_saved_is_reported() -> TestResult {
    let station = station(None, Orchard::empty())?;
    fs::create_dir_all(station.marker())?;
    for args in [vec!["onboard"], vec!["onboard", "--if-needed"]] {
        let script = Script::new()
            .then("ask_buttons", Reply::Button(1))
            .then("notify", Reply::Done);
        let (code, out) = run_with(&station, &script, || true, &args);
        assert_eq!(code, 1, "{out}");
        assert!(
            out.contains("Die Einstellung konnte nicht gespeichert werden"),
            "{out}"
        );
        assert_eq!(script.ops(), ["ask_buttons", "notify"], "{args:?}");
        assert!(script.mismatches().is_empty(), "{args:?}");
        let notice = script.seen()[1].text("text");
        assert!(
            notice.contains("Die Einstellung konnte nicht gespeichert werden"),
            "{notice}"
        );
        assert!(notice.ends_with(FAILED_HINT), "{notice}");
    }
    Ok(())
}

#[test]
fn a_notification_that_cannot_be_shown_does_not_change_how_a_failure_ends() -> TestResult {
    for reply in [Reply::Silent, Reply::Hung, Reply::Flood, Reply::Cancel] {
        let station = station(None, Orchard::empty())?;
        let script = Script::new()
            .then("ask_buttons", Reply::Silent)
            .then("notify", reply.clone());
        let (code, out) = onboard(&station, &script);
        assert_eq!(code, 1, "{reply:?}: {out}");
        assert!(out.starts_with("Fehler: "), "{reply:?}: {out}");
        assert_eq!(script.ops(), ["ask_buttons", "notify"], "{reply:?}");
        assert!(!Path::new(&station.ctx.settings.config_path).exists());
    }
    Ok(())
}

#[test]
fn a_config_that_turns_unreadable_before_the_summary_ends_the_run_with_a_notification() -> TestResult
{
    let station = station(Some("not json"), Orchard::empty())?;
    let script = Script::new().then("notify", Reply::Done);
    let mut flow = Flow::new(&station.ctx, &script);
    let Next::End(ending) = flow.summary() else {
        return Err("the summary should have ended the run".into());
    };
    assert!(matches!(ending, Ending::Failed(_)));
    let mut input = Cursor::new(Vec::new());
    let mut out = Vec::new();
    let code = {
        let mut console = Console::terminal(&mut input, &mut out);
        flow.conclude(ending, &mut console)
    };
    let out = String::from_utf8_lossy(&out).into_owned();
    assert_eq!(code, 1, "{out}");
    assert!(out.starts_with("Fehler: "), "{out}");
    assert_eq!(script.ops(), ["notify"]);
    let notice = script.seen()[0].text("text");
    assert!(notice.contains("config.json"), "{notice}");
    assert!(notice.ends_with(FAILED_HINT), "{notice}");
    assert_eq!(station.config(), "not json");
    Ok(())
}

#[test]
fn an_empty_choice_ends_the_run_without_changes() -> TestResult {
    let station = station(None, apple_orchard())?;
    let script = scripted(&[
        welcome(),
        vec![
            ("ask_buttons", Reply::Button(1)),
            ("choose_list", Reply::List(Vec::new())),
        ],
        vec![("notify", Reply::Done)],
    ]);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    assert!(out.contains("Es ist noch kein Konto freigegeben."), "{out}");
    assert!(!Path::new(&station.ctx.settings.config_path).exists());
    let seen = script.seen();
    assert_eq!(seen[2].field("defaults"), Some(json::parse("[]")?));
    let items = seen[2].field("items").ok_or("items")?;
    let items = json::dumps(&items, false);
    assert!(
        items.contains("iCloud")
            && items.contains("Privat")
            && items.contains("Feiertage [schreibgeschuetzt]"),
        "{items}"
    );
    assert_eq!(seen[3].op(), "notify");
    assert!(seen[3].text("text").contains("avery mail onboard"));
    Ok(())
}

#[test]
fn a_failed_step_offers_to_cancel_or_to_go_on_and_is_not_repeated() -> TestResult {
    for (reply, ending, notice) in [
        (
            Reply::Button(0),
            "Abgebrochen.",
            "Abgebrochen. Es wurde nichts gespeichert.",
        ),
        (
            Reply::Button(1),
            "Es ist noch kein Konto freigegeben.",
            "Nichts ausgewählt.",
        ),
    ] {
        let station = station(None, Orchard::broken())?;
        let script = scripted(&[
            welcome(),
            vec![
                ("ask_buttons", Reply::Button(1)),
                ("ask_buttons", reply),
                ("notify", Reply::Done),
            ],
        ]);
        let (code, out) = onboard(&station, &script);
        assert_eq!(code, 0, "{out}");
        assert_clean(&script);
        let seen = script.seen();
        let failure = &seen[2];
        assert_eq!(failure.text("title"), "Apple Mail und Kalender");
        assert!(
            failure.text("text").contains("Apple Mail ist nicht lesbar"),
            "{}",
            failure.text("text")
        );
        assert_eq!(failure.text("icon"), "caution");
        assert_eq!(
            failure.field("buttons"),
            Some(json::parse("[\"Abbrechen\", \"Weiter\"]")?)
        );
        assert!(out.contains(ending), "{out}");
        assert!(
            seen[3].text("text").starts_with(notice),
            "{}",
            seen[3].text("text")
        );
        assert_eq!(station.apple_calls(), 1);
        assert!(!Path::new(&station.ctx.settings.config_path).exists());
    }
    Ok(())
}

#[test]
fn declining_the_recap_saves_nothing_and_offers_to_cancel_or_to_go_on() -> TestResult {
    let station = station(None, apple_orchard())?;
    let script = scripted(&[
        welcome(),
        vec![
            ("ask_buttons", Reply::Button(1)),
            ("choose_list", Reply::List(vec![0, 1])),
            ("choose_list", Reply::List(Vec::new())),
            ("ask_buttons", Reply::Button(0)),
            ("ask_buttons", Reply::Button(0)),
            ("notify", Reply::Done),
        ],
    ]);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    let seen = script.seen();
    assert!(seen[4].text("text").starts_with("Neue Freigaben:"));
    assert!(
        seen[5].text("text").contains("Abgebrochen."),
        "{}",
        seen[5].text("text")
    );
    assert!(!Path::new(&station.ctx.settings.config_path).exists());
    Ok(())
}

#[test]
fn dialogs_that_cannot_be_shown_end_the_run_with_the_reason_and_change_nothing() -> TestResult {
    for (reply, wanted) in [
        (
            Reply::Raw {
                code: Some(1),
                stdout: String::new(),
                stderr:
                    "execution error: Not authorized to send Apple events to System Events. (-1743)"
                        .to_string(),
            },
            "Automation",
        ),
        (
            Reply::Raw {
                code: Some(0),
                stdout: "{\"ok\": false, \"error\": \"dialog_failed: no window server\"}"
                    .to_string(),
                stderr: String::new(),
            },
            "angemeldete Sitzung",
        ),
        (Reply::Silent, "Dialog"),
        (Reply::Flood, "Dialog"),
        (
            Reply::Data("{\"result\": \"pizza\"}".to_string()),
            "unerwartete Antwort",
        ),
    ] {
        let station = station(None, apple_orchard())?;
        let script = Script::new()
            .then("ask_buttons", reply)
            .then("notify", Reply::Done);
        let (code, out) = onboard(&station, &script);
        assert_eq!(code, 1, "{out}");
        assert!(out.starts_with("Fehler: "), "{out}");
        assert!(out.contains(wanted), "{out}");
        assert_eq!(script.ops(), ["ask_buttons", "notify"]);
        assert!(script.mismatches().is_empty());
        let seen = script.seen();
        assert_eq!(seen[1].text("title"), FAILED_TITLE);
        assert!(seen[1].text("text").contains(wanted), "{wanted}");
        assert!(seen[1].text("text").ends_with(FAILED_HINT), "{wanted}");
        assert!(chars(&seen[1].text("text")) <= 240);
        assert!(!Path::new(&station.ctx.settings.config_path).exists());
        assert!(!Path::new(&station.marker()).exists());
        assert_eq!(station.apple_calls(), 0);
    }
    Ok(())
}

#[test]
fn a_dialog_that_breaks_in_the_middle_leaves_a_consistent_state() -> TestResult {
    let station = station(None, apple_orchard())?;
    let script = scripted(&[
        welcome(),
        vec![("ask_buttons", Reply::Button(1))],
        vec![("choose_list", Reply::Silent)],
        vec![("ask_buttons", Reply::Silent)],
        vec![("notify", Reply::Done)],
    ]);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 1, "{out}");
    assert_clean(&script);
    assert!(!Path::new(&station.ctx.settings.config_path).exists());
    assert!(!Path::new(&station.marker()).exists());
    Ok(())
}

#[test]
fn a_revoke_from_another_terminal_while_the_recap_waits_is_not_undone() -> TestResult {
    let config = r#"{"version": 4, "mail": [{"id": "ACC-1", "name": "iCloud", "level": "read"}]}"#;
    let station = station(Some(config), apple_orchard())?;
    let path = station.ctx.settings.config_path.clone();
    let script = scripted(&[
        welcome(),
        levels(&[0, 1], &[]),
        vec![("ask_buttons", Reply::Button(1))],
    ])
    .after(5, move || {
        let other = Context::demo(settings_at(&path), FakeOptions::default());
        let _ = dispatch(
            &["revoke".to_string()],
            &other,
            &dark(),
            &mut Cursor::new(Vec::new()),
            &mut Vec::new(),
        );
    });
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    let rights = station.rights()?;
    assert!(!rights.enabled());
    let failure = script
        .seen()
        .iter()
        .map(|seen| seen.text("text"))
        .find(|text| text.contains("inzwischen geaendert"))
        .ok_or("no failure dialog")?;
    assert!(failure.contains("Nichts gespeichert"), "{failure}");
    Ok(())
}

#[test]
fn no_other_command_ever_shows_a_dialog_or_calls_the_screen() -> TestResult {
    let station = station(
        Some(
            r#"{"version": 4, "mail": [{"id": "ACC-1", "name": "iCloud", "level": "write"}],
                "calendars": [{"id": "CAL-1", "name": "Privat", "level": "read"}]}"#,
        ),
        apple_orchard(),
    )?;
    let script = Script::new();
    for (args, input) in [
        (vec!["accounts"], ""),
        (vec!["calendars"], ""),
        (vec!["status"], ""),
        (vec!["setup"], "1\n0\nj\n"),
        (vec!["revoke"], ""),
        (vec!["selftest"], ""),
        (vec!["selftest-write"], "n\nn\nn\nn\nn\n"),
        (vec!["sendlog"], ""),
        (vec!["changes"], ""),
        (vec!["undo"], ""),
        (vec!["help"], ""),
        (vec!["version"], ""),
        (vec!["nonsense"], ""),
    ] {
        let screen = Screen {
            host: &script,
            gui: &|| true,
        };
        let args: Vec<String> = args.iter().map(|argument| argument.to_string()).collect();
        let _ = dispatch(
            &args,
            &station.ctx,
            &screen,
            &mut Cursor::new(input.as_bytes().to_vec()),
            &mut Vec::new(),
        );
    }
    assert!(script.seen().is_empty(), "{:?}", script.ops());
    Ok(())
}

#[test]
fn the_server_has_no_way_to_onboard_or_to_open_a_dialog() -> TestResult {
    let station = station(None, Orchard::empty())?;
    let script = Script::new();
    let screen = Screen {
        host: &script,
        gui: &|| true,
    };
    let calls = ["onboard", "setup", "ask_text", "choose_list", "notify", "ask_buttons", "dismiss"]
        .iter()
        .enumerate()
        .map(|(number, name)| {
            format!(
                "{{\"jsonrpc\": \"2.0\", \"id\": {}, \"method\": \"tools/call\", \"params\": {{\"name\": \"{name}\", \"arguments\": {{}}}}}}\n",
                number + 10
            )
        })
        .collect::<String>();
    let input = format!(
        "{{\"jsonrpc\": \"2.0\", \"id\": 1, \"method\": \"initialize\", \"params\": {{\"protocolVersion\": \"2024-11-05\"}}}}\n{{\"jsonrpc\": \"2.0\", \"id\": 2, \"method\": \"tools/list\", \"params\": {{}}}}\n{calls}"
    );
    let mut out = Vec::new();
    let code = dispatch(
        &["serve".to_string()],
        &station.ctx,
        &screen,
        &mut Cursor::new(input.into_bytes()),
        &mut out,
    );
    assert_eq!(code, 0);
    let text = String::from_utf8_lossy(&out).into_owned();
    let listed = text.lines().nth(1).unwrap_or_default();
    for name in ["onboard", "setup", "dismiss", "dialog", "ask_"] {
        assert!(!listed.contains(name), "{name}: {listed}");
    }
    let answers: Vec<&str> = text.lines().skip(2).collect();
    assert_eq!(answers.len(), 7, "{text}");
    for answer in answers {
        assert!(
            answer.contains("\"isError\": true") || answer.contains("\"error\""),
            "{answer}"
        );
    }
    assert!(script.seen().is_empty());
    assert!(!Path::new(&station.ctx.settings.config_path).exists());
    assert!(!Path::new(&station.marker()).exists());
    Ok(())
}

#[test]
fn the_command_is_listed_in_the_help_and_among_the_suggestions() {
    assert!(crate::texts::COMMANDS.contains(&"onboard"));
    assert!(crate::texts::HELP.contains("avery mail onboard"));
    assert!(crate::texts::HELP.contains("--if-needed"));
    assert_eq!(crate::cli::closest_command("onbord"), Some("onboard"));
    assert_eq!(crate::cli::closest_command("onboarding"), Some("onboard"));
}

fn long_calendars(count: usize, length: usize) -> Orchard {
    Orchard::new(
        Vec::new(),
        (0..count)
            .map(|number| CalendarInfo {
                id: format!("CAL-{number}"),
                name: format!("{number:02} {}", "k".repeat(length - 3)),
                writable: true,
            })
            .collect(),
    )
}

#[test]
fn twelve_calendars_with_long_names_are_all_in_the_final_dialog_with_the_writes_first() -> TestResult
{
    let station = station(None, long_calendars(12, 60))?;
    let names: Vec<String> = (0..12)
        .map(|number| format!("{number:02} {}", "k".repeat(57)))
        .collect();
    let script = scripted(&[
        welcome(),
        levels(&(0..12).collect::<Vec<_>>(), &[9, 10, 11]),
        summary_later(),
    ]);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    let recap = script.seen()[4].text("text");
    let mut lines = vec!["Neue Freigaben: 3 Kalender zum Schreiben, 9 zum Lesen".to_string()];
    for name in &names[9..] {
        lines.push(format!("Kalender: {name} (schreiben)"));
    }
    for name in &names[..9] {
        lines.push(format!("Kalender: {name} (lesen)"));
    }
    lines.push(String::new());
    lines.push("Korrekt?".to_string());
    assert_eq!(recap, lines.join("\n"));
    let rights = station.rights()?;
    assert_eq!(rights.calendars_at(Level::Read).len(), 12);
    assert_eq!(rights.calendars_at(Level::Write).len(), 3);
    assert!(!recap.contains("[...]"));
    Ok(())
}

#[test]
fn after_eight_existing_grants_the_recap_lists_only_what_changes() -> TestResult {
    let old: Vec<String> = (1..=8).map(|number| format!("Alt {number}")).collect();
    let entries: Vec<String> = old
        .iter()
        .enumerate()
        .map(|(index, name)| {
            format!(r#"{{"id": "OLD-{index}", "name": "{name}", "level": "read"}}"#)
        })
        .collect();
    let config = format!(r#"{{"version": 4, "calendars": [{}]}}"#, entries.join(", "));
    let mut calendars: Vec<CalendarInfo> = old
        .iter()
        .enumerate()
        .map(|(index, name)| CalendarInfo {
            id: format!("OLD-{index}"),
            name: name.clone(),
            writable: true,
        })
        .collect();
    calendars.push(CalendarInfo {
        id: "NEW-1".to_string(),
        name: "Privat".to_string(),
        writable: true,
    });
    let station = station(Some(&config), Orchard::new(Vec::new(), calendars))?;
    let script = scripted(&[
        welcome(),
        levels(&(0..9).collect::<Vec<_>>(), &[8]),
        summary_later(),
    ]);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    let recap = script.seen()[4].text("text");
    assert_eq!(
        recap,
        "Neue Freigaben: 1 Kalender zum Schreiben\nKalender: Privat (schreiben)\n\nKorrekt?"
    );
    for name in &old {
        assert!(!recap.contains(name.as_str()), "{recap}");
    }
    let rights = station.rights()?;
    assert_eq!(rights.calendars.len(), 9);
    assert_eq!(rights.calendar_level("NEW-1"), Some(Level::Write));
    for index in 0..8 {
        assert_eq!(
            rights.calendar_level(&format!("OLD-{index}")),
            Some(Level::Read)
        );
    }
    Ok(())
}

#[test]
fn a_calendar_that_vanished_is_a_removed_line_and_not_a_note() -> TestResult {
    let config = r#"{"version": 4, "calendars": [
        {"id": "KEEP", "name": "Bleibt", "level": "read"},
        {"id": "GONE", "name": "Weg", "level": "read"}]}"#;
    let calendars = vec![
        CalendarInfo {
            id: "KEEP".to_string(),
            name: "Bleibt".to_string(),
            writable: true,
        },
        CalendarInfo {
            id: "NEW".to_string(),
            name: "Neu".to_string(),
            writable: true,
        },
    ];
    let station = station(Some(config), Orchard::new(Vec::new(), calendars))?;
    let script = scripted(&[
        welcome(),
        vec![
            ("ask_buttons", Reply::Button(1)),
            ("choose_list", Reply::List(vec![0, 1])),
            ("choose_list", Reply::List(Vec::new())),
            ("ask_buttons", Reply::Button(1)),
        ],
        summary_later(),
    ]);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    let seen = script.seen();
    assert_eq!(
        seen[4].text("text"),
        "Neue Freigaben: 1 Kalender zum Lesen, 1 entzogen\nKalender: Neu (lesen)\nKalender: Weg (aus) [bisher: lesen]\n\nKorrekt?"
    );
    assert!(seen
        .iter()
        .all(|dialog| !dialog.text("text").contains("Nicht mehr vorhanden")));
    let rights = station.rights()?;
    assert_eq!(rights.calendars.len(), 2);
    assert_eq!(rights.calendar_level("GONE"), None);
    Ok(())
}

#[test]
fn a_recap_that_would_be_cut_is_refused_before_any_yes_and_names_the_command() -> TestResult {
    let station = station(None, long_calendars(40, 60))?;
    let script = scripted(&[
        welcome(),
        vec![
            ("ask_buttons", Reply::Button(1)),
            ("choose_list", Reply::List((0..40).collect())),
            ("choose_list", Reply::List(Vec::new())),
        ],
        vec![("ask_buttons", Reply::Cancel), ("notify", Reply::Done)],
    ]);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    let seen = script.seen();
    for dialog in &seen {
        assert!(!dialog.text("text").contains("Korrekt?"), "{}", dialog.op());
    }
    let failure = seen[4].text("text");
    assert!(
        failure.contains(
            "zu viele Eintraege fuer einen Dialog, nichts gespeichert, bitte im Terminal: avery mail setup"
        ),
        "{failure}"
    );
    assert!(!Path::new(&station.ctx.settings.config_path).exists());
    assert!(station.apple_calls() >= 2);
    Ok(())
}

#[test]
fn hostile_names_are_cleaned_and_bounded_before_they_are_shown() -> TestResult {
    let nasty = "Fa\u{202e}mi\u{200b}lie\n(privat)";
    let long = format!("Fa\u{202e}mi\u{200b}lie\n{}", "x".repeat(500));
    let station = station(
        None,
        Orchard::new(
            vec![MailAccount {
                id: "ACC-1".to_string(),
                name: nasty.to_string(),
                kind: "imap".to_string(),
                addresses: vec![nasty.to_string()],
            }],
            vec![
                CalendarInfo {
                    id: "CAL-1".to_string(),
                    name: nasty.to_string(),
                    writable: true,
                },
                CalendarInfo {
                    id: "CAL-2".to_string(),
                    name: long,
                    writable: true,
                },
            ],
        ),
    )?;
    let script = scripted(&[welcome(), levels(&[0, 1], &[]), summary_later()]);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    let items = script.seen()[2].field("items").ok_or("items")?;
    let items = items.as_array().ok_or("items")?;
    assert_eq!(items.len(), 3);
    for item in items {
        let item = item.as_str().ok_or("item")?;
        assert!(
            !item.contains('\u{202e}') && !item.contains('\u{200b}') && !item.contains('\n'),
            "{item}"
        );
        assert!(item.chars().count() <= 180, "{}", item.chars().count());
        assert!(item.contains("Familie"), "{item}");
    }
    let rights = station.rights()?;
    assert_eq!(rights.mail.len(), 1);
    assert_eq!(rights.calendars.len(), 1);
    assert_eq!(rights.calendars[0].0.name, "Familie (privat)");
    assert!(!station.config().contains('\u{202e}'));
    Ok(())
}
