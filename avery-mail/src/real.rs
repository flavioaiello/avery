use std::time::Duration;

use crate::account::{Account, MailAccount};
use crate::backend::{
    Backend, ComposeRequest, Mailbox, MessageBody, MessageList, MessageQuery, Preview,
};
use crate::error::{ToolError, ToolResult};
use crate::json::{self, Json, Object};
use crate::mailerror::failure;
use crate::osascript::{Finished, Host, OsascriptHost};
use crate::text::text_of;
use crate::texts::{APPROVAL_SECONDS, JXA_TIMEOUT, MAX_HITS, MAX_RECIPIENTS};

pub(crate) struct RealBackend {
    host: Box<dyn Host>,
}

impl RealBackend {
    pub(crate) fn new(host: Box<dyn Host>) -> RealBackend {
        RealBackend { host }
    }

    pub(crate) fn system() -> RealBackend {
        RealBackend::new(Box::new(OsascriptHost))
    }

    fn call(&self, request: Object, timeout_seconds: u64) -> ToolResult<Json> {
        let op = request
            .get("op")
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_string();
        let op = op.as_str();
        if !matches!(op, "ping" | "approval_test") {
            self.host.ensure_mail_running();
        }
        let payload = json::dumps(&Json::Obj(request), true);
        let finished = self
            .host
            .run(&payload, Duration::from_secs(timeout_seconds));
        let (stdout, stderr) = match finished {
            Finished::TimedOut => return Err(failure("timed out", Some(op), false)),
            Finished::NotStarted => {
                return Err(failure("empty answer from osascript", Some(op), true))
            }
            Finished::TooLarge => {
                return Err(failure("unexpected answer from osascript", Some(op), true))
            }
            Finished::Done { stdout, stderr, .. } => (stdout, stderr),
        };
        let reason = |fallback: &str| {
            if stderr.is_empty() {
                fallback.to_string()
            } else {
                stderr.clone()
            }
        };
        let answer = stdout.trim();
        if answer.is_empty() {
            return Err(failure(
                &reason("empty answer from osascript"),
                Some(op),
                true,
            ));
        }
        let Ok(Json::Obj(result)) = json::parse(answer) else {
            return Err(failure(
                &reason("unexpected answer from osascript"),
                Some(op),
                true,
            ));
        };
        if result.get("ok") != Some(&Json::Bool(true)) {
            return Err(failure(&text_of(result.get("error")), Some(op), false));
        }
        Ok(result.get("data").cloned().unwrap_or(Json::Null))
    }

    fn account_request(op: &str, account: &Account) -> Object {
        let mut request = Object::new();
        request.insert("op", op);
        request.insert("accountId", account.id.as_str());
        request
    }

    fn unexpected(op: &str) -> ToolError {
        failure("unexpected answer from osascript", Some(op), true)
    }
}

fn optional_text(value: &Option<String>) -> Json {
    value.as_deref().map_or(Json::Null, Json::from)
}

impl Backend for RealBackend {
    fn all_accounts(&self) -> ToolResult<Vec<MailAccount>> {
        let mut request = Object::new();
        request.insert("op", "accounts");
        let data = self.call(request, JXA_TIMEOUT)?;
        let items = data
            .as_array()
            .ok_or_else(|| Self::unexpected("accounts"))?;
        Ok(items.iter().filter_map(MailAccount::from_json).collect())
    }

    fn account_info(&self, account: &Account) -> ToolResult<MailAccount> {
        let data = self.call(Self::account_request("account_info", account), JXA_TIMEOUT)?;
        MailAccount::from_json(&data).ok_or_else(|| Self::unexpected("account_info"))
    }

    fn mailboxes(&self, account: &Account) -> ToolResult<Vec<Mailbox>> {
        let mut request = Self::account_request("mailboxes", account);
        request.insert("depth", 2i64);
        let data = self.call(request, JXA_TIMEOUT)?;
        Mailbox::list_from_json(&data).ok_or_else(|| Self::unexpected("mailboxes"))
    }

    fn messages(&self, account: &Account, query: &MessageQuery) -> ToolResult<MessageList> {
        let mut request = Self::account_request("messages", account);
        request.insert("mailbox", optional_text(&query.mailbox));
        request.insert("sinceDays", query.since_days);
        request.insert("unreadOnly", Json::Bool(query.unread_only));
        request.insert("query", optional_text(&query.query));
        request.insert("field", query.field.as_str());
        request.insert("limit", query.limit);
        request.insert("maxHits", MAX_HITS);
        let data = self.call(request, JXA_TIMEOUT)?;
        MessageList::from_json(&data).ok_or_else(|| Self::unexpected("messages"))
    }

    fn read(
        &self,
        account: &Account,
        mailbox: Option<&str>,
        id: i64,
        max_chars: usize,
    ) -> ToolResult<MessageBody> {
        let mut request = Self::account_request("read", account);
        request.insert("mailbox", mailbox.map_or(Json::Null, Json::from));
        request.insert("id", id);
        request.insert("maxChars", i64::try_from(max_chars).unwrap_or(i64::MAX));
        let data = self.call(request, JXA_TIMEOUT)?;
        MessageBody::from_json(&data).ok_or_else(|| Self::unexpected("read"))
    }

    fn compose(&self, account: &Account, request: &ComposeRequest) -> ToolResult<Preview> {
        let mut payload = Self::account_request("compose", account);
        payload.insert("mode", request.mode.as_str());
        payload.insert("to", request.to.as_slice());
        payload.insert("cc", request.cc.as_slice());
        payload.insert("subject", request.subject.as_str());
        payload.insert("body", request.body.as_str());
        payload.insert(
            "reply",
            match &request.reply {
                Some(reply) => Json::object(vec![
                    ("mailbox", optional_text(&reply.mailbox)),
                    ("id", Json::Int(reply.id)),
                    ("all", Json::Bool(reply.all)),
                ]),
                None => Json::Null,
            },
        );
        payload.insert(
            "seconds",
            i64::try_from(APPROVAL_SECONDS).unwrap_or(i64::MAX),
        );
        payload.insert(
            "maxRecipients",
            i64::try_from(MAX_RECIPIENTS).unwrap_or(i64::MAX),
        );
        let data = self.call(payload, APPROVAL_SECONDS + 60)?;
        Preview::from_json(&data).ok_or_else(|| Self::unexpected("compose"))
    }

    fn approval_test(&self, seconds: u64) -> ToolResult<String> {
        let mut request = Object::new();
        request.insert("op", "approval_test");
        request.insert("seconds", i64::try_from(seconds).unwrap_or(i64::MAX));
        let data = self.call(request, seconds + 30)?;
        match data.as_object().and_then(|object| object.get("decision")) {
            Some(Json::Str(decision)) => Ok(decision.clone()),
            _ => Err(Self::unexpected("approval_test")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::with_labels;
    use crate::backend::{Mode, ReplySpec};
    use crate::testutil::{j, TestResult};
    use std::cell::RefCell;
    use std::collections::VecDeque;
    #[cfg(target_os = "macos")]
    use std::process::Command;
    use std::rc::Rc;

    #[derive(Default)]
    struct Script {
        answers: VecDeque<Finished>,
        payloads: Vec<String>,
        timeouts: Vec<Duration>,
        mail_checks: usize,
    }

    struct Scripted(Rc<RefCell<Script>>);

    impl Host for Scripted {
        fn ensure_mail_running(&self) {
            self.0.borrow_mut().mail_checks += 1;
        }

        fn run(&self, payload: &str, timeout: Duration) -> Finished {
            let mut script = self.0.borrow_mut();
            script.payloads.push(payload.to_string());
            script.timeouts.push(timeout);
            script.answers.pop_front().unwrap_or(Finished::NotStarted)
        }
    }

    fn backend(answers: Vec<Finished>) -> (RealBackend, Rc<RefCell<Script>>) {
        let script = Rc::new(RefCell::new(Script {
            answers: answers.into(),
            ..Script::default()
        }));
        (
            RealBackend::new(Box::new(Scripted(Rc::clone(&script)))),
            script,
        )
    }

    fn ok(data: &str) -> Finished {
        Finished::Done {
            code: Some(0),
            stdout: format!("{{\"ok\": true, \"data\": {data}}}\n"),
            stderr: String::new(),
        }
    }

    fn failed(error: &str) -> Finished {
        Finished::Done {
            code: Some(0),
            stdout: format!("{{\"ok\": false, \"error\": \"{error}\"}}"),
            stderr: String::new(),
        }
    }

    fn raw(stdout: &str, stderr: &str) -> Finished {
        Finished::Done {
            code: Some(1),
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    fn gmail() -> Account {
        let entry = MailAccount {
            id: "ACC-1".into(),
            name: "Privat".into(),
            kind: "imap".into(),
            addresses: vec!["me@example.org".into()],
        };
        with_labels(vec![entry]).remove(0)
    }

    fn payload(
        script: &Rc<RefCell<Script>>,
        index: usize,
    ) -> Result<Json, Box<dyn std::error::Error>> {
        let text = script
            .borrow()
            .payloads
            .get(index)
            .cloned()
            .ok_or("no payload")?;
        Ok(j(&text)?)
    }

    fn request(mode: Mode, reply: Option<ReplySpec>) -> ComposeRequest {
        ComposeRequest {
            mode,
            to: vec!["anna@example.org".into()],
            cc: vec![],
            subject: "Hallo".into(),
            body: "Text".into(),
            reply,
        }
    }

    #[test]
    fn accounts_are_decoded_and_mail_is_started_first() -> TestResult {
        let (backend, script) = backend(vec![ok(
            r#"[{"id": "A", "name": "N", "type": "imap", "addresses": ["a@example.org"]}, 5]"#,
        )]);
        let accounts = backend.all_accounts()?;
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].addresses, ["a@example.org"]);
        assert_eq!(script.borrow().mail_checks, 1);
        assert_eq!(script.borrow().timeouts, [Duration::from_secs(90)]);
        Ok(())
    }

    #[test]
    fn requests_carry_only_json_data_for_every_operation() -> TestResult {
        let (backend, script) = backend(vec![
            ok(r#"{"id": "A", "name": "N", "type": "imap", "addresses": []}"#),
            ok(r#"[{"path": "INBOX", "unread": 3}]"#),
            ok(r#"{"mailbox": "INBOX", "total": 0, "items": []}"#),
            ok(
                r#"{"mailbox": "INBOX", "subject": "S", "from": "F", "date": "d", "read": true, "to": [], "cc": [], "attachments": [], "bodyChars": 4, "body": "text"}"#,
            ),
        ]);
        let account = gmail();
        backend.account_info(&account)?;
        backend.mailboxes(&account)?;
        let query = MessageQuery {
            mailbox: Some("INBOX".into()),
            since_days: 30,
            unread_only: true,
            query: Some("a\"b".into()),
            field: "both".into(),
            limit: 5,
        };
        backend.messages(&account, &query)?;
        backend.read(&account, None, 7, 500)?;
        let first = payload(&script, 0)?;
        assert_eq!(first, j(r#"{"op": "account_info", "accountId": "ACC-1"}"#)?);
        assert_eq!(
            payload(&script, 1)?,
            j(r#"{"op": "mailboxes", "accountId": "ACC-1", "depth": 2}"#)?
        );
        assert_eq!(
            payload(&script, 2)?,
            j(
                r#"{"op": "messages", "accountId": "ACC-1", "mailbox": "INBOX", "sinceDays": 30,
                "unreadOnly": true, "query": "a\"b", "field": "both", "limit": 5, "maxHits": 1500}"#
            )?
        );
        assert_eq!(
            payload(&script, 3)?,
            j(
                r#"{"op": "read", "accountId": "ACC-1", "mailbox": null, "id": 7, "maxChars": 500}"#
            )?
        );
        Ok(())
    }

    #[test]
    fn compose_sends_the_request_with_the_approval_time_and_a_longer_deadline() -> TestResult {
        let preview = r#"{"from": "F", "to": ["anna@example.org"], "cc": [], "subject": "Hallo", "body": "Text", "reply": null, "status": "drafted", "saved": true}"#;
        let (backend, script) = backend(vec![ok(preview), ok(preview)]);
        let account = gmail();
        let reply = ReplySpec {
            mailbox: Some("INBOX".into()),
            id: 9,
            all: true,
        };
        backend.compose(&account, &request(Mode::Draft, None))?;
        let sent = backend.compose(&account, &request(Mode::Send, Some(reply)))?;
        assert_eq!(sent.status, "drafted");
        assert_eq!(
            payload(&script, 0)?,
            j(
                r#"{"op": "compose", "accountId": "ACC-1", "mode": "draft", "to": ["anna@example.org"],
                "cc": [], "subject": "Hallo", "body": "Text", "reply": null, "seconds": 75, "maxRecipients": 5}"#
            )?
        );
        assert_eq!(
            payload(&script, 1)?,
            j(
                r#"{"op": "compose", "accountId": "ACC-1", "mode": "send", "to": ["anna@example.org"],
                "cc": [], "subject": "Hallo", "body": "Text",
                "reply": {"mailbox": "INBOX", "id": 9, "all": true}, "seconds": 75, "maxRecipients": 5}"#
            )?
        );
        assert_eq!(script.borrow().timeouts, [Duration::from_secs(135); 2]);
        Ok(())
    }

    #[test]
    fn the_approval_test_skips_the_mail_start_and_reads_the_decision() -> TestResult {
        let (backend, script) = backend(vec![ok(r#"{"decision": "declined"}"#), ok("{}")]);
        assert_eq!(backend.approval_test(20)?, "declined");
        assert_eq!(script.borrow().mail_checks, 0);
        assert_eq!(script.borrow().timeouts, [Duration::from_secs(50)]);
        assert!(backend.approval_test(20).is_err());
        Ok(())
    }

    #[test]
    fn script_errors_are_mapped_for_the_model() {
        let (backend, _) = backend(vec![
            failed("account_not_found"),
            failed("mailbox_not_found"),
            failed("-1743 not authorized"),
        ]);
        let account = gmail();
        assert_eq!(
            backend.mailboxes(&account),
            Err(ToolError::msg(
                "The account no longer exists in Mail. Ask the user to run `avery mail setup` again."
            ))
        );
        assert!(
            matches!(backend.mailboxes(&account), Err(ToolError::Msg(m)) if m.starts_with("Mailbox not found."))
        );
        assert!(
            matches!(backend.mailboxes(&account), Err(ToolError::Msg(m)) if m.starts_with("macOS blocks access to Mail."))
        );
    }

    #[test]
    fn a_timeout_is_uncertain_only_for_compose() {
        let (backend, _) = backend(vec![Finished::TimedOut, Finished::TimedOut]);
        let account = gmail();
        let query = MessageQuery {
            mailbox: None,
            since_days: 3,
            unread_only: false,
            query: None,
            field: "both".into(),
            limit: 5,
        };
        assert!(
            matches!(backend.messages(&account, &query), Err(ToolError::Msg(m)) if m.contains("Retry with a smaller range"))
        );
        assert!(matches!(
            backend.compose(&account, &request(Mode::Send, None)),
            Err(ToolError::Uncertain(m)) if m.contains("may or may not")
        ));
    }

    #[test]
    fn empty_or_garbled_answers_are_uncertain_for_compose_and_plain_errors_otherwise() {
        let (backend, _) = backend(vec![
            raw("", ""),
            raw("", "execution error: boom"),
            raw("not json", ""),
            raw("[]", ""),
            Finished::NotStarted,
            Finished::TooLarge,
            raw("   \n", ""),
        ]);
        let account = gmail();
        let compose = request(Mode::Send, None);
        assert!(matches!(
            backend.compose(&account, &compose),
            Err(ToolError::Uncertain(_))
        ));
        assert!(
            matches!(backend.compose(&account, &compose), Err(ToolError::Uncertain(m)) if m.contains("boom"))
        );
        assert!(matches!(
            backend.compose(&account, &compose),
            Err(ToolError::Uncertain(_))
        ));
        assert!(matches!(
            backend.compose(&account, &compose),
            Err(ToolError::Uncertain(_))
        ));
        assert!(matches!(
            backend.compose(&account, &compose),
            Err(ToolError::Uncertain(_))
        ));
        assert!(matches!(
            backend.compose(&account, &compose),
            Err(ToolError::Uncertain(_))
        ));
        assert!(
            matches!(backend.mailboxes(&account), Err(ToolError::Msg(m)) if m.starts_with("Mail error:"))
        );
    }

    #[test]
    fn an_answer_of_the_wrong_shape_is_an_error_not_a_guess() {
        let (backend, _) = backend(vec![ok("{}"), ok("[1, 2]"), ok("null")]);
        let account = gmail();
        assert!(backend.mailboxes(&account).is_err());
        assert!(backend.account_info(&account).is_err());
        assert!(backend.all_accounts().is_err());
    }

    #[cfg(target_os = "macos")]
    const MOCK_MAIL: &str = r#"
var LOG = {dialog: null, sendCalled: false, deleted: 0};
var DECISION = 'Senden';
var SCENARIO = null;
var CURRENT = null;
function MOCKAPP() {
  return {
    includeStandardAdditions: false,
    activate: function () {},
    delay: function () {},
    displayDialog: function (text, opts) {
      LOG.dialog = text;
      if (SCENARIO.afterDialog) { SCENARIO.afterDialog(); }
      if (DECISION === 'Senden') { return {buttonReturned: 'Senden', gaveUp: false}; }
      throw new Error('User canceled. (-128)');
    }
  };
}
function collection(list) {
  return {
    push: function (x) { list.push(x); },
    address: function () { return list.map(function (r) { return r.address; }); },
    name: function () { return list.map(function () { return ''; }); }
  };
}
function makeMsg(o) {
  var stored = o.content || '', to = o.to || [], cc = [];
  var msg = {
    sender: function () { return o.sender; },
    subject: function () { return o.subject; },
    toRecipients: collection(to),
    ccRecipients: collection(cc),
    send: function () { LOG.sendCalled = true; if (SCENARIO.sendThrows) { throw new Error(SCENARIO.sendThrows); } },
    save: function () {}
  };
  Object.defineProperty(msg, 'content', {
    get: function () { return function () { return stored; }; },
    set: function (v) { stored = v; }
  });
  CURRENT = {to: to};
  return msg;
}
var ACC = {id: function () { return 'ACC-1'; }, emailAddresses: function () { return ['me@example.org']; }, fullName: function () { return 'Me'; }};
var fakeMail = {
  accounts: function () { return [ACC]; },
  outgoingMessages: {push: function () {}},
  OutgoingMessage: function (p) { return makeMsg({sender: p.sender, subject: p.subject, content: p.content}); },
  ToRecipient: function (p) { return {address: p.address}; },
  CcRecipient: function (p) { return {address: p.address}; },
  reply: function () {
    var to = SCENARIO.replyRecipients.map(function (a) { return {address: a}; });
    return makeMsg({sender: 'Me <me@example.org>', subject: SCENARIO.replySubject, content: 'quoted', to: to});
  },
  'delete': function () { LOG.deleted++; }
};
function mail() { return fakeMail; }
function original() {
  return {
    subject: function () { return SCENARIO.origSubject; },
    sender: function () { return SCENARIO.origFrom; },
    dateReceived: function () { return new Date(1700000000000); },
    readStatus: function () { return true; },
    toRecipients: collection([]),
    ccRecipients: collection([]),
    mailAttachments: {name: function () { return []; }, fileSize: function () { return []; }},
    content: function () { return 'Hallo'; },
    mailbox: function () {
      return {account: function () { return {id: function () { return SCENARIO.owner || 'ACC-1'; }}; }};
    }
  };
}
function findMailbox() { return {path: 'INBOX', ref: {messages: {byId: original}}}; }
function run(argv) {
  var input = JSON.parse(argv[0]), out;
  SCENARIO = input.scenario;
  DECISION = input.decision || 'Senden';
  if (SCENARIO.mutate) { SCENARIO.afterDialog = function () { CURRENT.to.push({address: 'extra@evil.example'}); }; }
  try { out = {ok: true, data: dispatch(input.req)}; }
  catch (e) { out = {ok: false, error: String((e && e.message) || e)}; }
  out.log = LOG;
  return JSON.stringify(out);
}
"#;

    #[cfg(target_os = "macos")]
    struct Probe {
        ok: bool,
        error: String,
        dialog: Option<String>,
        sent: bool,
        deleted: i64,
    }

    #[cfg(target_os = "macos")]
    fn compose_probe(
        scenario: &str,
        req: &str,
        decision: &str,
    ) -> Result<Probe, Box<dyn std::error::Error>> {
        let script = format!(
            "{}\n{MOCK_MAIL}",
            crate::osascript::mail_script()
                .replace("Application.currentApplication()", "MOCKAPP()")
        );
        let payload =
            format!(r#"{{"scenario": {scenario}, "req": {req}, "decision": "{decision}"}}"#);
        let output = Command::new(crate::osascript::OSASCRIPT)
            .args(["-l", "JavaScript", "-e", &script, &payload])
            .output()?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
        }
        let answer = j(String::from_utf8_lossy(&output.stdout).trim())?;
        let object = answer.as_object().ok_or("no object")?;
        let log = object
            .get("log")
            .and_then(Json::as_object)
            .ok_or("no log")?;
        Ok(Probe {
            ok: object.get("ok") == Some(&Json::Bool(true)),
            error: text_of(object.get("error")),
            dialog: log.get("dialog").and_then(Json::as_str).map(str::to_string),
            sent: log.get("sendCalled") == Some(&Json::Bool(true)),
            deleted: match log.get("deleted") {
                Some(Json::Int(count)) => *count,
                _ => -1,
            },
        })
    }

    #[cfg(target_os = "macos")]
    const NO_REPLY: &str =
        r#"{"replyRecipients": [], "replySubject": "", "origSubject": "", "origFrom": ""}"#;

    #[cfg(target_os = "macos")]
    fn new_mail(to: &str, mode: &str) -> String {
        format!(
            r#"{{"op": "compose", "accountId": "ACC-1", "mode": "{mode}", "to": {to}, "cc": [], "subject": "Hi", "body": "Text", "reply": null, "seconds": 75, "maxRecipients": 5}}"#
        )
    }

    #[cfg(target_os = "macos")]
    fn reply_mail(all: bool) -> String {
        format!(
            r#"{{"op": "compose", "accountId": "ACC-1", "mode": "send", "to": [], "cc": [], "subject": "", "body": "Text", "reply": {{"mailbox": null, "id": 5, "all": {all}}}, "seconds": 75, "maxRecipients": 5}}"#
        )
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_clean_send_shows_the_dialog_and_sends_after_the_click() -> TestResult {
        let probe = compose_probe(
            NO_REPLY,
            &new_mail(r#"["anna@example.org"]"#, "send"),
            "Senden",
        )?;
        assert!(
            probe.ok && probe.sent && probe.deleted == 0,
            "{}",
            probe.error
        );
        let dialog = probe.dialog.ok_or("no dialog")?;
        assert!(dialog.contains("An:         anna@example.org"));
        assert!(dialog.contains("Betreff:    Hi"));
        let declined = compose_probe(
            NO_REPLY,
            &new_mail(r#"["anna@example.org"]"#, "send"),
            "Abbrechen",
        )?;
        assert!(declined.ok && !declined.sent && declined.deleted == 1);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_reply_to_many_recipients_is_refused_before_any_dialog() -> TestResult {
        let many: Vec<String> = (0..40).map(|i| format!("\"p{i}@example.org\"")).collect();
        let scenario = format!(
            r#"{{"replyRecipients": [{}], "replySubject": "Re: Hi", "origSubject": "Hi", "origFrom": "Boss <boss@example.org>"}}"#,
            many.join(",")
        );
        let probe = compose_probe(&scenario, &reply_mail(true), "Senden")?;
        assert!(
            !probe.ok && probe.error == "too_many_recipients",
            "{}",
            probe.error
        );
        assert!(probe.dialog.is_none() && !probe.sent && probe.deleted == 1);
        let few = scenario.replace(&many.join(","), "\"a@example.org\", \"b@example.org\"");
        let allowed = compose_probe(&few, &reply_mail(true), "Senden")?;
        assert!(allowed.ok && allowed.sent, "{}", allowed.error);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn recipients_are_shown_completely_or_refused() -> TestResult {
        let long = format!(
            "ceo@company.example.{}.{}.attacker.example",
            "a".repeat(63),
            "b".repeat(63)
        );
        let probe = compose_probe(
            NO_REPLY,
            &new_mail(&format!("[\"{long}\"]"), "send"),
            "Senden",
        )?;
        assert!(probe.ok, "{}", probe.error);
        let dialog = probe.dialog.ok_or("no dialog")?;
        assert!(dialog.contains(&long) && dialog.contains("attacker.example"));
        let too_long = format!("a@{}.example", "b".repeat(250));
        for bad in [
            too_long.as_str(),
            "\u{e4}@example.org",
            "a@b.example\\u000bx@evil.example",
            "a b@example.org",
            "a@b",
        ] {
            let probe = compose_probe(
                NO_REPLY,
                &new_mail(&format!("[\"{bad}\"]"), "send"),
                "Senden",
            )?;
            assert!(
                !probe.ok && probe.error == "recipient_invalid",
                "{bad:?}: {}",
                probe.error
            );
            assert!(probe.dialog.is_none() && !probe.sent && probe.deleted == 1);
        }
        let draft = compose_probe(
            NO_REPLY,
            &new_mail(&format!("[\"{too_long}\"]"), "draft"),
            "Senden",
        )?;
        assert!(draft.ok && !draft.sent && draft.dialog.is_none());
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_hostile_reply_subject_cannot_forge_dialog_lines() -> TestResult {
        let subject = "Re: Rechnung\\u000b\\u000bAn:         ceo@company.example\\u000bBetreff:    Danke\\u0085--------------------------------\\u000cAlles in Ordnung \\udb40\\udc41\\ufe0f\\u202e";
        let scenario = format!(
            r#"{{"replyRecipients": ["x@example.org"], "replySubject": "{subject}", "origSubject": "{subject}", "origFrom": "Evil <evil@example.org>"}}"#
        );
        let probe = compose_probe(&scenario, &reply_mail(false), "Senden")?;
        assert!(probe.ok, "{}", probe.error);
        let dialog = probe.dialog.ok_or("no dialog")?;
        assert!(dialog.contains("Betreff:    Re: Rechnung An: ceo@company.example Betreff: Danke"));
        assert_eq!(
            dialog
                .lines()
                .filter(|line| line.starts_with("An:"))
                .count(),
            1
        );
        for forbidden in [
            '\u{b}',
            '\u{c}',
            '\u{85}',
            '\u{202e}',
            '\u{fe0f}',
            '\u{e0041}',
        ] {
            assert!(!dialog.contains(forbidden), "{:x}", u32::from(forbidden));
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_failure_after_the_click_is_marked_as_unknown_state() -> TestResult {
        let scenario = r#"{"sendThrows": "Connection is invalid. (-609)", "replyRecipients": [], "replySubject": "", "origSubject": "", "origFrom": ""}"#;
        let probe = compose_probe(
            scenario,
            &new_mail(r#"["anna@example.org"]"#, "send"),
            "Senden",
        )?;
        assert!(!probe.ok && probe.sent);
        assert!(
            probe.error.starts_with("send_state_unknown:"),
            "{}",
            probe.error
        );
        assert_eq!(probe.deleted, 0);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn recipients_changed_after_the_click_abort_the_send() -> TestResult {
        let scenario = r#"{"mutate": true, "replyRecipients": [], "replySubject": "", "origSubject": "", "origFrom": ""}"#;
        let probe = compose_probe(
            scenario,
            &new_mail(r#"["anna@example.org"]"#, "send"),
            "Senden",
        )?;
        assert!(
            !probe.ok && probe.error == "content_mismatch",
            "{}",
            probe.error
        );
        assert!(!probe.sent && probe.deleted == 1);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_message_of_another_account_is_neither_read_nor_answered() -> TestResult {
        let foreign = r#"{"owner": "ACC-2", "replyRecipients": ["x@example.org"], "replySubject": "Re: Hi", "origSubject": "Hi", "origFrom": "Boss <boss@example.org>"}"#;
        let probe = compose_probe(foreign, &reply_mail(false), "Senden")?;
        assert!(!probe.ok && probe.error == "message_not_in_account" && !probe.sent);
        let read =
            r#"{"op": "read", "accountId": "ACC-1", "mailbox": null, "id": 5, "maxChars": 100}"#;
        let probe = compose_probe(foreign, read, "Senden")?;
        assert!(!probe.ok && probe.error == "message_not_in_account");
        let own = foreign.replace("ACC-2", "ACC-1");
        let probe = compose_probe(&own, read, "Senden")?;
        assert!(probe.ok, "{}", probe.error);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn probe(body: &str) -> Result<String, Box<dyn std::error::Error>> {
        let script = format!("{}\n{body}", crate::osascript::mail_script());
        let output = Command::new(crate::osascript::OSASCRIPT)
            .args(["-l", "JavaScript", "-e", &script])
            .output()?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn dialog_fields_are_one_line_and_cut_in_the_script() -> TestResult {
        let answer = probe(
            r"
function run(argv) {
  return JSON.stringify([flat('a\nb\r\nc\td', 50), flat('x'.repeat(300), 20).length, flat(null, 5),
                         flatList(['a@b.c', 'd\n@e.f']), squash(' a b\n c\u00a0d '), squash(null)]);
}
",
        )?;
        assert_eq!(
            j(&answer)?,
            j(r#"["a b c d", 20, "", "a@b.c, d @e.f", "abcd", ""]"#)?
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn whose_clauses_are_built_the_way_mail_accepts_them() -> TestResult {
        let answer = probe(
            r"
var calls = [];
var mock = {
  whose: function (arg) {
    calls.push(arg);
    if (arg && arg._and && arg._and.length < 2) { throw new Error('Whose clause _and needs two elements.'); }
    return {id: function () { return []; }};
  }
};
function findMailbox() { return {ref: {messages: mock}, path: 'INBOX'}; }
function run(argv) {
  var cases = [{sinceDays: 30}, {sinceDays: 30, unreadOnly: true}, {sinceDays: 30, query: 'x', field: 'both'},
               {sinceDays: 30, unreadOnly: true, query: 'x', field: 'subject'}], out = [];
  for (var i = 0; i < cases.length; i++) {
    calls = [];
    var req = cases[i]; req.maxHits = 1500; req.limit = 5; req.mailbox = null;
    try {
      queryMessages({}, req);
      out.push(calls.map(function (c) { return c._and ? '_and:' + c._and.length : Object.keys(c)[0]; }).join(','));
    } catch (e) { out.push('ERROR ' + String(e.message)); }
  }
  return JSON.stringify(out);
}
",
        )?;
        assert_eq!(
            j(&answer)?,
            j(r#"["dateReceived", "_and:2", "_and:2", "_and:3"]"#)?
        );
        Ok(())
    }
}
