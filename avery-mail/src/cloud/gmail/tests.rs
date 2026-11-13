use super::*;
use crate::backend::{Attachment, ReplySpec};
use crate::http::scripted::Scripted;
use crate::http::{HttpError, Request};
use crate::json;
use crate::osascript::{App, Finished};
use crate::testutil::{j, mid, TestResult};
use std::error::Error;
use std::sync::Mutex;
use std::time::Duration;

const BASE: &str = "https://gmail.googleapis.com/gmail/v1/users/me";
const PROFILE: &str = r#"{"emailAddress": "alice@gmail.example", "messagesTotal": 12345, "threadsTotal": 9000, "historyId": "987654"}"#;
const DRAFT_ANSWER: &str =
    r#"{"id": "r-1", "message": {"id": "m-1", "threadId": "t-1", "labelIds": ["DRAFT"]}}"#;
const SENT_ANSWER: &str = r#"{"id": "m-9", "threadId": "t-9", "labelIds": ["SENT"]}"#;

type Failure = Box<dyn Error>;

fn api(transport: &Scripted) -> Api<'_> {
    Api::new(transport, "TOKEN".to_string(), Source::Google)
}

fn account(addresses: &[&str]) -> Account {
    Account {
        id: "google:alice@gmail.example".to_string(),
        name: "Alice".to_string(),
        kind: "gmail".to_string(),
        addresses: addresses
            .iter()
            .map(|address| address.to_string())
            .collect(),
        label: "Alice".to_string(),
    }
}

fn header_of(request: &Request, name: &str) -> Option<String> {
    request
        .headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
}

fn quoted(text: &str) -> String {
    json::dumps(&Json::from(text), true)
}

fn requests(transport: &Scripted, method: Method) -> Vec<Request> {
    transport
        .seen()
        .into_iter()
        .filter(|request| request.method == method)
        .collect()
}

fn urls(transport: &Scripted) -> Vec<String> {
    transport
        .seen()
        .into_iter()
        .map(|request| request.url)
        .collect()
}

fn percent_decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let mut index = 0;
    while let Some(byte) = bytes.get(index) {
        if *byte == b'%' {
            let pair = text.get(index + 1..index + 3).unwrap_or_default();
            out.push(u8::from_str_radix(pair, 16).unwrap_or(b'?'));
            index += 3;
        } else {
            out.push(*byte);
            index += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parameter(url: &str, name: &str) -> Option<String> {
    url.split_once('?')?.1.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then(|| percent_decoded(value))
    })
}

fn is_msg(result: &ToolResult<impl std::fmt::Debug>, needle: &str) -> bool {
    matches!(result, Err(ToolError::Msg(text)) if text.contains(needle))
}

struct Dialog<'a> {
    stdout: String,
    code: i32,
    watch: Option<&'a Scripted>,
    seen: Mutex<Vec<String>>,
    writes_before: Mutex<Vec<usize>>,
}

impl<'a> Dialog<'a> {
    fn deciding(decision: &str) -> Dialog<'a> {
        Dialog {
            stdout: format!("{{\"ok\": true, \"data\": {{\"decision\": \"{decision}\"}}}}\n"),
            code: 0,
            watch: None,
            seen: Mutex::new(Vec::new()),
            writes_before: Mutex::new(Vec::new()),
        }
    }

    fn broken() -> Dialog<'a> {
        Dialog {
            stdout: "{\"ok\": false, \"error\": \"approval_dialog_failed: boom\"}\n".to_string(),
            code: 1,
            ..Dialog::deciding("send")
        }
    }

    fn watching(mut self, transport: &'a Scripted) -> Dialog<'a> {
        self.watch = Some(transport);
        self
    }

    fn shown(&self) -> Vec<String> {
        self.seen
            .lock()
            .map(|seen| seen.clone())
            .unwrap_or_default()
    }

    fn writes_before_the_dialog(&self) -> Vec<usize> {
        self.writes_before
            .lock()
            .map(|seen| seen.clone())
            .unwrap_or_default()
    }
}

impl Host for Dialog<'_> {
    fn ensure_running(&self, _app: App) {}

    fn run(&self, _app: App, payload: &str, _timeout: Duration) -> Finished {
        if let Ok(mut seen) = self.seen.lock() {
            seen.push(payload.to_string());
        }
        if let (Some(transport), Ok(mut writes)) = (self.watch, self.writes_before.lock()) {
            writes.push(requests(transport, Method::Post).len());
        }
        Finished::Done {
            code: Some(self.code),
            stdout: self.stdout.clone(),
            stderr: String::new(),
        }
    }
}

fn label_list() -> String {
    r#"{"labels": [
        {"id": "CHAT", "name": "CHAT", "type": "system"},
        {"id": "SENT", "name": "SENT", "type": "system", "messageListVisibility": "hide"},
        {"id": "INBOX", "name": "INBOX", "type": "system", "messageListVisibility": "show"},
        {"id": "IMPORTANT", "name": "IMPORTANT", "type": "system"},
        {"id": "TRASH", "name": "TRASH", "type": "system"},
        {"id": "DRAFT", "name": "DRAFT", "type": "system"},
        {"id": "SPAM", "name": "SPAM", "type": "system"},
        {"id": "CATEGORY_PERSONAL", "name": "CATEGORY_PERSONAL", "type": "system"},
        {"id": "CATEGORY_SOCIAL", "name": "CATEGORY_SOCIAL", "type": "system"},
        {"id": "STARRED", "name": "STARRED", "type": "system"},
        {"id": "UNREAD", "name": "UNREAD", "type": "system"},
        {"id": "YELLOW_STAR", "name": "YELLOW_STAR", "type": "system"},
        {"id": "Label_3", "name": "Work/Reports", "type": "user"},
        {"id": "Label_1", "name": "receipts", "type": "user"},
        {"id": "Label_2", "name": "\u00c1lvaro", "type": "user"}
    ]}"#
    .to_string()
}

fn counted(id: &str, unread: i64) -> String {
    format!(
        "{{\"id\": \"{id}\", \"name\": \"{id}\", \"messagesTotal\": 100, \"messagesUnread\": {unread}, \"threadsTotal\": 90, \"threadsUnread\": {unread}}}"
    )
}

fn mailbox(path: &str, unread: &str) -> Mailbox {
    Mailbox {
        path: path.to_string(),
        unread: unread.to_string(),
    }
}

#[test]
fn the_profile_gives_the_address_of_the_account() -> TestResult {
    let transport = Scripted::new().json(Method::Get, "/profile", 200, PROFILE);
    let info = account_info(&api(&transport), &account(&[]))?;
    assert_eq!(
        info,
        MailAccount {
            id: "google:alice@gmail.example".to_string(),
            name: "Alice".to_string(),
            kind: "gmail".to_string(),
            addresses: vec!["alice@gmail.example".to_string()],
        }
    );
    let seen = transport.seen();
    let [request] = seen.as_slice() else {
        return Err("one request".into());
    };
    assert_eq!(request.method, Method::Get);
    assert_eq!(request.url, format!("{BASE}/profile"));
    assert_eq!(request.body, None);
    assert_eq!(
        header_of(request, "Authorization").as_deref(),
        Some("Bearer TOKEN")
    );
    Ok(())
}

#[test]
fn the_address_of_the_profile_replaces_whatever_the_configuration_said() -> TestResult {
    let transport = Scripted::new().json(
        Method::Get,
        "/profile",
        200,
        r#"{"emailAddress": " Alice.Work@Gmail.Example "}"#,
    );
    let info = account_info(&api(&transport), &account(&["old@gmail.example"]))?;
    assert_eq!(info.addresses, ["Alice.Work@Gmail.Example"]);
    Ok(())
}

#[test]
fn an_unusable_profile_is_an_error() {
    let long = format!(
        "{{\"emailAddress\": \"{}@gmail.example\"}}",
        "a".repeat(300)
    );
    for body in [
        "{}",
        "[]",
        "null",
        "",
        "\"alice@gmail.example\"",
        r#"{"emailAddress": ""}"#,
        r#"{"emailAddress": "   "}"#,
        r#"{"emailAddress": 5}"#,
        r#"{"emailAddress": ["alice@gmail.example"]}"#,
        r#"{"emailAddress": "not an address"}"#,
        r#"{"emailAddress": "a@b@c.example"}"#,
        r#"{"emailAddress": "a\r\nb@c.example"}"#,
        r#"{"emailAddress": "@c.example"}"#,
        r#"{"emailAddress": "a@"}"#,
        r#"{"emailAddress": "a<b>@c.example"}"#,
        long.as_str(),
    ] {
        let transport = Scripted::new().json(Method::Get, "/profile", 200, body);
        assert_eq!(
            account_info(&api(&transport), &account(&[])),
            Err(unexpected()),
            "{body}"
        );
    }
}

#[test]
fn provider_failures_of_the_profile_tell_the_model_what_to_do() {
    for (status, needle) in [
        (401, "avery mail link google"),
        (403, "403"),
        (429, "429"),
        (500, "temporary problem"),
    ] {
        let transport = Scripted::new().json(Method::Get, "/profile", status, "{}");
        let result = account_info(&api(&transport), &account(&[]));
        assert!(is_msg(&result, needle), "{status}: {result:?}");
    }
    let transport = Scripted::new().reply(Method::Get, "/profile", vec![Err(HttpError::TimedOut)]);
    assert!(is_msg(
        &account_info(&api(&transport), &account(&[])),
        "did not answer in time"
    ));
}

#[test]
fn system_and_user_labels_are_listed_under_friendly_names_with_unread_counts() -> TestResult {
    let transport = Scripted::new()
        .json(Method::Get, "/labels/INBOX", 200, &counted("INBOX", 7))
        .json(Method::Get, "/labels/SENT", 200, &counted("SENT", 0))
        .json(Method::Get, "/labels/DRAFT", 200, &counted("DRAFT", 2))
        .json(Method::Get, "/labels/SPAM", 200, &counted("SPAM", 0))
        .json(Method::Get, "/labels/TRASH", 200, &counted("TRASH", 0))
        .json(Method::Get, "/labels/STARRED", 200, &counted("STARRED", 3))
        .json(
            Method::Get,
            "/labels/IMPORTANT",
            200,
            &counted("IMPORTANT", 11),
        )
        .json(Method::Get, "/labels/Label_1", 200, &counted("Label_1", 5))
        .json(Method::Get, "/labels/Label_2", 404, "{}")
        .json(
            Method::Get,
            "/labels/Label_3",
            200,
            r#"{"id": "Label_3", "name": "Work/Reports", "type": "user"}"#,
        )
        .json(Method::Get, "/labels", 200, &label_list());
    let boxes = mailboxes(&api(&transport))?;
    assert_eq!(
        boxes,
        [
            mailbox("INBOX", "7"),
            mailbox("Sent", "0"),
            mailbox("Drafts", "2"),
            mailbox("Spam", "0"),
            mailbox("Trash", "0"),
            mailbox("Starred", "3"),
            mailbox("Important", "11"),
            mailbox("receipts", "5"),
            mailbox("Work/Reports", "?"),
            mailbox("\u{c1}lvaro", "?"),
        ]
    );
    let seen = transport.seen();
    assert_eq!(seen.len(), 11);
    assert!(seen.iter().all(|request| request.method == Method::Get));
    assert_eq!(
        seen.first().map(|r| r.url.clone()),
        Some(format!("{BASE}/labels"))
    );
    let mut counted_urls: Vec<String> = urls(&transport).into_iter().skip(1).collect();
    counted_urls.sort();
    let mut wanted: Vec<String> = [
        "INBOX",
        "SENT",
        "DRAFT",
        "SPAM",
        "TRASH",
        "STARRED",
        "IMPORTANT",
        "Label_1",
        "Label_2",
        "Label_3",
    ]
    .iter()
    .map(|id| format!("{BASE}/labels/{id}"))
    .collect();
    wanted.sort();
    assert_eq!(counted_urls, wanted);
    Ok(())
}

#[test]
fn label_ids_are_percent_encoded_in_the_count_requests() -> TestResult {
    let transport = Scripted::new()
        .json(Method::Get, "/labels/", 200, r#"{"messagesUnread": 1}"#)
        .json(
            Method::Get,
            "/labels",
            200,
            r#"{"labels": [{"id": "INBOX", "type": "system"}, {"id": "Label_a/b+c", "name": "Odd", "type": "user"}]}"#,
        );
    mailboxes(&api(&transport))?;
    let seen = urls(&transport);
    assert!(
        seen.contains(&format!("{BASE}/labels/Label_a%2Fb%2Bc")),
        "{seen:?}"
    );
    assert!(seen.contains(&format!("{BASE}/labels/INBOX")), "{seen:?}");
    Ok(())
}

#[test]
fn counts_are_fetched_for_at_most_forty_labels() -> TestResult {
    let users: Vec<String> = (0..50)
        .map(|n| format!(r#"{{"id": "Label_{n}", "name": "L{n:02}", "type": "user"}}"#))
        .collect();
    let list = format!(
        r#"{{"labels": [{{"id": "INBOX", "name": "INBOX", "type": "system"}}, {}]}}"#,
        users.join(", ")
    );
    let transport = Scripted::new()
        .json(Method::Get, "/labels/", 200, r#"{"messagesUnread": 4}"#)
        .json(Method::Get, "/labels", 200, &list);
    let boxes = mailboxes(&api(&transport))?;
    assert_eq!(boxes.len(), 51);
    assert_eq!(transport.seen().len(), 1 + MAX_COUNTS);
    assert!(boxes.iter().take(MAX_COUNTS).all(|b| b.unread == "4"));
    assert!(boxes.iter().skip(MAX_COUNTS).all(|b| b.unread == "?"));
    assert_eq!(boxes.get(40).map(|b| b.path.as_str()), Some("L39"));
    Ok(())
}

#[test]
fn the_number_of_listed_mailboxes_and_scanned_labels_is_bounded() -> TestResult {
    let users: Vec<String> = (0..300)
        .map(|n| format!(r#"{{"id": "Label_{n}", "name": "L{n:03}", "type": "user"}}"#))
        .collect();
    let list = format!(
        r#"{{"labels": [{{"id": "INBOX", "type": "system"}}, {}]}}"#,
        users.join(", ")
    );
    let transport = Scripted::new()
        .json(Method::Get, "/labels/", 200, r#"{"messagesUnread": 1}"#)
        .json(Method::Get, "/labels", 200, &list);
    let boxes = mailboxes(&api(&transport))?;
    assert_eq!(boxes.len(), MAX_MAILBOXES);
    assert_eq!(boxes.first().map(|b| b.path.as_str()), Some("INBOX"));
    assert_eq!(boxes.last().map(|b| b.path.as_str()), Some("L198"));

    let mut many: Vec<String> = (0..MAX_SCANNED_LABELS)
        .map(|n| format!(r#"{{"id": "Label_{n}", "name": "M{n:04}", "type": "user"}}"#))
        .collect();
    many.push(r#"{"id": "INBOX", "name": "INBOX", "type": "system"}"#.to_string());
    let late = format!("{{\"labels\": [{}]}}", many.join(", "));
    let scanned = labels(&j(&late)?);
    assert_eq!(scanned.len(), MAX_MAILBOXES);
    assert!(scanned.iter().all(|label| label.id != "INBOX"));
    Ok(())
}

#[test]
fn internal_labels_are_hidden_and_a_list_without_mailboxes_is_unusable() {
    let hidden = r#"{"labels": [{"id": "CHAT", "type": "system"}, {"id": "UNREAD", "type": "system"}, {"id": "CATEGORY_UPDATES", "type": "system"}]}"#;
    for body in [
        hidden,
        "{}",
        "[]",
        "null",
        r#"{"labels": "x"}"#,
        r#"{"labels": []}"#,
    ] {
        let transport = Scripted::new().json(Method::Get, "/labels", 200, body);
        assert_eq!(mailboxes(&api(&transport)), Err(unexpected()), "{body}");
    }
}

#[test]
fn a_failing_label_list_is_an_error_but_failing_counts_are_not() -> TestResult {
    let transport = Scripted::new().json(
        Method::Get,
        "/labels",
        403,
        r#"{"error": {"message": "Insufficient Permission"}}"#,
    );
    let result = mailboxes(&api(&transport));
    assert!(is_msg(&result, "403"), "{result:?}");
    assert!(is_msg(&result, "Insufficient Permission"), "{result:?}");
    let transport = Scripted::new()
        .json(Method::Get, "/labels/", 500, "{}")
        .json(Method::Get, "/labels", 200, &label_list());
    let boxes = mailboxes(&api(&transport))?;
    assert_eq!(boxes.len(), 10);
    assert!(boxes.iter().all(|b| b.unread == "?"));
    Ok(())
}

#[test]
fn counts_that_are_not_non_negative_numbers_show_as_unknown() -> TestResult {
    for (count, shown) in [
        ("5", "5"),
        ("\"5\"", "5"),
        ("0", "0"),
        ("-1", "?"),
        ("\"x\"", "?"),
        ("1.5", "?"),
        ("null", "?"),
        ("[1]", "?"),
    ] {
        let transport = Scripted::new()
            .json(
                Method::Get,
                "/labels/INBOX",
                200,
                &format!("{{\"messagesUnread\": {count}}}"),
            )
            .json(
                Method::Get,
                "/labels",
                200,
                r#"{"labels": [{"id": "INBOX", "type": "system"}]}"#,
            );
        assert_eq!(
            mailboxes(&api(&transport))?,
            [mailbox("INBOX", shown)],
            "{count}"
        );
    }
    let transport = Scripted::new()
        .json(Method::Get, "/labels/INBOX", 200, "[]")
        .json(
            Method::Get,
            "/labels",
            200,
            r#"{"labels": [{"id": "INBOX", "type": "system"}]}"#,
        );
    assert_eq!(mailboxes(&api(&transport))?, [mailbox("INBOX", "?")]);
    Ok(())
}

#[test]
fn labels_with_odd_data_are_ignored() -> TestResult {
    let json = j(r#"{"labels": [
            null, 5, "x", [], {},
            {"id": "INBOX", "type": "system"},
            {"id": "Label_1", "name": "Ok", "type": "user"},
            {"id": "Label_2", "type": "user"},
            {"id": "Label_3", "name": 5, "type": "user"},
            {"id": "Label_4", "name": "\u200b\u202e", "type": "user"},
            {"id": "", "name": "No id", "type": "user"},
            {"name": "Missing id", "type": "user"},
            {"id": "bad id", "name": "Space in id", "type": "user"},
            {"id": "Label_5", "name": "No type"},
            {"id": "Label_6", "name": "System type", "type": "system"},
            {"id": "Label_7", "name": "Wrong type", "type": 5},
            {"id": "Label_8", "name": "Line\r\nbreak\tand   spaces", "type": "user"}
        ]}"#)?;
    let found: Vec<(String, String)> = labels(&json)
        .into_iter()
        .map(|label| (label.id, label.shown))
        .collect();
    assert_eq!(
        found,
        [
            ("INBOX".to_string(), "INBOX".to_string()),
            ("Label_8".to_string(), "Line break and spaces".to_string()),
            ("Label_1".to_string(), "Ok".to_string()),
        ]
    );
    Ok(())
}

#[test]
fn very_long_label_names_are_cut() -> TestResult {
    let json = j(&format!(
        r#"{{"labels": [{{"id": "Label_1", "name": "{}", "type": "user"}}]}}"#,
        "n".repeat(1000)
    ))?;
    let found = labels(&json);
    assert_eq!(found.len(), 1);
    assert_eq!(
        found.first().map(|label| label.shown.chars().count()),
        Some(MAX_LABEL_NAME + 6)
    );
    Ok(())
}

#[test]
fn shadowed_or_duplicate_names_are_not_listed_twice() -> TestResult {
    let json = j(r#"{"labels": [
            {"id": "INBOX", "type": "system"}, {"id": "SENT", "type": "system"}, {"id": "DRAFT", "type": "system"},
            {"id": "Label_1", "name": "inbox", "type": "user"},
            {"id": "Label_2", "name": "Sent", "type": "user"},
            {"id": "Label_3", "name": "draft", "type": "user"},
            {"id": "Label_4", "name": "Drafts", "type": "user"},
            {"id": "Label_5", "name": "Receipts", "type": "user"},
            {"id": "Label_6", "name": "receipts", "type": "user"},
            {"id": "Label_7", "name": "RECEIPTS", "type": "user"}
        ]}"#)?;
    let listed = labels(&json);
    let shown: Vec<&str> = listed.iter().map(|label| label.shown.as_str()).collect();
    assert_eq!(shown, ["INBOX", "Sent", "Drafts", "Receipts"]);
    for label in &listed {
        for name in [
            label.shown.as_str(),
            &label.shown.to_uppercase(),
            &label.shown.to_lowercase(),
        ] {
            assert_eq!(resolve(&listed, name), Some(label), "{name}");
        }
        if label.system {
            assert_eq!(resolve(&listed, &label.id), Some(label), "{}", label.id);
        }
    }
    Ok(())
}

#[test]
fn mailbox_names_are_resolved_without_regard_to_case() -> TestResult {
    let listed = labels(&j(&label_list())?);
    for (wanted, id, shown) in [
        ("INBOX", "INBOX", "INBOX"),
        ("inbox", "INBOX", "INBOX"),
        ("Inbox", "INBOX", "INBOX"),
        ("SENT", "SENT", "Sent"),
        ("sent", "SENT", "Sent"),
        ("Drafts", "DRAFT", "Drafts"),
        ("DRAFT", "DRAFT", "Drafts"),
        ("draft", "DRAFT", "Drafts"),
        ("spam", "SPAM", "Spam"),
        ("SPAM", "SPAM", "Spam"),
        ("Trash", "TRASH", "Trash"),
        ("starred", "STARRED", "Starred"),
        ("STARRED", "STARRED", "Starred"),
        ("Important", "IMPORTANT", "Important"),
        ("receipts", "Label_1", "receipts"),
        ("RECEIPTS", "Label_1", "receipts"),
        ("work/reports", "Label_3", "Work/Reports"),
        ("  Work/Reports  ", "Label_3", "Work/Reports"),
        ("\u{e1}LVARO", "Label_2", "\u{c1}lvaro"),
    ] {
        let found = resolve(&listed, wanted).ok_or(wanted)?;
        assert_eq!(
            (found.id.as_str(), found.shown.as_str()),
            (id, shown),
            "{wanted}"
        );
    }
    for unknown in [
        "",
        "   ",
        "Archive",
        "CATEGORY_PERSONAL",
        "category_social",
        "UNREAD",
        "CHAT",
        "Label_1",
        "YELLOW_STAR",
        "All Mail",
        "Work",
        "Work/",
        "inbox/x",
        "receipt",
    ] {
        assert_eq!(resolve(&listed, unknown), None, "{unknown}");
    }
    assert_eq!(resolve(&[], "inbox"), None);
    Ok(())
}

fn headers_json(headers: &[(&str, &str)]) -> String {
    headers
        .iter()
        .map(|(name, value)| {
            format!(
                "{{\"name\": {}, \"value\": {}}}",
                quoted(name),
                quoted(value)
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn metadata(id: &str, labels: &[&str], millis: &str, headers: &[(&str, &str)]) -> String {
    let labels: Vec<String> = labels.iter().map(|label| quoted(label)).collect();
    format!(
        r#"{{"id": "{id}", "threadId": "t-{id}", "labelIds": [{}], "snippet": "Vorschau", "historyId": "42", "internalDate": "{millis}", "sizeEstimate": 3000, "payload": {{"partId": "", "mimeType": "multipart/alternative", "filename": "", "headers": [{}]}}}}"#,
        labels.join(", "),
        headers_json(headers)
    )
}

fn item(id: &str, labels: &[&str], millis: &str, subject: &str, from: &str) -> String {
    metadata(
        id,
        labels,
        millis,
        &[
            ("From", from),
            ("Subject", subject),
            ("Date", "Tue, 14 Nov 2023 22:13:20 +0000"),
        ],
    )
}

fn listing(ids: &[&str]) -> String {
    let entries: Vec<String> = ids
        .iter()
        .map(|id| format!("{{\"id\": {}, \"threadId\": \"t-{id}\"}}", quoted(id)))
        .collect();
    format!(
        "{{\"messages\": [{}], \"resultSizeEstimate\": {}}}",
        entries.join(", "),
        ids.len()
    )
}

fn search_for(text: Option<&str>, field: &str) -> MessageQuery {
    MessageQuery {
        mailbox: None,
        since_days: 3,
        unread_only: false,
        query: text.map(str::to_string),
        field: field.to_string(),
        limit: 20,
    }
}

fn metadata_url(id: &str) -> String {
    format!("{BASE}/messages/{id}?format=metadata&metadataHeaders=From&metadataHeaders=Subject&metadataHeaders=Date")
}

#[test]
fn the_default_search_lists_the_inbox_newest_first() -> TestResult {
    let transport = Scripted::new()
        .json(
            Method::Get,
            "/messages?maxResults",
            200,
            &listing(&["m1", "m2", "m3"]),
        )
        .json(
            Method::Get,
            "/messages/m1?format=metadata",
            200,
            &item(
                "m1",
                &["INBOX", "UNREAD"],
                "1700000000999",
                "=?UTF-8?B?R3LDvMOfZQ==?=",
                "=?UTF-8?B?QW5uYSBNw7xsbGVy?= <anna@example.org>",
            ),
        )
        .json(
            Method::Get,
            "/messages/m2?format=metadata",
            200,
            &item(
                "m2",
                &["INBOX", "STARRED", "CATEGORY_PERSONAL"],
                "1700003600000",
                "Plan",
                "Bob <bob@example.org>",
            ),
        )
        .json(
            Method::Get,
            "/messages/m3?format=metadata",
            200,
            &item(
                "m3",
                &["INBOX"],
                "1699990000000",
                "\u{c4}lter",
                "carl@example.org",
            ),
        );
    let list = messages(&api(&transport), &search_for(None, "both"))?;
    assert_eq!(list.mailbox.as_deref(), Some("INBOX"));
    assert_eq!(list.total, 3);
    let items: Vec<(&str, &str, &str, i64, bool, bool)> = list
        .items
        .iter()
        .map(|i| {
            (
                i.id.as_str(),
                i.subject.as_str(),
                i.from.as_str(),
                i.ts,
                i.read,
                i.flagged,
            )
        })
        .collect();
    assert_eq!(
        items,
        [
            (
                "m2",
                "Plan",
                "Bob <bob@example.org>",
                1_700_003_600,
                true,
                true
            ),
            (
                "m1",
                "Gr\u{fc}\u{df}e",
                "Anna M\u{fc}ller <anna@example.org>",
                1_700_000_000,
                false,
                false
            ),
            (
                "m3",
                "\u{c4}lter",
                "carl@example.org",
                1_699_990_000,
                true,
                false
            ),
        ]
    );
    for entry in &list.items {
        assert_eq!(entry.date, localtime::minute(entry.ts));
        assert_eq!(entry.date.len(), 16);
    }
    let seen = transport.seen();
    assert_eq!(seen.len(), 4);
    assert!(seen.iter().all(|request| request.method == Method::Get));
    assert_eq!(
        seen.first().map(|request| request.url.clone()),
        Some(format!(
            "{BASE}/messages?maxResults=20&labelIds=INBOX&q=newer_than%3A3d"
        ))
    );
    let mut fetched: Vec<String> = urls(&transport).into_iter().skip(1).collect();
    fetched.sort();
    assert_eq!(
        fetched,
        [metadata_url("m1"), metadata_url("m2"), metadata_url("m3")]
    );
    Ok(())
}

#[test]
fn an_empty_mailbox_gives_an_empty_list_without_metadata_requests() -> TestResult {
    for body in [
        r#"{"resultSizeEstimate": 0}"#,
        r#"{"messages": [], "resultSizeEstimate": 0}"#,
        "{}",
    ] {
        let transport = Scripted::new().json(Method::Get, "/messages?maxResults", 200, body);
        let list = messages(&api(&transport), &search_for(None, "both"))?;
        assert_eq!(
            list,
            MessageList {
                mailbox: Some("INBOX".to_string()),
                total: 0,
                items: Vec::new()
            }
        );
        assert_eq!(transport.seen().len(), 1);
    }
    Ok(())
}

fn query_with(since_days: i64, unread_only: bool, text: Option<&str>, field: &str) -> MessageQuery {
    MessageQuery {
        since_days,
        unread_only,
        ..search_for(text, field)
    }
}

#[test]
fn filters_and_search_text_become_a_gmail_query() -> TestResult {
    for (query, wanted) in [
        (query_with(3, false, None, "both"), "newer_than:3d"),
        (query_with(1, false, None, "subject"), "newer_than:1d"),
        (
            query_with(30, true, None, "both"),
            "newer_than:30d is:unread",
        ),
        (
            query_with(90, true, Some("invoice 2024"), "subject"),
            "newer_than:90d is:unread subject:(invoice 2024)",
        ),
        (
            query_with(90, false, Some("anna"), "sender"),
            "newer_than:90d from:(anna)",
        ),
        (
            query_with(3, false, Some("Rechnung M\u{e4}rz"), "both"),
            "newer_than:3d {subject:(Rechnung M\u{e4}rz) from:(Rechnung M\u{e4}rz)}",
        ),
        (query_with(0, false, None, "both"), ""),
        (query_with(-5, true, None, "both"), "is:unread"),
        (query_with(100_000, false, None, "both"), "newer_than:3650d"),
        (query_with(3650, false, None, "both"), "newer_than:3650d"),
        (query_with(0, false, Some("x"), "sender"), "from:(x)"),
    ] {
        assert_eq!(search(&query)?, wanted, "{query:?}");
    }
    Ok(())
}

#[test]
fn hostile_search_text_cannot_change_the_structure_of_the_query() -> TestResult {
    let hostile = "a\" OR in:anywhere (b) {c} \\d label:secret\r\n\t\u{200b}x\u{0}y\u{202e}z";
    assert_eq!(words(hostile), "a OR in anywhere b c d label secret x y z");
    let query = query_with(3, false, Some(hostile), "both");
    let q = search(&query)?;
    assert_eq!(
        q,
        "newer_than:3d {subject:(a OR in anywhere b c d label secret x y z) from:(a OR in anywhere b c d label secret x y z)}"
    );
    assert_eq!(q.matches('(').count(), 2);
    assert_eq!(q.matches(')').count(), 2);
    assert_eq!(q.matches('{').count(), 1);
    assert_eq!(q.matches('}').count(), 1);
    assert_eq!(q.matches('"').count(), 0);
    assert_eq!(q.matches(':').count(), 3);
    for field in ["subject", "sender"] {
        let single = search(&query_with(3, false, Some("}) OR (in:inbox"), field))?;
        assert_eq!(single.matches('(').count(), 1, "{single}");
        assert_eq!(single.matches(')').count(), 1, "{single}");
        assert!(single.ends_with("OR in inbox)"), "{single}");
    }
    assert_eq!(
        words("\u{3000}tab\tseparated\u{a0}words\u{2028}here"),
        "tab separated words here"
    );
    Ok(())
}

#[test]
fn the_search_text_is_cut_and_may_not_be_empty() -> TestResult {
    let long = "word ".repeat(1000);
    let cut = words(&long);
    assert!(cut.chars().count() <= MAX_QUERY_CHARS, "{}", cut.len());
    assert!(!cut.ends_with(' '));
    assert!(cut.starts_with("word word"));
    assert_eq!(
        words(&"x".repeat(1_000_000)).chars().count(),
        MAX_QUERY_CHARS
    );
    for empty in ["", "   ", "\"(){}:\\", "\u{200b}\u{200b}", "\r\n\t", "((("] {
        let result = search(&query_with(3, false, Some(empty), "both"));
        assert_eq!(
            result,
            Err(ToolError::msg("The search text is empty.")),
            "{empty:?}"
        );
    }
    Ok(())
}

#[test]
fn an_unknown_field_is_only_refused_when_there_is_text_to_search() -> TestResult {
    for field in ["body", "", "Subject", "from"] {
        let result = search(&query_with(3, false, Some("x"), field));
        assert_eq!(
            result,
            Err(ToolError::msg(
                "Parameter 'field' must be subject, sender or both."
            )),
            "{field:?}"
        );
        assert_eq!(search(&query_with(3, false, None, field))?, "newer_than:3d");
    }
    Ok(())
}

#[test]
fn nothing_is_requested_when_the_search_is_refused() {
    let transport = Scripted::new();
    let result = messages(&api(&transport), &search_for(Some("  "), "both"));
    assert_eq!(result, Err(ToolError::msg("The search text is empty.")));
    let result = messages(&api(&transport), &search_for(Some("x"), "bogus"));
    assert!(is_msg(&result, "must be subject, sender or both"));
    assert!(transport.seen().is_empty());
}

#[test]
fn the_search_reaches_gmail_percent_encoded_and_within_bounds() -> TestResult {
    let transport = Scripted::new().json(Method::Get, "/messages?maxResults", 200, "{}");
    let mut query = search_for(Some("Rechnung M\u{e4}rz"), "both");
    query.unread_only = true;
    messages(&api(&transport), &query)?;
    let seen = urls(&transport);
    let [url] = seen.as_slice() else {
        return Err("one request".into());
    };
    assert_eq!(
        parameter(url, "q").as_deref(),
        Some("newer_than:3d is:unread {subject:(Rechnung M\u{e4}rz) from:(Rechnung M\u{e4}rz)}")
    );
    assert_eq!(parameter(url, "labelIds").as_deref(), Some("INBOX"));
    assert_eq!(parameter(url, "maxResults").as_deref(), Some("20"));
    assert!(!url.contains(['{', '}', '(', ')', ' ', '"']));
    let query_string = url.split_once('?').map(|(_, q)| q).unwrap_or_default();
    assert!(
        query_string
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-._~%=&".contains(c)),
        "{query_string}"
    );
    Ok(())
}

#[test]
fn the_limit_and_the_number_of_days_are_bounded() -> TestResult {
    for (limit, wanted) in [
        (500, "50"),
        (50, "50"),
        (20, "20"),
        (1, "1"),
        (0, "1"),
        (-3, "1"),
    ] {
        let transport = Scripted::new().json(Method::Get, "/messages?maxResults", 200, "{}");
        let mut query = search_for(None, "both");
        query.limit = limit;
        messages(&api(&transport), &query)?;
        let first = urls(&transport).into_iter().next().ok_or("url")?;
        assert_eq!(
            parameter(&first, "maxResults").as_deref(),
            Some(wanted),
            "{limit}"
        );
    }
    let transport = Scripted::new().json(Method::Get, "/messages?maxResults", 200, "{}");
    let mut query = search_for(None, "both");
    query.since_days = 0;
    messages(&api(&transport), &query)?;
    let first = urls(&transport).into_iter().next().ok_or("url")?;
    assert_eq!(
        first,
        format!("{BASE}/messages?maxResults=20&labelIds=INBOX")
    );
    Ok(())
}

#[test]
fn a_named_mailbox_is_resolved_through_the_label_list() -> TestResult {
    let transport = Scripted::new()
        .json(Method::Get, "/messages?maxResults", 200, &listing(&["m1"]))
        .json(
            Method::Get,
            "/messages/m1?format=metadata",
            200,
            &item(
                "m1",
                &["Label_1"],
                "1700000000000",
                "Beleg",
                "shop@example.org",
            ),
        )
        .json(Method::Get, "/labels", 200, &label_list());
    let mut query = search_for(None, "both");
    query.mailbox = Some(" RECEIPTS ".to_string());
    let list = messages(&api(&transport), &query)?;
    assert_eq!(list.mailbox.as_deref(), Some("receipts"));
    assert_eq!(list.items.len(), 1);
    let seen = urls(&transport);
    assert_eq!(
        seen.first().map(String::as_str),
        Some(format!("{BASE}/labels").as_str())
    );
    assert_eq!(
        seen.get(1).map(String::as_str),
        Some(format!("{BASE}/messages?maxResults=20&labelIds=Label_1&q=newer_than%3A3d").as_str())
    );
    Ok(())
}

#[test]
fn system_mailboxes_use_their_ids_and_spam_and_trash_are_asked_for_explicitly() -> TestResult {
    for (name, id, shown, flag) in [
        ("sent", "SENT", "Sent", false),
        ("DRAFT", "DRAFT", "Drafts", false),
        ("Drafts", "DRAFT", "Drafts", false),
        ("starred", "STARRED", "Starred", false),
        ("Important", "IMPORTANT", "Important", false),
        ("Spam", "SPAM", "Spam", true),
        ("trash", "TRASH", "Trash", true),
        ("TRASH", "TRASH", "Trash", true),
    ] {
        let transport = Scripted::new()
            .json(Method::Get, "/messages?maxResults", 200, "{}")
            .json(Method::Get, "/labels", 200, &label_list());
        let mut query = search_for(None, "both");
        query.mailbox = Some(name.to_string());
        let list = messages(&api(&transport), &query)?;
        assert_eq!(list.mailbox.as_deref(), Some(shown), "{name}");
        let list_url = urls(&transport).into_iter().nth(1).ok_or("url")?;
        assert_eq!(
            parameter(&list_url, "labelIds").as_deref(),
            Some(id),
            "{name}"
        );
        assert_eq!(
            parameter(&list_url, "includeSpamTrash").is_some(),
            flag,
            "{name}"
        );
        if flag {
            assert_eq!(
                parameter(&list_url, "includeSpamTrash").as_deref(),
                Some("true")
            );
        }
    }
    Ok(())
}

#[test]
fn naming_the_inbox_needs_no_label_request() -> TestResult {
    for name in ["INBOX", "inbox", " Inbox ", "", "   "] {
        let transport = Scripted::new().json(Method::Get, "/messages?maxResults", 200, "{}");
        let mut query = search_for(None, "both");
        query.mailbox = Some(name.to_string());
        let list = messages(&api(&transport), &query)?;
        assert_eq!(list.mailbox.as_deref(), Some("INBOX"));
        assert_eq!(transport.seen().len(), 1, "{name:?}");
    }
    Ok(())
}

#[test]
fn an_unknown_mailbox_is_refused_before_anything_is_listed() {
    for name in ["Archive", "CATEGORY_PERSONAL", "Label_1", "x/y"] {
        let transport = Scripted::new()
            .json(Method::Get, "/messages?maxResults", 200, &listing(&["m1"]))
            .json(Method::Get, "/labels", 200, &label_list());
        let mut query = search_for(None, "both");
        query.mailbox = Some(name.to_string());
        let result = messages(&api(&transport), &query);
        assert_eq!(
            result,
            Err(ToolError::msg(
                "Mailbox not found. Use list_mailboxes to see the valid names."
            )),
            "{name}"
        );
        assert_eq!(transport.seen().len(), 1, "{name}");
    }
}

#[test]
fn a_failing_label_list_stops_a_named_search() {
    let transport = Scripted::new().json(Method::Get, "/labels", 500, "{}");
    let mut query = search_for(None, "both");
    query.mailbox = Some("Sent".to_string());
    let result = messages(&api(&transport), &query);
    assert!(is_msg(&result, "temporary problem"), "{result:?}");
    assert_eq!(transport.seen().len(), 1);
}

fn three_ids() -> Scripted {
    Scripted::new().json(
        Method::Get,
        "/messages?maxResults",
        200,
        &listing(&["m1", "m2", "m3"]),
    )
}

fn good(id: &str, millis: &str) -> String {
    item(
        id,
        &["INBOX"],
        millis,
        &format!("Betreff {id}"),
        "a@example.org",
    )
}

#[test]
fn a_message_whose_metadata_cannot_be_fetched_is_skipped_when_others_work() -> TestResult {
    for failing in [404, 500, 429, 403] {
        let transport = three_ids()
            .json(
                Method::Get,
                "/messages/m1?format=metadata",
                200,
                &good("m1", "1700000000000"),
            )
            .json(Method::Get, "/messages/m2?format=metadata", failing, "{}")
            .json(
                Method::Get,
                "/messages/m3?format=metadata",
                200,
                &good("m3", "1700000100000"),
            );
        let list = messages(&api(&transport), &search_for(None, "both"))?;
        let ids: Vec<&str> = list.items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["m3", "m1"], "{failing}");
        assert_eq!(list.total, 3);
    }
    Ok(())
}

#[test]
fn when_every_metadata_fetch_fails_the_first_error_is_returned() {
    let transport = three_ids()
        .json(Method::Get, "/messages/m1?format=metadata", 429, "{}")
        .json(Method::Get, "/messages/m2?format=metadata", 500, "{}")
        .json(Method::Get, "/messages/m3?format=metadata", 404, "{}");
    let result = messages(&api(&transport), &search_for(None, "both"));
    assert!(is_msg(&result, "429"), "{result:?}");
    let transport = three_ids()
        .json(Method::Get, "/messages/m1?format=metadata", 404, "{}")
        .json(Method::Get, "/messages/m2?format=metadata", 500, "{}")
        .json(Method::Get, "/messages/m3?format=metadata", 500, "{}");
    let result = messages(&api(&transport), &search_for(None, "both"));
    assert!(is_msg(&result, "Not found"), "{result:?}");
}

#[test]
fn metadata_that_is_not_a_message_counts_as_a_failed_fetch() -> TestResult {
    let transport = three_ids()
        .json(Method::Get, "/messages/m1?format=metadata", 200, "[]")
        .json(
            Method::Get,
            "/messages/m2?format=metadata",
            200,
            &good("m2", "1700000000000"),
        )
        .json(Method::Get, "/messages/m3?format=metadata", 200, "\"text\"");
    let list = messages(&api(&transport), &search_for(None, "both"))?;
    assert_eq!(list.items.len(), 1);
    assert_eq!(list.total, 3);
    let all_bad = three_ids().json(Method::Get, "/messages/", 200, "null");
    assert_eq!(
        messages(&api(&all_bad), &search_for(None, "both")),
        Err(unexpected())
    );
    Ok(())
}

#[test]
fn the_total_follows_the_paging_information() -> TestResult {
    for (token, estimate, wanted) in [
        ("", "\"resultSizeEstimate\": 999", 3),
        (
            "\"nextPageToken\": \"abc\",",
            "\"resultSizeEstimate\": 120",
            120,
        ),
        (
            "\"nextPageToken\": \"abc\",",
            "\"resultSizeEstimate\": 1",
            3,
        ),
        (
            "\"nextPageToken\": \"abc\",",
            "\"resultSizeEstimate\": \"77\"",
            77,
        ),
        (
            "\"nextPageToken\": \"abc\",",
            "\"resultSizeEstimate\": -4",
            3,
        ),
        ("\"nextPageToken\": \"abc\",", "\"x\": 1", 3),
        ("\"nextPageToken\": \"\",", "\"resultSizeEstimate\": 50", 3),
        ("\"nextPageToken\": 5,", "\"resultSizeEstimate\": 50", 3),
    ] {
        let list = format!(
            "{{\"messages\": [{{\"id\": \"m1\"}}, {{\"id\": \"m2\"}}, {{\"id\": \"m3\"}}], {token} {estimate}}}"
        );
        let transport = Scripted::new()
            .json(Method::Get, "/messages?maxResults", 200, &list)
            .json(Method::Get, "/messages/", 200, &good("m1", "1700000000000"));
        let result = messages(&api(&transport), &search_for(None, "both"))?;
        assert_eq!(result.total, wanted, "{token} {estimate}");
        assert_eq!(result.items.len(), 3);
    }
    Ok(())
}

#[test]
fn only_one_page_is_ever_requested() -> TestResult {
    let list =
        r#"{"messages": [{"id": "m1"}], "nextPageToken": "NEXT", "resultSizeEstimate": 400}"#;
    let transport = Scripted::new()
        .json(Method::Get, "/messages?maxResults", 200, list)
        .json(
            Method::Get,
            "/messages/m1",
            200,
            &good("m1", "1700000000000"),
        );
    messages(&api(&transport), &search_for(None, "both"))?;
    assert_eq!(transport.seen().len(), 2);
    assert!(urls(&transport)
        .iter()
        .all(|url| !url.contains("pageToken")));
    Ok(())
}

#[test]
fn entries_without_a_usable_id_are_skipped_and_ids_are_percent_encoded() -> TestResult {
    let list = r#"{"messages": [{"id": "bad id"}, {"id": ""}, {"id": 5}, {"id": null}, {"threadId": "x"}, 7, "m", {"id": "ok1"}, {"id": "a/b"}, {"id": " spaced "}, {"id": "q?x"}, {"id": "p#x"}], "resultSizeEstimate": 12}"#;
    let transport = Scripted::new()
        .json(Method::Get, "/messages?maxResults", 200, list)
        .json(Method::Get, "/messages/", 200, &good("x", "1700000000000"));
    let result = messages(&api(&transport), &search_for(None, "both"))?;
    let mut fetched: Vec<String> = urls(&transport).into_iter().skip(1).collect();
    fetched.sort();
    assert_eq!(
        fetched,
        [
            metadata_url("a%2Fb"),
            metadata_url("ok1"),
            metadata_url("spaced")
        ]
    );
    let mut ids: Vec<&str> = result.items.iter().map(|i| i.id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(ids, ["a/b", "ok1", "spaced"]);
    assert_eq!(result.total, 3);
    Ok(())
}

#[test]
fn more_ids_than_the_limit_are_ignored() -> TestResult {
    let ids: Vec<String> = (0..60).map(|n| format!("m{n}")).collect();
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let transport = Scripted::new()
        .json(Method::Get, "/messages?maxResults", 200, &listing(&refs))
        .json(Method::Get, "/messages/", 200, &good("x", "1700000000000"));
    let mut query = search_for(None, "both");
    query.limit = 5;
    let result = messages(&api(&transport), &query)?;
    assert_eq!(result.items.len(), 5);
    assert_eq!(transport.seen().len(), 6);
    assert_eq!(result.total, 5);
    Ok(())
}

#[test]
fn headers_become_clean_one_line_text_and_missing_ones_become_empty() -> TestResult {
    let transport = Scripted::new()
        .json(
            Method::Get,
            "/messages?maxResults",
            200,
            &listing(&["m1", "m2", "m3"]),
        )
        .json(
            Method::Get,
            "/messages/m1?format=metadata",
            200,
            &item(
                "m1",
                &["INBOX"],
                "1700000000000",
                "=?UTF-8?B?R3LDvMOfZQ==?= \r\n\t Plan\u{202e}\u{0}",
                "\"M\u{fc}ller, Anna\" <anna@example.org>",
            ),
        )
        .json(
            Method::Get,
            "/messages/m2?format=metadata",
            200,
            &metadata(
                "m2",
                &[],
                "1699999999000",
                &[("subject", "klein geschrieben")],
            ),
        )
        .json(
            Method::Get,
            "/messages/m3?format=metadata",
            200,
            &metadata(
                "m3",
                &["INBOX"],
                "1699999990000",
                &[("Subject", &"s".repeat(2000)), ("From", &"f".repeat(2000))],
            ),
        );
    let list = messages(&api(&transport), &search_for(None, "both"))?;
    let by_id = |id: &str| list.items.iter().find(|i| i.id == id).ok_or(id.to_string());
    let first = by_id("m1")?;
    assert_eq!(first.subject, "Gr\u{fc}\u{df}e Plan");
    assert_eq!(first.from, "\"M\u{fc}ller, Anna\" <anna@example.org>");
    let second = by_id("m2")?;
    assert_eq!(second.subject, "klein geschrieben");
    assert_eq!(second.from, "");
    assert!(second.read && !second.flagged);
    let third = by_id("m3")?;
    assert_eq!(third.subject.chars().count(), MAX_SHOWN_SUBJECT + 6);
    assert_eq!(third.from.chars().count(), MAX_SHOWN_FROM + 6);
    Ok(())
}

#[test]
fn a_missing_or_implausible_date_gives_time_zero_and_no_date_text() -> TestResult {
    let bare = r#"{"id": "m1", "payload": {"headers": []}}"#;
    let odd = r#"{"id": "m2", "internalDate": "abc", "labelIds": "INBOX", "payload": {}}"#;
    let transport = Scripted::new()
        .json(
            Method::Get,
            "/messages?maxResults",
            200,
            &listing(&["m1", "m2"]),
        )
        .json(Method::Get, "/messages/m1?format=metadata", 200, bare)
        .json(Method::Get, "/messages/m2?format=metadata", 200, odd);
    let list = messages(&api(&transport), &search_for(None, "both"))?;
    assert_eq!(list.items.len(), 2);
    for entry in &list.items {
        assert_eq!((entry.ts, entry.date.as_str()), (0, ""));
        assert!(entry.read && !entry.flagged);
        assert_eq!(entry.subject, "");
    }
    let ids: Vec<&str> = list.items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, ["m1", "m2"]);
    Ok(())
}

#[test]
fn the_label_flags_decide_read_and_flagged() -> TestResult {
    let transport = Scripted::new()
        .json(
            Method::Get,
            "/messages?maxResults",
            200,
            &listing(&["a", "b", "c", "d"]),
        )
        .json(
            Method::Get,
            "/messages/a?format=metadata",
            200,
            &item("a", &["UNREAD"], "1700000000000", "a", "a@x.example"),
        )
        .json(
            Method::Get,
            "/messages/b?format=metadata",
            200,
            &item(
                "b",
                &["STARRED", "UNREAD"],
                "1700000001000",
                "b",
                "b@x.example",
            ),
        )
        .json(
            Method::Get,
            "/messages/c?format=metadata",
            200,
            &item(
                "c",
                &["unread", "starred"],
                "1700000002000",
                "c",
                "c@x.example",
            ),
        )
        .json(
            Method::Get,
            "/messages/d?format=metadata",
            200,
            &item(
                "d",
                &["INBOX", "IMPORTANT"],
                "1700000003000",
                "d",
                "d@x.example",
            ),
        );
    let list = messages(&api(&transport), &search_for(None, "both"))?;
    let flags: Vec<(&str, bool, bool)> = list
        .items
        .iter()
        .map(|i| (i.id.as_str(), i.read, i.flagged))
        .collect();
    assert_eq!(
        flags,
        [
            ("d", true, false),
            ("c", true, false),
            ("b", false, true),
            ("a", false, false)
        ]
    );
    Ok(())
}

#[test]
fn listing_failures_are_reported_for_the_model() {
    for (status, needle) in [
        (401, "avery mail link google"),
        (403, "403"),
        (429, "429"),
        (503, "temporary problem"),
    ] {
        let transport = Scripted::new().json(Method::Get, "/messages?maxResults", status, "{}");
        let result = messages(&api(&transport), &search_for(None, "both"));
        assert!(is_msg(&result, needle), "{status}: {result:?}");
        assert_eq!(transport.seen().len(), 1);
    }
    for body in ["[]", "\"x\"", "5", "null"] {
        let transport = Scripted::new().json(Method::Get, "/messages?maxResults", 200, body);
        assert_eq!(
            messages(&api(&transport), &search_for(None, "both")),
            Err(unexpected()),
            "{body}"
        );
    }
    let transport = Scripted::new().json(Method::Get, "/messages?maxResults", 200, "<html>");
    assert!(is_msg(
        &messages(&api(&transport), &search_for(None, "both")),
        "unexpected answer"
    ));
}

fn leaf(mime: &str, charset: &str, bytes: &[u8]) -> String {
    format!(
        r#"{{"partId": "0", "mimeType": "{mime}", "filename": "", "headers": [{{"name": "Content-Type", "value": "{mime}; charset=\"{charset}\""}}, {{"name": "Content-Transfer-Encoding", "value": "quoted-printable"}}], "body": {{"size": {}, "data": "{}"}}}}"#,
        bytes.len(),
        base64::encode_url(bytes)
    )
}

fn text_part(mime: &str, text: &str) -> String {
    leaf(mime, "UTF-8", text.as_bytes())
}

fn pending(mime: &str, attachment_id: &str, size: i64) -> String {
    format!(
        r#"{{"partId": "0", "mimeType": "{mime}", "filename": "", "headers": [], "body": {{"attachmentId": {}, "size": {size}}}}}"#,
        quoted(attachment_id)
    )
}

fn attachment(name: &str, mime: &str, size: i64) -> String {
    format!(
        r#"{{"partId": "2", "mimeType": "{mime}", "filename": {}, "headers": [], "body": {{"attachmentId": "ANGj-file", "size": {size}}}}}"#,
        quoted(name)
    )
}

fn container(mime: &str, parts: &[String]) -> String {
    format!(
        r#"{{"partId": "", "mimeType": "{mime}", "filename": "", "headers": [], "body": {{"size": 0}}, "parts": [{}]}}"#,
        parts.join(", ")
    )
}

fn full(
    id: &str,
    labels: &[&str],
    millis: &str,
    headers: &[(&str, &str)],
    payload: &str,
) -> String {
    let labels: Vec<String> = labels.iter().map(|label| quoted(label)).collect();
    let mut root = match j(payload) {
        Ok(Json::Obj(object)) => object,
        _ => Object::new(),
    };
    let mut merged: Vec<Json> = headers
        .iter()
        .map(|(name, value)| {
            Json::object(vec![
                ("name", Json::from(*name)),
                ("value", Json::from(*value)),
            ])
        })
        .collect();
    if let Some(Json::Arr(existing)) = root.get("headers") {
        merged.extend(existing.iter().cloned());
    }
    root.insert("headers", Json::Arr(merged));
    format!(
        r#"{{"id": "{id}", "threadId": "t-{id}", "labelIds": [{}], "snippet": "x", "historyId": "9", "internalDate": "{millis}", "sizeEstimate": 5000, "payload": {}}}"#,
        labels.join(", "),
        json::dumps(&Json::Obj(root), true)
    )
}

fn read_with(transport: &Scripted, id: &str, max_chars: usize) -> ToolResult<MessageBody> {
    read(
        &api(transport),
        &mid(id).map_err(|_| unexpected())?,
        max_chars,
    )
}

fn person(name: &str, address: &str) -> Person {
    Person {
        name: name.to_string(),
        address: address.to_string(),
    }
}

const FULL_ID: &str = "18c3f0a1b2c3d4e5";

#[test]
fn a_mail_is_read_with_headers_text_and_attachments() -> TestResult {
    let payload = container(
        "multipart/mixed",
        &[
            container(
                "multipart/alternative",
                &[
                    text_part(
                        "text/plain",
                        "Hallo Alice,\n\nanbei der Plan.\n\nGr\u{fc}\u{df}e\nAnna",
                    ),
                    text_part(
                        "text/html",
                        "<p>Hallo Alice,</p><p>anbei der <b>Plan</b>.</p>",
                    ),
                ],
            ),
            attachment("Rechnung.pdf", "application/pdf", 2048),
        ],
    );
    let message = full(
        FULL_ID,
        &["INBOX", "UNREAD", "CATEGORY_PERSONAL"],
        "1700000000999",
        &[
            ("Received", "from mx.example.org"),
            ("From", "=?UTF-8?B?QW5uYSBNw7xsbGVy?= <anna@example.org>"),
            (
                "To",
                "Alice <alice@gmail.example>, \"Muster, Bob\" <bob@example.org>",
            ),
            ("Cc", "carl@example.org, Team: dora@example.org;"),
            ("Subject", "Plan f\u{fc}r =?UTF-8?B?R3LDvMOfZQ==?="),
            ("Date", "Tue, 14 Nov 2023 22:13:20 +0000"),
        ],
        &payload,
    );
    let transport = Scripted::new().json(
        Method::Get,
        "/messages/18c3f0a1b2c3d4e5?format=full",
        200,
        &message,
    );
    let body = read_with(&transport, FULL_ID, 6000)?;
    let text = "Hallo Alice,\n\nanbei der Plan.\n\nGr\u{fc}\u{df}e\nAnna";
    assert_eq!(
        body,
        MessageBody {
            mailbox: "INBOX".to_string(),
            subject: "Plan f\u{fc}r Gr\u{fc}\u{df}e".to_string(),
            from: "Anna M\u{fc}ller <anna@example.org>".to_string(),
            date: localtime::minute(1_700_000_000),
            read: false,
            to: vec![
                person("Alice", "alice@gmail.example"),
                person("Muster, Bob", "bob@example.org")
            ],
            cc: vec![
                person("", "carl@example.org"),
                person("", "dora@example.org")
            ],
            attachments: vec![Attachment {
                name: "Rechnung.pdf".to_string(),
                size: Some(2048)
            }],
            body_chars: Some(text.chars().count()),
            body: text.to_string(),
        }
    );
    let seen = transport.seen();
    let [request] = seen.as_slice() else {
        return Err("one request".into());
    };
    assert_eq!(request.method, Method::Get);
    assert_eq!(
        request.url,
        format!("{BASE}/messages/{FULL_ID}?format=full")
    );
    assert_eq!(request.body, None);
    Ok(())
}

#[test]
fn the_body_is_cut_and_the_total_is_reported() -> TestResult {
    let long: String = (0..100)
        .map(|n| format!("Zeile {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let message = full(
        "m1",
        &["INBOX"],
        "1700000000000",
        &[("Subject", "Lang")],
        &text_part("text/plain", &long),
    );
    let transport = Scripted::new().json(Method::Get, "/messages/m1?format=full", 200, &message);
    let body = read_with(&transport, "m1", 50)?;
    assert_eq!(body.body.chars().count(), 50);
    assert_eq!(body.body_chars, Some(long.chars().count()));
    assert!(long.starts_with(&body.body));
    let all = read_with(&transport, "m1", 1_000_000)?;
    assert_eq!(all.body, long);
    let none = read_with(&transport, "m1", 0)?;
    assert_eq!(
        (none.body.as_str(), none.body_chars),
        ("", Some(long.chars().count()))
    );
    Ok(())
}

#[test]
fn invisible_characters_and_blank_runs_are_removed_before_counting() -> TestResult {
    let message = full(
        "m1",
        &["INBOX"],
        "1700000000000",
        &[],
        &text_part(
            "text/plain",
            "a\u{202e}b\u{200b}c\r\n\r\n\r\n\r\n\r\nd   \r\n\u{e0041}",
        ),
    );
    let transport = Scripted::new().json(Method::Get, "/messages/m1?format=full", 200, &message);
    let body = read_with(&transport, "m1", 6000)?;
    assert_eq!(body.body, "abc\n\nd");
    assert_eq!(body.body_chars, Some(6));
    Ok(())
}

#[test]
fn an_html_only_mail_is_converted_and_a_mail_without_text_is_empty() -> TestResult {
    let html = "<html><head><style>p{color:red}</style></head><body><p>Guten Tag,</p><p>Ihre Bestellung &amp; <a href=\"https://shop.example/x?y=1\">Beleg</a>.</p><script>x()</script></body></html>";
    let message = full(
        "m1",
        &["INBOX"],
        "1700000000000",
        &[],
        &text_part("text/html", html),
    );
    let transport = Scripted::new().json(Method::Get, "/messages/m1?format=full", 200, &message);
    let body = read_with(&transport, "m1", 6000)?;
    assert_eq!(body.body, "Guten Tag,\n\nIhre Bestellung & Beleg.");
    let empty = full(
        "m2",
        &["INBOX"],
        "1700000000000",
        &[],
        &container(
            "multipart/mixed",
            &[attachment("a.zip", "application/zip", 5)],
        ),
    );
    let transport = Scripted::new().json(Method::Get, "/messages/m2?format=full", 200, &empty);
    let body = read_with(&transport, "m2", 6000)?;
    assert_eq!((body.body.as_str(), body.body_chars), ("", Some(0)));
    assert_eq!(body.attachments.len(), 1);
    Ok(())
}

#[test]
fn bodies_in_other_charsets_are_converted() -> TestResult {
    let message = full(
        "m1",
        &["INBOX"],
        "1700000000000",
        &[],
        &leaf(
            "text/plain",
            "ISO-8859-1",
            &[0x4B, 0xE4, 0x73, 0x65, 0x20, 0x80],
        ),
    );
    let transport = Scripted::new().json(Method::Get, "/messages/m1?format=full", 200, &message);
    assert_eq!(
        read_with(&transport, "m1", 6000)?.body,
        "K\u{e4}se \u{20ac}"
    );
    Ok(())
}

#[test]
fn a_message_without_payload_or_dates_is_read_as_empty() -> TestResult {
    for message in [
        r#"{"id": "m1"}"#,
        r#"{"id": "m1", "labelIds": ["INBOX"], "payload": {}}"#,
        r#"{"id": "m1", "internalDate": "x", "payload": {"headers": "no", "parts": "no"}}"#,
    ] {
        let transport = Scripted::new().json(Method::Get, "/messages/m1?format=full", 200, message);
        let body = read_with(&transport, "m1", 6000)?;
        assert_eq!(body.date, "");
        assert_eq!((body.subject.as_str(), body.from.as_str()), ("", ""));
        assert!(body.to.is_empty() && body.cc.is_empty() && body.attachments.is_empty());
        assert_eq!((body.body.as_str(), body.body_chars), ("", Some(0)));
    }
    Ok(())
}

#[test]
fn the_mailbox_is_taken_from_the_system_labels() -> TestResult {
    let cases: [(&[&str], &str, bool); 13] = [
        (&["INBOX"], "INBOX", true),
        (&["INBOX", "UNREAD"], "INBOX", false),
        (&["SENT"], "Sent", true),
        (&["DRAFT"], "Drafts", true),
        (&["SPAM", "UNREAD"], "Spam", false),
        (&["TRASH", "STARRED"], "Trash", true),
        (&["STARRED", "IMPORTANT"], "All Mail", true),
        (&["Label_5"], "All Mail", true),
        (&["CATEGORY_UPDATES", "Label_1"], "All Mail", true),
        (&[], "All Mail", true),
        (&["SENT", "INBOX"], "INBOX", true),
        (&["TRASH", "INBOX"], "INBOX", true),
        (&["SPAM", "SENT"], "Sent", true),
    ];
    for (labels, wanted, read_flag) in cases {
        let message = full(
            "m1",
            labels,
            "1700000000000",
            &[],
            &text_part("text/plain", "x"),
        );
        let transport =
            Scripted::new().json(Method::Get, "/messages/m1?format=full", 200, &message);
        let body = read_with(&transport, "m1", 6000)?;
        assert_eq!(
            (body.mailbox.as_str(), body.read),
            (wanted, read_flag),
            "{labels:?}"
        );
    }
    Ok(())
}

#[test]
fn headers_with_hostile_text_are_cleaned() -> TestResult {
    let message = full(
        "m1",
        &["INBOX"],
        "1700000000000",
        &[
            ("Subject", "Hallo\r\nBcc: evil@example.org\u{202e}"),
            ("From", "\"Evil\r\n Name\u{200b}\" <evil@example.org>"),
            ("To", &format!("{} <a@example.org>", "n".repeat(1000))),
        ],
        &text_part("text/plain", "x"),
    );
    let transport = Scripted::new().json(Method::Get, "/messages/m1?format=full", 200, &message);
    let body = read_with(&transport, "m1", 6000)?;
    assert_eq!(body.subject, "Hallo Bcc: evil@example.org");
    assert_eq!(body.from, "\"Evil Name\" <evil@example.org>");
    assert_eq!(body.to.len(), 1);
    assert_eq!(body.to.first().map(|p| p.name.chars().count()), Some(206));
    Ok(())
}

#[test]
fn an_unknown_message_is_reported_with_the_standard_text() {
    let transport = Scripted::new().json(
        Method::Get,
        "/messages/gone?format=full",
        404,
        r#"{"error": {"code": 404, "message": "Requested entity was not found.", "status": "NOT_FOUND"}}"#,
    );
    assert_eq!(
        read_with(&transport, "gone", 6000),
        Err(ToolError::msg(
            "Message not found. Use an id from list_messages or search_messages together with the same account and mailbox."
        ))
    );
    assert_eq!(transport.seen().len(), 1);
}

#[test]
fn other_read_failures_are_reported_for_the_model() {
    for (status, needle) in [
        (400, "(400)"),
        (401, "avery mail link google"),
        (403, "403"),
        (429, "429"),
        (503, "temporary problem"),
    ] {
        let transport = Scripted::new().json(Method::Get, "/messages/m1?format=full", status, "{}");
        let result = read_with(&transport, "m1", 6000);
        assert!(is_msg(&result, needle), "{status}: {result:?}");
    }
    for body in ["[]", "\"x\"", "5", "null"] {
        let transport = Scripted::new().json(Method::Get, "/messages/m1?format=full", 200, body);
        assert_eq!(
            read_with(&transport, "m1", 6000),
            Err(unexpected()),
            "{body}"
        );
    }
}

#[test]
fn the_message_id_is_percent_encoded() -> TestResult {
    let transport = Scripted::new().json(Method::Get, "/messages/", 200, r#"{"id": "x"}"#);
    read_with(&transport, "a/b+c=", 6000)?;
    assert_eq!(
        urls(&transport),
        [format!("{BASE}/messages/a%2Fb%2Bc%3D?format=full")]
    );
    Ok(())
}

#[test]
fn a_large_text_part_without_inline_data_is_fetched_with_one_more_read() -> TestResult {
    let html = "<p>Sehr lang</p><p>und wichtig</p>";
    let message = full(
        "m1",
        &["INBOX"],
        "1700000000000",
        &[("Subject", "Gross")],
        &container(
            "multipart/alternative",
            &[pending("text/html", "ANGj/x+y=", 90_000)],
        ),
    );
    let transport = Scripted::new()
        .json(Method::Get, "/messages/m1?format=full", 200, &message)
        .json(
            Method::Get,
            "/messages/m1/attachments/",
            200,
            &format!(
                "{{\"size\": 34, \"data\": \"{}\"}}",
                base64::encode_url(html.as_bytes())
            ),
        );
    let body = read_with(&transport, "m1", 6000)?;
    assert_eq!(body.body, "Sehr lang\n\nund wichtig");
    assert_eq!(
        urls(&transport),
        [
            format!("{BASE}/messages/m1?format=full"),
            format!("{BASE}/messages/m1/attachments/ANGj%2Fx%2By%3D"),
        ]
    );
    assert!(transport.seen().iter().all(|r| r.method == Method::Get));
    Ok(())
}

#[test]
fn text_that_is_present_is_never_fetched_again_and_fetches_are_bounded() -> TestResult {
    let message = full(
        "m1",
        &["INBOX"],
        "1700000000000",
        &[],
        &container(
            "multipart/alternative",
            &[
                text_part("text/plain", "da"),
                pending("text/html", "ANGj-1", 90_000),
            ],
        ),
    );
    let transport = Scripted::new().json(Method::Get, "/messages/m1?format=full", 200, &message);
    assert_eq!(read_with(&transport, "m1", 6000)?.body, "da");
    assert_eq!(transport.seen().len(), 1);

    let many = full(
        "m2",
        &["INBOX"],
        "1700000000000",
        &[],
        &container(
            "multipart/mixed",
            &(0..5)
                .map(|n| pending("text/plain", &format!("ANGj-{n}"), 100))
                .collect::<Vec<_>>(),
        ),
    );
    let transport = Scripted::new()
        .json(Method::Get, "/messages/m2?format=full", 200, &many)
        .json(
            Method::Get,
            "/attachments/",
            200,
            &format!("{{\"data\": \"{}\"}}", base64::encode_url(b"Teil")),
        );
    let body = read_with(&transport, "m2", 6000)?;
    assert_eq!(body.body, "Teil\n\nTeil");
    assert_eq!(transport.seen().len(), 3);
    Ok(())
}

#[test]
fn a_failing_or_unusable_extra_read_fails_the_whole_read() -> TestResult {
    let message = full(
        "m1",
        &["INBOX"],
        "1700000000000",
        &[],
        &pending("text/plain", "ANGj-1", 5000),
    );
    for (status, answer, needle) in [
        (404, "{}", "Not found"),
        (500, "{}", "temporary problem"),
        (403, "{}", "403"),
        (200, "{}", "unexpected answer"),
        (200, r#"{"data": 5}"#, "unexpected answer"),
        (200, r#"{"data": "!!!"}"#, "unexpected answer"),
        (200, "[]", "unexpected answer"),
    ] {
        let transport = Scripted::new()
            .json(Method::Get, "/messages/m1?format=full", 200, &message)
            .json(Method::Get, "/attachments/", status, answer);
        let result = read_with(&transport, "m1", 6000);
        assert!(is_msg(&result, needle), "{status} {answer}: {result:?}");
    }
    Ok(())
}

#[test]
fn reading_only_ever_sends_get_requests() -> TestResult {
    let message = full(
        "m1",
        &["INBOX", "UNREAD"],
        "1700000000000",
        &[],
        &container(
            "multipart/mixed",
            &[
                pending("text/plain", "ANGj-1", 5000),
                attachment("a.pdf", "application/pdf", 9),
            ],
        ),
    );
    let transport = Scripted::new()
        .json(Method::Get, "/messages/m1?format=full", 200, &message)
        .json(Method::Get, "/attachments/", 200, r#"{"data": "SGk"}"#);
    read_with(&transport, "m1", 6000)?;
    assert!(!transport.seen().is_empty());
    assert!(transport
        .seen()
        .iter()
        .all(|r| r.method == Method::Get && r.body.is_none()));
    assert!(urls(&transport)
        .iter()
        .all(|url| !url.contains("modify") && !url.contains("trash")));
    Ok(())
}

#[test]
fn huge_and_deeply_nested_mails_are_read_within_bounds() -> TestResult {
    let parts: Vec<String> = (0..400).map(|_| text_part("text/plain", "x")).collect();
    let message = full(
        "m1",
        &["INBOX"],
        "1700000000000",
        &[],
        &container("multipart/mixed", &parts),
    );
    let transport = Scripted::new().json(Method::Get, "/messages/m1?format=full", 200, &message);
    let body = read_with(&transport, "m1", 100_000)?;
    assert_eq!(body.body.split("\n\n").count(), 299);

    let mut deep = text_part("text/plain", "tief");
    for _ in 0..30 {
        deep = container("multipart/mixed", &[deep]);
    }
    let message = full("m2", &["INBOX"], "1700000000000", &[], &deep);
    let transport = Scripted::new().json(Method::Get, "/messages/m2?format=full", 200, &message);
    let body = read_with(&transport, "m2", 6000)?;
    assert_eq!((body.body.as_str(), body.body_chars), ("", Some(0)));

    let big = "a".repeat(mime::MAX_TEXT_BYTES + 10_000);
    let message = full(
        "m3",
        &["INBOX"],
        "1700000000000",
        &[],
        &text_part("text/plain", &big),
    );
    let transport = Scripted::new().json(Method::Get, "/messages/m3?format=full", 200, &message);
    let body = read_with(&transport, "m3", 6000)?;
    assert_eq!(body.body.len(), 6000);
    assert_eq!(body.body_chars, Some(mime::MAX_TEXT_BYTES));
    Ok(())
}

fn sender() -> Scripted {
    Scripted::new().json(Method::Get, "/profile", 200, PROFILE)
}

fn mail(mode: Mode, to: &[&str], cc: &[&str], subject: &str, body: &str) -> ComposeRequest {
    ComposeRequest {
        mode,
        to: to.iter().map(|address| address.to_string()).collect(),
        cc: cc.iter().map(|address| address.to_string()).collect(),
        subject: subject.to_string(),
        body: body.to_string(),
        reply: None,
    }
}

fn answer_to(mode: Mode, id: &str, all: bool, body: &str) -> Result<ComposeRequest, Failure> {
    Ok(ComposeRequest {
        mode,
        to: Vec::new(),
        cc: Vec::new(),
        subject: String::new(),
        body: body.to_string(),
        reply: Some(ReplySpec {
            mailbox: None,
            id: mid(id)?,
            all,
        }),
    })
}

fn run(
    transport: &Scripted,
    host: &Dialog,
    addresses: &[&str],
    request: &ComposeRequest,
) -> ToolResult<Preview> {
    compose(&api(transport), host, &account(addresses), request)
}

struct Sent {
    raw: String,
    keys: Vec<String>,
    inner_keys: Option<Vec<String>>,
    thread_id: Option<String>,
    headers: Vec<(String, String)>,
    body: String,
}

impl Sent {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(found, _)| found.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn names(&self) -> Vec<&str> {
        self.headers.iter().map(|(name, _)| name.as_str()).collect()
    }
}

fn sent(request: &Request) -> Result<Sent, Failure> {
    let text = request.body.as_deref().ok_or("no body")?;
    let parsed = j(text)?;
    let object = parsed.as_object().ok_or("object")?;
    let keys: Vec<String> = object.keys().map(str::to_string).collect();
    let (holder, inner_keys) = match object.get("message") {
        Some(Json::Obj(inner)) => (inner, Some(inner.keys().map(str::to_string).collect())),
        _ => (object, None),
    };
    let raw = holder.get("raw").and_then(Json::as_str).ok_or("raw")?;
    if raw.contains(['+', '/', '=']) {
        return Err("raw is not unpadded base64url".into());
    }
    let thread_id = holder
        .get("threadId")
        .and_then(Json::as_str)
        .map(str::to_string);
    let message = String::from_utf8(base64::decode(raw).ok_or("base64")?)?;
    if message.replace("\r\n", "").contains(['\r', '\n']) {
        return Err("bare line end".into());
    }
    if message.split("\r\n").any(|line| line.len() > 998) {
        return Err("line too long".into());
    }
    let (head, body) = message.split_once("\r\n\r\n").ok_or("blank line")?;
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in head.split("\r\n") {
        if line.starts_with(' ') {
            let last = headers.last_mut().ok_or("fold")?;
            last.1.push_str(line);
        } else {
            let (name, value) = line.split_once(':').ok_or("header")?;
            headers.push((
                name.to_string(),
                value.strip_prefix(' ').unwrap_or(value).to_string(),
            ));
        }
    }
    let body = String::from_utf8(base64::decode(body).ok_or("body")?)?;
    Ok(Sent {
        raw: message,
        keys,
        inner_keys,
        thread_id,
        headers,
        body,
    })
}

fn the_post(transport: &Scripted) -> Result<Sent, Failure> {
    let posts = requests(transport, Method::Post);
    let [post] = posts.as_slice() else {
        return Err(format!("expected one POST, saw {}", posts.len()).into());
    };
    sent(post)
}

fn dialog_payload(host: &Dialog) -> Result<Object, Failure> {
    let shown = host.shown();
    let [payload] = shown.as_slice() else {
        return Err(format!("expected one dialog, saw {}", shown.len()).into());
    };
    match j(payload)? {
        Json::Obj(object) => Ok(object),
        _ => Err("dialog payload".into()),
    }
}

fn list_of(object: &Object, key: &str) -> Vec<String> {
    crate::account::string_list(object.get(key))
}

fn preview(to: &[&str], cc: &[&str], subject: &str, body: &str, status: &str) -> Preview {
    Preview {
        from: "alice@gmail.example".to_string(),
        to: to.iter().map(|address| address.to_string()).collect(),
        cc: cc.iter().map(|address| address.to_string()).collect(),
        subject: subject.to_string(),
        body: body.to_string(),
        reply: None,
        status: status.to_string(),
        saved: None,
    }
}

const BODY: &str = "Hallo Anna,\n\nbis morgen!\n\nAlice";
const SUBJECT: &str = "Gr\u{fc}\u{df}e aus Z\u{fc}rich";

#[test]
fn a_draft_is_saved_with_one_post_and_without_a_dialog() -> TestResult {
    let transport = sender().json(Method::Post, "/drafts", 200, DRAFT_ANSWER);
    let host = Dialog::deciding("send");
    let request = mail(Mode::Draft, &["anna@example.org"], &[], SUBJECT, BODY);
    let result = run(&transport, &host, &["alice@gmail.example"], &request)?;
    assert_eq!(
        result,
        Preview {
            saved: Some(true),
            ..preview(&["anna@example.org"], &[], SUBJECT, BODY, "drafted")
        }
    );
    assert!(host.shown().is_empty());
    let seen = transport.seen();
    assert_eq!(
        seen.iter()
            .map(|r| (r.method, r.url.clone()))
            .collect::<Vec<_>>(),
        [
            (Method::Get, format!("{BASE}/profile")),
            (Method::Post, format!("{BASE}/drafts")),
        ]
    );
    let post = seen.get(1).ok_or("post")?;
    assert_eq!(
        header_of(post, "Content-Type").as_deref(),
        Some("application/json")
    );
    assert_eq!(
        header_of(post, "Authorization").as_deref(),
        Some("Bearer TOKEN")
    );
    let message = sent(post)?;
    assert_eq!(message.keys, ["message"]);
    assert_eq!(message.inner_keys, Some(vec!["raw".to_string()]));
    assert_eq!(message.thread_id, None);
    assert_eq!(
        message.names(),
        [
            "To",
            "Subject",
            "MIME-Version",
            "Content-Type",
            "Content-Transfer-Encoding"
        ]
    );
    assert_eq!(message.header("To"), Some("anna@example.org"));
    assert_eq!(
        message
            .header("Subject")
            .map(|s| header::text(s, 500))
            .as_deref(),
        Some(SUBJECT)
    );
    assert_eq!(message.header("MIME-Version"), Some("1.0"));
    assert_eq!(
        message.header("Content-Type"),
        Some("text/plain; charset=UTF-8")
    );
    assert_eq!(message.header("Content-Transfer-Encoding"), Some("base64"));
    assert_eq!(message.body, "Hallo Anna,\r\n\r\nbis morgen!\r\n\r\nAlice");
    Ok(())
}

#[test]
fn a_send_shows_exactly_what_is_sent_and_sends_once_after_approval() -> TestResult {
    let transport = sender().json(Method::Post, "/messages/send", 200, SENT_ANSWER);
    let host = Dialog::deciding("send").watching(&transport);
    let request = mail(
        Mode::Send,
        &["anna@example.org", "carl@example.org"],
        &["bob@example.org"],
        SUBJECT,
        BODY,
    );
    let result = run(&transport, &host, &["alice@gmail.example"], &request)?;
    assert_eq!(
        result,
        preview(
            &["anna@example.org", "carl@example.org"],
            &["bob@example.org"],
            SUBJECT,
            BODY,
            "sent"
        )
    );
    assert_eq!(host.writes_before_the_dialog(), [0]);
    let seen = transport.seen();
    assert_eq!(
        seen.iter()
            .map(|r| (r.method, r.url.clone()))
            .collect::<Vec<_>>(),
        [
            (Method::Get, format!("{BASE}/profile")),
            (Method::Post, format!("{BASE}/messages/send")),
        ]
    );
    let message = the_post(&transport)?;
    assert_eq!(message.keys, ["raw"]);
    assert_eq!(message.inner_keys, None);
    assert_eq!(
        message.names(),
        [
            "To",
            "Cc",
            "Subject",
            "MIME-Version",
            "Content-Type",
            "Content-Transfer-Encoding"
        ]
    );

    let shown = dialog_payload(&host)?;
    assert_eq!(shown.get("op").and_then(Json::as_str), Some("approve_send"));
    assert_eq!(
        shown.get("from").and_then(Json::as_str),
        Some("alice@gmail.example")
    );
    assert_eq!(shown.get("reply"), Some(&Json::Null));
    assert_eq!(shown.get("seconds"), Some(&Json::Int(75)));
    let dialog_to = list_of(&shown, "to");
    let dialog_cc = list_of(&shown, "cc");
    let dialog_subject = shown
        .get("subject")
        .and_then(Json::as_str)
        .ok_or("subject")?;
    let dialog_body = shown.get("body").and_then(Json::as_str).ok_or("body")?;
    assert_eq!(dialog_to, ["anna@example.org", "carl@example.org"]);
    assert_eq!(dialog_cc, ["bob@example.org"]);
    assert_eq!(
        message
            .header("To")
            .map(|v| v.split(", ").map(str::to_string).collect::<Vec<_>>()),
        Some(dialog_to)
    );
    assert_eq!(
        message
            .header("Cc")
            .map(|v| v.split(", ").map(str::to_string).collect::<Vec<_>>()),
        Some(dialog_cc)
    );
    assert_eq!(
        message
            .header("Subject")
            .map(|s| header::text(s, 500))
            .as_deref(),
        Some(dialog_subject)
    );
    assert_eq!(message.body, dialog_body.replace('\n', "\r\n"));
    assert_eq!(dialog_body, BODY);
    Ok(())
}

#[test]
fn a_declined_send_sends_nothing() -> TestResult {
    for (decision, status) in [("declined", "declined"), ("timeout", "timeout")] {
        let transport = sender()
            .json(Method::Post, "/messages/send", 200, SENT_ANSWER)
            .json(Method::Post, "/drafts", 200, DRAFT_ANSWER);
        let host = Dialog::deciding(decision).watching(&transport);
        let request = mail(Mode::Send, &["anna@example.org"], &[], SUBJECT, BODY);
        let result = run(&transport, &host, &["alice@gmail.example"], &request)?;
        assert_eq!(
            result,
            preview(&["anna@example.org"], &[], SUBJECT, BODY, status),
            "{decision}"
        );
        assert_eq!(host.shown().len(), 1);
        assert_eq!(urls(&transport), [format!("{BASE}/profile")]);
        assert!(requests(&transport, Method::Post).is_empty());
    }
    Ok(())
}

#[test]
fn a_dialog_that_fails_or_is_not_understood_sends_nothing() {
    let transport = sender().json(Method::Post, "/messages/send", 200, SENT_ANSWER);
    let host = Dialog::broken();
    let request = mail(Mode::Send, &["anna@example.org"], &[], SUBJECT, BODY);
    let result = run(&transport, &host, &["alice@gmail.example"], &request);
    assert!(is_msg(&result, "NOTHING was done"), "{result:?}");
    assert!(requests(&transport, Method::Post).is_empty());

    for decision in ["approved", "yes", "", "SEND"] {
        let transport = sender().json(Method::Post, "/messages/send", 200, SENT_ANSWER);
        let host = Dialog::deciding(decision);
        let result = run(&transport, &host, &["alice@gmail.example"], &request);
        assert!(
            matches!(result, Err(ToolError::Msg(_))),
            "{decision}: {result:?}"
        );
        assert!(requests(&transport, Method::Post).is_empty(), "{decision}");
    }
}

#[test]
fn a_failing_send_is_uncertain_when_the_outcome_is_unknown_and_never_retried() {
    let request = mail(Mode::Send, &["anna@example.org"], &[], SUBJECT, BODY);
    let unknown: Vec<(&str, Vec<Result<crate::http::Response, HttpError>>)> = vec![
        (
            "timeout",
            vec![
                Err(HttpError::TimedOut),
                crate::http::scripted::ok(SENT_ANSWER),
            ],
        ),
        (
            "broken",
            vec![
                Err(HttpError::Failed("curl exit 52".into())),
                crate::http::scripted::ok(SENT_ANSWER),
            ],
        ),
        (
            "too large",
            vec![
                Err(HttpError::TooLarge),
                crate::http::scripted::ok(SENT_ANSWER),
            ],
        ),
        (
            "500",
            vec![
                Ok(crate::http::Response {
                    status: 500,
                    body: "{}".into(),
                }),
                crate::http::scripted::ok(SENT_ANSWER),
            ],
        ),
        (
            "503",
            vec![
                Ok(crate::http::Response {
                    status: 503,
                    body: "<html>".into(),
                }),
                crate::http::scripted::ok(SENT_ANSWER),
            ],
        ),
        (
            "junk",
            vec![
                Ok(crate::http::Response {
                    status: 200,
                    body: "junk".into(),
                }),
                crate::http::scripted::ok(SENT_ANSWER),
            ],
        ),
    ];
    for (name, replies) in unknown {
        let transport = sender().reply(Method::Post, "/messages/send", replies);
        let host = Dialog::deciding("send");
        let result = run(&transport, &host, &["alice@gmail.example"], &request);
        assert!(
            matches!(result, Err(ToolError::Uncertain(_))),
            "{name}: {result:?}"
        );
        assert_eq!(requests(&transport, Method::Post).len(), 1, "{name}");
    }
    for status in [400, 401, 403, 404, 409, 429] {
        let transport = sender().json(Method::Post, "/messages/send", status, "{}");
        let host = Dialog::deciding("send");
        let result = run(&transport, &host, &["alice@gmail.example"], &request);
        assert!(
            matches!(result, Err(ToolError::Msg(_))),
            "{status}: {result:?}"
        );
        assert_eq!(requests(&transport, Method::Post).len(), 1, "{status}");
    }
}

#[test]
fn a_send_without_confirmation_from_gmail_is_uncertain() {
    let request = mail(Mode::Send, &["anna@example.org"], &[], SUBJECT, BODY);
    for answer in [
        "{}",
        r#"{"id": ""}"#,
        r#"{"id": 5}"#,
        r#"{"threadId": "t"}"#,
        "[]",
        "\"id\"",
        "",
        "null",
    ] {
        let transport = sender().json(Method::Post, "/messages/send", 200, answer);
        let host = Dialog::deciding("send");
        let result = run(&transport, &host, &["alice@gmail.example"], &request);
        assert!(
            matches!(&result, Err(ToolError::Uncertain(text)) if text.contains("Sent in Gmail")),
            "{answer:?}: {result:?}"
        );
        assert_eq!(requests(&transport, Method::Post).len(), 1);
    }
}

#[test]
fn a_draft_without_confirmation_from_gmail_is_uncertain() {
    let request = mail(Mode::Draft, &["anna@example.org"], &[], SUBJECT, BODY);
    for answer in [
        "{}",
        r#"{"id": ""}"#,
        r#"{"message": {"id": "m"}}"#,
        "[]",
        "",
    ] {
        let transport = sender().json(Method::Post, "/drafts", 200, answer);
        let host = Dialog::deciding("send");
        let result = run(&transport, &host, &["alice@gmail.example"], &request);
        assert!(
            matches!(&result, Err(ToolError::Uncertain(text)) if text.contains("Drafts in Gmail")),
            "{answer:?}: {result:?}"
        );
    }
    let transport = sender().json(Method::Post, "/drafts", 500, "{}");
    let host = Dialog::deciding("send");
    assert!(matches!(
        run(&transport, &host, &["alice@gmail.example"], &request),
        Err(ToolError::Uncertain(_))
    ));
    let transport = sender().json(Method::Post, "/drafts", 403, "{}");
    let result = run(&transport, &host, &["alice@gmail.example"], &request);
    assert!(is_msg(&result, "403"), "{result:?}");
    assert_eq!(requests(&transport, Method::Post).len(), 1);
}

fn original() -> String {
    metadata(
        "orig1",
        &["INBOX"],
        "1700000000000",
        &[
            ("From", "Anna Muster <anna@example.org>"),
            ("To", "Alice <alice@gmail.example>, Bob <bob@example.org>"),
            ("Cc", "carl@example.org"),
            ("Subject", "Plan"),
            ("Message-ID", "<orig1@mail.example.org>"),
            ("References", "<root@mail.example.org>"),
        ],
    )
}

fn replying(original: &str) -> Scripted {
    sender()
        .json(
            Method::Get,
            "/messages/orig1?format=metadata",
            200,
            original,
        )
        .json(Method::Post, "/messages/send", 200, SENT_ANSWER)
        .json(Method::Post, "/drafts", 200, DRAFT_ANSWER)
}

const ORIGINAL_URL: &str = concat!(
    "https://gmail.googleapis.com/gmail/v1/users/me/messages/orig1?format=metadata",
    "&metadataHeaders=From&metadataHeaders=To&metadataHeaders=Cc&metadataHeaders=Reply-To",
    "&metadataHeaders=Subject&metadataHeaders=Message-ID&metadataHeaders=References",
    "&metadataHeaders=In-Reply-To"
);

#[test]
fn a_reply_goes_to_the_sender_and_joins_the_thread() -> TestResult {
    let transport = replying(&original());
    let host = Dialog::deciding("send").watching(&transport);
    let request = answer_to(Mode::Send, "orig1", false, "Gerne, bis dann.")?;
    let result = run(&transport, &host, &["alice@gmail.example"], &request)?;
    assert_eq!(
        result,
        Preview {
            reply: Some((
                "Plan".to_string(),
                "Anna Muster <anna@example.org>".to_string()
            )),
            ..preview(
                &["anna@example.org"],
                &[],
                "Re: Plan",
                "Gerne, bis dann.",
                "sent"
            )
        }
    );
    assert_eq!(
        urls(&transport),
        [
            format!("{BASE}/profile"),
            ORIGINAL_URL.to_string(),
            format!("{BASE}/messages/send"),
        ]
    );
    assert_eq!(host.writes_before_the_dialog(), [0]);
    let message = the_post(&transport)?;
    assert_eq!(message.keys, ["raw", "threadId"]);
    assert_eq!(message.thread_id.as_deref(), Some("t-orig1"));
    assert_eq!(
        message.names(),
        [
            "To",
            "Subject",
            "In-Reply-To",
            "References",
            "MIME-Version",
            "Content-Type",
            "Content-Transfer-Encoding"
        ]
    );
    assert_eq!(message.header("To"), Some("anna@example.org"));
    assert_eq!(message.header("Subject"), Some("Re: Plan"));
    assert_eq!(
        message.header("In-Reply-To"),
        Some("<orig1@mail.example.org>")
    );
    assert_eq!(
        message.header("References"),
        Some("<root@mail.example.org> <orig1@mail.example.org>")
    );
    assert_eq!(message.body, "Gerne, bis dann.");
    let shown = dialog_payload(&host)?;
    assert_eq!(
        shown
            .get("reply")
            .and_then(Json::as_object)
            .and_then(|r| r.get("subject"))
            .and_then(Json::as_str),
        Some("Plan")
    );
    assert_eq!(
        shown
            .get("reply")
            .and_then(Json::as_object)
            .and_then(|r| r.get("from"))
            .and_then(Json::as_str),
        Some("Anna Muster <anna@example.org>")
    );
    assert_eq!(
        shown.get("subject").and_then(Json::as_str),
        Some("Re: Plan")
    );
    Ok(())
}

#[test]
fn reply_all_adds_the_other_recipients_but_not_the_own_addresses() -> TestResult {
    let original = metadata(
        "orig1",
        &["INBOX"],
        "1700000000000",
        &[
            ("From", "Anna Muster <anna@example.org>"),
            (
                "To",
                "Alice <ALICE@gmail.example>, Bob <bob@example.org>, anna@example.org",
            ),
            (
                "Cc",
                "carl@example.org, Alias@gmail.example, BOB@example.org",
            ),
            ("Subject", "re: Plan"),
            ("Message-ID", "<orig1@mail.example.org>"),
        ],
    );
    let transport = replying(&original);
    let host = Dialog::deciding("send");
    let request = answer_to(Mode::Send, "orig1", true, "Danke allen.")?;
    let result = run(
        &transport,
        &host,
        &["alice@gmail.example", "alias@gmail.example"],
        &request,
    )?;
    assert_eq!(result.to, ["anna@example.org", "bob@example.org"]);
    assert_eq!(result.cc, ["carl@example.org"]);
    assert_eq!(result.subject, "re: Plan");
    let message = the_post(&transport)?;
    assert_eq!(
        message.header("To"),
        Some("anna@example.org, bob@example.org")
    );
    assert_eq!(message.header("Cc"), Some("carl@example.org"));
    assert_eq!(
        message.header("References"),
        Some("<orig1@mail.example.org>")
    );
    Ok(())
}

#[test]
fn the_reply_to_header_decides_where_an_answer_goes() -> TestResult {
    let original = metadata(
        "orig1",
        &["INBOX"],
        "1700000000000",
        &[
            ("From", "Shop <news@shop.example>"),
            ("Reply-To", "Support <help@shop.example>"),
            ("Subject", "Angebot"),
            ("In-Reply-To", "<earlier@shop.example>"),
            ("Message-ID", "<orig1@shop.example>"),
        ],
    );
    let transport = replying(&original);
    let host = Dialog::deciding("send");
    let request = answer_to(Mode::Send, "orig1", false, "Bitte senden.")?;
    let result = run(&transport, &host, &[], &request)?;
    assert_eq!(result.to, ["help@shop.example"]);
    let message = the_post(&transport)?;
    assert_eq!(
        message.header("References"),
        Some("<earlier@shop.example> <orig1@shop.example>")
    );
    Ok(())
}

#[test]
fn an_own_message_is_answered_to_its_recipients() -> TestResult {
    let original = metadata(
        "orig1",
        &["SENT"],
        "1700000000000",
        &[
            ("From", "Alice <alice@gmail.example>"),
            ("To", "Anna <anna@example.org>, Bob <bob@example.org>"),
            ("Subject", "Plan"),
            ("Message-ID", "<orig1@mail.example.org>"),
        ],
    );
    let transport = replying(&original);
    let host = Dialog::deciding("send");
    let request = answer_to(Mode::Send, "orig1", false, "Nachtrag.")?;
    let result = run(&transport, &host, &["alice@gmail.example"], &request)?;
    assert_eq!(result.to, ["anna@example.org", "bob@example.org"]);
    Ok(())
}

#[test]
fn a_reply_can_be_saved_as_a_draft_in_the_same_thread() -> TestResult {
    let transport = replying(&original());
    let host = Dialog::deciding("send");
    let request = answer_to(Mode::Draft, "orig1", true, "Entwurf.")?;
    let result = run(&transport, &host, &["alice@gmail.example"], &request)?;
    assert_eq!(result.status, "drafted");
    assert_eq!(result.saved, Some(true));
    assert_eq!(result.to, ["anna@example.org", "bob@example.org"]);
    assert_eq!(result.cc, ["carl@example.org"]);
    assert!(host.shown().is_empty());
    let message = the_post(&transport)?;
    assert_eq!(message.keys, ["message"]);
    assert_eq!(
        message.inner_keys,
        Some(vec!["raw".to_string(), "threadId".to_string()])
    );
    assert_eq!(message.thread_id.as_deref(), Some("t-orig1"));
    assert_eq!(
        urls(&transport).last().map(String::as_str),
        Some(format!("{BASE}/drafts").as_str())
    );
    Ok(())
}

#[test]
fn recipients_and_subject_of_the_request_are_ignored_for_a_reply() -> TestResult {
    let transport = replying(&original());
    let host = Dialog::deciding("send");
    let mut request = answer_to(Mode::Send, "orig1", false, "Antwort.")?;
    request.to = vec!["someone@else.example".to_string()];
    request.cc = vec!["other@else.example".to_string()];
    request.subject = "Anderer Betreff".to_string();
    let result = run(&transport, &host, &["alice@gmail.example"], &request)?;
    assert_eq!(result.to, ["anna@example.org"]);
    assert!(result.cc.is_empty());
    assert_eq!(result.subject, "Re: Plan");
    Ok(())
}

#[test]
fn an_unknown_original_stops_everything_before_the_dialog() -> TestResult {
    let transport = sender()
        .json(Method::Get, "/messages/orig1?format=metadata", 404, "{}")
        .json(Method::Post, "/messages/send", 200, SENT_ANSWER);
    let host = Dialog::deciding("send");
    let request = answer_to(Mode::Send, "orig1", false, "Antwort.")?;
    assert_eq!(
        run(&transport, &host, &["alice@gmail.example"], &request),
        Err(message_missing())
    );
    assert!(host.shown().is_empty());
    assert_eq!(
        urls(&transport),
        [format!("{BASE}/profile"), ORIGINAL_URL.to_string()]
    );
    for body in ["[]", "\"x\"", "null"] {
        let transport = sender().json(Method::Get, "/messages/orig1?format=metadata", 200, body);
        assert_eq!(
            run(&transport, &host, &["alice@gmail.example"], &request),
            Err(unexpected()),
            "{body}"
        );
    }
    let transport = sender().json(Method::Get, "/messages/orig1?format=metadata", 500, "{}");
    let result = run(&transport, &host, &["alice@gmail.example"], &request);
    assert!(is_msg(&result, "temporary problem"), "{result:?}");
    Ok(())
}

#[test]
fn the_id_of_the_original_is_percent_encoded() -> TestResult {
    let transport = sender().json(Method::Get, "/messages/a%2Fb%2Bc", 404, "{}");
    let host = Dialog::deciding("send");
    let request = answer_to(Mode::Send, "a/b+c", false, "x")?;
    let _ = run(&transport, &host, &[], &request);
    assert!(urls(&transport)
        .iter()
        .any(|u| u.contains("/messages/a%2Fb%2Bc?format=metadata")));
    Ok(())
}

#[test]
fn a_reply_with_too_many_recipients_is_refused_before_the_dialog() -> TestResult {
    let original = metadata(
        "orig1",
        &["INBOX"],
        "1700000000000",
        &[
            ("From", "a@example.org"),
            ("To", "b@example.org, c@example.org, d@example.org"),
            ("Cc", "e@example.org, f@example.org"),
            ("Subject", "Gross"),
        ],
    );
    let transport = replying(&original);
    let host = Dialog::deciding("send");
    let send = answer_to(Mode::Send, "orig1", true, "x")?;
    let result = run(&transport, &host, &[], &send);
    assert_eq!(
        result,
        Err(ToolError::msg(map_error(
            "too_many_recipients",
            Some("compose")
        )))
    );
    assert!(host.shown().is_empty());
    assert!(requests(&transport, Method::Post).is_empty());
    let draft = answer_to(Mode::Draft, "orig1", true, "x")?;
    let result = run(&transport, &host, &[], &draft)?;
    assert_eq!(result.to.len() + result.cc.len(), 6);
    let answered = answer_to(Mode::Send, "orig1", false, "x")?;
    assert_eq!(run(&transport, &host, &[], &answered)?.status, "sent");
    Ok(())
}

#[test]
fn a_reply_without_anyone_to_write_to_is_refused() -> TestResult {
    let original = metadata(
        "orig1",
        &["SENT"],
        "1700000000000",
        &[
            ("From", "alice@gmail.example"),
            ("To", "alice@gmail.example"),
            ("Subject", "Notiz"),
        ],
    );
    let transport = replying(&original);
    let host = Dialog::deciding("send");
    let request = answer_to(Mode::Send, "orig1", true, "x")?;
    let result = run(&transport, &host, &["alice@gmail.example"], &request);
    assert_eq!(
        result,
        Err(ToolError::msg(map_error("no_recipients", Some("compose"))))
    );
    let bare = metadata(
        "orig1",
        &["INBOX"],
        "1700000000000",
        &[("Subject", "Ohne Absender")],
    );
    let transport = replying(&bare);
    let result = run(
        &transport,
        &host,
        &["alice@gmail.example"],
        &answer_to(Mode::Draft, "orig1", false, "x")?,
    );
    assert_eq!(
        result,
        Err(ToolError::msg(map_error("no_recipients", Some("compose"))))
    );
    assert!(requests(&transport, Method::Post).is_empty());
    assert!(host.shown().is_empty());
    Ok(())
}

#[test]
fn hostile_headers_of_the_original_cannot_inject_headers_into_the_answer() -> TestResult {
    let original = metadata(
        "orig1",
        &["INBOX"],
        "1700000000000",
        &[
            (
                "From",
                "\"Evil\r\nBcc: spy@example.org\" <evil@example.org>",
            ),
            ("Subject", "Hallo\r\nBcc: spy@example.org\u{202e}\u{200b}"),
            ("Message-ID", "<id@x.example>\r\nBcc: spy@example.org"),
            (
                "References",
                "<ok@x.example> <bad\r\nBcc: spy@example.org> <also bad@x.example>",
            ),
        ],
    );
    let transport = replying(&original);
    let host = Dialog::deciding("send");
    let request = answer_to(Mode::Send, "orig1", false, "Antwort.")?;
    let result = run(&transport, &host, &["alice@gmail.example"], &request)?;
    assert_eq!(result.subject, "Re: Hallo Bcc: spy@example.org");
    assert_eq!(result.to, ["evil@example.org"]);
    assert_eq!(
        result.reply,
        Some((
            "Hallo Bcc: spy@example.org".to_string(),
            "\"Evil Bcc: spy@example.org\" <evil@example.org>".to_string()
        ))
    );
    let message = the_post(&transport)?;
    assert!(message.header("Bcc").is_none());
    assert_eq!(
        message.names(),
        [
            "To",
            "Subject",
            "In-Reply-To",
            "References",
            "MIME-Version",
            "Content-Type",
            "Content-Transfer-Encoding"
        ]
    );
    assert_eq!(message.header("In-Reply-To"), Some("<id@x.example>"));
    assert_eq!(
        message.header("References"),
        Some("<ok@x.example> <id@x.example>")
    );
    Ok(())
}

#[test]
fn a_very_long_original_subject_is_cut_so_that_the_dialog_shows_all_of_it() -> TestResult {
    let original = metadata(
        "orig1",
        &["INBOX"],
        "1700000000000",
        &[("From", "a@example.org"), ("Subject", &"x".repeat(5000))],
    );
    let transport = replying(&original);
    let host = Dialog::deciding("send");
    let request = answer_to(Mode::Send, "orig1", false, "ok")?;
    let result = run(&transport, &host, &[], &request)?;
    assert!(
        result.subject.chars().count() <= 210,
        "{}",
        result.subject.chars().count()
    );
    assert!(result.subject.starts_with("Re: xxx"));
    let message = the_post(&transport)?;
    assert_eq!(
        message.header("Subject").map(|s| s.to_string()),
        Some(result.subject)
    );
    Ok(())
}

fn addresses(count: usize) -> Vec<String> {
    (0..count).map(|n| format!("user{n}@example.org")).collect()
}

fn borrowed(items: &[String]) -> Vec<&str> {
    items.iter().map(String::as_str).collect()
}

#[test]
fn at_most_five_recipients_may_be_sent_to_but_drafts_allow_many_more() -> TestResult {
    let five = addresses(5);
    let five = borrowed(&five);
    let (five_to, five_cc) = five.split_at_checked(3).ok_or("split")?;
    let transport = sender().json(Method::Post, "/messages/send", 200, SENT_ANSWER);
    let host = Dialog::deciding("send");
    let request = mail(Mode::Send, five_to, five_cc, "s", "b");
    assert_eq!(run(&transport, &host, &[], &request)?.status, "sent");

    let six = addresses(6);
    let six = borrowed(&six);
    let (six_to, six_cc) = six.split_at_checked(3).ok_or("split")?;
    let transport = sender().json(Method::Post, "/messages/send", 200, SENT_ANSWER);
    let host = Dialog::deciding("send");
    let request = mail(Mode::Send, six_to, six_cc, "s", "b");
    assert_eq!(
        run(&transport, &host, &[], &request),
        Err(ToolError::msg(map_error(
            "too_many_recipients",
            Some("compose")
        )))
    );
    assert!(host.shown().is_empty());
    assert_eq!(urls(&transport), [format!("{BASE}/profile")]);

    for (count, allowed) in [
        (6, true),
        (MAX_DRAFT_RECIPIENTS, true),
        (MAX_DRAFT_RECIPIENTS + 1, false),
    ] {
        let many = addresses(count);
        let many = borrowed(&many);
        let transport = sender().json(Method::Post, "/drafts", 200, DRAFT_ANSWER);
        let host = Dialog::deciding("send");
        let (to, cc) = many.split_at(1);
        let result = run(&transport, &host, &[], &mail(Mode::Draft, to, cc, "s", "b"));
        if allowed {
            assert_eq!(result?.status, "drafted", "{count}");
        } else {
            let text = "The draft would have more than 50 recipients, so NOTHING was saved.";
            assert_eq!(result, Err(ToolError::msg(text)));
            assert!(requests(&transport, Method::Post).is_empty());
        }
    }
    Ok(())
}

#[test]
fn a_message_needs_at_least_one_to_recipient() {
    for mode in [Mode::Send, Mode::Draft] {
        for request in [
            mail(mode, &[], &[], "s", "b"),
            mail(mode, &[], &["cc@example.org"], "s", "b"),
        ] {
            let transport = sender()
                .json(Method::Post, "/messages/send", 200, SENT_ANSWER)
                .json(Method::Post, "/drafts", 200, DRAFT_ANSWER);
            let host = Dialog::deciding("send");
            assert_eq!(
                run(&transport, &host, &[], &request),
                Err(ToolError::msg(map_error("no_recipients", Some("compose"))))
            );
            assert!(requests(&transport, Method::Post).is_empty());
            assert!(host.shown().is_empty());
        }
    }
}

#[test]
fn recipients_must_be_plain_ascii_addresses_in_both_modes() {
    let long = format!("{}@example.org", "a".repeat(65));
    let bad = [
        "o'neil@example.org",
        "\u{e4}@example.org",
        "a@b",
        "a b@example.org",
        "user@exam_ple.org",
        "a@example.org.",
        "a@@example.org",
        "name <a@example.org>",
        "\"a\"@example.org",
        "",
        long.as_str(),
    ];
    for address in bad {
        for (mode, text) in [
            (Mode::Send, map_error("recipient_invalid", Some("compose"))),
            (
                Mode::Draft,
                "A recipient address is not a plain ASCII address (at most 254 characters), so NOTHING was saved.".to_string(),
            ),
        ] {
            for (to, cc) in [(vec![address], vec![]), (vec!["ok@example.org"], vec![address])] {
                let transport = sender()
                    .json(Method::Post, "/messages/send", 200, SENT_ANSWER)
                    .json(Method::Post, "/drafts", 200, DRAFT_ANSWER);
                let host = Dialog::deciding("send");
                let result = run(&transport, &host, &[], &mail(mode, &to, &cc, "s", "b"));
                assert_eq!(result, Err(ToolError::msg(text.clone())), "{address:?} {mode:?}");
                assert!(requests(&transport, Method::Post).is_empty());
                assert!(host.shown().is_empty());
            }
        }
    }
}

#[test]
fn duplicate_recipients_are_sent_once_without_regard_to_case() -> TestResult {
    let transport = sender().json(Method::Post, "/messages/send", 200, SENT_ANSWER);
    let host = Dialog::deciding("send");
    let request = mail(
        Mode::Send,
        &["a@x.example", "A@X.example", "b@x.example"],
        &["a@x.example", "c@x.example", "C@X.EXAMPLE"],
        "s",
        "b",
    );
    let result = run(&transport, &host, &[], &request)?;
    assert_eq!(result.to, ["a@x.example", "b@x.example"]);
    assert_eq!(result.cc, ["c@x.example"]);
    let message = the_post(&transport)?;
    assert_eq!(message.header("To"), Some("a@x.example, b@x.example"));
    assert_eq!(message.header("Cc"), Some("c@x.example"));
    Ok(())
}

#[test]
fn requests_that_could_confuse_the_dialog_or_the_headers_are_refused_before_any_request() {
    let long_subject = "s".repeat(MAX_SUBJECT + 1);
    let huge_body = "a".repeat(MAX_BODY_BYTES + 1);
    let single_line = "The subject must be a single line without control or invisible characters.";
    let cases: Vec<(ComposeRequest, String)> = vec![
        (mail(Mode::Send, &["a@example.org"], &[], "Hallo\r\nBcc: evil@example.org", "b"), single_line.into()),
        (mail(Mode::Draft, &["a@example.org"], &[], "a\nb", "b"), single_line.into()),
        (mail(Mode::Send, &["a@example.org"], &[], "a\rb", "b"), single_line.into()),
        (mail(Mode::Send, &["a@example.org"], &[], "a\tb", "b"), single_line.into()),
        (mail(Mode::Send, &["a@example.org"], &[], "a\u{0}b", "b"), single_line.into()),
        (mail(Mode::Send, &["a@example.org"], &[], "a\u{202e}b", "b"), single_line.into()),
        (mail(Mode::Send, &["a@example.org"], &[], "a\u{200b}b", "b"), single_line.into()),
        (mail(Mode::Send, &["a@example.org"], &[], &long_subject, "b"), "The subject is too long (max 200 characters).".into()),
        (mail(Mode::Send, &["a@example.org\r\nBcc: evil@example.org"], &[], "s", "b"), "An address contains a line break.".into()),
        (mail(Mode::Draft, &["a@example.org"], &["b@example.org\n"], "s", "b"), "An address contains a line break.".into()),
        (mail(Mode::Send, &["a@example.org"], &[], "s", "text\u{202e}evil"), "The text contains control or invisible characters that the approval dialog cannot show.".into()),
        (mail(Mode::Send, &["a@example.org"], &[], "s", "text\u{200b}"), "The text contains control or invisible characters that the approval dialog cannot show.".into()),
        (mail(Mode::Draft, &["a@example.org"], &[], "s", "te\u{0}xt"), "The text contains control or invisible characters that the approval dialog cannot show.".into()),
        (mail(Mode::Draft, &["a@example.org"], &[], "s", &huge_body), "The text is too long.".into()),
    ];
    for (request, reason) in cases {
        let transport = sender()
            .json(Method::Post, "/messages/send", 200, SENT_ANSWER)
            .json(Method::Post, "/drafts", 200, DRAFT_ANSWER);
        let host = Dialog::deciding("send");
        let result = run(&transport, &host, &["alice@gmail.example"], &request);
        assert_eq!(
            result,
            Err(ToolError::msg(format!(
                "{reason} NOTHING was sent or saved."
            ))),
            "{request:?}"
        );
        assert!(transport.seen().is_empty(), "{request:?}");
        assert!(host.shown().is_empty());
    }
}

#[test]
fn tabs_and_line_breaks_are_fine_in_the_text_and_the_largest_text_is_accepted() -> TestResult {
    let text = "  eingerueckt\n\tTab\r\nWindows\rMac  ";
    let transport = sender().json(Method::Post, "/drafts", 200, DRAFT_ANSWER);
    let host = Dialog::deciding("send");
    let result = run(
        &transport,
        &host,
        &[],
        &mail(Mode::Draft, &["a@example.org"], &[], "s", text),
    )?;
    assert_eq!(result.body, text);
    assert_eq!(
        the_post(&transport)?.body,
        "  eingerueckt\r\n\tTab\r\nWindows\r\nMac  "
    );

    let biggest = "a".repeat(MAX_BODY_BYTES);
    let transport = sender().json(Method::Post, "/drafts", 200, DRAFT_ANSWER);
    run(
        &transport,
        &host,
        &[],
        &mail(Mode::Draft, &["a@example.org"], &[], "s", &biggest),
    )?;
    assert_eq!(the_post(&transport)?.body, biggest);
    Ok(())
}

#[test]
fn only_an_account_that_matches_the_signed_in_address_may_compose() {
    for mode in [Mode::Send, Mode::Draft] {
        let transport = sender()
            .json(Method::Post, "/messages/send", 200, SENT_ANSWER)
            .json(Method::Post, "/drafts", 200, DRAFT_ANSWER);
        let host = Dialog::deciding("send");
        let request = mail(mode, &["a@example.org"], &[], "s", "b");
        let result = run(
            &transport,
            &host,
            &["other@gmail.example", "third@gmail.example"],
            &request,
        );
        assert_eq!(
            result,
            Err(ToolError::msg(map_error(
                "sender_mismatch",
                Some("compose")
            ))),
            "{mode:?}"
        );
        assert_eq!(urls(&transport), [format!("{BASE}/profile")]);
        assert!(host.shown().is_empty());
    }
}

#[test]
fn the_shown_sender_is_the_signed_in_address_however_the_account_lists_it() -> TestResult {
    let configurations: [&[&str]; 5] = [
        &[],
        &["alice@gmail.example"],
        &["ALICE@GMAIL.EXAMPLE"],
        &["alias@gmail.example", "Alice@gmail.example"],
        &["  alice@gmail.example  ", ""],
    ];
    for configured in configurations {
        let transport = sender().json(Method::Post, "/drafts", 200, DRAFT_ANSWER);
        let host = Dialog::deciding("send");
        let request = mail(Mode::Draft, &["a@example.org"], &[], "s", "b");
        let result = run(&transport, &host, configured, &request)?;
        assert_eq!(result.from, "alice@gmail.example", "{configured:?}");
    }
    let original = metadata(
        "orig1",
        &["INBOX"],
        "1700000000000",
        &[
            ("From", "Anna <anna@example.org>"),
            ("To", "alice@gmail.example, bob@example.org"),
            ("Subject", "Plan"),
        ],
    );
    let transport = replying(&original);
    let host = Dialog::deciding("send");
    let request = answer_to(Mode::Send, "orig1", true, "x")?;
    let result = run(&transport, &host, &[], &request)?;
    assert_eq!(result.to, ["anna@example.org", "bob@example.org"]);
    Ok(())
}

#[test]
fn a_profile_that_cannot_be_read_stops_every_compose_call() {
    for (status, body) in [
        (401, "{}"),
        (403, "{}"),
        (500, "{}"),
        (200, "{}"),
        (200, r#"{"emailAddress": "x"}"#),
        (200, "junk"),
    ] {
        let transport = Scripted::new()
            .json(Method::Get, "/profile", status, body)
            .json(Method::Post, "/messages/send", 200, SENT_ANSWER)
            .json(Method::Post, "/drafts", 200, DRAFT_ANSWER);
        for mode in [Mode::Send, Mode::Draft] {
            let host = Dialog::deciding("send");
            let result = run(
                &transport,
                &host,
                &[],
                &mail(mode, &["a@example.org"], &[], "s", "b"),
            );
            assert!(
                matches!(result, Err(ToolError::Msg(_))),
                "{status} {body}: {result:?}"
            );
            assert!(host.shown().is_empty());
        }
        assert!(requests(&transport, Method::Post).is_empty());
    }
}

#[test]
fn a_non_ascii_subject_is_encoded_and_no_header_line_gets_long() -> TestResult {
    let subject = "\u{dc}berpr\u{fc}fung der Rechnung f\u{fc}r M\u{e4}rz \u{2013} bitte bis Freitag best\u{e4}tigen, danke f\u{fc}r Ihre Hilfe!";
    let transport = sender().json(Method::Post, "/drafts", 200, DRAFT_ANSWER);
    let host = Dialog::deciding("send");
    let result = run(
        &transport,
        &host,
        &[],
        &mail(Mode::Draft, &["a@example.org"], &[], subject, "b"),
    )?;
    assert_eq!(result.subject, subject);
    let message = the_post(&transport)?;
    assert!(
        message.raw.split("\r\n").all(|line| line.len() <= 76),
        "{}",
        message.raw
    );
    let header_value = message.header("Subject").ok_or("subject")?;
    assert!(header_value.is_ascii());
    assert_eq!(header::text(header_value, 500), subject);
    Ok(())
}

#[test]
fn long_ascii_subjects_and_recipient_lists_are_folded_and_unfold_to_the_original() -> TestResult {
    let subject = (0..30)
        .map(|n| format!("wort{n}"))
        .collect::<Vec<_>>()
        .join(" ");
    let long = [
        "firstname.lastname.one@subdomain.example.org",
        "firstname.lastname.two@subdomain.example.org",
        "firstname.lastname.three@subdomain.example.org",
        "firstname.lastname.four@subdomain.example.org",
        "firstname.lastname.five@subdomain.example.org",
    ];
    let transport = sender().json(Method::Post, "/messages/send", 200, SENT_ANSWER);
    let host = Dialog::deciding("send");
    run(
        &transport,
        &host,
        &[],
        &mail(Mode::Send, &long, &[], &subject, "b"),
    )?;
    let message = the_post(&transport)?;
    assert!(
        message
            .raw
            .split("\r\n")
            .all(|line| line.len() <= 76 || !line.contains(' ')),
        "{}",
        message.raw
    );
    assert_eq!(message.header("Subject"), Some(subject.as_str()));
    assert_eq!(message.header("To"), Some(long.join(", ").as_str()));
    assert!(message.raw.contains(",\r\n firstname"));
    Ok(())
}

#[test]
fn the_text_in_the_message_is_the_text_in_the_dialog_with_crlf_line_ends() -> TestResult {
    let body = "Zeile 1\nZeile 2\r\nZeile 3\rZeile 4\n\n\u{20ac}-Betrag: 5 \u{fc}ber 3";
    let transport = sender().json(Method::Post, "/messages/send", 200, SENT_ANSWER);
    let host = Dialog::deciding("send");
    run(
        &transport,
        &host,
        &[],
        &mail(Mode::Send, &["a@example.org"], &[], "s", body),
    )?;
    let shown = dialog_payload(&host)?;
    assert_eq!(shown.get("body").and_then(Json::as_str), Some(body));
    let message = the_post(&transport)?;
    assert_eq!(
        message.body,
        "Zeile 1\r\nZeile 2\r\nZeile 3\r\nZeile 4\r\n\r\n\u{20ac}-Betrag: 5 \u{fc}ber 3"
    );
    let long: String = (0..400)
        .map(|n| format!("Zeile {n} mit etwas l\u{e4}ngerem Text\n"))
        .collect::<String>()
        .trim()
        .to_string();
    let transport = sender().json(Method::Post, "/messages/send", 200, SENT_ANSWER);
    run(
        &transport,
        &host_for_long(),
        &[],
        &mail(Mode::Send, &["a@example.org"], &[], "s", &long),
    )?;
    assert_eq!(the_post(&transport)?.body, long.replace('\n', "\r\n"));
    assert!(the_post(&transport)?
        .raw
        .split("\r\n")
        .all(|line| line.len() <= 76));
    Ok(())
}

fn host_for_long() -> Dialog<'static> {
    Dialog::deciding("send")
}

#[test]
fn the_message_never_carries_sender_date_message_id_or_hidden_recipients() -> TestResult {
    let transport = sender().json(Method::Post, "/messages/send", 200, SENT_ANSWER);
    let host = Dialog::deciding("send");
    run(
        &transport,
        &host,
        &["alice@gmail.example"],
        &mail(Mode::Send, &["a@example.org"], &["b@example.org"], "s", "b"),
    )?;
    let message = the_post(&transport)?;
    for name in [
        "From",
        "Sender",
        "Date",
        "Message-ID",
        "Bcc",
        "Reply-To",
        "Return-Path",
        "X-Mailer",
    ] {
        assert!(message.header(name).is_none(), "{name}");
    }
    assert!(!message.raw.to_lowercase().contains("bcc"));
    Ok(())
}

#[test]
fn the_write_requests_use_the_write_timeout_and_the_token() -> TestResult {
    let transport = sender().json(Method::Post, "/messages/send", 200, SENT_ANSWER);
    let host = Dialog::deciding("send");
    run(
        &transport,
        &host,
        &[],
        &mail(Mode::Send, &["a@example.org"], &[], "s", "b"),
    )?;
    let posts = requests(&transport, Method::Post);
    let post = posts.first().ok_or("post")?;
    assert_eq!(post.timeout, super::super::WRITE_TIMEOUT);
    assert_eq!(
        header_of(post, "Authorization").as_deref(),
        Some("Bearer TOKEN")
    );
    assert_eq!(
        header_of(post, "Content-Type").as_deref(),
        Some("application/json")
    );
    let gets = requests(&transport, Method::Get);
    assert!(gets
        .iter()
        .all(|get| get.timeout == super::super::READ_TIMEOUT && get.body.is_none()));
    Ok(())
}

#[test]
fn an_unsafe_letter_is_refused_by_the_builder_stage() {
    let letter = |to: &str, subject: &str, parent: Option<&str>| Letter {
        to: vec![to.to_string()],
        cc: Vec::new(),
        subject: subject.to_string(),
        in_reply_to: parent.map(str::to_string),
        references: Vec::new(),
        thread_id: None,
        reply: None,
    };
    assert!(raw_message(&letter("a@example.org", "ok", None), "text").is_ok());
    for bad in [
        letter("a@example.org\r\nBcc: x@example.org", "ok", None),
        letter("a@example.org", "bad\r\nBcc: x@example.org", None),
        letter("a@example.org", "ok", Some("<a\r\nBcc: x@example.org>")),
        letter("", "ok", None),
    ] {
        assert_eq!(
            raw_message(&bad, "text"),
            Err(ToolError::msg(
                "The message could not be built safely. NOTHING was sent or saved."
            ))
        );
    }
}

#[test]
fn usable_addresses_are_single_plain_tokens() {
    for good in [
        "alice@gmail.example",
        "a.b+c@sub.example.org",
        "o'neil@example.org",
        "x@y",
    ] {
        assert!(usable_address(good), "{good}");
    }
    let long = format!("{}@example.org", "a".repeat(250));
    for bad in [
        "",
        "alice",
        "@x.example",
        "alice@",
        "a@b@c",
        "a b@x.example",
        "a@x.example\n",
        "<a@x.example>",
        "a,b@x.example",
        "a;b@x.example",
        "\u{e4}@x.example",
        "a\"b@x.example",
        "a[b]@x.example",
        "a:b@x.example",
        long.as_str(),
    ] {
        assert!(!usable_address(bad), "{bad:?}");
    }
}
