use super::{
    email, field, message_path, person_text, unexpected, url, MAX_ID, MAX_TEXT, NO_MESSAGE,
};
use crate::account::Account;
use crate::backend::{ComposeRequest, MessageId, Mode, Preview};
use crate::error::{ToolError, ToolResult};
use crate::http::Method;
use crate::json::{Json, Object};
use crate::mailerror::map_error;
use crate::osascript::Host;
use crate::provider::account_address;
use crate::provider::dialog::{approve_send, Verdict};
use crate::provider::Api;
use crate::text::{
    char_len, clean_str, is_hidden, one_line_str, plain_address, take_chars, MAX_ADDRESS,
};
use crate::texts::{APPROVAL_SECONDS, MAX_RECIPIENTS};

const DRAFT_FIELDS: &str =
    "id,subject,from,toRecipients,ccRecipients,bccRecipients,body,hasAttachments";
const MAX_DRAFT_RECIPIENTS: usize = 50;
const MAX_READ_RECIPIENTS: usize = 100;
const MAX_RAW_SUBJECT: usize = 1000;
const EXTRA_CHARS: usize = 1500;
const NOT_SENT: &str = "NOTHING was sent.";
const LEFTOVER: &str = "A draft may remain in the Drafts folder.";

#[derive(Clone, Debug, PartialEq, Eq)]
struct Draft {
    from: String,
    to: Vec<String>,
    cc: Vec<String>,
    bcc: usize,
    attachments: bool,
    subject: String,
    kind: String,
    body: String,
}

struct Own {
    shown: String,
    listed: Vec<String>,
}

impl Own {
    fn canonical(&self, address: &str) -> Option<&str> {
        self.listed
            .iter()
            .find(|known| known.eq_ignore_ascii_case(address))
            .map(String::as_str)
    }
}

fn refusal(code: &str) -> ToolError {
    ToolError::msg(map_error(code, Some("compose")))
}

fn mismatch(mode: Mode) -> ToolError {
    match mode {
        Mode::Send => refusal("content_mismatch"),
        Mode::Draft => ToolError::msg(format!(
            "The draft that Outlook.com saved differs from the text you gave, so it was not kept. {NOT_SENT}"
        )),
    }
}

fn before_send(error: ToolError) -> ToolError {
    match error {
        ToolError::Msg(text) if text.contains("NOTHING") => ToolError::Msg(text),
        ToolError::Msg(text) => ToolError::Msg(format!("{text} {NOT_SENT}")),
        ToolError::Uncertain(_) => ToolError::Msg(format!(
            "The provider did not confirm a step while the message was prepared. {NOT_SENT}"
        )),
        internal @ ToolError::Internal(_) => internal,
    }
}

fn unplaced(error: ToolError) -> ToolError {
    match error {
        ToolError::Uncertain(_) => ToolError::Msg(format!(
            "The provider did not confirm that the draft was created. {NOT_SENT} {LEFTOVER}"
        )),
        other => before_send(other),
    }
}

fn own(account: &Account) -> ToolResult<Own> {
    let mut listed: Vec<String> = account
        .addresses
        .iter()
        .map(|address| address.trim().to_string())
        .filter(|address| !address.is_empty())
        .collect();
    if let Some((_, address)) = account_address(&account.id) {
        listed.push(address.to_string());
    }
    let shown = listed
        .first()
        .cloned()
        .ok_or_else(|| refusal("no_sender_address"))?;
    Ok(Own { shown, listed })
}

fn check_recipients(mode: Mode, to: &[String], cc: &[String]) -> ToolResult<()> {
    if to.is_empty() {
        return Err(refusal("no_recipients"));
    }
    let count = to.len() + cc.len();
    match mode {
        Mode::Send => {
            if count > MAX_RECIPIENTS {
                return Err(refusal("too_many_recipients"));
            }
            if !to.iter().chain(cc).all(|address| plain_address(address)) {
                return Err(refusal("recipient_invalid"));
            }
        }
        Mode::Draft => {
            if count > MAX_DRAFT_RECIPIENTS {
                return Err(ToolError::msg(format!(
                    "The draft would have more than {MAX_DRAFT_RECIPIENTS} recipients, so it was not kept. {NOT_SENT} Answer without reply_all or write to fewer people."
                )));
            }
        }
    }
    Ok(())
}

fn squash(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

fn body_ok(draft: &Draft, wanted: &str) -> bool {
    if draft.kind != "text" || draft.body.chars().any(is_hidden) {
        return false;
    }
    let have = squash(&draft.body);
    let want = squash(wanted);
    have.contains(&want)
        && char_len(&have) <= char_len(&want) + EXTRA_CHARS
        && char_len(&draft.body.replace("\r\n", "\n")) <= char_len(wanted) + 2 * EXTRA_CHARS
}

fn vet(draft: &Draft, own: &Own, request: &ComposeRequest) -> ToolResult<()> {
    if own.canonical(&draft.from).is_none() {
        return Err(refusal("sender_mismatch"));
    }
    check_recipients(request.mode, &draft.to, &draft.cc)?;
    if request.mode == Mode::Send && (draft.bcc > 0 || draft.attachments) {
        return Err(mismatch(request.mode));
    }
    if body_ok(draft, &request.body) {
        Ok(())
    } else {
        Err(mismatch(request.mode))
    }
}

fn recipients_json(addresses: &[String]) -> Json {
    Json::Arr(
        addresses
            .iter()
            .map(|address| {
                Json::object(vec![(
                    "emailAddress",
                    Json::object(vec![("address", Json::from(address.as_str()))]),
                )])
            })
            .collect(),
    )
}

fn text_body(content: &str) -> Json {
    Json::object(vec![
        ("contentType", Json::from("text")),
        ("content", Json::from(content)),
    ])
}

fn new_message(request: &ComposeRequest) -> Json {
    Json::object(vec![
        ("subject", Json::from(request.subject.as_str())),
        ("body", text_body(&request.body)),
        ("toRecipients", recipients_json(&request.to)),
        ("ccRecipients", recipients_json(&request.cc)),
    ])
}

fn draft_id(created: &Json) -> ToolResult<String> {
    created
        .as_object()
        .map(|message| field(message, "id"))
        .filter(|id| !id.is_empty() && id.len() <= MAX_ID)
        .map(str::to_string)
        .ok_or_else(|| {
            ToolError::msg(format!(
                "Outlook.com did not return the new draft. {NOT_SENT} {LEFTOVER}"
            ))
        })
}

fn original(api: &Api, id: &MessageId) -> ToolResult<(String, String)> {
    let address = url(&message_path(id.as_str()), &[("$select", "subject,from")]);
    let Some(json) = api.get_optional(&address).map_err(before_send)? else {
        return Err(ToolError::msg(NO_MESSAGE));
    };
    let message = json.as_object().ok_or_else(unexpected)?;
    let (name, from) = email(message.get("from"));
    Ok((
        one_line_str(field(message, "subject"), MAX_TEXT),
        person_text(name, from),
    ))
}

fn start(api: &Api, request: &ComposeRequest) -> ToolResult<String> {
    let created = match &request.reply {
        None => api.send(
            Method::Post,
            &url("/messages", &[]),
            Some(&new_message(request)),
        )?,
        Some(spec) => {
            let action = if spec.all {
                "createReplyAll"
            } else {
                "createReply"
            };
            api.send(
                Method::Post,
                &url(&format!("{}/{action}", message_path(spec.id.as_str())), &[]),
                Some(&Json::Obj(Object::new())),
            )?
        }
    };
    draft_id(&created)
}

fn fill(api: &Api, id: &str, request: &ComposeRequest) -> ToolResult<()> {
    if request.reply.is_none() {
        return Ok(());
    }
    let patch = Json::object(vec![("body", text_body(&request.body))]);
    api.send(Method::Patch, &url(&message_path(id), &[]), Some(&patch))
        .map(|_| ())
}

fn recipients(value: Option<&Json>) -> Vec<String> {
    value
        .and_then(Json::as_array)
        .map_or_else(Vec::new, |items| {
            items
                .iter()
                .take(MAX_READ_RECIPIENTS)
                .map(|item| email(Some(item)).1.to_string())
                .collect()
        })
}

fn read_back(api: &Api, id: &str, own: &Own) -> ToolResult<Draft> {
    let json = api.get(&url(&message_path(id), &[("$select", DRAFT_FIELDS)]))?;
    let message = json.as_object().ok_or_else(unexpected)?;
    let body = message.get("body").and_then(Json::as_object);
    let from = email(message.get("from")).1;
    let sender = match own.canonical(from) {
        Some(address) => address,
        None if from.is_empty() => own.shown.as_str(),
        None => from,
    };
    Ok(Draft {
        from: sender.to_string(),
        to: recipients(message.get("toRecipients")),
        cc: recipients(message.get("ccRecipients")),
        bcc: message
            .get("bccRecipients")
            .and_then(Json::as_array)
            .map_or(0, Vec::len),
        attachments: message.get("hasAttachments") == Some(&Json::Bool(true)),
        subject: take_chars(field(message, "subject"), MAX_RAW_SUBJECT),
        kind: body
            .map(|found| field(found, "contentType").to_ascii_lowercase())
            .unwrap_or_default(),
        body: body
            .map(|found| field(found, "content").to_string())
            .unwrap_or_default(),
    })
}

fn inspect(api: &Api, id: &str, own: &Own, request: &ComposeRequest) -> ToolResult<Draft> {
    fill(api, id, request).map_err(before_send)?;
    let draft = read_back(api, id, own).map_err(before_send)?;
    vet(&draft, own, request)?;
    Ok(draft)
}

fn discard(api: &Api, id: &str) -> bool {
    api.send(Method::Delete, &url(&message_path(id), &[]), None)
        .is_ok()
}

fn abandon(api: &Api, id: &str, error: ToolError) -> ToolError {
    let removed = discard(api, id);
    match error {
        ToolError::Msg(text) if !removed => ToolError::Msg(format!("{text} {LEFTOVER}")),
        other => other,
    }
}

fn shown_addresses(addresses: &[String]) -> Vec<String> {
    addresses
        .iter()
        .map(|address| one_line_str(address, MAX_ADDRESS))
        .collect()
}

fn preview_of(draft: &Draft, reply: Option<(String, String)>) -> Preview {
    Preview {
        from: one_line_str(&draft.from, MAX_ADDRESS),
        to: shown_addresses(&draft.to),
        cc: shown_addresses(&draft.cc),
        subject: one_line_str(&draft.subject, MAX_TEXT),
        body: clean_str(&draft.body, None),
        reply,
        status: String::new(),
        saved: None,
    }
}

fn send(api: &Api, id: &str, own: &Own, approved: &Draft, preview: Preview) -> ToolResult<Preview> {
    match read_back(api, id, own) {
        Ok(current) if current == *approved => {}
        Ok(_) => return Err(abandon(api, id, refusal("content_mismatch"))),
        Err(error) => return Err(abandon(api, id, before_send(error))),
    }
    let sender = api.with_header("Content-Length", "0");
    sender.send(
        Method::Post,
        &url(&format!("{}/send", message_path(id)), &[]),
        None,
    )?;
    Ok(Preview {
        status: "sent".to_string(),
        ..preview
    })
}

fn deliver(
    api: &Api,
    host: &dyn Host,
    id: &str,
    own: &Own,
    approved: &Draft,
    preview: Preview,
) -> ToolResult<Preview> {
    let verdict = match approve_send(host, &preview, APPROVAL_SECONDS) {
        Ok(verdict) => verdict,
        Err(error) => return Err(abandon(api, id, error)),
    };
    let status = match verdict {
        Verdict::Approved => return send(api, id, own, approved, preview),
        Verdict::Declined => "declined",
        Verdict::TimedOut => "timeout",
    };
    discard(api, id);
    Ok(Preview {
        status: status.to_string(),
        ..preview
    })
}

pub(crate) fn compose(
    api: &Api,
    host: &dyn Host,
    account: &Account,
    request: &ComposeRequest,
) -> ToolResult<Preview> {
    let own = own(account)?;
    let reply = match &request.reply {
        Some(spec) => Some(original(api, &spec.id)?),
        None => {
            check_recipients(request.mode, &request.to, &request.cc)?;
            None
        }
    };
    let id = start(api, request).map_err(unplaced)?;
    let draft = match inspect(api, &id, &own, request) {
        Ok(draft) => draft,
        Err(error) => return Err(abandon(api, &id, error)),
    };
    let preview = preview_of(&draft, reply);
    match request.mode {
        Mode::Draft => Ok(Preview {
            status: "drafted".to_string(),
            saved: Some(true),
            ..preview
        }),
        Mode::Send => deliver(api, host, &id, &own, &draft, preview),
    }
}

#[cfg(test)]
mod tests {
    use super::super::{compose as facade, IDS_AND_TEXT};
    use super::*;
    use crate::backend::ReplySpec;
    use crate::http::scripted::Scripted;
    use crate::http::{HttpError, Request};
    use crate::json;
    use crate::osascript::{App, Finished};
    use crate::provider::Provider;
    use crate::testutil::{mid, TestResult};
    use std::error::Error;
    use std::sync::Mutex;
    use std::time::Duration;

    const BASE: &str = "https://graph.microsoft.com/v1.0/me";
    const ORIGINAL: &str = "AAMkORIGINAL=";
    const DRAFT: &str = "AAMkDRAFT-1=";
    const DRAFT_PATH: &str = "/messages/AAMkDRAFT-1%3D";
    const READ_BACK: &str = concat!(
        "GET /messages/AAMkDRAFT-1%3D?$select=id%2Csubject%2Cfrom%2CtoRecipients%2CccRecipients",
        "%2CbccRecipients%2Cbody%2ChasAttachments"
    );
    const ORIGINAL_GET: &str = "GET /messages/AAMkORIGINAL%3D?$select=subject%2Cfrom";
    const SEND: &str = "POST /messages/AAMkDRAFT-1%3D/send";
    const DELETE: &str = "DELETE /messages/AAMkDRAFT-1%3D";

    struct Dialogs {
        stdout: String,
        seen: Mutex<Vec<String>>,
    }

    impl Dialogs {
        fn saying(decision: &str) -> Dialogs {
            Dialogs {
                stdout: format!(r#"{{"ok": true, "data": {{"decision": "{decision}"}}}}"#),
                seen: Mutex::new(Vec::new()),
            }
        }

        fn failing() -> Dialogs {
            Dialogs {
                stdout: r#"{"ok": false, "error": "approval_dialog_failed: no screen"}"#
                    .to_string(),
                seen: Mutex::new(Vec::new()),
            }
        }

        fn payloads(&self) -> Vec<String> {
            self.seen
                .lock()
                .map(|seen| seen.clone())
                .unwrap_or_default()
        }
    }

    impl Host for Dialogs {
        fn ensure_running(&self, _app: App) {}

        fn run(&self, _app: App, payload: &str, _timeout: Duration) -> Finished {
            if let Ok(mut seen) = self.seen.lock() {
                seen.push(payload.to_string());
            }
            Finished::Done {
                code: Some(0),
                stdout: self.stdout.clone(),
                stderr: String::new(),
            }
        }
    }

    #[derive(Clone)]
    struct Back {
        from: Option<&'static str>,
        to: Vec<String>,
        cc: Vec<String>,
        bcc: Vec<String>,
        subject: String,
        kind: &'static str,
        body: String,
        attachments: bool,
    }

    fn recipient(address: &str) -> Json {
        Json::object(vec![(
            "emailAddress",
            Json::object(vec![
                ("name", Json::from("Some Name")),
                ("address", Json::from(address)),
            ]),
        )])
    }

    fn recipients_of(addresses: &[String]) -> Json {
        Json::Arr(addresses.iter().map(|address| recipient(address)).collect())
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_string()).collect()
    }

    impl Back {
        fn new() -> Back {
            Back {
                from: None,
                to: strings(&["anna@example.org"]),
                cc: vec![],
                bcc: vec![],
                subject: "Hallo".to_string(),
                kind: "text",
                body: "Kurzer Text.\r\n".to_string(),
                attachments: false,
            }
        }

        fn reply() -> Back {
            Back {
                subject: "RE: Einladung zum Geburtstag".to_string(),
                body: "Danke, passt.\r\n".to_string(),
                ..Back::new()
            }
        }

        fn json(&self) -> String {
            let mut object = Object::new();
            object.insert(
                "@odata.context",
                "https://graph.microsoft.com/v1.0/$metadata#users('bb8775a4')/messages(id,subject)/$entity",
            );
            object.insert("@odata.etag", "W/\"CQAAABYAAADHcgC8Hl9tRZ\"");
            object.insert("id", DRAFT);
            object.insert("subject", self.subject.as_str());
            object.insert("hasAttachments", self.attachments);
            object.insert(
                "body",
                Json::object(vec![
                    ("contentType", Json::from(self.kind)),
                    ("content", Json::from(self.body.as_str())),
                ]),
            );
            if let Some(from) = self.from {
                object.insert("from", recipient(from));
            }
            object.insert("toRecipients", recipients_of(&self.to));
            object.insert("ccRecipients", recipients_of(&self.cc));
            object.insert("bccRecipients", recipients_of(&self.bcc));
            json::dumps(&Json::Obj(object), false)
        }
    }

    fn created() -> String {
        format!(r#"{{"id": "{DRAFT}", "isDraft": true}}"#)
    }

    fn mail_transport(back: &Back) -> Scripted {
        Scripted::new()
            .json(Method::Post, "/send", 202, "")
            .json(Method::Delete, DRAFT_PATH, 204, "")
            .json(Method::Get, DRAFT_PATH, 200, &back.json())
            .json(Method::Post, "/me/messages", 201, &created())
    }

    fn original_json() -> String {
        format!(
            r#"{{"id": "{ORIGINAL}", "subject": "Einladung zum Geburtstag", "from": {{"emailAddress": {{"name": "Anna", "address": "anna@example.org"}}}}}}"#
        )
    }

    fn reply_transport(back: &Back) -> Scripted {
        Scripted::new()
            .json(Method::Post, "/send", 202, "")
            .json(Method::Delete, DRAFT_PATH, 204, "")
            .json(Method::Patch, DRAFT_PATH, 200, &back.json())
            .json(Method::Get, DRAFT_PATH, 200, &back.json())
            .json(
                Method::Get,
                "/messages/AAMkORIGINAL%3D?",
                200,
                &original_json(),
            )
            .json(Method::Post, "/createReply", 201, &created())
    }

    fn api(transport: &Scripted) -> Api<'_> {
        Api::new(transport, "TOKEN".to_string(), Provider::Microsoft)
    }

    fn account() -> Account {
        Account {
            id: "microsoft:bob@outlook.example".to_string(),
            name: "Bob Outlook".to_string(),
            kind: "outlook".to_string(),
            addresses: vec!["bob@outlook.example".to_string()],
            label: "Bob Outlook".to_string(),
        }
    }

    fn new_mail(mode: Mode) -> ComposeRequest {
        ComposeRequest {
            mode,
            to: vec!["anna@example.org".to_string()],
            cc: vec![],
            subject: "Hallo".to_string(),
            body: "Kurzer Text.".to_string(),
            reply: None,
        }
    }

    fn answer(mode: Mode, all: bool) -> Result<ComposeRequest, Box<dyn Error>> {
        Ok(ComposeRequest {
            mode,
            to: vec![],
            cc: vec![],
            subject: String::new(),
            body: "Danke, passt.".to_string(),
            reply: Some(ReplySpec {
                mailbox: Some("Inbox".to_string()),
                id: mid(ORIGINAL)?,
                all,
            }),
        })
    }

    fn run(transport: &Scripted, host: &Dialogs, request: &ComposeRequest) -> ToolResult<Preview> {
        facade(&api(transport), host, &account(), request)
    }

    fn calls(transport: &Scripted) -> Vec<String> {
        transport
            .seen()
            .iter()
            .map(|call| {
                format!(
                    "{} {}",
                    call.method.as_str(),
                    call.url.strip_prefix(BASE).unwrap_or(&call.url)
                )
            })
            .collect()
    }

    fn header(request: &Request, name: &str) -> Option<String> {
        request
            .headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    }

    fn dialog(host: &Dialogs) -> Result<Object, Box<dyn Error>> {
        let payloads = host.payloads();
        let [payload] = payloads.as_slice() else {
            return Err(format!("expected one dialog, got {}", payloads.len()).into());
        };
        match json::parse(payload)? {
            Json::Obj(object) => Ok(object),
            other => Err(format!("dialog payload: {other:?}").into()),
        }
    }

    fn refused(result: ToolResult<Preview>, needle: &str) -> Result<ToolError, Box<dyn Error>> {
        match result {
            Err(error) if error.message().contains(needle) => Ok(error),
            Err(error) => Err(format!("wrong error: {error}").into()),
            Ok(preview) => Err(format!("expected a refusal, got {preview:?}").into()),
        }
    }

    fn sent_nothing(transport: &Scripted) -> bool {
        transport
            .seen()
            .iter()
            .all(|call| !call.url.ends_with("/send"))
    }

    #[test]
    fn an_approved_new_message_is_drafted_shown_checked_and_sent_exactly_once() -> TestResult {
        let transport = mail_transport(&Back::new());
        let host = Dialogs::saying("send");
        let preview = run(&transport, &host, &new_mail(Mode::Send))?;
        assert_eq!(
            preview,
            Preview {
                from: "bob@outlook.example".to_string(),
                to: vec!["anna@example.org".to_string()],
                cc: vec![],
                subject: "Hallo".to_string(),
                body: "Kurzer Text.".to_string(),
                reply: None,
                status: "sent".to_string(),
                saved: None,
            }
        );
        assert_eq!(
            calls(&transport),
            ["POST /messages", READ_BACK, READ_BACK, SEND]
        );
        let seen = transport.seen();
        let [create, first, second, send] = seen.as_slice() else {
            return Err("four requests".into());
        };
        assert_eq!(
            create.body.as_deref(),
            Some(
                r#"{"subject": "Hallo", "body": {"contentType": "text", "content": "Kurzer Text."}, "toRecipients": [{"emailAddress": {"address": "anna@example.org"}}], "ccRecipients": []}"#
            )
        );
        assert_eq!(
            header(create, "Content-Type").as_deref(),
            Some("application/json")
        );
        for read in [first, second] {
            assert_eq!(read.body, None);
            assert_eq!(header(read, "Content-Type"), None);
        }
        assert_eq!(send.method, Method::Post);
        assert_eq!(send.body, None);
        assert_eq!(header(send, "Content-Length").as_deref(), Some("0"));
        assert_eq!(header(send, "Content-Type"), None);
        for call in &seen {
            assert_eq!(header(call, "Prefer").as_deref(), Some(IDS_AND_TEXT));
            assert!(header(call, "Authorization").is_some_and(|value| value.ends_with(" TOKEN")));
        }
        for call in [create, first, second] {
            assert_eq!(header(call, "Content-Length"), None);
        }
        Ok(())
    }

    #[test]
    fn the_dialog_shows_the_values_read_back_from_the_server() -> TestResult {
        let back = Back {
            from: Some("Bob@Outlook.example"),
            to: strings(&["anna@example.org", "carl@example.org"]),
            cc: strings(&["dora@example.org"]),
            subject: "Hallo \u{e4}\u{20ac}".to_string(),
            body: "Kurzer Text.\r\n\r\n\r\n\r\nZweiter Absatz.\r\n".to_string(),
            ..Back::new()
        };
        let transport = mail_transport(&back);
        let host = Dialogs::saying("send");
        let mut request = new_mail(Mode::Send);
        request.to = vec![
            "anna@example.org".to_string(),
            "carl@example.org".to_string(),
        ];
        request.cc = vec!["dora@example.org".to_string()];
        request.subject = "Hallo \u{e4}\u{20ac}".to_string();
        request.body = "Kurzer Text.\n\nZweiter Absatz.".to_string();
        let preview = run(&transport, &host, &request)?;
        assert_eq!(preview.from, "bob@outlook.example");
        let fields = dialog(&host)?;
        let text = |key: &str| fields.get(key).and_then(Json::as_str).map(str::to_string);
        assert_eq!(text("op").as_deref(), Some("approve_send"));
        assert_eq!(text("from").as_deref(), Some("bob@outlook.example"));
        assert_eq!(text("subject").as_deref(), Some("Hallo \u{e4}\u{20ac}"));
        assert_eq!(
            text("body").as_deref(),
            Some("Kurzer Text.\n\nZweiter Absatz.")
        );
        assert_eq!(
            fields.get("to"),
            Some(&Json::Arr(vec![
                Json::from("anna@example.org"),
                Json::from("carl@example.org")
            ]))
        );
        assert_eq!(
            fields.get("cc"),
            Some(&Json::Arr(vec![Json::from("dora@example.org")]))
        );
        assert_eq!(fields.get("reply"), Some(&Json::Null));
        assert_eq!(fields.get("seconds"), Some(&Json::Int(75)));
        Ok(())
    }

    #[test]
    fn a_declined_message_is_deleted_and_never_sent() -> TestResult {
        let transport = mail_transport(&Back::new());
        let host = Dialogs::saying("declined");
        let preview = run(&transport, &host, &new_mail(Mode::Send))?;
        assert_eq!(preview.status, "declined");
        assert_eq!(preview.to, ["anna@example.org"]);
        assert_eq!(calls(&transport), ["POST /messages", READ_BACK, DELETE]);
        assert!(sent_nothing(&transport));
        assert_eq!(host.payloads().len(), 1);
        Ok(())
    }

    #[test]
    fn a_dialog_that_times_out_deletes_the_draft_and_sends_nothing() -> TestResult {
        let transport = mail_transport(&Back::new());
        let host = Dialogs::saying("timeout");
        let preview = run(&transport, &host, &new_mail(Mode::Send))?;
        assert_eq!(preview.status, "timeout");
        assert_eq!(calls(&transport), ["POST /messages", READ_BACK, DELETE]);
        assert!(sent_nothing(&transport));
        Ok(())
    }

    #[test]
    fn a_dialog_that_cannot_be_shown_deletes_the_draft_and_reports_the_failure() -> TestResult {
        let transport = mail_transport(&Back::new());
        let host = Dialogs::failing();
        let error = refused(
            run(&transport, &host, &new_mail(Mode::Send)),
            "could not be shown",
        )?;
        assert!(matches!(error, ToolError::Msg(_)));
        assert_eq!(calls(&transport), ["POST /messages", READ_BACK, DELETE]);
        assert!(sent_nothing(&transport));
        Ok(())
    }

    #[test]
    fn a_failing_cleanup_never_hides_the_decision_of_the_user() -> TestResult {
        for status in [500, 404, 403] {
            for decision in ["declined", "timeout"] {
                let transport = Scripted::new()
                    .json(Method::Delete, DRAFT_PATH, status, "{}")
                    .json(Method::Get, DRAFT_PATH, 200, &Back::new().json())
                    .json(Method::Post, "/me/messages", 201, &created());
                let preview = run(
                    &transport,
                    &Dialogs::saying(decision),
                    &new_mail(Mode::Send),
                )?;
                assert_eq!(preview.status, decision);
                assert_eq!(calls(&transport).last().map(String::as_str), Some(DELETE));
            }
        }
        let transport = Scripted::new()
            .reply(Method::Delete, DRAFT_PATH, vec![Err(HttpError::TimedOut)])
            .json(Method::Get, DRAFT_PATH, 200, &Back::new().json())
            .json(Method::Post, "/me/messages", 201, &created());
        let preview = run(
            &transport,
            &Dialogs::saying("declined"),
            &new_mail(Mode::Send),
        )?;
        assert_eq!(preview.status, "declined");
        let transport = Scripted::new()
            .json(Method::Delete, DRAFT_PATH, 500, "{}")
            .json(Method::Get, DRAFT_PATH, 200, &Back::new().json())
            .json(Method::Post, "/me/messages", 201, &created());
        let error = refused(
            run(&transport, &Dialogs::failing(), &new_mail(Mode::Send)),
            "could not be shown",
        )?;
        assert!(error
            .message()
            .ends_with("A draft may remain in the Drafts folder."));
        Ok(())
    }

    #[test]
    fn a_draft_that_changed_while_the_dialog_was_open_is_deleted_and_not_sent() -> TestResult {
        let changes: Vec<(&str, Back)> = vec![
            (
                "body",
                Back {
                    body: "Kurzer Text. Und noch etwas.\r\n".to_string(),
                    ..Back::new()
                },
            ),
            (
                "recipient",
                Back {
                    to: strings(&["anna@example.org", "evil@example.org"]),
                    ..Back::new()
                },
            ),
            (
                "swapped recipient",
                Back {
                    to: strings(&["evil@example.org"]),
                    ..Back::new()
                },
            ),
            (
                "cc",
                Back {
                    cc: strings(&["evil@example.org"]),
                    ..Back::new()
                },
            ),
            (
                "bcc",
                Back {
                    bcc: strings(&["evil@example.org"]),
                    ..Back::new()
                },
            ),
            (
                "subject",
                Back {
                    subject: "Anders".to_string(),
                    ..Back::new()
                },
            ),
            (
                "attachment",
                Back {
                    attachments: true,
                    ..Back::new()
                },
            ),
            (
                "sender",
                Back {
                    from: Some("someone.else@example.org"),
                    ..Back::new()
                },
            ),
            (
                "hidden text",
                Back {
                    body: "Kurzer Text.\u{200b}\r\n".to_string(),
                    ..Back::new()
                },
            ),
            (
                "whitespace only",
                Back {
                    body: "Kurzer  Text.\r\n".to_string(),
                    ..Back::new()
                },
            ),
        ];
        for (what, changed) in changes {
            let transport = Scripted::new()
                .json(Method::Post, "/send", 202, "")
                .json(Method::Delete, DRAFT_PATH, 204, "")
                .reply(
                    Method::Get,
                    DRAFT_PATH,
                    vec![
                        crate::http::scripted::ok(&Back::new().json()),
                        crate::http::scripted::ok(&changed.json()),
                    ],
                )
                .json(Method::Post, "/me/messages", 201, &created());
            let host = Dialogs::saying("send");
            let error = refused(
                run(&transport, &host, &new_mail(Mode::Send)),
                "differs from the text you gave",
            )?;
            assert_eq!(error, refusal("content_mismatch"), "{what}");
            assert_eq!(
                calls(&transport),
                ["POST /messages", READ_BACK, READ_BACK, DELETE],
                "{what}"
            );
            assert_eq!(host.payloads().len(), 1, "{what}");
        }
        Ok(())
    }

    #[test]
    fn a_sender_that_shows_up_later_is_fine_when_it_is_the_account() -> TestResult {
        let later = Back {
            from: Some("BOB@outlook.example"),
            ..Back::new()
        };
        let transport = Scripted::new()
            .json(Method::Post, "/send", 202, "")
            .json(Method::Delete, DRAFT_PATH, 204, "")
            .reply(
                Method::Get,
                DRAFT_PATH,
                vec![
                    crate::http::scripted::ok(&Back::new().json()),
                    crate::http::scripted::ok(&later.json()),
                ],
            )
            .json(Method::Post, "/me/messages", 201, &created());
        let preview = run(&transport, &Dialogs::saying("send"), &new_mail(Mode::Send))?;
        assert_eq!(preview.status, "sent");
        assert_eq!(preview.from, "bob@outlook.example");
        assert_eq!(calls(&transport).last().map(String::as_str), Some(SEND));
        Ok(())
    }

    #[test]
    fn a_failing_check_after_the_approval_deletes_the_draft_and_sends_nothing() -> TestResult {
        for (status, body) in [
            (500, "{}"),
            (404, "{}"),
            (401, "{}"),
            (200, "[]"),
            (200, "null"),
        ] {
            let transport = Scripted::new()
                .json(Method::Delete, DRAFT_PATH, 204, "")
                .reply(
                    Method::Get,
                    DRAFT_PATH,
                    vec![
                        crate::http::scripted::ok(&Back::new().json()),
                        Ok(crate::http::Response {
                            status,
                            body: body.to_string(),
                        }),
                    ],
                )
                .json(Method::Post, "/me/messages", 201, &created());
            let error = refused(
                run(&transport, &Dialogs::saying("send"), &new_mail(Mode::Send)),
                "NOTHING was sent",
            )?;
            assert!(matches!(error, ToolError::Msg(_)), "{status}");
            assert_eq!(
                calls(&transport),
                ["POST /messages", READ_BACK, READ_BACK, DELETE],
                "{status}"
            );
        }
        Ok(())
    }

    #[test]
    fn a_failed_send_keeps_the_draft_and_is_never_retried() -> TestResult {
        for (status, body, uncertain) in [
            (500, "{}", true),
            (502, "", true),
            (503, "<html>", true),
            (200, "junk", true),
            (202, "junk", true),
            (
                400,
                r#"{"error": {"code": "ErrorInvalidRecipients", "message": "At least one recipient is not valid."}}"#,
                false,
            ),
            (403, "{}", false),
            (401, "{}", false),
            (404, "{}", false),
            (429, "{}", false),
        ] {
            let transport = Scripted::new()
                .json(Method::Post, "/send", status, body)
                .json(Method::Delete, DRAFT_PATH, 204, "")
                .json(Method::Get, DRAFT_PATH, 200, &Back::new().json())
                .json(Method::Post, "/me/messages", 201, &created());
            let result = run(&transport, &Dialogs::saying("send"), &new_mail(Mode::Send));
            match (&result, uncertain) {
                (Err(ToolError::Uncertain(_)), true) | (Err(ToolError::Msg(_)), false) => {}
                other => return Err(format!("{status}: {other:?}").into()),
            }
            let all = calls(&transport);
            assert_eq!(
                all,
                ["POST /messages", READ_BACK, READ_BACK, SEND],
                "{status}"
            );
        }
        Ok(())
    }

    #[test]
    fn transport_failures_of_the_send_call_are_uncertain_and_keep_the_draft() -> TestResult {
        for failure in [
            HttpError::TimedOut,
            HttpError::TooLarge,
            HttpError::Failed("curl exit 52".to_string()),
        ] {
            let transport = Scripted::new()
                .reply(Method::Post, "/send", vec![Err(failure.clone())])
                .json(Method::Delete, DRAFT_PATH, 204, "")
                .json(Method::Get, DRAFT_PATH, 200, &Back::new().json())
                .json(Method::Post, "/me/messages", 201, &created());
            let result = run(&transport, &Dialogs::saying("send"), &new_mail(Mode::Send));
            assert!(
                matches!(&result, Err(ToolError::Uncertain(text)) if text.contains("may or may not")),
                "{failure:?}: {result:?}"
            );
            assert_eq!(calls(&transport).last().map(String::as_str), Some(SEND));
            assert_eq!(calls(&transport).len(), 4);
        }
        Ok(())
    }

    fn plain(count: usize) -> Vec<String> {
        (0..count)
            .map(|number| format!("p{number:02}@example.org"))
            .collect()
    }

    fn many(count: usize, mode: Mode) -> ComposeRequest {
        ComposeRequest {
            to: plain(count),
            ..new_mail(mode)
        }
    }

    #[test]
    fn an_answer_goes_through_createreply_a_body_patch_and_the_same_checks() -> TestResult {
        let back = Back {
            from: Some("bob@outlook.example"),
            ..Back::reply()
        };
        let transport = reply_transport(&back);
        let host = Dialogs::saying("send");
        let preview = run(&transport, &host, &answer(Mode::Send, false)?)?;
        assert_eq!(
            preview,
            Preview {
                from: "bob@outlook.example".to_string(),
                to: vec!["anna@example.org".to_string()],
                cc: vec![],
                subject: "RE: Einladung zum Geburtstag".to_string(),
                body: "Danke, passt.".to_string(),
                reply: Some((
                    "Einladung zum Geburtstag".to_string(),
                    "Anna <anna@example.org>".to_string()
                )),
                status: "sent".to_string(),
                saved: None,
            }
        );
        assert_eq!(
            calls(&transport),
            [
                ORIGINAL_GET,
                "POST /messages/AAMkORIGINAL%3D/createReply",
                "PATCH /messages/AAMkDRAFT-1%3D",
                READ_BACK,
                READ_BACK,
                SEND
            ]
        );
        let seen = transport.seen();
        assert_eq!(
            seen.get(1).and_then(|call| call.body.as_deref()),
            Some("{}")
        );
        assert_eq!(
            seen.get(2).and_then(|call| call.body.as_deref()),
            Some(r#"{"body": {"contentType": "text", "content": "Danke, passt."}}"#)
        );
        for call in &seen {
            assert_eq!(header(call, "Prefer").as_deref(), Some(IDS_AND_TEXT));
        }
        let fields = dialog(&host)?;
        assert_eq!(
            fields.get("reply"),
            Some(&Json::object(vec![
                ("subject", Json::from("Einladung zum Geburtstag")),
                ("from", Json::from("Anna <anna@example.org>")),
            ]))
        );
        assert_eq!(
            fields.get("subject").and_then(Json::as_str),
            Some("RE: Einladung zum Geburtstag")
        );
        Ok(())
    }

    #[test]
    fn reply_all_uses_the_reply_all_endpoint() -> TestResult {
        let back = Back {
            cc: strings(&["carl@example.org"]),
            ..Back::reply()
        };
        let transport = reply_transport(&back);
        let preview = run(
            &transport,
            &Dialogs::saying("send"),
            &answer(Mode::Send, true)?,
        )?;
        assert_eq!(preview.cc, ["carl@example.org"]);
        assert_eq!(
            calls(&transport).get(1).map(String::as_str),
            Some("POST /messages/AAMkORIGINAL%3D/createReplyAll")
        );
        let single = reply_transport(&back);
        run(
            &single,
            &Dialogs::saying("send"),
            &answer(Mode::Send, false)?,
        )?;
        assert_eq!(
            calls(&single).get(1).map(String::as_str),
            Some("POST /messages/AAMkORIGINAL%3D/createReply")
        );
        Ok(())
    }

    #[test]
    fn a_declined_answer_is_deleted_and_the_original_is_untouched() -> TestResult {
        let transport = reply_transport(&Back::reply());
        let preview = run(
            &transport,
            &Dialogs::saying("declined"),
            &answer(Mode::Send, false)?,
        )?;
        assert_eq!(preview.status, "declined");
        assert_eq!(
            calls(&transport),
            [
                ORIGINAL_GET,
                "POST /messages/AAMkORIGINAL%3D/createReply",
                "PATCH /messages/AAMkDRAFT-1%3D",
                READ_BACK,
                DELETE
            ]
        );
        assert!(sent_nothing(&transport));
        assert!(transport
            .seen()
            .iter()
            .all(|call| call.method != Method::Delete || call.url.contains("AAMkDRAFT-1")));
        Ok(())
    }

    #[test]
    fn the_original_must_exist_before_anything_is_created() -> TestResult {
        let transport = Scripted::new().json(
            Method::Get,
            "/messages/AAMkORIGINAL%3D?",
            404,
            r#"{"error": {"code": "ErrorItemNotFound", "message": "The specified object was not found in the store."}}"#,
        );
        let host = Dialogs::saying("send");
        let result = run(&transport, &host, &answer(Mode::Send, false)?);
        assert_eq!(result, Err(ToolError::msg(NO_MESSAGE)));
        assert_eq!(calls(&transport), [ORIGINAL_GET]);
        assert!(host.payloads().is_empty());
        Ok(())
    }

    #[test]
    fn failures_while_reading_the_original_say_that_nothing_was_sent() -> TestResult {
        for (status, body) in [
            (500, "{}"),
            (403, "{}"),
            (401, "{}"),
            (200, "[]"),
            (200, "null"),
        ] {
            let transport =
                Scripted::new().json(Method::Get, "/messages/AAMkORIGINAL%3D?", status, body);
            let result = run(
                &transport,
                &Dialogs::saying("send"),
                &answer(Mode::Send, false)?,
            );
            match &result {
                Err(ToolError::Msg(text))
                    if text.contains("NOTHING was sent") || *text == unexpected().message() => {}
                other => return Err(format!("{status}: {other:?}").into()),
            }
            assert_eq!(calls(&transport).len(), 1);
        }
        Ok(())
    }

    #[test]
    fn a_failing_draft_creation_stops_everything_without_a_leftover() -> TestResult {
        for (status, wanted) in [
            (400, "NOTHING was sent"),
            (403, "NOTHING was sent"),
            (404, "NOTHING was sent"),
            (429, "NOTHING was sent"),
            (500, "A draft may remain in the Drafts folder"),
            (502, "A draft may remain in the Drafts folder"),
        ] {
            for mode in [Mode::Send, Mode::Draft] {
                let transport = Scripted::new().json(Method::Post, "/me/messages", status, "{}");
                let host = Dialogs::saying("send");
                let error = refused(run(&transport, &host, &new_mail(mode)), wanted)?;
                assert!(matches!(error, ToolError::Msg(_)), "{status} {mode:?}");
                assert!(
                    error.message().contains("NOTHING was sent"),
                    "{status} {mode:?}"
                );
                assert_eq!(calls(&transport), ["POST /messages"], "{status} {mode:?}");
                assert!(host.payloads().is_empty());
            }
        }
        Ok(())
    }

    #[test]
    fn transport_trouble_while_creating_is_a_certain_non_send() -> TestResult {
        for failure in [
            HttpError::TimedOut,
            HttpError::TooLarge,
            HttpError::Failed("curl exit 7".to_string()),
        ] {
            let transport =
                Scripted::new().reply(Method::Post, "/me/messages", vec![Err(failure.clone())]);
            let error = refused(
                run(&transport, &Dialogs::saying("send"), &new_mail(Mode::Send)),
                "NOTHING was sent",
            )?;
            assert!(matches!(error, ToolError::Msg(_)), "{failure:?}");
        }
        let transport =
            Scripted::new().reply(Method::Post, "/me/messages", vec![Err(HttpError::Refused)]);
        let result = run(&transport, &Dialogs::saying("send"), &new_mail(Mode::Send));
        assert!(matches!(result, Err(ToolError::Internal(_))), "{result:?}");
        Ok(())
    }

    #[test]
    fn an_answer_without_a_usable_draft_id_stops_after_the_creation_call() -> TestResult {
        let long = format!(r#"{{"id": "{}"}}"#, "x".repeat(MAX_ID + 1));
        for body in [
            "{}",
            r#"{"id": ""}"#,
            r#"{"id": 5}"#,
            r#"{"id": null}"#,
            "[]",
            "",
            long.as_str(),
        ] {
            let transport = Scripted::new()
                .json(
                    Method::Get,
                    "/messages/AAMkORIGINAL%3D?",
                    200,
                    &original_json(),
                )
                .json(Method::Post, "/createReply", 201, body);
            let host = Dialogs::saying("send");
            refused(
                run(&transport, &host, &answer(Mode::Send, false)?),
                "did not return the new draft",
            )?;
            assert_eq!(calls(&transport).len(), 2, "{body}");
            assert!(host.payloads().is_empty());
        }
        let transport = Scripted::new().json(Method::Post, "/me/messages", 201, "{}");
        refused(
            run(&transport, &Dialogs::saying("send"), &new_mail(Mode::Draft)),
            "did not return the new draft",
        )?;
        Ok(())
    }

    #[test]
    fn a_failing_body_patch_deletes_the_half_made_draft() -> TestResult {
        for status in [400, 403, 404, 500, 503] {
            let transport = Scripted::new()
                .json(Method::Delete, DRAFT_PATH, 204, "")
                .json(Method::Patch, DRAFT_PATH, status, "{}")
                .json(
                    Method::Get,
                    "/messages/AAMkORIGINAL%3D?",
                    200,
                    &original_json(),
                )
                .json(Method::Post, "/createReply", 201, &created());
            let host = Dialogs::saying("send");
            let error = refused(
                run(&transport, &host, &answer(Mode::Send, false)?),
                "NOTHING was sent",
            )?;
            assert!(matches!(error, ToolError::Msg(_)), "{status}");
            assert!(!error.message().contains("may remain"), "{status}");
            assert_eq!(
                calls(&transport),
                [
                    ORIGINAL_GET,
                    "POST /messages/AAMkORIGINAL%3D/createReply",
                    "PATCH /messages/AAMkDRAFT-1%3D",
                    DELETE
                ],
                "{status}"
            );
            assert!(host.payloads().is_empty());
        }
        let transport = Scripted::new()
            .json(Method::Delete, DRAFT_PATH, 500, "{}")
            .reply(Method::Patch, DRAFT_PATH, vec![Err(HttpError::TimedOut)])
            .json(
                Method::Get,
                "/messages/AAMkORIGINAL%3D?",
                200,
                &original_json(),
            )
            .json(Method::Post, "/createReply", 201, &created());
        let error = refused(
            run(
                &transport,
                &Dialogs::saying("send"),
                &answer(Mode::Draft, false)?,
            ),
            "NOTHING was sent",
        )?;
        assert!(error
            .message()
            .ends_with("A draft may remain in the Drafts folder."));
        Ok(())
    }

    #[test]
    fn a_failing_read_back_deletes_the_draft_before_anything_is_shown() -> TestResult {
        for (status, body) in [(500, "{}"), (404, "{}"), (200, "[]"), (200, "")] {
            let transport = Scripted::new()
                .json(Method::Delete, DRAFT_PATH, 204, "")
                .json(Method::Get, DRAFT_PATH, status, body)
                .json(Method::Post, "/me/messages", 201, &created());
            let host = Dialogs::saying("send");
            refused(run(&transport, &host, &new_mail(Mode::Send)), "")?;
            assert_eq!(
                calls(&transport),
                ["POST /messages", READ_BACK, DELETE],
                "{status}"
            );
            assert!(host.payloads().is_empty());
        }
        Ok(())
    }

    #[test]
    fn a_new_draft_is_kept_for_the_user_without_a_dialog() -> TestResult {
        let back = Back {
            from: Some("bob@outlook.example"),
            ..Back::new()
        };
        let transport = mail_transport(&back);
        let host = Dialogs::saying("send");
        let preview = run(&transport, &host, &new_mail(Mode::Draft))?;
        assert_eq!(
            preview,
            Preview {
                from: "bob@outlook.example".to_string(),
                to: vec!["anna@example.org".to_string()],
                cc: vec![],
                subject: "Hallo".to_string(),
                body: "Kurzer Text.".to_string(),
                reply: None,
                status: "drafted".to_string(),
                saved: Some(true),
            }
        );
        assert_eq!(calls(&transport), ["POST /messages", READ_BACK]);
        assert!(host.payloads().is_empty());
        Ok(())
    }

    #[test]
    fn an_answer_draft_is_kept_with_the_server_chosen_recipients() -> TestResult {
        let transport = reply_transport(&Back::reply());
        let host = Dialogs::saying("send");
        let preview = run(&transport, &host, &answer(Mode::Draft, false)?)?;
        assert_eq!(preview.status, "drafted");
        assert_eq!(preview.saved, Some(true));
        assert_eq!(preview.to, ["anna@example.org"]);
        assert_eq!(preview.subject, "RE: Einladung zum Geburtstag");
        assert_eq!(
            preview.reply,
            Some((
                "Einladung zum Geburtstag".to_string(),
                "Anna <anna@example.org>".to_string()
            ))
        );
        assert_eq!(
            calls(&transport),
            [
                ORIGINAL_GET,
                "POST /messages/AAMkORIGINAL%3D/createReply",
                "PATCH /messages/AAMkDRAFT-1%3D",
                READ_BACK
            ]
        );
        assert!(host.payloads().is_empty());
        assert!(sent_nothing(&transport));
        Ok(())
    }

    #[test]
    fn drafts_may_have_more_recipients_and_unusual_addresses_up_to_fifty() -> TestResult {
        for count in [6, 50] {
            let back = Back {
                to: plain(count),
                ..Back::new()
            };
            let transport = mail_transport(&back);
            let preview = run(
                &transport,
                &Dialogs::saying("send"),
                &many(count, Mode::Draft),
            )?;
            assert_eq!(preview.to.len(), count);
            assert_eq!(preview.status, "drafted");
        }
        let back = Back {
            to: strings(&["o'neil@example.org"]),
            ..Back::new()
        };
        let transport = mail_transport(&back);
        let preview = run(&transport, &Dialogs::saying("send"), &new_mail(Mode::Draft))?;
        assert_eq!(preview.to, ["o'neil@example.org"]);
        Ok(())
    }

    #[test]
    fn drafts_with_more_than_fifty_recipients_are_refused_and_removed() -> TestResult {
        let transport = Scripted::new();
        let error = refused(
            run(&transport, &Dialogs::saying("send"), &many(51, Mode::Draft)),
            "more than 50 recipients",
        )?;
        assert!(error.message().contains("NOTHING was sent"));
        assert!(transport.seen().is_empty());
        let back = Back {
            to: plain(51),
            ..Back::reply()
        };
        let transport = reply_transport(&back);
        refused(
            run(
                &transport,
                &Dialogs::saying("send"),
                &answer(Mode::Draft, true)?,
            ),
            "more than 50 recipients",
        )?;
        assert_eq!(calls(&transport).last().map(String::as_str), Some(DELETE));
        let crowd = Back {
            to: plain(30),
            cc: plain(21),
            ..Back::reply()
        };
        let transport = reply_transport(&crowd);
        refused(
            run(
                &transport,
                &Dialogs::saying("send"),
                &answer(Mode::Draft, true)?,
            ),
            "more than 50 recipients",
        )?;
        Ok(())
    }

    #[test]
    fn a_message_without_recipients_is_refused_before_anything_is_created_or_shown() -> TestResult {
        let transport = Scripted::new();
        let request = ComposeRequest {
            to: vec![],
            cc: vec!["carl@example.org".to_string()],
            ..new_mail(Mode::Send)
        };
        let host = Dialogs::saying("send");
        let error = refused(
            run(&transport, &host, &request),
            "did not accept the recipients",
        )?;
        assert_eq!(error, refusal("no_recipients"));
        assert!(transport.seen().is_empty());
        assert!(host.payloads().is_empty());
        Ok(())
    }

    #[test]
    fn an_answer_that_the_server_addresses_to_nobody_is_removed() -> TestResult {
        for mode in [Mode::Send, Mode::Draft] {
            let back = Back {
                to: vec![],
                cc: strings(&["carl@example.org"]),
                ..Back::reply()
            };
            let transport = reply_transport(&back);
            let host = Dialogs::saying("send");
            let error = refused(
                run(&transport, &host, &answer(mode, false)?),
                "did not accept the recipients",
            )?;
            assert_eq!(error, refusal("no_recipients"));
            assert_eq!(calls(&transport).last().map(String::as_str), Some(DELETE));
            assert!(host.payloads().is_empty());
        }
        Ok(())
    }

    #[test]
    fn at_most_five_recipients_may_receive_a_sent_message() -> TestResult {
        let transport = mail_transport(&Back {
            to: plain(5),
            ..Back::new()
        });
        let preview = run(&transport, &Dialogs::saying("send"), &many(5, Mode::Send))?;
        assert_eq!(preview.status, "sent");
        let split = Back {
            to: plain(3),
            cc: plain(2),
            ..Back::reply()
        };
        let transport = reply_transport(&split);
        assert_eq!(
            run(
                &transport,
                &Dialogs::saying("send"),
                &answer(Mode::Send, true)?
            )?
            .status,
            "sent"
        );
        for (to, cc) in [(6, 0), (3, 3), (1, 5), (5, 1), (50, 50)] {
            let request = ComposeRequest {
                to: plain(to),
                cc: plain(cc).into_iter().map(|a| format!("c{a}")).collect(),
                ..new_mail(Mode::Send)
            };
            let transport = Scripted::new();
            let host = Dialogs::saying("send");
            let error = refused(run(&transport, &host, &request), "more than 5 recipients")?;
            assert_eq!(error, refusal("too_many_recipients"), "{to}+{cc}");
            assert!(transport.seen().is_empty());
            assert!(host.payloads().is_empty());
            let big = Back {
                to: plain(to),
                cc: plain(cc).into_iter().map(|a| format!("c{a}")).collect(),
                ..Back::reply()
            };
            let transport = reply_transport(&big);
            let host = Dialogs::saying("send");
            let error = refused(
                run(&transport, &host, &answer(Mode::Send, true)?),
                "more than 5 recipients",
            )?;
            assert_eq!(error, refusal("too_many_recipients"), "{to}+{cc}");
            assert_eq!(calls(&transport).last().map(String::as_str), Some(DELETE));
            assert!(host.payloads().is_empty());
        }
        Ok(())
    }

    #[test]
    fn only_plain_ascii_addresses_may_receive_a_sent_message() -> TestResult {
        let bad = [
            "",
            "anna",
            "anna@",
            "@example.org",
            "a b@example.org",
            "o'neil@example.org",
            "anna@example",
            "a@b@example.org",
            "\u{e4}nna@example.org",
            "anna@ex\u{e4}mple.org",
            "anna@-bad.example",
            "anna@bad..example",
            "<anna@example.org>",
            "anna@example.org,",
            "anna@example.org\n",
        ];
        let long = format!("{}@example.org", "a".repeat(65));
        for address in bad.iter().copied().chain([long.as_str()]) {
            let request = ComposeRequest {
                to: vec![address.to_string()],
                ..new_mail(Mode::Send)
            };
            let transport = Scripted::new();
            let host = Dialogs::saying("send");
            let error = refused(run(&transport, &host, &request), "ASCII addresses")?;
            assert_eq!(error, refusal("recipient_invalid"), "{address:?}");
            assert!(transport.seen().is_empty());
            let in_cc = ComposeRequest {
                cc: vec![address.to_string()],
                ..new_mail(Mode::Send)
            };
            assert_eq!(
                run(&Scripted::new(), &host, &in_cc),
                Err(refusal("recipient_invalid")),
                "{address:?}"
            );
            let served = Back {
                to: vec![address.to_string()],
                ..Back::reply()
            };
            let transport = reply_transport(&served);
            let error = refused(
                run(&transport, &host, &answer(Mode::Send, false)?),
                "ASCII addresses",
            )?;
            assert_eq!(error, refusal("recipient_invalid"), "{address:?}");
            assert_eq!(calls(&transport).last().map(String::as_str), Some(DELETE));
            let served_cc = Back {
                cc: vec![address.to_string()],
                ..Back::reply()
            };
            let transport = reply_transport(&served_cc);
            assert_eq!(
                run(&transport, &host, &answer(Mode::Send, true)?),
                Err(refusal("recipient_invalid")),
                "{address:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn a_recipient_without_an_address_is_not_a_plain_address() -> TestResult {
        let json = Back::reply().json().replace(
            r#""toRecipients": [{"emailAddress": {"name": "Some Name", "address": "anna@example.org"}}]"#,
            r#""toRecipients": [{"emailAddress": {"name": "Group"}}, "junk", {"emailAddress": null}]"#,
        );
        let transport = Scripted::new()
            .json(Method::Delete, DRAFT_PATH, 204, "")
            .json(Method::Patch, DRAFT_PATH, 200, &json)
            .json(Method::Get, DRAFT_PATH, 200, &json)
            .json(
                Method::Get,
                "/messages/AAMkORIGINAL%3D?",
                200,
                &original_json(),
            )
            .json(Method::Post, "/createReply", 201, &created());
        let error = refused(
            run(
                &transport,
                &Dialogs::saying("send"),
                &answer(Mode::Send, false)?,
            ),
            "ASCII addresses",
        )?;
        assert_eq!(error, refusal("recipient_invalid"));
        Ok(())
    }

    #[test]
    fn bcc_recipients_and_attachments_are_not_acceptable_in_a_sent_message() -> TestResult {
        for (what, back) in [
            (
                "bcc",
                Back {
                    bcc: strings(&["secret@example.org"]),
                    ..Back::new()
                },
            ),
            (
                "attachment",
                Back {
                    attachments: true,
                    ..Back::new()
                },
            ),
        ] {
            let transport = mail_transport(&back);
            let host = Dialogs::saying("send");
            let error = refused(
                run(&transport, &host, &new_mail(Mode::Send)),
                "differs from the text you gave",
            )?;
            assert_eq!(error, refusal("content_mismatch"), "{what}");
            assert_eq!(
                calls(&transport),
                ["POST /messages", READ_BACK, DELETE],
                "{what}"
            );
            assert!(host.payloads().is_empty());
            let draft = mail_transport(&back);
            let preview = run(&draft, &host, &new_mail(Mode::Draft))?;
            assert_eq!(preview.status, "drafted", "{what}");
        }
        Ok(())
    }

    #[test]
    fn the_sender_must_be_one_of_the_addresses_of_the_account() -> TestResult {
        let transport = mail_transport(&Back {
            from: Some("someone.else@example.org"),
            ..Back::new()
        });
        let host = Dialogs::saying("send");
        let error = refused(
            run(&transport, &host, &new_mail(Mode::Send)),
            "different address than the selected account",
        )?;
        assert_eq!(error, refusal("sender_mismatch"));
        assert_eq!(calls(&transport), ["POST /messages", READ_BACK, DELETE]);
        assert!(host.payloads().is_empty());
        let draft = mail_transport(&Back {
            from: Some("someone.else@example.org"),
            ..Back::new()
        });
        assert_eq!(
            run(&draft, &host, &new_mail(Mode::Draft)),
            Err(refusal("sender_mismatch"))
        );
        let mut aliases = account();
        aliases.addresses = vec![
            "bob@outlook.example".to_string(),
            "Bob.Alias@Live.example".to_string(),
        ];
        let transport = mail_transport(&Back {
            from: Some("bob.alias@live.example"),
            ..Back::new()
        });
        let preview = facade(
            &api(&transport),
            &Dialogs::saying("send"),
            &aliases,
            &new_mail(Mode::Send),
        )?;
        assert_eq!(preview.from, "Bob.Alias@Live.example");
        assert_eq!(preview.status, "sent");
        Ok(())
    }

    #[test]
    fn the_sender_defaults_to_the_first_address_of_the_account() -> TestResult {
        let mut other = account();
        other.addresses = vec![
            "main@outlook.example".to_string(),
            "alias@live.example".to_string(),
        ];
        let transport = mail_transport(&Back::new());
        let preview = facade(
            &api(&transport),
            &Dialogs::saying("declined"),
            &other,
            &new_mail(Mode::Send),
        )?;
        assert_eq!(preview.from, "main@outlook.example");
        let mut bare = account();
        bare.addresses = vec![];
        let transport = mail_transport(&Back {
            from: Some("bob@outlook.example"),
            ..Back::new()
        });
        let preview = facade(
            &api(&transport),
            &Dialogs::saying("declined"),
            &bare,
            &new_mail(Mode::Send),
        )?;
        assert_eq!(preview.from, "bob@outlook.example");
        let mut nameless = account();
        nameless.addresses = vec![];
        nameless.id = "5F3E-4C".to_string();
        let transport = Scripted::new();
        let result = facade(
            &api(&transport),
            &Dialogs::saying("send"),
            &nameless,
            &new_mail(Mode::Send),
        );
        assert_eq!(result, Err(refusal("no_sender_address")));
        assert!(transport.seen().is_empty());
        Ok(())
    }

    fn with_body(kind: &'static str, body: &str) -> Back {
        Back {
            kind,
            body: body.to_string(),
            ..Back::new()
        }
    }

    #[test]
    fn the_saved_text_must_contain_the_requested_text_apart_from_whitespace() -> TestResult {
        let extra = "x".repeat(EXTRA_CHARS);
        let accepted = [
            ("text", "Kurzer Text.".to_string()),
            ("text", "Kurzer Text.\r\n".to_string()),
            ("TEXT", "Kurzer Text.".to_string()),
            ("text", "  Kurzer\r\n  Text.  \r\n\r\n".to_string()),
            ("text", "Kurzer\u{a0}Text.".to_string()),
            ("text", "KurzerText.".to_string()),
            (
                "text",
                "Hi!\r\nKurzer Text.\r\n-- \r\nSignature".to_string(),
            ),
            ("text", format!("Kurzer Text.{extra}")),
            ("text", format!("{extra}Kurzer Text.")),
            ("text", format!("Kurzer Text.{}", " ".repeat(3000))),
        ];
        for (kind, body) in accepted {
            let transport = mail_transport(&with_body(kind, &body));
            let preview = run(&transport, &Dialogs::saying("send"), &new_mail(Mode::Send))?;
            assert_eq!(preview.status, "sent", "{body:?}");
        }
        let refused_texts = [
            ("text", "Anderer Text.".to_string()),
            ("text", "Kurzer Text".to_string()),
            ("text", "kurzer text.".to_string()),
            ("text", "Text. Kurzer".to_string()),
            ("text", String::new()),
            ("text", " \r\n ".to_string()),
            ("html", "Kurzer Text.".to_string()),
            ("HTML", "<p>Kurzer Text.</p>".to_string()),
            ("", "Kurzer Text.".to_string()),
            ("text", "Kurzer Text.\u{200b}".to_string()),
            ("text", "Kurz\u{200b}er Text.".to_string()),
            ("text", "Kurzer Text.\u{202e}".to_string()),
            ("text", "Kurzer Text.\u{feff}".to_string()),
            ("text", "Kurzer Text.\u{e0041}".to_string()),
            ("text", "Kurzer Text.\u{0}".to_string()),
            ("text", format!("Kurzer Text.{extra}x")),
            ("text", format!("x{extra}Kurzer Text.")),
            ("text", format!("Kurzer Text.{}", " ".repeat(3001))),
        ];
        for (kind, body) in refused_texts {
            let transport = mail_transport(&with_body(kind, &body));
            let host = Dialogs::saying("send");
            let error = refused(
                run(&transport, &host, &new_mail(Mode::Send)),
                "differs from the text you gave",
            )?;
            assert_eq!(error, refusal("content_mismatch"), "{kind} {body:?}");
            assert_eq!(calls(&transport), ["POST /messages", READ_BACK, DELETE]);
            assert!(host.payloads().is_empty());
        }
        Ok(())
    }

    #[test]
    fn a_response_without_a_body_is_a_content_mismatch() -> TestResult {
        let json = Back::new().json().replace(
            r#""body": {"contentType": "text", "content": "Kurzer Text.\r\n"}, "#,
            "",
        );
        assert!(!json.contains("Kurzer Text."));
        let transport = Scripted::new()
            .json(Method::Delete, DRAFT_PATH, 204, "")
            .json(Method::Get, DRAFT_PATH, 200, &json)
            .json(Method::Post, "/me/messages", 201, &created());
        let error = refused(
            run(&transport, &Dialogs::saying("send"), &new_mail(Mode::Send)),
            "differs from the text you gave",
        )?;
        assert_eq!(error, refusal("content_mismatch"));
        Ok(())
    }

    #[test]
    fn a_mismatching_draft_is_not_kept_and_says_so() -> TestResult {
        let transport = mail_transport(&with_body("text", "Something else"));
        let host = Dialogs::saying("send");
        let error = refused(
            run(&transport, &host, &new_mail(Mode::Draft)),
            "differs from the text you gave, so it was not kept",
        )?;
        assert!(error.message().contains("NOTHING was sent"));
        assert!(!error.message().contains("create_draft"));
        assert_eq!(calls(&transport), ["POST /messages", READ_BACK, DELETE]);
        assert!(host.payloads().is_empty());
        Ok(())
    }

    #[test]
    fn long_requested_text_is_checked_against_a_proportional_bound() -> TestResult {
        let body = "line\n".repeat(4000);
        let wanted = body.trim().to_string();
        let served = Back {
            body: wanted.replace('\n', "\r\n"),
            ..Back::new()
        };
        let transport = mail_transport(&served);
        let request = ComposeRequest {
            body: wanted,
            ..new_mail(Mode::Draft)
        };
        let preview = run(&transport, &Dialogs::saying("send"), &request)?;
        assert_eq!(preview.status, "drafted");
        assert_eq!(preview.body.lines().count(), 4000);
        Ok(())
    }

    #[test]
    fn hostile_server_text_is_cleaned_before_it_reaches_the_dialog() -> TestResult {
        let back = Back {
            subject: format!("Re:\r\nBcc: evil@example.org\u{200b} {}", "s".repeat(400)),
            to: strings(&["anna@example.org"]),
            ..Back::reply()
        };
        let transport = reply_transport(&back);
        let host = Dialogs::saying("send");
        let preview = run(&transport, &host, &answer(Mode::Send, false)?)?;
        assert!(preview.subject.starts_with("Re: Bcc: evil@example.org sss"));
        assert!(preview.subject.ends_with(" [...]"));
        assert!(!preview.subject.contains('\n'));
        let fields = dialog(&host)?;
        assert_eq!(
            fields.get("subject").and_then(Json::as_str),
            Some(preview.subject.as_str())
        );
        Ok(())
    }

    #[test]
    fn the_original_is_shown_in_a_cleaned_form() -> TestResult {
        let original = r#"{"id": "AAMkORIGINAL=", "subject": "Line\nBreak\u202e", "from": {"emailAddress": {"name": "Eve\n<boss@corp.example>", "address": "eve@evil.example"}}}"#;
        let transport = Scripted::new()
            .json(Method::Delete, DRAFT_PATH, 204, "")
            .json(Method::Patch, DRAFT_PATH, 200, &Back::reply().json())
            .json(Method::Get, DRAFT_PATH, 200, &Back::reply().json())
            .json(Method::Get, "/messages/AAMkORIGINAL%3D?", 200, original)
            .json(Method::Post, "/createReply", 201, &created());
        let preview = run(
            &transport,
            &Dialogs::saying("declined"),
            &answer(Mode::Send, false)?,
        )?;
        assert_eq!(
            preview.reply,
            Some((
                "Line Break".to_string(),
                "Eve <boss@corp.example> <eve@evil.example>".to_string()
            ))
        );
        Ok(())
    }

    #[test]
    fn ids_with_special_characters_are_percent_encoded_everywhere() -> TestResult {
        let transport = Scripted::new()
            .json(Method::Post, "/send", 202, "")
            .json(
                Method::Patch,
                "/messages/AAMk%2Bnew%2Fid%3D",
                200,
                &Back::reply().json(),
            )
            .json(
                Method::Get,
                "/messages/AAMk%2Bnew%2Fid%3D",
                200,
                &Back::reply().json(),
            )
            .json(
                Method::Get,
                "/messages/a%2Bb%2Fc%3Dd.e-f_g?",
                200,
                &original_json(),
            )
            .json(
                Method::Post,
                "/createReply",
                201,
                r#"{"id": "AAMk+new/id="}"#,
            );
        let request = ComposeRequest {
            reply: Some(ReplySpec {
                mailbox: None,
                id: mid("a+b/c=d.e-f_g")?,
                all: false,
            }),
            ..answer(Mode::Send, false)?
        };
        run(&transport, &Dialogs::saying("send"), &request)?;
        let all = calls(&transport);
        assert!(all
            .iter()
            .any(|call| call.starts_with("GET /messages/a%2Bb%2Fc%3Dd.e-f_g?")));
        assert!(all.contains(&"POST /messages/a%2Bb%2Fc%3Dd.e-f_g/createReply".to_string()));
        assert!(all.contains(&"PATCH /messages/AAMk%2Bnew%2Fid%3D".to_string()));
        assert_eq!(
            all.last().map(String::as_str),
            Some("POST /messages/AAMk%2Bnew%2Fid%3D/send")
        );
        Ok(())
    }

    #[test]
    fn every_failure_before_the_send_call_is_a_certain_non_send() {
        let uncertain =
            ToolError::Uncertain("The provider did not confirm the change.".to_string());
        assert_eq!(
            before_send(uncertain),
            ToolError::msg("The provider did not confirm a step while the message was prepared. NOTHING was sent.")
        );
        assert_eq!(
            before_send(ToolError::msg("The provider refused the request (400).")),
            ToolError::msg("The provider refused the request (400). NOTHING was sent.")
        );
        assert_eq!(
            before_send(ToolError::msg("Already says NOTHING was sent.")),
            ToolError::msg("Already says NOTHING was sent.")
        );
        assert_eq!(
            before_send(ToolError::internal("request")),
            ToolError::internal("request")
        );
    }

    #[test]
    fn a_draft_of_unknown_fate_is_named_when_its_creation_was_not_confirmed() {
        let uncertain =
            ToolError::Uncertain("The provider did not confirm the change.".to_string());
        assert_eq!(
            unplaced(uncertain),
            ToolError::msg("The provider did not confirm that the draft was created. NOTHING was sent. A draft may remain in the Drafts folder.")
        );
        assert_eq!(
            unplaced(ToolError::msg("The provider refused the request (400).")),
            ToolError::msg("The provider refused the request (400). NOTHING was sent.")
        );
        assert_eq!(unplaced(ToolError::internal("x")), ToolError::internal("x"));
    }

    #[test]
    fn the_cleanup_text_is_only_added_when_the_delete_failed() -> TestResult {
        let transport = Scripted::new().json(Method::Delete, "/messages/", 204, "");
        let error = abandon(&api(&transport), "AAMk1=", ToolError::msg("Failed."));
        assert_eq!(error, ToolError::msg("Failed."));
        let transport = Scripted::new().json(Method::Delete, "/messages/", 500, "{}");
        let error = abandon(&api(&transport), "AAMk1=", ToolError::msg("Failed."));
        assert_eq!(
            error,
            ToolError::msg("Failed. A draft may remain in the Drafts folder.")
        );
        let uncertain = abandon(
            &api(&transport),
            "AAMk1=",
            ToolError::Uncertain("x".to_string()),
        );
        assert_eq!(uncertain, ToolError::Uncertain("x".to_string()));
        let internal = abandon(&api(&transport), "AAMk1=", ToolError::internal("y"));
        assert_eq!(internal, ToolError::internal("y"));
        Ok(())
    }

    #[test]
    fn whitespace_is_ignored_when_texts_are_compared() {
        assert_eq!(squash(" a\r\n\tb\u{a0}c\u{2003}d "), "abcd");
        assert_eq!(squash(""), "");
        let draft = Draft {
            from: String::new(),
            to: vec![],
            cc: vec![],
            bcc: 0,
            attachments: false,
            subject: String::new(),
            kind: "text".to_string(),
            body: "a b".to_string(),
        };
        assert!(body_ok(&draft, "ab"));
        assert!(body_ok(&draft, "a\nb"));
        assert!(!body_ok(&draft, "abc"));
    }

    #[test]
    fn every_refusal_of_an_answer_deletes_the_draft_exactly_once() -> TestResult {
        let cases: Vec<(&str, Back, &str)> = vec![
            (
                "nobody",
                Back {
                    to: vec![],
                    ..Back::reply()
                },
                "did not accept the recipients",
            ),
            (
                "too many",
                Back {
                    to: plain(6),
                    ..Back::reply()
                },
                "more than 5 recipients",
            ),
            (
                "invalid",
                Back {
                    to: strings(&["x y@example.org"]),
                    ..Back::reply()
                },
                "ASCII addresses",
            ),
            (
                "bcc",
                Back {
                    bcc: strings(&["s@example.org"]),
                    ..Back::reply()
                },
                "differs from the text you gave",
            ),
            (
                "attachment",
                Back {
                    attachments: true,
                    ..Back::reply()
                },
                "differs from the text you gave",
            ),
            (
                "sender",
                Back {
                    from: Some("x@example.org"),
                    ..Back::reply()
                },
                "different address",
            ),
            (
                "body",
                Back {
                    body: "Other".to_string(),
                    ..Back::reply()
                },
                "differs from the text you gave",
            ),
            (
                "html",
                Back {
                    kind: "html",
                    ..Back::reply()
                },
                "differs from the text you gave",
            ),
            (
                "hidden",
                Back {
                    body: "Danke, passt.\u{200b}".to_string(),
                    ..Back::reply()
                },
                "differs from the text you gave",
            ),
        ];
        for (what, back, wanted) in cases {
            let transport = reply_transport(&back);
            let host = Dialogs::saying("send");
            refused(run(&transport, &host, &answer(Mode::Send, false)?), wanted)?;
            let all = calls(&transport);
            assert_eq!(all.last().map(String::as_str), Some(DELETE), "{what}");
            assert_eq!(
                all.iter().filter(|call| call.as_str() == DELETE).count(),
                1,
                "{what}"
            );
            assert_eq!(all.len(), 5, "{what}");
            assert!(sent_nothing(&transport), "{what}");
            assert!(host.payloads().is_empty(), "{what}");
        }
        Ok(())
    }

    #[test]
    fn server_lists_and_texts_are_read_up_to_fixed_bounds() -> TestResult {
        let crowd = Back {
            to: plain(150),
            cc: plain(150),
            subject: "s".repeat(2000),
            ..Back::new()
        };
        let transport = Scripted::new().json(Method::Get, DRAFT_PATH, 200, &crowd.json());
        let draft = read_back(&api(&transport), DRAFT, &own(&account())?)?;
        assert_eq!(draft.to.len(), MAX_READ_RECIPIENTS);
        assert_eq!(draft.cc.len(), MAX_READ_RECIPIENTS);
        assert_eq!(draft.subject.chars().count(), MAX_RAW_SUBJECT);
        let preview = preview_of(&draft, None);
        assert_eq!(preview.subject.chars().count(), MAX_TEXT + 6);
        assert_eq!(preview.to.len(), MAX_READ_RECIPIENTS);
        let many_bcc = Back {
            bcc: plain(150),
            ..Back::new()
        };
        let transport = Scripted::new().json(Method::Get, DRAFT_PATH, 200, &many_bcc.json());
        assert_eq!(
            read_back(&api(&transport), DRAFT, &own(&account())?)?.bcc,
            150
        );
        Ok(())
    }

    #[test]
    fn a_hundred_and_fifty_recipients_are_refused_whatever_the_mode() -> TestResult {
        for mode in [Mode::Send, Mode::Draft] {
            let transport = reply_transport(&Back {
                to: plain(150),
                ..Back::reply()
            });
            let host = Dialogs::saying("send");
            refused(run(&transport, &host, &answer(mode, true)?), "more than")?;
            assert_eq!(calls(&transport).last().map(String::as_str), Some(DELETE));
        }
        Ok(())
    }

    #[test]
    fn recipients_given_with_an_answer_never_reach_the_server() -> TestResult {
        let transport = reply_transport(&Back::reply());
        let request = ComposeRequest {
            to: vec!["evil@example.org".to_string()],
            cc: vec!["evil@example.org".to_string()],
            subject: "Ignored".to_string(),
            ..answer(Mode::Draft, false)?
        };
        run(&transport, &Dialogs::saying("send"), &request)?;
        for call in transport.seen() {
            let text = call.body.unwrap_or_default();
            assert!(!text.contains("evil@example.org"), "{text}");
            assert!(!text.contains("Ignored"), "{text}");
        }
        Ok(())
    }

    #[test]
    fn the_send_call_is_the_only_post_to_a_send_endpoint_and_always_comes_last() -> TestResult {
        let transport = reply_transport(&Back::reply());
        run(
            &transport,
            &Dialogs::saying("send"),
            &answer(Mode::Send, false)?,
        )?;
        let all = calls(&transport);
        assert_eq!(all.iter().filter(|call| call.ends_with("/send")).count(), 1);
        assert_eq!(all.last().map(String::as_str), Some(SEND));
        assert!(all.iter().all(|call| !call.starts_with("DELETE")));
        Ok(())
    }

    #[test]
    fn requested_texts_travel_as_escaped_json_values_and_never_change_the_structure() -> TestResult
    {
        let subject = "He said \"hi\" \\ then\u{e4}\u{20ac}\u{1f600}";
        let body = "Line one with \"quotes\" and a \\ backslash,\n\nline two \u{1f600} </script>{\"evil\": 1}";
        let hostile = "a@example.org\",\"bcc\":\"evil@example.org";
        let back = Back {
            to: strings(&[hostile]),
            subject: subject.to_string(),
            body: body.to_string(),
            ..Back::new()
        };
        let transport = mail_transport(&back);
        let request = ComposeRequest {
            mode: Mode::Draft,
            to: vec![hostile.to_string()],
            cc: vec![],
            subject: subject.to_string(),
            body: body.to_string(),
            reply: None,
        };
        run(&transport, &Dialogs::saying("send"), &request)?;
        let seen = transport.seen();
        let create = seen.first().ok_or("create request")?;
        let text = create.body.clone().ok_or("body")?;
        assert!(text.is_ascii());
        assert!(!text.contains('\n'));
        let Json::Obj(sent) = json::parse(&text)? else {
            return Err("object".into());
        };
        let keys: Vec<&str> = sent.keys().collect();
        assert_eq!(keys, ["subject", "body", "toRecipients", "ccRecipients"]);
        assert_eq!(sent.get("subject").and_then(Json::as_str), Some(subject));
        let content = sent
            .get("body")
            .and_then(Json::as_object)
            .and_then(|found| found.get("content"))
            .and_then(Json::as_str);
        assert_eq!(content, Some(body));
        let address = sent
            .get("toRecipients")
            .and_then(Json::as_array)
            .and_then(|list| list.first())
            .and_then(Json::as_object)
            .and_then(|entry| entry.get("emailAddress"))
            .and_then(Json::as_object)
            .and_then(|entry| entry.get("address"))
            .and_then(Json::as_str);
        assert_eq!(address, Some(hostile));
        Ok(())
    }
}
