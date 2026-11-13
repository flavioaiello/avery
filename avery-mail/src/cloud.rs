mod auth;
mod dialog;
mod gmail;
mod google_calendar;
mod graph;
mod graph_calendar;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use crate::account::{Account, MailAccount};
use crate::backend::{
    Backend, ComposeRequest, Mailbox, MessageBody, MessageId, MessageList, MessageQuery, Preview,
};
use crate::calendar::{
    Approval, Calendar, CalendarBackend, CalendarInfo, Change, ChangeResult, EventDetail, RawEvent,
    Window,
};
use crate::error::{ToolError, ToolResult};
use crate::http::{Curl, HttpError, Method, Request, Transport};
use crate::json::{self, Json};
use crate::localtime;
use crate::osascript::{Host, OsascriptHost};
use crate::source::{account_address, calendar_id, calendar_parts, Source};
use crate::text::one_line_str;
use crate::vault::{Keychain, Vault};

const READ_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_TIMEOUT: Duration = Duration::from_secs(45);
const PARALLEL: usize = 8;
const MAX_PARALLEL_REQUESTS: usize = 100;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Reply {
    pub(crate) status: u16,
    pub(crate) json: Json,
}

pub(crate) struct Api<'a> {
    transport: &'a dyn Transport,
    token: String,
    source: Source,
    headers: Vec<(String, String)>,
}

fn provider_detail(json: &Json) -> String {
    let Some(object) = json.as_object() else {
        return String::new();
    };
    let text = match object.get("error") {
        Some(Json::Obj(error)) => error.get("message").and_then(Json::as_str),
        Some(Json::Str(code)) => object
            .get("error_description")
            .and_then(Json::as_str)
            .or(Some(code.as_str())),
        _ => None,
    };
    match text {
        Some(text) if !text.trim().is_empty() => format!(": {}", one_line_str(text, 160)),
        _ => String::new(),
    }
}

fn uncertain() -> ToolError {
    ToolError::Uncertain(
        concat!(
            "The provider did not confirm the change. It may or may not have happened: ask the ",
            "user to check before you try again."
        )
        .to_string(),
    )
}

fn transport_error(error: &HttpError, write: bool) -> ToolError {
    match error {
        HttpError::Refused => ToolError::internal("request"),
        HttpError::NotStarted => ToolError::msg(
            "curl could not be started, so the provider cannot be reached. NOTHING was done.",
        ),
        HttpError::TooLarge | HttpError::TimedOut | HttpError::Failed(_) if write => uncertain(),
        HttpError::TooLarge => ToolError::msg("The answer of the provider was too large."),
        HttpError::TimedOut => ToolError::msg(
            "The provider did not answer in time. Retry later or with a smaller range.",
        ),
        HttpError::Failed(text) => ToolError::msg(format!(
            "The provider could not be reached ({}). Check the internet connection and retry.",
            one_line_str(text, 120)
        )),
    }
}

impl<'a> Api<'a> {
    pub(crate) fn new(transport: &'a dyn Transport, token: String, source: Source) -> Api<'a> {
        Api {
            transport,
            token,
            source,
            headers: Vec::new(),
        }
    }

    pub(crate) fn with_header(mut self, name: &str, value: &str) -> Api<'a> {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    fn request(&self, method: Method, url: &str, body: Option<&Json>) -> Request {
        let timeout = if method == Method::Get {
            READ_TIMEOUT
        } else {
            WRITE_TIMEOUT
        };
        let mut request = Request::new(method, url, timeout)
            .header("Authorization", &format!("Bearer {}", self.token));
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }
        match body {
            Some(body) => request
                .header("Content-Type", "application/json")
                .body(&json::dumps(body, true)),
            None => request,
        }
    }

    fn call(&self, method: Method, url: &str, body: Option<&Json>) -> ToolResult<Reply> {
        let write = method != Method::Get;
        let response = self
            .transport
            .send(&self.request(method, url, body))
            .map_err(|error| transport_error(&error, write))?;
        let success = (200..300).contains(&response.status);
        let json = if response.body.trim().is_empty() {
            Json::Null
        } else {
            match json::parse(&response.body) {
                Ok(json) => json,
                Err(_) if success && !write => {
                    return Err(ToolError::msg("The provider gave an unexpected answer."))
                }
                Err(_) if success => return Err(uncertain()),
                Err(_) => Json::Null,
            }
        };
        Ok(Reply {
            status: response.status,
            json,
        })
    }

    fn failed(&self, reply: &Reply, write: bool) -> ToolError {
        let detail = provider_detail(&reply.json);
        match reply.status {
            401 => ToolError::msg(format!(
                "The provider no longer accepts the sign-in of this account. {}",
                auth::relink(self.source)
            )),
            403 => ToolError::msg(format!(
                "The provider refused the request (403){detail}. The account may lack a permission (the user can link it again with more rights) or the API is not enabled for the client."
            )),
            404 => ToolError::msg(
                "Not found at the provider. The id may be wrong or the item no longer exists.",
            ),
            429 => ToolError::msg(
                "The provider limits requests right now (429). Retry in a minute.",
            ),
            500..=599 if write => uncertain(),
            500..=599 => ToolError::msg(format!(
                "The provider has a temporary problem ({}). Retry later.",
                reply.status
            )),
            status => ToolError::msg(format!(
                "The provider refused the request ({status}){detail}."
            )),
        }
    }

    pub(crate) fn get(&self, url: &str) -> ToolResult<Json> {
        let reply = self.call(Method::Get, url, None)?;
        if (200..300).contains(&reply.status) {
            Ok(reply.json)
        } else {
            Err(self.failed(&reply, false))
        }
    }

    pub(crate) fn get_optional(&self, url: &str) -> ToolResult<Option<Json>> {
        let reply = self.call(Method::Get, url, None)?;
        match reply.status {
            200..=299 => Ok(Some(reply.json)),
            404 => Ok(None),
            _ => Err(self.failed(&reply, false)),
        }
    }

    pub(crate) fn send(&self, method: Method, url: &str, body: Option<&Json>) -> ToolResult<Json> {
        let reply = self.call(method, url, body)?;
        if (200..300).contains(&reply.status) {
            Ok(reply.json)
        } else {
            Err(self.failed(&reply, true))
        }
    }

    pub(crate) fn get_many(&self, urls: &[String]) -> Vec<ToolResult<Json>> {
        let urls = urls.get(..MAX_PARALLEL_REQUESTS).unwrap_or(urls);
        let next = AtomicUsize::new(0);
        let slots: Vec<Mutex<Option<ToolResult<Json>>>> =
            urls.iter().map(|_| Mutex::new(None)).collect();
        thread::scope(|scope| {
            for _ in 0..PARALLEL.min(urls.len()) {
                scope.spawn(|| loop {
                    let index = next.fetch_add(1, Ordering::SeqCst);
                    let Some(url) = urls.get(index) else {
                        break;
                    };
                    let result = self.get(url);
                    if let Some(Ok(mut slot)) = slots.get(index).map(Mutex::lock) {
                        *slot = Some(result);
                    }
                });
            }
        });
        slots
            .into_iter()
            .map(|slot| {
                slot.into_inner()
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| Err(ToolError::internal("parallel")))
            })
            .collect()
    }
}

pub(crate) struct CloudMail {
    transport: Box<dyn Transport>,
    vault: Box<dyn Vault>,
    host: Box<dyn Host>,
}

fn not_linked() -> ToolError {
    ToolError::msg("This account is not a linked Gmail or Outlook.com account.")
}

impl CloudMail {
    pub(crate) fn new(
        transport: Box<dyn Transport>,
        vault: Box<dyn Vault>,
        host: Box<dyn Host>,
    ) -> CloudMail {
        CloudMail {
            transport,
            vault,
            host,
        }
    }

    pub(crate) fn system() -> CloudMail {
        CloudMail::new(
            Box::new(Curl::system()),
            Box::new(Keychain::system()),
            Box::new(OsascriptHost),
        )
    }

    fn with_api<T>(
        &self,
        account: &Account,
        action: impl FnOnce(&Api, Source) -> ToolResult<T>,
    ) -> ToolResult<T> {
        let (source, address) = account_address(&account.id).ok_or_else(not_linked)?;
        let token = auth::access_token(
            self.transport.as_ref(),
            self.vault.as_ref(),
            source,
            address,
        )?;
        action(&Api::new(self.transport.as_ref(), token, source), source)
    }
}

impl Backend for CloudMail {
    fn all_accounts(&self) -> ToolResult<Vec<MailAccount>> {
        Ok(Vec::new())
    }

    fn account_info(&self, account: &Account) -> ToolResult<MailAccount> {
        self.with_api(account, |api, source| match source {
            Source::Google => gmail::account_info(api, account),
            Source::Microsoft => graph::account_info(api, account),
            Source::Apple => Err(not_linked()),
        })
    }

    fn mailboxes(&self, account: &Account) -> ToolResult<Vec<Mailbox>> {
        self.with_api(account, |api, source| match source {
            Source::Google => gmail::mailboxes(api),
            Source::Microsoft => graph::mailboxes(api),
            Source::Apple => Err(not_linked()),
        })
    }

    fn messages(&self, account: &Account, query: &MessageQuery) -> ToolResult<MessageList> {
        self.with_api(account, |api, source| match source {
            Source::Google => gmail::messages(api, query),
            Source::Microsoft => graph::messages(api, query),
            Source::Apple => Err(not_linked()),
        })
    }

    fn read(
        &self,
        account: &Account,
        _mailbox: Option<&str>,
        id: &MessageId,
        max_chars: usize,
    ) -> ToolResult<MessageBody> {
        self.with_api(account, |api, source| match source {
            Source::Google => gmail::read(api, id, max_chars),
            Source::Microsoft => graph::read(api, id, max_chars),
            Source::Apple => Err(not_linked()),
        })
    }

    fn compose(&self, account: &Account, request: &ComposeRequest) -> ToolResult<Preview> {
        self.with_api(account, |api, source| match source {
            Source::Google => gmail::compose(api, self.host.as_ref(), account, request),
            Source::Microsoft => graph::compose(api, self.host.as_ref(), account, request),
            Source::Apple => Err(not_linked()),
        })
    }

    fn approval_test(&self, seconds: u64) -> ToolResult<String> {
        dialog::test(self.host.as_ref(), seconds)
    }
}

pub(crate) struct MailRouter {
    apple: Box<dyn Backend>,
    cloud: CloudMail,
}

impl MailRouter {
    pub(crate) fn new(apple: Box<dyn Backend>, cloud: CloudMail) -> MailRouter {
        MailRouter { apple, cloud }
    }

    fn pick(&self, account: &Account) -> &dyn Backend {
        match Source::of_id(&account.id) {
            Source::Apple => self.apple.as_ref(),
            Source::Google | Source::Microsoft => &self.cloud,
        }
    }
}

impl Backend for MailRouter {
    fn all_accounts(&self) -> ToolResult<Vec<MailAccount>> {
        self.apple.all_accounts()
    }

    fn account_info(&self, account: &Account) -> ToolResult<MailAccount> {
        self.pick(account).account_info(account)
    }

    fn mailboxes(&self, account: &Account) -> ToolResult<Vec<Mailbox>> {
        self.pick(account).mailboxes(account)
    }

    fn messages(&self, account: &Account, query: &MessageQuery) -> ToolResult<MessageList> {
        self.pick(account).messages(account, query)
    }

    fn read(
        &self,
        account: &Account,
        mailbox: Option<&str>,
        id: &MessageId,
        max_chars: usize,
    ) -> ToolResult<MessageBody> {
        self.pick(account).read(account, mailbox, id, max_chars)
    }

    fn compose(&self, account: &Account, request: &ComposeRequest) -> ToolResult<Preview> {
        self.pick(account).compose(account, request)
    }

    fn approval_test(&self, seconds: u64) -> ToolResult<String> {
        self.cloud.approval_test(seconds)
    }
}

pub(crate) struct CloudCalendar {
    transport: Box<dyn Transport>,
    vault: Box<dyn Vault>,
    host: Box<dyn Host>,
}

fn no_zone() -> ToolError {
    ToolError::msg(
        "The time zone of this Mac could not be determined, so calendars of Google or Outlook.com cannot be used. Ask the user to set the time zone in System Settings.",
    )
}

impl CloudCalendar {
    pub(crate) fn new(
        transport: Box<dyn Transport>,
        vault: Box<dyn Vault>,
        host: Box<dyn Host>,
    ) -> CloudCalendar {
        CloudCalendar {
            transport,
            vault,
            host,
        }
    }

    pub(crate) fn system() -> CloudCalendar {
        CloudCalendar::new(
            Box::new(Curl::system()),
            Box::new(Keychain::system()),
            Box::new(OsascriptHost),
        )
    }

    fn with_api<T>(
        &self,
        source: Source,
        address: &str,
        action: impl FnOnce(&Api) -> ToolResult<T>,
    ) -> ToolResult<T> {
        let token = auth::access_token(
            self.transport.as_ref(),
            self.vault.as_ref(),
            source,
            address,
        )?;
        action(&Api::new(self.transport.as_ref(), token, source))
    }

    fn with_calendar<T>(
        &self,
        calendar: &Calendar,
        action: impl FnOnce(&Api, Source, &str, &str) -> ToolResult<T>,
    ) -> ToolResult<T> {
        let (source, address, id) = calendar_parts(&calendar.id).ok_or_else(not_linked)?;
        let zone = localtime::zone().ok_or_else(no_zone)?;
        self.with_api(source, address, |api| action(api, source, id, &zone))
    }
}

impl CalendarBackend for CloudCalendar {
    fn calendars(&self, source: Source, address: &str) -> ToolResult<Vec<CalendarInfo>> {
        let found = self.with_api(source, address, |api| match source {
            Source::Google => google_calendar::calendars(api),
            Source::Microsoft => graph_calendar::calendars(api),
            Source::Apple => Err(not_linked()),
        })?;
        Ok(found
            .into_iter()
            .filter_map(|info| {
                Some(CalendarInfo {
                    id: calendar_id(source, address, &info.id)?,
                    ..info
                })
            })
            .collect())
    }

    fn events(&self, calendar: &Calendar, window: Window) -> ToolResult<Vec<RawEvent>> {
        self.with_calendar(calendar, |api, source, id, zone| match source {
            Source::Google => google_calendar::events(api, id, window, zone),
            Source::Microsoft => graph_calendar::events(api, id, window, zone),
            Source::Apple => Err(not_linked()),
        })
    }

    fn event(&self, calendar: &Calendar, uid: &str) -> ToolResult<EventDetail> {
        self.with_calendar(calendar, |api, source, id, zone| match source {
            Source::Google => google_calendar::event(api, id, uid, zone),
            Source::Microsoft => graph_calendar::event(api, id, uid, zone),
            Source::Apple => Err(not_linked()),
        })
    }

    fn change(
        &self,
        calendar: &Calendar,
        change: &Change,
        approval: &Approval,
    ) -> ToolResult<ChangeResult> {
        let host = self.host.as_ref();
        self.with_calendar(calendar, |api, source, id, zone| match source {
            Source::Google => {
                google_calendar::change(api, host, calendar, id, zone, change, approval)
            }
            Source::Microsoft => {
                graph_calendar::change(api, host, calendar, id, zone, change, approval)
            }
            Source::Apple => Err(not_linked()),
        })
    }
}

pub(crate) struct CalendarRouter {
    apple: Box<dyn CalendarBackend>,
    cloud: CloudCalendar,
}

impl CalendarRouter {
    pub(crate) fn new(apple: Box<dyn CalendarBackend>, cloud: CloudCalendar) -> CalendarRouter {
        CalendarRouter { apple, cloud }
    }

    fn pick(&self, calendar: &Calendar) -> &dyn CalendarBackend {
        match Source::of_id(&calendar.id) {
            Source::Apple => self.apple.as_ref(),
            Source::Google | Source::Microsoft => &self.cloud,
        }
    }
}

impl CalendarBackend for CalendarRouter {
    fn calendars(&self, source: Source, address: &str) -> ToolResult<Vec<CalendarInfo>> {
        match source {
            Source::Apple => self.apple.calendars(source, address),
            Source::Google | Source::Microsoft => self.cloud.calendars(source, address),
        }
    }

    fn events(&self, calendar: &Calendar, window: Window) -> ToolResult<Vec<RawEvent>> {
        self.pick(calendar).events(calendar, window)
    }

    fn event(&self, calendar: &Calendar, uid: &str) -> ToolResult<EventDetail> {
        self.pick(calendar).event(calendar, uid)
    }

    fn change(
        &self,
        calendar: &Calendar,
        change: &Change,
        approval: &Approval,
    ) -> ToolResult<ChangeResult> {
        self.pick(calendar).change(calendar, change, approval)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::scripted::Scripted;
    use crate::testutil::TestResult;

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
}
