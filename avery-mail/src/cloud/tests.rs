use super::auth::Record;
use super::*;
use crate::context::Context;
use crate::http::scripted::{Scripted, Shared};
use crate::osascript::clicks::Clicks;
use crate::testutil::{args, settings_at, TempDir, TestResult};
use crate::vault::memory::{Memory, Shared as SharedVault};
use std::error::Error;
use std::fs;
use std::sync::Arc;

const BASE: &str = "https://gmail.googleapis.com/gmail/v1/users/me";

fn api(transport: &Scripted) -> Api<'_> {
    Api::new(transport, "TOKEN".to_string(), Source::Google)
}

#[test]
fn requests_carry_the_token_headers_and_json_bodies() -> TestResult {
    let transport = Scripted::new()
        .json(
            Method::Get,
            "/profile",
            200,
            r#"{"emailAddress": "a@gmail.example"}"#,
        )
        .json(Method::Post, "/drafts", 200, r#"{"id": "d1"}"#);
    let handle = api(&transport).with_header("Prefer", "x=\"y\"");
    let profile = handle.get(&format!("{BASE}/profile"))?;
    assert_eq!(
        profile
            .as_object()
            .and_then(|o| o.get("emailAddress"))
            .and_then(Json::as_str),
        Some("a@gmail.example")
    );
    let body = json::parse(r#"{"message": {"raw": "abc"}}"#)?;
    handle.send(Method::Post, &format!("{BASE}/drafts"), Some(&body))?;
    let seen = transport.seen();
    assert_eq!(seen.len(), 2);
    let header = |request: &Request, name: &str| {
        request
            .headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };
    assert_eq!(
        header(&seen[0], "Authorization").as_deref(),
        Some("Bearer TOKEN")
    );
    assert_eq!(header(&seen[0], "Prefer").as_deref(), Some("x=\"y\""));
    assert_eq!(header(&seen[0], "Content-Type"), None);
    assert_eq!(seen[0].body, None);
    assert_eq!(
        header(&seen[1], "Content-Type").as_deref(),
        Some("application/json")
    );
    assert_eq!(
        seen[1].body.as_deref(),
        Some(r#"{"message": {"raw": "abc"}}"#)
    );
    assert_eq!(seen[0].timeout, READ_TIMEOUT);
    assert_eq!(seen[1].timeout, WRITE_TIMEOUT);
    Ok(())
}

#[test]
fn statuses_become_errors_that_tell_the_model_what_to_do() -> TestResult {
    let cases: [(u16, &str, &str); 7] = [
        (401, "{}", "avery mail link google"),
        (
            403,
            r#"{"error": {"message": "Gmail API has not been used\nin project"}}"#,
            "403): Gmail API has not been used in project",
        ),
        (404, "", "Not found"),
        (429, "{}", "429"),
        (503, "<html>", "temporary problem (503)"),
        (
            400,
            r#"{"error": "invalid_request", "error_description": "Bad  thing"}"#,
            "(400): Bad thing",
        ),
        (418, "[]", "(418)."),
    ];
    for (status, body, wanted) in cases {
        let transport = Scripted::new().json(Method::Get, "/x", status, body);
        let Err(error) = api(&transport).get(&format!("{BASE}/x")) else {
            return Err("expected an error".into());
        };
        assert!(error.message().contains(wanted), "{status}: {error}");
        assert!(matches!(error, ToolError::Msg(_)), "{status}");
    }
    Ok(())
}

#[test]
fn a_missing_item_can_be_asked_for_and_writes_are_uncertain_when_unsure() -> TestResult {
    let transport = Scripted::new()
        .json(Method::Get, "/gone", 404, "{}")
        .json(Method::Get, "/here", 200, "{\"a\": 1}")
        .json(Method::Get, "/bad", 500, "{}")
        .json(Method::Post, "/send500", 502, "{}")
        .json(Method::Post, "/send400", 400, "{}")
        .json(Method::Post, "/sendjunk", 200, "junk")
        .json(Method::Delete, "/empty", 204, "")
        .reply(Method::Post, "/sendtimeout", vec![Err(HttpError::TimedOut)])
        .reply(
            Method::Post,
            "/sendbroken",
            vec![Err(HttpError::Failed("curl exit 52".into()))],
        )
        .reply(Method::Get, "/readtimeout", vec![Err(HttpError::TimedOut)])
        .reply(
            Method::Get,
            "/readbroken",
            vec![Err(HttpError::Failed("curl exit 6: no host".into()))],
        )
        .reply(Method::Get, "/big", vec![Err(HttpError::TooLarge)]);
    let handle = api(&transport);
    assert_eq!(handle.get_optional(&format!("{BASE}/gone"))?, None);
    assert!(handle.get_optional(&format!("{BASE}/here"))?.is_some());
    assert!(handle.get_optional(&format!("{BASE}/bad")).is_err());
    let body = Json::object(vec![("a", Json::Int(1))]);
    for name in ["send500", "sendtimeout", "sendbroken", "sendjunk"] {
        let result = handle.send(Method::Post, &format!("{BASE}/{name}"), Some(&body));
        assert!(
            matches!(result, Err(ToolError::Uncertain(_))),
            "{name}: {result:?}"
        );
    }
    let refused = handle.send(Method::Post, &format!("{BASE}/send400"), Some(&body));
    assert!(matches!(refused, Err(ToolError::Msg(_))), "{refused:?}");
    assert_eq!(
        handle.send(Method::Delete, &format!("{BASE}/empty"), None)?,
        Json::Null
    );
    for (name, wanted) in [
        ("readtimeout", "did not answer in time"),
        ("readbroken", "could not be reached (curl exit 6: no host)"),
        ("big", "too large"),
    ] {
        let Err(error) = handle.get(&format!("{BASE}/{name}")) else {
            return Err("expected an error".into());
        };
        assert!(error.message().contains(wanted), "{error}");
        assert!(matches!(error, ToolError::Msg(_)));
    }
    let junk = Scripted::new().json(Method::Get, "/junk", 200, "<html>");
    assert!(api(&junk).get(&format!("{BASE}/junk")).is_err());
    Ok(())
}

#[test]
fn many_urls_are_fetched_in_parallel_and_stay_in_order() -> TestResult {
    let mut transport = Scripted::new();
    let urls: Vec<String> = (0..30)
        .map(|number| format!("{BASE}/messages/m{number:03}"))
        .collect();
    for number in 0..30 {
        transport = transport.json(
            Method::Get,
            &format!("/messages/m{number:03}"),
            200,
            &format!("{{\"n\": {number}}}"),
        );
    }
    let results = api(&transport).get_many(&urls);
    assert_eq!(results.len(), 30);
    for (number, result) in results.iter().enumerate() {
        let value = result
            .as_ref()
            .ok()
            .and_then(Json::as_object)
            .and_then(|o| o.get("n"));
        assert_eq!(value, Some(&Json::Int(i64::try_from(number)?)), "{number}");
    }
    assert_eq!(transport.seen().len(), 30);
    let many: Vec<String> = (0..MAX_PARALLEL_REQUESTS + 20)
        .map(|n| format!("{BASE}/messages/m{:03}", n % 30))
        .collect();
    assert_eq!(api(&transport).get_many(&many).len(), MAX_PARALLEL_REQUESTS);
    assert!(api(&transport).get_many(&[]).is_empty());
    Ok(())
}
struct Never;

impl Host for Never {
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

fn account(id: &str) -> Account {
    crate::account::with_labels(vec![MailAccount {
        id: id.to_string(),
        name: id.to_string(),
        kind: String::new(),
        addresses: Vec::new(),
    }])
    .remove(0)
}

fn mail_with(vault: crate::vault::memory::Memory) -> CloudMail {
    CloudMail::new(Box::new(Scripted::new()), Box::new(vault), Box::new(Never))
}

fn query() -> MessageQuery {
    MessageQuery {
        mailbox: None,
        since_days: 3,
        unread_only: false,
        query: None,
        field: "both".to_string(),
        limit: 5,
    }
}

#[test]
fn cloud_mail_needs_a_cloud_account_and_a_stored_sign_in() -> TestResult {
    let mail = mail_with(crate::vault::memory::Memory::new());
    let apple = account("5F3E-4C");
    let nonsense = account("google:nonsense");
    let unlinked = account("google:alice@gmail.example");
    for subject in [&apple, &nonsense] {
        let Err(error) = mail.mailboxes(subject) else {
            return Err("expected an error".into());
        };
        assert!(
            error
                .message()
                .contains("not a linked Gmail or Outlook.com account"),
            "{error}"
        );
    }
    for result in [
        mail.mailboxes(&unlinked).map(|_| ()),
        mail.messages(&unlinked, &query()).map(|_| ()),
        mail.account_info(&unlinked).map(|_| ()),
    ] {
        assert!(
            matches!(&result, Err(ToolError::Msg(text)) if text.contains("not linked") && text.contains("avery mail link google")),
            "{result:?}"
        );
    }
    assert_eq!(mail.all_accounts(), Ok(Vec::new()));
    Ok(())
}

#[test]
fn the_mail_router_sends_apple_accounts_to_apple_and_the_rest_to_the_cloud() -> TestResult {
    let router = MailRouter::new(
        Box::new(crate::fake::FakeBackend::new(
            crate::fake::FakeOptions::default(),
        )),
        mail_with(crate::vault::memory::Memory::new()),
    );
    let demo = account("FAKE-GMAIL");
    assert!(router.mailboxes(&demo).is_ok());
    assert!(!router.all_accounts()?.is_empty());
    let cloud = account("microsoft:bob@outlook.example");
    assert!(matches!(
        router.mailboxes(&cloud),
        Err(ToolError::Msg(text)) if text.contains("not linked")
    ));
    Ok(())
}

#[test]
fn cloud_calendars_need_a_stored_sign_in_and_apple_ones_stay_with_apple() -> TestResult {
    let calendar = CloudCalendar::new(
        Box::new(Scripted::new()),
        Box::new(crate::vault::memory::Memory::new()),
        Box::new(Never),
    );
    let router = CalendarRouter::new(
        Box::new(crate::calendar::FakeCalendar::new(
            crate::fake::FakeOptions::default(),
        )),
        calendar,
    );
    assert!(!router.calendars(Source::Apple, "")?.is_empty());
    assert!(matches!(
        router.calendars(Source::Google, "alice@gmail.example"),
        Err(ToolError::Msg(text)) if text.contains("not linked")
    ));
    let window = Window {
        from: crate::civil::DateTime::parse("2030-05-06 00:00").ok_or("from")?,
        to: crate::civil::DateTime::parse("2030-05-07 00:00").ok_or("to")?,
    };
    let cloud = Calendar {
        id: "google:alice@gmail.example:alice@gmail.example".to_string(),
        name: "alice@gmail.example".to_string(),
        label: "alice@gmail.example".to_string(),
    };
    assert!(matches!(
        router.events(&cloud, window),
        Err(ToolError::Msg(text)) if text.contains("not linked") || text.contains("time zone")
    ));
    let broken = Calendar {
        id: "google:nonsense".to_string(),
        ..cloud
    };
    assert!(matches!(
        router.event(&broken, "x"),
        Err(ToolError::Msg(text)) if text.contains("not a linked")
    ));
    Ok(())
}

const GOOGLE_READ: &str = r#"{"version": 3,
    "mail": {"accounts": [{"id": "google:alice@gmail.example", "name": "alice@gmail.example", "type": "gmail", "addresses": ["alice@gmail.example"], "level": "read"}]},
    "cloud": {"accounts": [{"source": "google", "address": "alice@gmail.example"}]}}"#;
const GOOGLE_WRITE: &str = r#"{"version": 3,
    "mail": {"accounts": [{"id": "google:alice@gmail.example", "name": "alice@gmail.example", "type": "gmail", "addresses": ["alice@gmail.example"], "level": "write"}]},
    "calendar": {"calendars": [{"id": "google:alice@gmail.example:alice@gmail.example", "name": "alice@gmail.example", "level": "write"}]},
    "cloud": {"accounts": [{"source": "google", "address": "alice@gmail.example"}]}}"#;
const MICROSOFT_WRITE: &str = r#"{"version": 3,
    "mail": {"accounts": [{"id": "microsoft:bob@outlook.example", "name": "bob@outlook.example", "type": "outlook", "addresses": ["bob@outlook.example"], "level": "write"}]},
    "calendar": {"calendars": [{"id": "microsoft:bob@outlook.example:AQMkCAL", "name": "Calendar (bob@outlook.example)", "level": "read"}]},
    "cloud": {"accounts": [{"source": "microsoft", "address": "bob@outlook.example"}]}}"#;
const GOOGLE_SCOPES: &str = "https://www.googleapis.com/auth/gmail.readonly https://www.googleapis.com/auth/calendar.readonly https://www.googleapis.com/auth/gmail.compose https://www.googleapis.com/auth/calendar.events";
const MICROSOFT_SCOPES: &str =
    "Mail.Read Calendars.Read Mail.ReadWrite Mail.Send Calendars.ReadWrite";
const TOKEN: &str =
    r#"{"access_token": "ya29.test-token", "expires_in": 3599, "token_type": "Bearer"}"#;

struct Wired {
    ctx: Context,
    transport: Arc<Scripted>,
    clicks: Clicks,
    _dir: TempDir,
}

fn wired(
    config: &str,
    records: &[(&str, &str)],
    transport: Scripted,
    click: &'static str,
) -> Result<Wired, Box<dyn Error>> {
    let dir = TempDir::new()?;
    let settings = settings_at(&dir.path("config.json"));
    fs::write(&settings.config_path, config)?;
    let mut memory = Memory::new();
    for (name, scope) in records {
        let record = Record {
            client_id: "client".to_string(),
            client_secret: None,
            refresh_token: "refresh".to_string(),
            scope: scope.to_string(),
        };
        memory = memory.with(name, &record.to_json());
    }
    let memory = Arc::new(memory);
    let transport = Arc::new(transport);
    let clicks = Clicks::new(click);
    let mail = CloudMail::new(
        Box::new(Shared(Arc::clone(&transport))),
        Box::new(SharedVault(Arc::clone(&memory))),
        Box::new(clicks.clone()),
    );
    let calendar = CloudCalendar::new(
        Box::new(Shared(Arc::clone(&transport))),
        Box::new(SharedVault(Arc::clone(&memory))),
        Box::new(clicks.clone()),
    );
    let options = crate::fake::FakeOptions::default();
    let ctx = Context::new(
        settings,
        Box::new(MailRouter::new(
            Box::new(crate::fake::FakeBackend::new(options.clone())),
            mail,
        )),
        Box::new(CalendarRouter::new(
            Box::new(crate::calendar::FakeCalendar::new(options)),
            calendar,
        )),
        CloudAdmin::inert(),
    );
    Ok(Wired {
        ctx,
        transport,
        clicks,
        _dir: dir,
    })
}

fn header(request: &Request, name: &str) -> Option<String> {
    request
        .headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
}

fn calls(wired: &Wired, method: Method, needle: &str) -> Vec<Request> {
    wired
        .transport
        .seen()
        .into_iter()
        .filter(|request| request.method == method && request.url.contains(needle))
        .collect()
}

fn api_calls(wired: &Wired) -> Vec<Request> {
    wired
        .transport
        .seen()
        .into_iter()
        .filter(|request| !request.url.contains("/token"))
        .collect()
}

fn outcomes(wired: &Wired) -> Result<Vec<String>, Box<dyn Error>> {
    let view = crate::sendlog::read_entries(&wired.ctx.settings, 20)?;
    Ok(view
        .entries
        .iter()
        .map(|entry| format!("{}:{}", entry.outcome.as_str(), entry.by.as_str()))
        .collect())
}

fn gmail_metadata(id: &str, subject: &str) -> String {
    format!(
        r#"{{"id": "{id}", "threadId": "t-{id}", "labelIds": ["INBOX", "UNREAD"], "internalDate": "1700000000999", "payload": {{"mimeType": "text/plain", "headers": [{{"name": "From", "value": "Anna <anna@example.org>"}}, {{"name": "Subject", "value": "{subject}"}}, {{"name": "Date", "value": "Tue, 14 Nov 2023 22:13:20 +0000"}}]}}}}"#
    )
}

fn gmail_full(id: &str, text: &str) -> String {
    format!(
        r#"{{"id": "{id}", "threadId": "t-{id}", "labelIds": ["INBOX", "UNREAD"], "internalDate": "1700000000999", "payload": {{"mimeType": "text/plain", "headers": [{{"name": "From", "value": "Anna <anna@example.org>"}}, {{"name": "To", "value": "alice@gmail.example"}}, {{"name": "Subject", "value": "Hallo"}}, {{"name": "Date", "value": "Tue, 14 Nov 2023 22:13:20 +0000"}}], "body": {{"size": {}, "data": "{}"}}}}}}"#,
        text.len(),
        crate::base64::encode_url(text.as_bytes())
    )
}

#[test]
fn a_linked_gmail_account_is_read_through_the_tools() -> TestResult {
    let transport = Scripted::new()
        .json(Method::Post, "/token", 200, TOKEN)
        .json(
            Method::Get,
            "/profile",
            200,
            r#"{"emailAddress": "alice@gmail.example"}"#,
        )
        .json(
            Method::Get,
            "/messages?maxResults",
            200,
            r#"{"messages": [{"id": "m1", "threadId": "t-m1"}], "resultSizeEstimate": 1}"#,
        )
        .json(
            Method::Get,
            "/messages/m1?format=metadata",
            200,
            &gmail_metadata("m1", "Hallo Alice"),
        )
        .json(
            Method::Get,
            "/messages/m1?format=full",
            200,
            &gmail_full("m1", "Zeile eins\nZeile zwei"),
        );
    let wired = wired(
        GOOGLE_READ,
        &[(
            "google:alice@gmail.example",
            "https://www.googleapis.com/auth/gmail.readonly",
        )],
        transport,
        "no",
    )?;
    let accounts = crate::read_tools::list_accounts(&wired.ctx, &args("{}")?)?;
    assert_eq!(
        accounts,
        "- alice@gmail.example [gmail]: alice@gmail.example"
    );
    let listed = crate::read_tools::list_messages(&wired.ctx, &args(r#"{"limit": 5}"#)?)?;
    assert!(
        listed.contains("[alice@gmail.example/INBOX] id=m1 | "),
        "{listed}"
    );
    assert!(
        listed.contains("| UNREAD | From: Anna <anna@example.org> | Subject: Hallo Alice"),
        "{listed}"
    );
    assert!(
        listed.contains("1 matches") || listed.contains("matches"),
        "{listed}"
    );
    let read = crate::read_tools::read_message(
        &wired.ctx,
        &args(r#"{"id": "m1", "account": "alice@gmail.example"}"#)?,
    )?;
    assert!(read.contains("From: Anna <anna@example.org>"), "{read}");
    assert!(read.contains("Subject: Hallo"), "{read}");
    assert!(read.contains("Zeile eins\nZeile zwei"), "{read}");
    assert!(
        read.starts_with("NOTE: The text below comes from third parties"),
        "{read}"
    );
    for text in [&accounts, &listed, &read] {
        assert!(
            !text.contains("ya29") && !text.contains("refresh"),
            "{text}"
        );
    }
    let seen = api_calls(&wired);
    assert!(!seen.is_empty());
    for request in &seen {
        assert!(
            request
                .url
                .starts_with("https://gmail.googleapis.com/gmail/v1/users/me/"),
            "{}",
            request.url
        );
        assert_eq!(
            header(request, "Authorization").as_deref(),
            Some("Bearer ya29.test-token")
        );
        assert_eq!(request.method, Method::Get);
    }
    assert_eq!(calls(&wired, Method::Post, "/token").len(), 3);
    assert!(wired.clicks.dialogs().is_empty());
    let refused = crate::write_tools::send_message(
        &wired.ctx,
        &args(r#"{"to": ["anna@example.org"], "subject": "x", "body": "y"}"#)?,
    );
    assert!(
        matches!(&refused, Err(ToolError::Msg(text)) if text.contains("not enabled")),
        "{refused:?}"
    );
    assert!(calls(&wired, Method::Post, "/messages/send").is_empty());
    Ok(())
}

fn raw_of(request: &Request) -> Result<String, Box<dyn Error>> {
    let body = request.body.as_deref().ok_or("body")?;
    let parsed = json::parse(body)?;
    let raw = parsed
        .as_object()
        .and_then(|object| object.get("raw"))
        .and_then(Json::as_str)
        .ok_or("raw")?;
    Ok(String::from_utf8(
        crate::base64::decode(raw).ok_or("base64")?,
    )?)
}

#[test]
fn a_gmail_send_needs_the_click_goes_out_once_and_is_logged() -> TestResult {
    let script = || {
        Scripted::new()
            .json(Method::Post, "/token", 200, TOKEN)
            .json(
                Method::Get,
                "/profile",
                200,
                r#"{"emailAddress": "alice@gmail.example"}"#,
            )
            .json(
                Method::Post,
                "/messages/send",
                200,
                r#"{"id": "m-9", "threadId": "t-9", "labelIds": ["SENT"]}"#,
            )
    };
    let message = r#"{"to": ["anna@example.org"], "subject": "Hallo", "body": "Kurzer Text."}"#;
    let approved = wired(
        GOOGLE_WRITE,
        &[("google:alice@gmail.example", GOOGLE_SCOPES)],
        script(),
        "yes",
    )?;
    let answer = crate::write_tools::send_message(&approved.ctx, &args(message)?)?;
    assert!(answer.starts_with("Sent: the user approved"), "{answer}");
    let sends = calls(&approved, Method::Post, "/messages/send");
    assert_eq!(sends.len(), 1);
    let mime = raw_of(&sends[0])?;
    assert!(mime.contains("To: anna@example.org"), "{mime}");
    assert!(mime.contains("Subject: Hallo"), "{mime}");
    assert!(
        mime.contains("Content-Type: text/plain; charset=UTF-8"),
        "{mime}"
    );
    assert!(!mime.contains("Bcc"), "{mime}");
    let dialogs = approved.clicks.dialogs();
    assert_eq!(dialogs.len(), 1);
    assert!(
        dialogs[0].contains("\"op\": \"approve_send\""),
        "{}",
        dialogs[0]
    );
    assert!(dialogs[0].contains("Kurzer Text."), "{}", dialogs[0]);
    assert!(dialogs[0].contains("anna@example.org"), "{}", dialogs[0]);
    assert_eq!(outcomes(&approved)?, ["pending:model", "sent:model"]);
    let logged = fs::read_to_string(approved.ctx.settings.sendlog_path())?;
    assert!(!logged.contains("Kurzer Text."), "{logged}");

    let declined = wired(
        GOOGLE_WRITE,
        &[("google:alice@gmail.example", GOOGLE_SCOPES)],
        script(),
        "no",
    )?;
    let answer = crate::write_tools::send_message(&declined.ctx, &args(message)?)?;
    assert!(answer.contains("did NOT approve"), "{answer}");
    assert!(calls(&declined, Method::Post, "/messages/send").is_empty());
    assert_eq!(outcomes(&declined)?, ["pending:model", "declined:model"]);

    let silent = wired(
        GOOGLE_WRITE,
        &[("google:alice@gmail.example", GOOGLE_SCOPES)],
        script(),
        "timeout",
    )?;
    let answer = crate::write_tools::send_message(&silent.ctx, &args(message)?)?;
    assert!(answer.contains("did not answer"), "{answer}");
    assert!(calls(&silent, Method::Post, "/messages/send").is_empty());
    assert_eq!(outcomes(&silent)?, ["pending:model", "timeout:model"]);
    Ok(())
}

#[test]
fn a_failing_gmail_send_call_is_unclear_and_counts_against_the_limit() -> TestResult {
    let transport = Scripted::new()
        .json(Method::Post, "/token", 200, TOKEN)
        .json(
            Method::Get,
            "/profile",
            200,
            r#"{"emailAddress": "alice@gmail.example"}"#,
        )
        .json(Method::Post, "/messages/send", 503, "{}");
    let config = GOOGLE_WRITE.replacen(
        r#""level": "write"}]},"#,
        r#""level": "write"}], "sends_per_hour": 1},"#,
        1,
    );
    assert_ne!(config, GOOGLE_WRITE);
    let wired = wired(
        &config,
        &[("google:alice@gmail.example", GOOGLE_SCOPES)],
        transport,
        "yes",
    )?;
    let message = r#"{"to": ["anna@example.org"], "subject": "Hallo", "body": "Kurzer Text."}"#;
    let first = crate::write_tools::send_message(&wired.ctx, &args(message)?);
    assert!(
        matches!(&first, Err(ToolError::Uncertain(text)) if text.contains("may or may not")),
        "{first:?}"
    );
    assert_eq!(outcomes(&wired)?, ["pending:model"]);
    assert_eq!(calls(&wired, Method::Post, "/messages/send").len(), 1);
    let second = crate::write_tools::send_message(&wired.ctx, &args(message)?);
    assert!(
        matches!(&second, Err(ToolError::Msg(text)) if text.starts_with("Send limit reached (1 per hour)")),
        "{second:?}"
    );
    assert_eq!(calls(&wired, Method::Post, "/messages/send").len(), 1);
    assert_eq!(wired.clicks.dialogs().len(), 1);
    Ok(())
}

const DRAFT: &str = "AAMkDRAFT1=";

fn draft_json(to: &str, body: &str) -> String {
    format!(
        r#"{{"id": "{DRAFT}", "subject": "Hallo", "hasAttachments": false, "body": {{"contentType": "text", "content": "{body}"}}, "toRecipients": [{{"emailAddress": {{"address": "{to}"}}}}], "ccRecipients": [], "bccRecipients": []}}"#
    )
}

fn graph_script() -> Scripted {
    Scripted::new()
        .json(Method::Post, "/token", 200, TOKEN)
        .json(Method::Post, "/send", 202, "")
        .json(Method::Delete, "/me/messages/AAMkDRAFT1%3D", 204, "")
        .json(
            Method::Get,
            "/me/messages/AAMkDRAFT1%3D",
            200,
            &draft_json("anna@example.org", r"Kurzer Text.\r\n"),
        )
        .json(
            Method::Post,
            "/me/messages",
            201,
            &format!(r#"{{"id": "{DRAFT}", "isDraft": true}}"#),
        )
}

#[test]
fn an_outlook_send_goes_through_a_draft_that_is_removed_unless_the_user_clicks() -> TestResult {
    let message = r#"{"to": ["anna@example.org"], "subject": "Hallo", "body": "Kurzer Text."}"#;
    let approved = wired(
        MICROSOFT_WRITE,
        &[("microsoft:bob@outlook.example", MICROSOFT_SCOPES)],
        graph_script(),
        "yes",
    )?;
    let answer = crate::write_tools::send_message(&approved.ctx, &args(message)?)?;
    assert!(answer.starts_with("Sent: the user approved"), "{answer}");
    assert_eq!(calls(&approved, Method::Post, "/send").len(), 1);
    assert!(calls(&approved, Method::Delete, "/me/messages").is_empty());
    for request in api_calls(&approved) {
        assert!(
            request
                .url
                .starts_with("https://graph.microsoft.com/v1.0/me/"),
            "{}",
            request.url
        );
        assert_eq!(
            header(&request, "Authorization").as_deref(),
            Some("Bearer ya29.test-token")
        );
    }
    let dialogs = approved.clicks.dialogs();
    assert_eq!(dialogs.len(), 1);
    assert!(dialogs[0].contains("Kurzer Text."), "{}", dialogs[0]);
    assert_eq!(outcomes(&approved)?, ["pending:model", "sent:model"]);

    let declined = wired(
        MICROSOFT_WRITE,
        &[("microsoft:bob@outlook.example", MICROSOFT_SCOPES)],
        graph_script(),
        "no",
    )?;
    let answer = crate::write_tools::send_message(&declined.ctx, &args(message)?)?;
    assert!(answer.contains("did NOT approve"), "{answer}");
    assert!(calls(&declined, Method::Post, "/send").is_empty());
    assert_eq!(calls(&declined, Method::Delete, "/me/messages").len(), 1);
    assert_eq!(outcomes(&declined)?, ["pending:model", "declined:model"]);

    let draft = crate::write_tools::create_draft(&approved.ctx, &args(message)?)?;
    assert!(draft.starts_with("Draft saved"), "{draft}");
    Ok(())
}

fn google_event(summary: &str) -> String {
    format!(
        r#"{{"id": "ev-1", "status": "confirmed", "summary": "{summary}", "start": {{"dateTime": "2030-05-07T18:30:00+02:00", "timeZone": "Europe/Zurich"}}, "end": {{"dateTime": "2030-05-07T19:30:00+02:00", "timeZone": "Europe/Zurich"}}}}"#
    )
}

#[test]
fn a_google_calendar_change_needs_the_click_and_is_logged_with_its_origin() -> TestResult {
    let script = || {
        Scripted::new()
            .json(Method::Post, "/token", 200, TOKEN)
            .json(
                Method::Get,
                "/users/me/calendarList/",
                200,
                r#"{"id": "alice@gmail.example", "summary": "alice@gmail.example", "accessRole": "owner"}"#,
            )
            .json(Method::Post, "/events", 200, &google_event("Tennis"))
    };
    let event = r#"{"summary": "Tennis", "start": "2030-05-07 18:30", "duration_minutes": 60}"#;
    let approved = wired(
        GOOGLE_WRITE,
        &[("google:alice@gmail.example", GOOGLE_SCOPES)],
        script(),
        "yes",
    )?;
    let answer = crate::calendar::create_event(&approved.ctx, &args(event)?)?;
    assert!(answer.contains("Tennis"), "{answer}");
    let creates = calls(&approved, Method::Post, "/events");
    assert_eq!(creates.len(), 1);
    assert!(
        creates[0].url.contains("sendUpdates=none"),
        "{}",
        creates[0].url
    );
    let body = creates[0].body.clone().ok_or("body")?;
    assert!(body.contains("\"summary\": \"Tennis\""), "{body}");
    assert!(!body.contains("attendees"), "{body}");
    let dialogs = approved.clicks.dialogs();
    assert_eq!(dialogs.len(), 1);
    assert!(
        dialogs[0].contains("\"op\": \"approve_change\""),
        "{}",
        dialogs[0]
    );
    assert!(dialogs[0].contains("Tennis"), "{}", dialogs[0]);
    let log = crate::changelog::read_entries(&approved.ctx.settings, 20)?;
    let kinds: Vec<String> = log
        .entries
        .iter()
        .map(|entry| {
            format!(
                "{}:{}:{}",
                entry.action.as_str(),
                entry.outcome.as_str(),
                entry.by.as_str()
            )
        })
        .collect();
    assert_eq!(kinds, ["create:pending:model", "create:done:model"]);
    assert_eq!(log.entries[1].uid.as_deref(), Some("ev-1"));
    assert_eq!(
        log.entries[1].calendar_id,
        "google:alice@gmail.example:alice@gmail.example"
    );

    let declined = wired(
        GOOGLE_WRITE,
        &[("google:alice@gmail.example", GOOGLE_SCOPES)],
        script(),
        "no",
    )?;
    let answer = crate::calendar::create_event(&declined.ctx, &args(event)?)?;
    assert!(
        answer.contains("did NOT approve") || answer.contains("declined"),
        "{answer}"
    );
    assert!(calls(&declined, Method::Post, "/events").is_empty());
    Ok(())
}

#[test]
fn an_outlook_calendar_is_listed_through_the_tools_in_local_time() -> TestResult {
    let zone = crate::localtime::zone().ok_or("zone")?;
    let item = format!(
        r#"{{"value": [{{"id": "EV1", "subject": "Zahnarzt", "start": {{"dateTime": "2030-05-06T09:00:00.0000000", "timeZone": "{zone}"}}, "end": {{"dateTime": "2030-05-06T10:00:00.0000000", "timeZone": "{zone}"}}, "isAllDay": false, "location": {{"displayName": "Praxis"}}, "attendees": [], "isCancelled": false}}]}}"#
    );
    let transport = Scripted::new()
        .json(Method::Post, "/token", 200, TOKEN)
        .json(Method::Get, "/calendarView", 200, &item);
    let wired = wired(
        MICROSOFT_WRITE,
        &[("microsoft:bob@outlook.example", MICROSOFT_SCOPES)],
        transport,
        "no",
    )?;
    let listed =
        crate::calendar::list_events(&wired.ctx, &args(r#"{"start": "2030-05-06", "days": 2}"#)?)?;
    assert!(listed.contains("Zahnarzt"), "{listed}");
    assert!(listed.contains("09:00-10:00"), "{listed}");
    assert!(listed.contains("Praxis"), "{listed}");
    let view = calls(&wired, Method::Get, "/calendarView");
    assert_eq!(view.len(), 1);
    assert!(
        header(&view[0], "Prefer").is_some_and(|value| value.contains(&zone)),
        "{:?}",
        header(&view[0], "Prefer")
    );
    let refused = crate::calendar::create_event(
        &wired.ctx,
        &args(r#"{"summary": "x", "start": "2030-05-07 10:00"}"#)?,
    );
    assert!(refused.is_err());
    assert!(calls(&wired, Method::Post, "/events").is_empty());
    Ok(())
}

#[test]
fn an_expired_sign_in_is_explained_to_the_model_without_leaking_anything() -> TestResult {
    let transport = Scripted::new().json(
        Method::Post,
        "/token",
        400,
        r#"{"error": "invalid_grant", "error_description": "Token has been expired or revoked."}"#,
    );
    let wired = wired(
        GOOGLE_READ,
        &[(
            "google:alice@gmail.example",
            "https://www.googleapis.com/auth/gmail.readonly",
        )],
        transport,
        "no",
    )?;
    let error = crate::read_tools::list_messages(&wired.ctx, &args("{}")?);
    let Err(error) = error else {
        return Err("expected an error".into());
    };
    let text = error.message();
    assert!(
        text.contains("expired or was revoked") && text.contains("avery mail link google"),
        "{text}"
    );
    assert!(!text.contains("refresh"), "{text}");
    assert!(api_calls(&wired).is_empty());
    let missing = wired_without_record()?;
    let error = crate::read_tools::list_messages(&missing.ctx, &args("{}")?);
    let Err(error) = error else {
        return Err("expected an error".into());
    };
    assert!(error.message().contains("not linked"), "{error}");
    Ok(())
}

fn wired_without_record() -> Result<Wired, Box<dyn Error>> {
    wired(GOOGLE_READ, &[], Scripted::new(), "no")
}
