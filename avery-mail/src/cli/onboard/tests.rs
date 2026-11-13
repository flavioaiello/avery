use std::error::Error;
use std::fs;
use std::io::Cursor;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
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
use crate::http::scripted::{Scripted, Shared};
use crate::http::Method;
use crate::json::{self, Json};
use crate::provider::fakes::{google_transport, Visit, Visitor, GOOGLE_TOKENS};
use crate::provider::{Calendars, Linker, Mail};
use crate::rights::Level;
use crate::testutil::{dark, gm, settings_at, Listing, Reply, Script, TempDir, TestResult};
use crate::vault::memory::{Memory, Shared as SharedVault};

const SECRET: &str = "GOCSPX-hunter2_secret";
const CLIENT: &str = "123-abc.apps.example";
const MS_CLIENT: &str = "0a1b2c3d-1111-2222-3333-444455556666";
const MS_TOKENS: &str = r#"{"access_token": "EwB-access", "refresh_token": "M.R3-refresh", "expires_in": 3600, "token_type": "Bearer", "scope": "Mail.Read Calendars.Read User.Read offline_access"}"#;
const MS_WRITE_TOKENS: &str = r#"{"access_token": "EwB-access", "refresh_token": "M.R3-refresh", "expires_in": 3600, "token_type": "Bearer", "scope": "Mail.Read Calendars.Read User.Read offline_access Mail.ReadWrite Mail.Send Calendars.ReadWrite"}"#;
const TOKEN: &str =
    r#"{"access_token": "ya29.test-token", "expires_in": 3599, "token_type": "Bearer"}"#;
const CALENDAR_LIST: &str = r#"{"kind": "calendar#calendarList", "items": [
    {"id": "{alice}", "summary": "{alice}", "accessRole": "owner"},
    {"id": "familie@group.example", "summary": "Familie", "accessRole": "writer"},
    {"id": "feiertage@group.example", "summary": "Feiertage", "accessRole": "reader"}]}"#;
const OUTLOOK_CALENDARS: &str =
    r#"{"value": [{"id": "AQMkCAL", "name": "Calendar", "canEdit": true}]}"#;

type Step = (&'static str, Reply);

fn chars(text: &str) -> usize {
    text.chars().count()
}

fn alice() -> String {
    gm("alice@{gmail}")
}

fn google_tokens(write: bool) -> String {
    let extra = if write {
        " https://www.googleapis.com/auth/gmail.compose https://www.googleapis.com/auth/calendar.events"
    } else {
        ""
    };
    GOOGLE_TOKENS.replace(
        "calendar.readonly\"",
        &format!("calendar.readonly{extra}\""),
    )
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
    fn calendars(
        &self,
        provider: crate::provider::Provider,
        address: &str,
    ) -> ToolResult<Vec<CalendarInfo>> {
        self.touch();
        self.listing.calendars(provider, address)
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
    transport: Arc<Scripted>,
    vault: Arc<Memory>,
    apple: Arc<AtomicUsize>,
    browser: Arc<Mutex<Vec<String>>>,
}

fn station(
    config: Option<&str>,
    transport: Scripted,
    visit: Visit,
    orchard: Orchard,
) -> Result<Station, Box<dyn Error>> {
    let dir = TempDir::new()?;
    let settings = settings_at(&dir.path("mail/config.json"));
    if let Some(text) = config {
        fs::create_dir_all(dir.path("mail"))?;
        fs::write(&settings.config_path, text)?;
    }
    let transport = Arc::new(transport);
    let vault = Arc::new(Memory::new());
    let browser = Arc::new(Mutex::new(Vec::new()));
    let apple = Arc::clone(&orchard.touched);
    let deadline = if matches!(visit, Visit::Nothing) {
        Duration::from_millis(900)
    } else {
        Duration::from_secs(8)
    };
    let linker = Linker::new(
        Box::new(Shared(Arc::clone(&transport))),
        Box::new(SharedVault(Arc::clone(&vault))),
        Box::new(Visitor {
            visit,
            seen: Arc::clone(&browser),
        }),
    )
    .within(deadline);
    let mail = Mail::new(
        Box::new(orchard.clone()),
        Box::new(Shared(Arc::clone(&transport))),
        Box::new(SharedVault(Arc::clone(&vault))),
        Box::new(DARK_HOST),
    );
    let calendars = Calendars::new(
        Box::new(orchard),
        Box::new(Shared(Arc::clone(&transport))),
        Box::new(SharedVault(Arc::clone(&vault))),
        Box::new(DARK_HOST),
    );
    Ok(Station {
        _dir: dir,
        ctx: Context::new(settings, Box::new(mail), Box::new(calendars), linker),
        transport,
        vault,
        apple,
        browser,
    })
}

struct DarkHost;

const DARK_HOST: DarkHost = DarkHost;

impl crate::osascript::Host for DarkHost {
    fn ensure_running(&self, _app: crate::osascript::App) {}

    fn run(
        &self,
        _app: crate::osascript::App,
        _payload: &str,
        _timeout: Duration,
    ) -> crate::osascript::Finished {
        crate::osascript::Finished::NotStarted
    }
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

    fn tokens(&self) -> usize {
        self.vault.count()
    }

    fn apple_calls(&self) -> usize {
        self.apple.load(Ordering::SeqCst)
    }

    fn browser_visits(&self) -> usize {
        self.browser.lock().map(|seen| seen.len()).unwrap_or(0)
    }
}

fn gmail_station(
    config: Option<&str>,
    visit: Visit,
    write: bool,
    orchard: Orchard,
) -> Result<Station, Box<dyn Error>> {
    let transport = google_transport(&google_tokens(write))
        .json(Method::Post, "/token", 200, TOKEN)
        .json(
            Method::Get,
            "/users/me/calendarList",
            200,
            &CALENDAR_LIST.replace("{alice}", &alice()),
        );
    station(config, transport, visit, orchard)
}

fn both_station(
    config: Option<&str>,
    visit: Visit,
    write: bool,
    orchard: Orchard,
) -> Result<Station, Box<dyn Error>> {
    let transport = google_transport(&google_tokens(write))
        .json(
            Method::Post,
            "oauth2/v2.0/token",
            200,
            if write { MS_WRITE_TOKENS } else { MS_TOKENS },
        )
        .json(
            Method::Get,
            "/me?$select=mail",
            200,
            r#"{"mail": "bob@outlook.example", "userPrincipalName": "bob@outlook.example"}"#,
        )
        .json(Method::Get, "/me/calendars", 200, OUTLOOK_CALENDARS)
        .json(
            Method::Post,
            "oauth2.googleapis.com/token",
            200,
            &google_tokens(write),
        )
        .json(
            Method::Get,
            "/users/me/calendarList",
            200,
            &CALENDAR_LIST.replace("{alice}", &alice()),
        );
    station(config, transport, visit, orchard)
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

fn accounts(chosen: &[usize]) -> Vec<Step> {
    vec![("choose_list", Reply::List(chosen.to_vec()))]
}

fn link_google(write: bool) -> Vec<Step> {
    vec![
        ("ask_buttons", Reply::Button(2)),
        ("ask_text", Reply::text(CLIENT)),
        ("ask_text", Reply::text(SECRET)),
        ("ask_buttons", Reply::Button(if write { 2 } else { 1 })),
        ("notify", Reply::Done),
        ("ask_buttons", Reply::Button(0)),
    ]
}

fn link_microsoft(write: bool) -> Vec<Step> {
    vec![
        ("ask_buttons", Reply::Button(2)),
        ("ask_text", Reply::text(MS_CLIENT)),
        ("ask_buttons", Reply::Button(if write { 2 } else { 1 })),
        ("notify", Reply::Done),
        ("ask_buttons", Reply::Button(0)),
    ]
}

fn read_calendars(read: &[usize]) -> Vec<Step> {
    vec![
        ("choose_list", Reply::List(read.to_vec())),
        ("ask_buttons", Reply::Button(1)),
    ]
}

fn summary_later() -> Vec<Step> {
    vec![("ask_buttons", Reply::Cancel), ("notify", Reply::Done)]
}

fn gmail_only() -> Vec<Vec<Step>> {
    vec![
        welcome(),
        accounts(&[0]),
        link_google(false),
        read_calendars(&[0, 1]),
        summary_later(),
    ]
}

fn assert_clean(script: &Script) {
    assert!(script.mismatches().is_empty(), "{:?}", script.mismatches());
    assert_eq!(script.left(), 0);
    for seen in script.seen() {
        assert!(seen.payload.len() < 64 * 1024, "{}", seen.payload.len());
        assert!(seen.wait <= Duration::from_secs(700), "{:?}", seen.wait);
        assert!(!seen.payload.contains(SECRET), "{}", seen.payload);
        assert!(!seen.payload.contains("hunter2"), "{}", seen.payload);
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
    let station = gmail_station(None, Visit::Nothing, false, Orchard::empty())?;
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
    let cases: [(&str, Option<&str>, bool, bool, bool); 7] = [
        (
            "an account is linked",
            Some(
                r#"{"version": 3, "accounts": [{"provider": "microsoft", "address": "bob@outlook.example"}]}"#,
            ),
            false,
            false,
            true,
        ),
        (
            "an Apple entry exists",
            Some(
                r#"{"version": 3, "apple": {"calendars": [{"id": "C1", "name": "Privat", "level": "read"}]}}"#,
            ),
            false,
            false,
            true,
        ),
        ("the dialogs were dismissed", None, true, false, true),
        ("the variable is set", None, false, false, true),
        ("demo mode", None, false, false, true),
        ("there is no graphical session", None, false, false, false),
        (
            "a config without grants but with an account",
            Some(
                r#"{"version": 3, "accounts": [{"provider": "google", "address": "x@gmail.example"}]}"#,
            ),
            false,
            false,
            true,
        ),
    ];
    for (name, config, dismissed_before, _unused, gui) in cases {
        let mut station = gmail_station(config, Visit::Nothing, false, Orchard::empty())?;
        if dismissed_before {
            dismiss(&station.ctx.settings)?;
        }
        match name {
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
    for config in [None, Some(r#"{"version": 3}"#)] {
        let station = gmail_station(config, Visit::Nothing, false, Orchard::empty())?;
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
        "[]",
        "{\"version\": 3, \"accounts\": 5}",
    ] {
        let station = gmail_station(Some(config), Visit::Nothing, false, Orchard::empty())?;
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
    let station = gmail_station(None, Visit::Nothing, false, Orchard::empty())?;
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
    let mut station = gmail_station(None, Visit::Good("c"), false, Orchard::empty())?;
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
    assert_eq!(station.tokens(), 0);
    assert_eq!(station.apple_calls(), 0);
    Ok(())
}

#[test]
fn a_dismissal_does_not_stop_the_plain_command() -> TestResult {
    let station = gmail_station(None, Visit::Nothing, false, Orchard::empty())?;
    dismiss(&station.ctx.settings)?;
    let script = Script::new().then("ask_buttons", Reply::Cancel);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_eq!(script.ops(), ["ask_buttons"]);
    Ok(())
}

#[test]
fn the_overview_names_every_account_calendar_and_the_apple_part_within_bounds() -> TestResult {
    let config = gm(r#"{"version": 3, "accounts": [
        {"provider": "google", "address": "alice@{gmail}", "mail": "write",
         "calendars": [{"id": "alice@{gmail}", "name": "Privat", "level": "write"}]},
        {"provider": "microsoft", "address": "bob@outlook.example"}],
        "apple": {"mail": [{"id": "A1", "name": "iCloud", "level": "read"}],
                  "calendars": [{"id": "C1", "name": "Privat", "level": "read"}]}}"#);
    let station = gmail_station(Some(&config), Visit::Nothing, false, Orchard::empty())?;
    let text = overview(&station.rights()?);
    assert_eq!(
        text,
        gm("Gmail: alice@{gmail}\n    Mail: schreiben\n    Kalender: Privat (schreiben)\nOutlook.com: bob@outlook.example\n    Mail: aus\n    Kalender: keine\nApple Mail und Kalender: 1 Konten, 1 Kalender")
    );
    assert!(holds_something(&station.rights()?));
    assert!(!holds_something(&Rights::empty()));
    Ok(())
}

#[test]
fn a_stop_becomes_the_matching_ending() {
    assert_eq!(ending_of(Stop::Cancelled), Ending::Cancelled);
    assert_eq!(ending_of(Stop::TimedOut), Ending::TimedOut);
    assert_eq!(ending_of(Stop::Away), Ending::Away);
    assert_eq!(
        ending_of(Stop::Failed("x".to_string())),
        Ending::Failed("x".to_string())
    );
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

fn leaked(station: &Station, out: &str) -> Vec<&'static str> {
    let mut found = Vec::new();
    if station.config().contains(SECRET) {
        found.push("config");
    }
    if out.contains(SECRET) {
        found.push("output");
    }
    found
}

#[test]
fn a_gmail_only_setup_links_the_account_and_saves_the_chosen_calendars() -> TestResult {
    let station = gmail_station(None, Visit::Good("4/0code"), false, Orchard::empty())?;
    let script = scripted(&gmail_only());
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    assert_eq!(
        script.ops(),
        [
            "ask_buttons",
            "choose_list",
            "ask_buttons",
            "ask_text",
            "ask_text",
            "ask_buttons",
            "notify",
            "ask_buttons",
            "choose_list",
            "ask_buttons",
            "ask_buttons",
            "notify"
        ]
    );
    assert!(out.contains("Einrichtung abgeschlossen."), "{out}");
    let rights = station.rights()?;
    assert_eq!(rights.accounts.len(), 1);
    assert_eq!(
        rights.mail_level(&gm("google:alice@{gmail}")),
        Some(Level::Read)
    );
    assert_eq!(
        rights.calendar_level(&gm("google:alice@{gmail}:alice@{gmail}")),
        Some(Level::Read)
    );
    assert_eq!(
        rights.calendar_level(&gm("google:alice@{gmail}:familie@group.example")),
        Some(Level::Read)
    );
    assert_eq!(
        rights.calendar_level(&gm("google:alice@{gmail}:feiertage@group.example")),
        None
    );
    assert!(rights.apple.mail.is_empty() && rights.apple.calendars.is_empty());
    assert_eq!(station.tokens(), 1);
    assert!(station.vault.get(&gm("google:alice@{gmail}")).is_some());
    assert!(leaked(&station, &out).is_empty());
    assert_eq!(station.apple_calls(), 0);
    assert_eq!(station.browser_visits(), 1);
    assert!(!Path::new(&station.marker()).exists());
    let seen = script.seen();
    assert_eq!(seen[3].field("hidden"), Some(Json::Bool(false)));
    assert_eq!(seen[4].field("hidden"), Some(Json::Bool(true)));
    assert_eq!(seen[1].text("title"), "Willkommen bei Avery");
    assert_eq!(seen[2].text("title"), "Gmail verbinden");
    assert_eq!(seen[8].text("title"), "Gmail: Kalender");
    assert!(
        seen[9].text("text").contains("Neue Freigaben:"),
        "{}",
        seen[9].text("text")
    );
    assert!(seen[10].text("text").contains("Das ist eingerichtet:"));
    assert!(seen[10].text("text").contains(&alice()));
    assert!(seen[11].text("text").contains("Einrichtung abgeschlossen"));
    assert!(seen[11].text("text").contains("Goose danach neu starten"));
    assert!(script.seen().iter().all(|seen| seen.op() != "open_url"));
    let again = scripted(&[]);
    let (code, out) = run_with(&station, &again, || true, &["onboard", "--if-needed"]);
    assert_eq!((code, out.as_str()), (0, ""));
    assert!(again.seen().is_empty());
    Ok(())
}

#[test]
fn the_write_level_adds_a_second_list_and_the_scopes_decide_what_is_granted() -> TestResult {
    let station = gmail_station(None, Visit::Good("c"), true, Orchard::empty())?;
    let script = scripted(&[
        welcome(),
        accounts(&[0]),
        link_google(true),
        vec![
            ("choose_list", Reply::List(vec![0, 1, 2])),
            ("choose_list", Reply::List(vec![1])),
            ("ask_buttons", Reply::Button(1)),
        ],
        summary_later(),
    ]);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    let rights = station.rights()?;
    assert_eq!(
        rights.mail_level(&gm("google:alice@{gmail}")),
        Some(Level::Write)
    );
    assert_eq!(
        rights.calendar_level(&gm("google:alice@{gmail}:alice@{gmail}")),
        Some(Level::Read)
    );
    assert_eq!(
        rights.calendar_level(&gm("google:alice@{gmail}:familie@group.example")),
        Some(Level::Write)
    );
    assert_eq!(
        rights.calendar_level(&gm("google:alice@{gmail}:feiertage@group.example")),
        Some(Level::Read)
    );
    let write_list = &script.seen()[9];
    assert_eq!(write_list.op(), "choose_list");
    let items = write_list.field("items").ok_or("items")?;
    assert_eq!(items.as_array().map(Vec::len), Some(2));
    assert!(leaked(&station, &out).is_empty());
    Ok(())
}

#[test]
fn an_outlook_only_setup_asks_no_secret() -> TestResult {
    let station = both_station(None, Visit::Good("c"), false, Orchard::empty())?;
    let script = scripted(&[
        welcome(),
        accounts(&[1]),
        link_microsoft(false),
        read_calendars(&[0]),
        summary_later(),
    ]);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    let rights = station.rights()?;
    assert_eq!(rights.accounts.len(), 1);
    assert_eq!(
        rights.mail_level("microsoft:bob@outlook.example"),
        Some(Level::Read)
    );
    assert_eq!(
        rights.calendar_level("microsoft:bob@outlook.example:AQMkCAL"),
        Some(Level::Read)
    );
    assert_eq!(
        script.ops().iter().filter(|op| *op == "ask_text").count(),
        1
    );
    assert_eq!(station.tokens(), 1);
    assert_eq!(station.apple_calls(), 0);
    let seen = script.seen();
    assert_eq!(seen[2].text("title"), "Outlook.com verbinden");
    assert!(seen[2].text("text").contains("App-Registrierung"));
    Ok(())
}

#[test]
fn gmail_and_outlook_are_linked_one_after_the_other() -> TestResult {
    let station = both_station(None, Visit::Good("c"), false, Orchard::empty())?;
    let script = scripted(&[
        welcome(),
        accounts(&[0, 1]),
        link_google(false),
        read_calendars(&[0]),
        link_microsoft(false),
        read_calendars(&[0]),
        summary_later(),
    ]);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    let rights = station.rights()?;
    assert_eq!(rights.accounts.len(), 2);
    assert_eq!(station.tokens(), 2);
    assert_eq!(rights.calendars_at(Level::Read).len(), 2);
    let overview = script
        .seen()
        .iter()
        .map(|seen| seen.text("text"))
        .find(|text| text.starts_with("Das ist eingerichtet:"))
        .ok_or("overview")?;
    assert!(overview.contains("Gmail: ") && overview.contains("Outlook.com: bob@outlook.example"));
    assert_eq!(station.apple_calls(), 0);
    assert!(leaked(&station, &out).is_empty());
    Ok(())
}

#[test]
fn an_apple_only_setup_starts_with_the_explanation_and_saves_the_apple_section() -> TestResult {
    let station = gmail_station(None, Visit::Nothing, false, apple_orchard())?;
    let script = scripted(&[
        welcome(),
        accounts(&[2]),
        vec![
            ("ask_buttons", Reply::Button(1)),
            ("choose_list", Reply::List(vec![0, 1, 2])),
            ("choose_list", Reply::List(vec![1])),
            ("ask_buttons", Reply::Button(1)),
        ],
        summary_later(),
    ]);
    assert_eq!(station.apple_calls(), 0);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    let rights = station.rights()?;
    assert!(rights.accounts.is_empty());
    assert_eq!(rights.apple.mail.len(), 1);
    assert_eq!(rights.mail_level("ACC-1"), Some(Level::Read));
    assert_eq!(rights.calendar_level("CAL-1"), Some(Level::Write));
    assert_eq!(rights.calendar_level("CAL-2"), Some(Level::Read));
    assert!(station.apple_calls() >= 2);
    assert_eq!(station.tokens(), 0);
    assert_eq!(station.browser_visits(), 0);
    let seen = script.seen();
    assert!(seen[2].text("text").contains("Automation"));
    assert!(seen[2].text("text").contains(PRIVATE_ONLY));
    assert_eq!(seen[2].text("title"), "Apple Mail und Kalender");
    assert!(seen[2]
        .text("text")
        .starts_with("Avery startet jetzt Mail und Kalender"));
    Ok(())
}

#[test]
fn apple_is_not_touched_until_the_explanation_is_confirmed_and_never_when_not_chosen() -> TestResult
{
    let station = gmail_station(None, Visit::Nothing, false, apple_orchard())?;
    let script = scripted(&[
        welcome(),
        accounts(&[2]),
        vec![("ask_buttons", Reply::Cancel), ("notify", Reply::Done)],
    ]);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    assert_eq!(station.apple_calls(), 0);
    assert!(!Path::new(&station.ctx.settings.config_path).exists());
    let chosen = gmail_station(None, Visit::Good("c"), false, apple_orchard())?;
    let script = scripted(&gmail_only());
    let (code, out) = onboard(&chosen, &script);
    assert_eq!(code, 0, "{out}");
    assert_eq!(chosen.apple_calls(), 0);
    Ok(())
}

#[test]
fn the_offer_to_check_runs_the_same_read_check_and_shows_the_result() -> TestResult {
    let transport = google_transport(&google_tokens(false))
        .json(
            Method::Post,
            "oauth2.googleapis.com/token",
            200,
            &google_tokens(false),
        )
        .json(
            Method::Get,
            "/users/me/calendarList",
            200,
            &CALENDAR_LIST.replace("{alice}", &alice()),
        )
        .json(
            Method::Get,
            "/profile",
            200,
            &gm(r#"{"emailAddress": "alice@{gmail}"}"#),
        )
        .json(
            Method::Get,
            "/labels/INBOX",
            200,
            r#"{"id": "INBOX", "messagesUnread": 2}"#,
        )
        .json(
            Method::Get,
            "/labels",
            200,
            r#"{"labels": [{"id": "INBOX", "name": "INBOX", "type": "system"}]}"#,
        )
        .json(
            Method::Get,
            "/messages?maxResults",
            200,
            r#"{"resultSizeEstimate": 0}"#,
        )
        .json(
            Method::Get,
            "/events",
            200,
            r#"{"kind": "calendar#events", "items": []}"#,
        );
    let station = station(None, transport, Visit::Good("c"), Orchard::empty())?;
    let script = scripted(&[
        welcome(),
        accounts(&[0]),
        link_google(false),
        read_calendars(&[0, 1]),
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
    assert_eq!(seen[11].op(), "notify");
    assert!(seen[11].text("text").contains("Lesetest"));
    let result = seen[12].text("text");
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
    assert_eq!(seen[12].text("icon"), "note");
    assert_eq!(station.apple_calls(), 0);
    let failing = gmail_station(None, Visit::Good("c"), false, apple_orchard())?;
    let script = scripted(&[
        welcome(),
        accounts(&[0]),
        link_google(false),
        read_calendars(&[0]),
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
        seen[12]
            .text("text")
            .starts_with("Lesetest fehlgeschlagen."),
        "{}",
        seen[12].text("text")
    );
    assert!(seen[12].text("text").contains("FAIL"));
    assert_eq!(seen[12].text("icon"), "caution");
    Ok(())
}

fn trailing(index: usize) -> Vec<Step> {
    match index {
        0 => Vec::new(),
        1..=5 => vec![("notify", Reply::Done)],
        _ => vec![("ask_buttons", Reply::Button(0))],
    }
}

#[test]
fn cancelling_at_every_step_stops_cleanly_and_keeps_only_what_was_completed() -> TestResult {
    let happy = gmail_only().concat();
    for index in [0usize, 1, 2, 3, 4, 5, 7, 8, 9] {
        let (op, _) = happy[index];
        let way_out = if index == 7 {
            Reply::Timeout
        } else {
            Reply::Cancel
        };
        let mut steps: Vec<Step> = happy[..index].to_vec();
        steps.push((op, way_out));
        steps.extend(trailing(index));
        let mut script = Script::new();
        for (op, reply) in steps {
            script = script.then(op, reply);
        }
        let station = gmail_station(None, Visit::Good("c"), false, Orchard::empty())?;
        let (code, out) = onboard(&station, &script);
        assert_eq!(code, 0, "step {index}: {out}");
        assert_clean(&script);
        assert_eq!(station.apple_calls(), 0, "step {index}");
        assert!(!Path::new(&station.marker()).exists(), "step {index}");
        if index < 7 {
            assert!(
                !Path::new(&station.ctx.settings.config_path).exists(),
                "step {index}"
            );
            assert_eq!(station.tokens(), 0, "step {index}");
            assert_eq!(station.browser_visits(), 0, "step {index}");
        } else {
            let rights = station.rights()?;
            assert_eq!(rights.accounts.len(), 1, "step {index}");
            assert_eq!(
                rights.mail_level(&gm("google:alice@{gmail}")),
                Some(Level::Read)
            );
            assert!(rights.calendars_at(Level::Read).is_empty(), "step {index}");
            assert_eq!(station.tokens(), 1, "step {index}");
            let kept = script
                .seen()
                .last()
                .map(|seen| seen.text("text"))
                .unwrap_or_default();
            assert!(
                kept.contains("Das ist schon gespeichert und bleibt:"),
                "step {index}: {kept}"
            );
            assert!(kept.contains(&alice()), "step {index}: {kept}");
            assert!(kept.contains(RESTART_GOOSE), "step {index}: {kept}");
        }
        let expected = match index {
            0 => "spaeter verschoben",
            7 => "Keine Antwort im Dialog",
            _ => "Abgebrochen.",
        };
        assert!(out.contains(expected), "step {index}: {out}");
        assert!(leaked(&station, &out).is_empty());
    }
    Ok(())
}

#[test]
fn cancelling_the_second_account_keeps_the_first_one_complete() -> TestResult {
    let station = both_station(None, Visit::Good("c"), false, Orchard::empty())?;
    let script = scripted(&[
        welcome(),
        accounts(&[0, 1]),
        link_google(false),
        read_calendars(&[0, 1]),
        vec![
            ("ask_buttons", Reply::Cancel),
            ("ask_buttons", Reply::Button(0)),
        ],
    ]);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    let rights = station.rights()?;
    assert_eq!(rights.accounts.len(), 1);
    assert_eq!(rights.calendars_at(Level::Read).len(), 2);
    assert_eq!(station.tokens(), 1);
    let kept = script
        .seen()
        .last()
        .map(|seen| seen.text("text"))
        .unwrap_or_default();
    assert!(
        kept.contains("Abgebrochen. Das ist schon gespeichert und bleibt:"),
        "{kept}"
    );
    assert!(kept.contains("Kalender: "), "{kept}");
    assert!(kept.contains(RESTART_GOOSE), "{kept}");
    Ok(())
}

#[test]
fn the_finished_notice_and_the_hint_of_the_terminal_say_the_same_about_goose() {
    assert!(FINISHED_NOTICE.contains(RESTART_GOOSE));
    assert!(FINISHED_NOTICE.contains("Goose danach neu starten"));
    assert!(FINISHED_NOTICE.chars().count() < 240);
}

#[test]
fn the_welcome_can_postpone_or_dismiss_for_good() -> TestResult {
    let station = gmail_station(None, Visit::Nothing, false, Orchard::empty())?;
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
    let station = gmail_station(None, Visit::Nothing, false, Orchard::empty())?;
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
        let station = gmail_station(None, Visit::Nothing, false, Orchard::empty())?;
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
    let station = gmail_station(Some("not json"), Visit::Nothing, false, Orchard::empty())?;
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
fn an_empty_choice_of_accounts_ends_the_run_without_changes() -> TestResult {
    let station = gmail_station(None, Visit::Nothing, false, apple_orchard())?;
    let script = scripted(&[welcome(), accounts(&[]), vec![("notify", Reply::Done)]]);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    assert!(out.contains("Nichts ausgewaehlt"), "{out}");
    assert!(!Path::new(&station.ctx.settings.config_path).exists());
    assert_eq!(station.apple_calls(), 0);
    let seen = script.seen();
    assert_eq!(seen[1].field("defaults"), Some(json::parse("[0, 1]")?));
    let labels = seen[1].field("items").ok_or("items")?;
    let labels = json::dumps(&labels, false);
    assert!(
        labels.contains("Gmail")
            && labels.contains("Outlook.com")
            && labels.contains("Apple Mail und Kalender (optional)"),
        "{labels}"
    );
    Ok(())
}

#[test]
fn the_guide_opens_the_pages_and_ends_the_run_for_now() -> TestResult {
    let station = gmail_station(None, Visit::Nothing, false, Orchard::empty())?;
    let script = scripted(&[
        welcome(),
        accounts(&[0]),
        vec![
            ("ask_buttons", Reply::Button(1)),
            ("choose_list", Reply::List(vec![0, 3])),
            ("open_url", Reply::Done),
            ("open_url", Reply::Done),
            ("notify", Reply::Done),
        ],
    ]);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    let seen = script.seen();
    assert_eq!(seen[4].text("url"), crate::provider::Page::Readme.url());
    assert_eq!(
        seen[5].text("url"),
        crate::provider::Page::CalendarApi.url()
    );
    assert!(out.contains("Die Anleitung wurde geoeffnet"), "{out}");
    assert!(!Path::new(&station.ctx.settings.config_path).exists());
    assert!(!Path::new(&station.marker()).exists());
    assert_eq!(station.tokens(), 0);
    Ok(())
}

fn failing_flow(failure_reply: Reply, following: Vec<Vec<Step>>) -> Script {
    let mut steps = vec![
        welcome(),
        accounts(&[0]),
        link_google(false)[..5].to_vec(),
        vec![("ask_buttons", failure_reply)],
    ];
    steps.extend(following);
    scripted(&steps)
}

#[test]
fn a_refused_sign_in_shows_the_message_and_leaves_nothing_behind() -> TestResult {
    let station = gmail_station(
        None,
        Visit::Denied("access_denied"),
        false,
        Orchard::empty(),
    )?;
    let script = failing_flow(Reply::Button(1), vec![vec![("notify", Reply::Done)]]);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    let failure = &script.seen()[7];
    assert_eq!(failure.op(), "ask_buttons");
    assert_eq!(failure.text("title"), "Gmail verbinden: fehlgeschlagen");
    assert!(
        failure
            .text("text")
            .contains("The sign-in was refused (access_denied)"),
        "{}",
        failure.text("text")
    );
    assert_eq!(failure.text("icon"), "caution");
    assert!(out.contains("Es ist noch kein Konto verbunden."), "{out}");
    assert!(!Path::new(&station.ctx.settings.config_path).exists());
    assert_eq!(station.tokens(), 0);
    Ok(())
}

#[test]
fn a_browser_that_never_answers_ends_in_a_failure_dialog_and_the_cancel_keeps_nothing() -> TestResult
{
    let station = gmail_station(None, Visit::Nothing, false, Orchard::empty())?;
    let script = failing_flow(Reply::Cancel, vec![vec![("notify", Reply::Done)]]);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    assert!(script.seen()[7]
        .text("text")
        .contains("No sign-in arrived in time"));
    assert!(out.contains("Abgebrochen."), "{out}");
    assert_eq!(station.tokens(), 0);
    assert!(!Path::new(&station.ctx.settings.config_path).exists());
    Ok(())
}

#[test]
fn a_failed_account_does_not_stop_the_next_one() -> TestResult {
    let station = both_station(None, Visit::Good("c"), false, Orchard::empty())?;
    let wrong_id = vec![
        ("ask_buttons", Reply::Button(2)),
        ("ask_text", Reply::text("bad id!")),
        ("ask_text", Reply::text(SECRET)),
    ];
    let script = scripted(&[
        welcome(),
        accounts(&[0, 1]),
        wrong_id,
        vec![("ask_buttons", Reply::Button(1))],
        link_microsoft(false),
        read_calendars(&[0]),
        summary_later(),
    ]);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    let failure = &script.seen()[5];
    assert!(
        failure.text("text").contains("The client id looks wrong"),
        "{}",
        failure.text("text")
    );
    assert_eq!(station.browser_visits(), 1);
    let rights = station.rights()?;
    assert_eq!(rights.accounts.len(), 1);
    assert_eq!(rights.accounts[0].address(), "bob@outlook.example");
    assert_eq!(station.tokens(), 1);
    assert!(leaked(&station, &out).is_empty());
    Ok(())
}

#[test]
fn an_account_that_is_not_a_personal_gmail_account_is_refused_and_not_stored() -> TestResult {
    let transport = Scripted::new()
        .json(
            Method::Post,
            "oauth2.googleapis.com/token",
            200,
            &google_tokens(false),
        )
        .json(
            Method::Get,
            "openidconnect.googleapis.com",
            200,
            r#"{"sub": "1", "email": "alice@work.example", "email_verified": true}"#,
        );
    let station = station(None, transport, Visit::Good("c"), Orchard::empty())?;
    let script = failing_flow(Reply::Cancel, vec![vec![("notify", Reply::Done)]]);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    assert!(
        script.seen()[7]
            .text("text")
            .contains("Avery verbindet nur persoenliche Gmail-Konten"),
        "{}",
        script.seen()[7].text("text")
    );
    assert_eq!(station.tokens(), 0);
    assert!(!Path::new(&station.ctx.settings.config_path).exists());
    Ok(())
}

#[test]
fn a_busy_config_lock_removes_the_fresh_sign_in_again() -> TestResult {
    let station = gmail_station(None, Visit::Good("c"), false, Orchard::empty())?;
    fs::create_dir_all(dirname(&station.ctx.settings.config_path))?;
    let _held = crate::lock::acquire(&station.ctx.settings.lock_path(), Duration::ZERO)
        .map_err(|error| format!("{error:?}"))?;
    let script = failing_flow(Reply::Cancel, vec![vec![("notify", Reply::Done)]]);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    let failure = script.seen()[7].text("text");
    assert!(
        failure.contains("Ein anderer Avery-Befehl aendert gerade die Konfiguration"),
        "{failure}"
    );
    assert!(failure.contains("wieder entfernt"), "{failure}");
    assert_eq!(station.tokens(), 0);
    assert!(!Path::new(&station.ctx.settings.config_path).exists());
    Ok(())
}

#[test]
fn an_address_that_apple_already_serves_is_refused_and_its_sign_in_removed() -> TestResult {
    let config = gm(
        r#"{"version": 3, "apple": {"mail": [{"id": "A1", "name": "Privat", "addresses": ["alice@{gmail}"], "level": "read"}]}}"#,
    );
    let station = gmail_station(Some(&config), Visit::Good("c"), false, Orchard::empty())?;
    let script = failing_flow(Reply::Cancel, vec![vec![("ask_buttons", Reply::Button(0))]]);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    let failure = script.seen()[7].text("text");
    assert!(
        failure.contains("ist schon ueber Apple Mail und Kalender eingebunden"),
        "{failure}"
    );
    assert_eq!(station.tokens(), 0);
    assert_eq!(station.config(), config);
    Ok(())
}

#[test]
fn a_revoke_from_another_terminal_while_the_calendars_wait_is_not_undone() -> TestResult {
    let station = gmail_station(None, Visit::Good("c"), false, Orchard::empty())?;
    let path = station.ctx.settings.config_path.clone();
    let script = scripted(&[
        welcome(),
        accounts(&[0]),
        link_google(false),
        read_calendars(&[0, 1]),
        vec![("ask_buttons", Reply::Button(1))],
        summary_later(),
    ])
    .after(10, move || {
        let other = Context::demo(settings_at(&path), crate::fake::FakeOptions::default());
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
    assert_eq!(rights.mail_level(&gm("google:alice@{gmail}")), None);
    assert!(rights.calendars_at(Level::Read).is_empty());
    let failure = script
        .seen()
        .iter()
        .map(|seen| seen.text("text"))
        .find(|text| text.contains("inzwischen geaendert"))
        .ok_or("no failure dialog")?;
    assert!(failure.contains("Nichts gespeichert"), "{failure}");
    assert_eq!(station.tokens(), 1);
    Ok(())
}

#[test]
fn an_account_added_by_another_terminal_meanwhile_survives_the_calendar_step() -> TestResult {
    let station = both_station(None, Visit::Good("c"), false, Orchard::empty())?;
    let path = station.ctx.settings.config_path.clone();
    let script = scripted(&[
        welcome(),
        accounts(&[0]),
        link_google(false),
        read_calendars(&[0, 1]),
        summary_later(),
    ])
    .after(10, move || {
        let other = settings_at(&path);
        let _ = other.update_rights(|rights| {
            rights.add_account(
                crate::rights::RemoteAccount::new(
                    crate::provider::Provider::Microsoft,
                    "bob@outlook.example",
                )
                .ok_or(crate::rights::Refusal::Changed)?,
            )
        });
    });
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    let rights = station.rights()?;
    assert_eq!(rights.accounts.len(), 2);
    assert!(rights
        .account(crate::provider::Provider::Microsoft, "bob@outlook.example")
        .is_some());
    assert_eq!(rights.calendars_at(Level::Read).len(), 2);
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
        let station = gmail_station(None, Visit::Good("c"), false, apple_orchard())?;
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
        assert_eq!(station.tokens(), 0);
        assert_eq!(station.apple_calls(), 0);
    }
    Ok(())
}

#[test]
fn a_dialog_that_breaks_in_the_middle_leaves_a_consistent_state() -> TestResult {
    let station = gmail_station(None, Visit::Good("c"), false, Orchard::empty())?;
    let script = scripted(&[
        welcome(),
        accounts(&[0]),
        link_google(false),
        vec![("choose_list", Reply::Silent)],
        vec![("ask_buttons", Reply::Silent)],
        vec![("notify", Reply::Done)],
    ]);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 1, "{out}");
    assert_clean(&script);
    let rights = station.rights()?;
    assert_eq!(rights.accounts.len(), 1);
    assert_eq!(station.tokens(), 1);
    assert!(rights.calendars_at(Level::Read).is_empty());
    Ok(())
}

#[test]
fn hostile_answers_never_reach_the_config_the_output_or_the_next_dialog() -> TestResult {
    let hostile_id = format!("{CLIENT}\u{202e}\u{200b}\u{0}\n\"; touch pwned; echo \"");
    let station = gmail_station(None, Visit::Good("c"), false, Orchard::empty())?;
    let script = scripted(&[
        welcome(),
        accounts(&[0]),
        vec![
            ("ask_buttons", Reply::Button(2)),
            ("ask_text", Reply::text(&hostile_id)),
            ("ask_text", Reply::text(&format!("{SECRET}\u{200b}"))),
        ],
        vec![("ask_buttons", Reply::Cancel), ("notify", Reply::Done)],
    ]);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    let failure = script.seen()[5].text("text");
    assert!(failure.contains("The client id looks wrong"), "{failure}");
    assert!(!failure.contains("touch"), "{failure}");
    assert!(!out.contains("touch"), "{out}");
    assert_eq!(station.tokens(), 0);
    assert!(!Path::new(&station.ctx.settings.config_path).exists());
    assert_eq!(station.browser_visits(), 0);

    for reply in [
        Reply::text(&"9".repeat(100_000)),
        Reply::TooLong,
        Reply::Data("{\"result\": \"text\", \"text\": null}".to_string()),
        Reply::Cancel,
        Reply::Timeout,
    ] {
        let station = gmail_station(None, Visit::Good("c"), false, Orchard::empty())?;
        let script = scripted(&[
            welcome(),
            accounts(&[0]),
            vec![
                ("ask_buttons", Reply::Button(2)),
                ("ask_text", reply.clone()),
            ],
        ]);
        let script = match reply {
            Reply::Cancel | Reply::Timeout => script.then("notify", Reply::Done),
            _ => script
                .then("ask_buttons", Reply::Cancel)
                .then("notify", Reply::Done),
        };
        let (code, out) = onboard(&station, &script);
        assert_eq!(code, 0, "{reply:?}: {out}");
        assert_clean(&script);
        assert_eq!(station.tokens(), 0, "{reply:?}");
        assert!(
            !Path::new(&station.ctx.settings.config_path).exists(),
            "{reply:?}"
        );
    }
    Ok(())
}

#[test]
fn a_secret_only_ever_travels_on_the_standard_output_of_the_dialog() -> TestResult {
    let station = gmail_station(None, Visit::Good("c"), false, Orchard::empty())?;
    let script = scripted(&gmail_only());
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    let all: String = script
        .seen()
        .iter()
        .map(|seen| seen.payload.clone())
        .collect();
    assert!(!all.contains(SECRET));
    assert!(!all.contains("GOCSPX"));
    assert!(leaked(&station, &out).is_empty());
    let exchange = station
        .transport
        .seen()
        .into_iter()
        .find(|request| request.url.contains("oauth2.googleapis.com/token"))
        .ok_or("token request")?;
    assert!(exchange
        .body
        .as_deref()
        .unwrap_or_default()
        .contains("client_secret="));
    assert!(!exchange.url.contains(SECRET));
    let stored = station
        .vault
        .get(&gm("google:alice@{gmail}"))
        .ok_or("token")?;
    assert!(stored.contains(SECRET));
    for line in fs::read_dir(dirname(&station.ctx.settings.config_path))? {
        let path = line?.path();
        if path.is_file() {
            assert!(!fs::read_to_string(&path)?.contains(SECRET), "{path:?}");
        }
    }
    Ok(())
}

#[test]
fn no_other_command_ever_shows_a_dialog_or_calls_the_screen() -> TestResult {
    let station = gmail_station(
        Some(&gm(
            r#"{"version": 3, "accounts": [{"provider": "google", "address": "alice@{gmail}", "mail": "write",
                "calendars": [{"id": "alice@{gmail}", "name": "alice@{gmail}", "level": "read"}]}]}"#,
        )),
        Visit::Nothing,
        false,
        Orchard::empty(),
    )?;
    let script = Script::new();
    for (args, input) in [
        (vec!["accounts"], ""),
        (vec!["calendars"], ""),
        (vec!["status"], ""),
        (vec!["setup"], "1\n0\nj\n"),
        (vec!["setup", "apple"], ""),
        (vec!["link", "google"], "id\nsecret\n1\n"),
        (vec!["link", "microsoft"], "id\n1\n"),
        (vec!["unlink", "alice@gmail.example"], "n\n"),
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
    let station = gmail_station(None, Visit::Nothing, false, Orchard::empty())?;
    let script = Script::new();
    let screen = Screen {
        host: &script,
        gui: &|| true,
    };
    let calls = ["onboard", "link", "setup", "ask_text", "choose_list", "notify", "open_url", "ask_buttons", "dismiss", "link_account"]
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
    for name in ["onboard", "link", "setup", "dismiss", "dialog", "ask_"] {
        assert!(!listed.contains(name), "{name}: {listed}");
    }
    let answers: Vec<&str> = text.lines().skip(2).collect();
    assert_eq!(answers.len(), 10, "{text}");
    for answer in answers {
        assert!(
            answer.contains("\"isError\": true") || answer.contains("\"error\""),
            "{answer}"
        );
    }
    assert!(script.seen().is_empty());
    assert_eq!(station.tokens(), 0);
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

fn gmail_station_with_list(status: u16, list: &str) -> Result<Station, Box<dyn Error>> {
    let transport = google_transport(&google_tokens(false))
        .json(
            Method::Post,
            "oauth2.googleapis.com/token",
            200,
            &google_tokens(false),
        )
        .json(Method::Get, "/users/me/calendarList", status, list);
    station(None, transport, Visit::Good("c"), Orchard::empty())
}

#[test]
fn an_unreadable_calendar_list_is_reported_and_the_account_stays_linked() -> TestResult {
    let station = gmail_station_with_list(403, r#"{"error": {"message": "API not enabled"}}"#)?;
    let script = scripted(&[
        welcome(),
        accounts(&[0]),
        link_google(false),
        vec![("ask_buttons", Reply::Button(0))],
        summary_later(),
    ]);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    let notice = script.seen()[8].text("text");
    assert!(notice.contains("nicht lesbar"), "{notice}");
    assert!(
        notice.contains("deren Freigaben bleiben unveraendert"),
        "{notice}"
    );
    let rights = station.rights()?;
    assert_eq!(rights.accounts.len(), 1);
    assert!(rights.calendars_at(Level::Read).is_empty());
    assert_eq!(station.tokens(), 1);
    Ok(())
}

#[test]
fn an_account_without_calendars_gets_a_notice_instead_of_an_empty_list() -> TestResult {
    let station =
        gmail_station_with_list(200, r#"{"kind": "calendar#calendarList", "items": []}"#)?;
    let script = scripted(&[
        welcome(),
        accounts(&[0]),
        link_google(false),
        vec![("ask_buttons", Reply::Button(0))],
        summary_later(),
    ]);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    assert!(script.seen()[8]
        .text("text")
        .contains("Dieses Konto hat keine Kalender."));
    assert!(
        script
            .ops()
            .iter()
            .filter(|op| *op == "choose_list")
            .count()
            == 1
    );
    Ok(())
}

#[test]
fn hostile_calendar_names_are_cleaned_and_bounded_before_they_are_shown() -> TestResult {
    let nasty = format!("Fa\u{202e}mi\u{200b}lie\n{}", "x".repeat(500));
    let list = format!(
        r#"{{"kind": "calendar#calendarList", "items": [{{"id": "familie@group.example", "summary": {}, "accessRole": "owner"}}]}}"#,
        json::dumps(&Json::from(nasty.as_str()), true)
    );
    let station = gmail_station_with_list(200, &list)?;
    let script = scripted(&[
        welcome(),
        accounts(&[0]),
        link_google(false),
        read_calendars(&[0]),
        summary_later(),
    ]);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    let items = script.seen()[8].field("items").ok_or("items")?;
    let item = items
        .as_array()
        .and_then(|list| list.first())
        .and_then(Json::as_str)
        .ok_or("item")?;
    assert!(
        !item.contains('\u{202e}') && !item.contains('\u{200b}') && !item.contains('\n'),
        "{item}"
    );
    assert!(item.chars().count() <= 180, "{}", item.chars().count());
    assert!(item.starts_with("1. Familie"), "{item}");
    let rights = station.rights()?;
    assert_eq!(rights.calendars_at(Level::Read).len(), 1);
    Ok(())
}

#[test]
fn a_new_account_at_the_limit_is_refused_after_the_sign_in_and_its_token_removed() -> TestResult {
    let accounts_json: Vec<String> = (0..16)
        .map(|number| {
            format!(r#"{{"provider": "microsoft", "address": "user{number}@outlook.example"}}"#)
        })
        .collect();
    let config = format!(
        r#"{{"version": 3, "accounts": [{}]}}"#,
        accounts_json.join(", ")
    );
    let transport = google_transport(&google_tokens(false));
    let station = station(Some(&config), transport, Visit::Good("c"), Orchard::empty())?;
    let script = scripted(&[
        welcome(),
        accounts(&[0]),
        link_google(false)[..5].to_vec(),
        vec![
            ("ask_buttons", Reply::Cancel),
            ("ask_buttons", Reply::Button(0)),
        ],
    ]);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    let seen = script.seen();
    assert!(
        seen[2]
            .text("text")
            .starts_with("Hinweis: Es sind schon 16 Konten verbunden."),
        "{}",
        seen[2].text("text")
    );
    assert!(
        seen[7]
            .text("text")
            .contains("Es sind schon 16 Konten verbunden."),
        "{}",
        seen[7].text("text")
    );
    assert_eq!(station.tokens(), 0);
    assert_eq!(station.config(), config);
    Ok(())
}

#[test]
fn an_apple_step_that_fails_is_not_repeated_and_leaves_the_rest_alone() -> TestResult {
    let station = gmail_station(None, Visit::Good("c"), false, Orchard::broken())?;
    let script = scripted(&[
        welcome(),
        accounts(&[0, 2]),
        link_google(false),
        read_calendars(&[0]),
        vec![
            ("ask_buttons", Reply::Button(1)),
            ("ask_buttons", Reply::Button(1)),
        ],
        summary_later(),
    ]);
    let (code, out) = onboard(&station, &script);
    assert_eq!(code, 0, "{out}");
    assert_clean(&script);
    let failure = script.seen()[11].text("text");
    assert!(failure.contains("Apple Mail ist nicht lesbar"), "{failure}");
    assert!(script.seen()[12]
        .text("text")
        .starts_with("Das ist eingerichtet:"));
    assert_eq!(station.apple_calls(), 1);
    let rights = station.rights()?;
    assert_eq!(rights.accounts.len(), 1);
    assert!(rights.apple.mail.is_empty() && rights.apple.calendars.is_empty());
    Ok(())
}
