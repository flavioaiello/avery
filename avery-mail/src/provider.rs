mod auth;
mod dialog;
mod gmail;
mod google_calendar;
mod graph;
mod graph_calendar;
mod link;
#[cfg(test)]
mod tests;

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
    RealCalendar, Window,
};
use crate::error::{ToolError, ToolResult};
use crate::http::{Curl, HttpError, Method, Request, Transport};
use crate::json::{self, Json, Object};
use crate::localtime;
use crate::osascript::{Host, OsascriptHost};
use crate::real::RealBackend;
use crate::text::{one_line_str, plain_address};
use crate::vault::{Keychain, Vault};

#[cfg(test)]
pub(crate) use link::fakes;
pub(crate) use link::{Client, Linker};

const MAX_CALENDAR_ID: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Provider {
    Apple,
    Google,
    Microsoft,
}

pub(crate) struct OAuth {
    pub(crate) authorize: &'static str,
    pub(crate) token: &'static str,
    pub(crate) userinfo: &'static str,
    pub(crate) loopback: &'static str,
    pub(crate) secret: bool,
    pub(crate) scope_on_refresh: bool,
    pub(crate) authorize_extras: &'static [(&'static str, &'static str)],
    pub(crate) identity: &'static [&'static str],
    pub(crate) mail_read: &'static [&'static str],
    pub(crate) calendar_read: &'static [&'static str],
    pub(crate) mail_write: &'static [&'static str],
    pub(crate) calendar_write: &'static [&'static str],
    pub(crate) registration: &'static [&'static str],
    pub(crate) identify: fn(&Object) -> Result<String, String>,
}

static GOOGLE: OAuth = OAuth {
    authorize: "https://accounts.google.com/o/oauth2/v2/auth",
    token: "https://oauth2.googleapis.com/token",
    userinfo: "https://openidconnect.googleapis.com/v1/userinfo",
    loopback: "127.0.0.1",
    secret: true,
    scope_on_refresh: false,
    authorize_extras: &[("access_type", "offline"), ("prompt", "consent")],
    identity: &["openid", "email"],
    mail_read: &["https://www.googleapis.com/auth/gmail.readonly"],
    calendar_read: &["https://www.googleapis.com/auth/calendar.readonly"],
    mail_write: &["https://www.googleapis.com/auth/gmail.compose"],
    calendar_write: &["https://www.googleapis.com/auth/calendar.events"],
    registration: &[
        "Gmail und Google Kalender direkt verbinden. Du brauchst eine eigene OAuth-Client-ID",
        "(Google Cloud Console, Typ 'Desktop-App', Gmail API und Google Calendar API aktiviert).",
        "Nur persoenliche Gmail-Konten, keine Google-Workspace- oder Organisationskonten.",
    ],
    identify: google_address,
};

static MICROSOFT: OAuth = OAuth {
    authorize: "https://login.microsoftonline.com/consumers/oauth2/v2.0/authorize",
    token: "https://login.microsoftonline.com/consumers/oauth2/v2.0/token",
    userinfo: "https://graph.microsoft.com/v1.0/me?$select=mail,userPrincipalName",
    loopback: "localhost",
    secret: false,
    scope_on_refresh: true,
    authorize_extras: &[("response_mode", "query"), ("prompt", "select_account")],
    identity: &["offline_access", "User.Read"],
    mail_read: &["Mail.Read"],
    calendar_read: &["Calendars.Read"],
    mail_write: &["Mail.ReadWrite", "Mail.Send"],
    calendar_write: &["Calendars.ReadWrite"],
    registration: &[
        "Outlook.com und dessen Kalender direkt verbinden (nur persoenliche Microsoft-Konten).",
        "Du brauchst eine eigene App-Registrierung (Entra: 'Nur persoenliche Microsoft-Konten',",
        "Plattform 'Mobil- und Desktopanwendungen', Umleitungs-URI http://localhost).",
    ],
    identify: microsoft_address,
};

fn first_address(candidates: &[Option<&Json>]) -> Result<String, String> {
    candidates
        .iter()
        .filter_map(|candidate| candidate.and_then(Json::as_str))
        .find(|candidate| plain_address(candidate))
        .map(str::to_lowercase)
        .ok_or_else(|| "The account has no usable mail address.".to_string())
}

fn google_address(userinfo: &Object) -> Result<String, String> {
    if !matches!(userinfo.get("hd"), None | Some(Json::Null)) {
        return Err(
            "Avery verbindet nur persoenliche Gmail-Konten, keine Organisationskonten (Google Workspace)."
                .to_string(),
        );
    }
    if userinfo.get("email_verified") == Some(&Json::Bool(false)) {
        return Err("Google says that this address is not verified.".to_string());
    }
    first_address(&[userinfo.get("email")])
}

fn microsoft_address(userinfo: &Object) -> Result<String, String> {
    first_address(&[userinfo.get("mail"), userinfo.get("userPrincipalName")])
}

impl OAuth {
    pub(crate) fn scopes(&self, write: bool) -> Vec<&'static str> {
        let mut scopes: Vec<&'static str> = Vec::new();
        scopes.extend(self.identity);
        scopes.extend(self.mail_read);
        scopes.extend(self.calendar_read);
        if write {
            scopes.extend(self.mail_write);
            scopes.extend(self.calendar_write);
        }
        scopes
    }

    pub(crate) fn required(&self, write: bool) -> Vec<&'static str> {
        let mut scopes: Vec<&'static str> = Vec::new();
        scopes.extend(self.mail_read);
        scopes.extend(self.calendar_read);
        if write {
            scopes.extend(self.mail_write);
            scopes.extend(self.calendar_write);
        }
        scopes
    }

    pub(crate) fn redirect(&self, port: u16) -> String {
        format!("http://{}:{port}/", self.loopback)
    }
}

impl Provider {
    pub(crate) const ALL: [Provider; 3] = [Provider::Apple, Provider::Google, Provider::Microsoft];

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Provider::Apple => "apple",
            Provider::Google => "google",
            Provider::Microsoft => "microsoft",
        }
    }

    pub(crate) fn parse_remote(text: &str) -> Option<Provider> {
        Provider::ALL
            .into_iter()
            .find(|provider| provider.oauth().is_some() && provider.as_str() == text)
    }

    pub(crate) fn kind(self) -> &'static str {
        match self {
            Provider::Apple => "apple",
            Provider::Google => "gmail",
            Provider::Microsoft => "outlook",
        }
    }

    pub(crate) fn oauth(self) -> Option<&'static OAuth> {
        match self {
            Provider::Apple => None,
            Provider::Google => Some(&GOOGLE),
            Provider::Microsoft => Some(&MICROSOFT),
        }
    }

    pub(crate) fn hosts(self) -> &'static [&'static str] {
        match self {
            Provider::Apple => &[],
            Provider::Google => &[
                "gmail.googleapis.com",
                "www.googleapis.com",
                "oauth2.googleapis.com",
                "openidconnect.googleapis.com",
                "accounts.google.com",
            ],
            Provider::Microsoft => &["graph.microsoft.com", "login.microsoftonline.com"],
        }
    }

    pub(crate) fn of_id(id: &str) -> Provider {
        match id.split_once(':') {
            Some(("google", _)) => Provider::Google,
            Some(("microsoft", _)) => Provider::Microsoft,
            _ => Provider::Apple,
        }
    }
}

pub(crate) fn account_id(provider: Provider, address: &str) -> Option<String> {
    (provider.oauth().is_some() && plain_address(address))
        .then(|| format!("{}:{address}", provider.as_str()))
}

pub(crate) fn account_address(id: &str) -> Option<(Provider, &str)> {
    let provider = Provider::of_id(id);
    let (_, address) = id.split_once(':')?;
    (provider.oauth().is_some() && plain_address(address)).then_some((provider, address))
}

fn calendar_token(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_CALENDAR_ID
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._%+-=@#~".contains(c))
}

pub(crate) fn calendar_id(provider: Provider, address: &str, calendar: &str) -> Option<String> {
    (provider.oauth().is_some() && plain_address(address) && calendar_token(calendar))
        .then(|| format!("{}:{address}:{calendar}", provider.as_str()))
}

pub(crate) fn calendar_parts(id: &str) -> Option<(Provider, &str, &str)> {
    let provider = Provider::of_id(id);
    provider.oauth()?;
    let mut parts = id.splitn(3, ':');
    parts.next()?;
    let address = parts.next()?;
    let calendar = parts.next()?;
    (plain_address(address) && calendar_token(calendar)).then_some((provider, address, calendar))
}

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
    provider: Provider,
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
    pub(crate) fn new(transport: &'a dyn Transport, token: String, provider: Provider) -> Api<'a> {
        Api {
            transport,
            token,
            provider,
            headers: Vec::new(),
        }
    }

    pub(crate) fn with_header(&self, name: &str, value: &str) -> Api<'a> {
        let mut headers = self.headers.clone();
        headers.push((name.to_string(), value.to_string()));
        Api {
            transport: self.transport,
            token: self.token.clone(),
            provider: self.provider,
            headers,
        }
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
                auth::relink(self.provider)
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

pub(crate) struct Mail {
    apple: Box<dyn Backend>,
    transport: Box<dyn Transport>,
    vault: Box<dyn Vault>,
    host: Box<dyn Host>,
}

fn not_linked() -> ToolError {
    ToolError::msg("This account is not a linked Gmail or Outlook.com account.")
}

impl Mail {
    pub(crate) fn new(
        apple: Box<dyn Backend>,
        transport: Box<dyn Transport>,
        vault: Box<dyn Vault>,
        host: Box<dyn Host>,
    ) -> Mail {
        Mail {
            apple,
            transport,
            vault,
            host,
        }
    }

    pub(crate) fn system() -> Mail {
        Mail::new(
            Box::new(RealBackend::system()),
            Box::new(Curl::system()),
            Box::new(Keychain::system()),
            Box::new(OsascriptHost),
        )
    }

    fn remote<T>(
        &self,
        account: &Account,
        action: impl FnOnce(&Api) -> ToolResult<T>,
    ) -> ToolResult<T> {
        let (provider, address) = account_address(&account.id).ok_or_else(not_linked)?;
        let token = auth::access_token(
            self.transport.as_ref(),
            self.vault.as_ref(),
            provider,
            address,
        )?;
        action(&Api::new(self.transport.as_ref(), token, provider))
    }
}

impl Backend for Mail {
    fn all_accounts(&self) -> ToolResult<Vec<MailAccount>> {
        self.apple.all_accounts()
    }

    fn account_info(&self, account: &Account) -> ToolResult<MailAccount> {
        match Provider::of_id(&account.id) {
            Provider::Apple => self.apple.account_info(account),
            Provider::Google => self.remote(account, |api| gmail::account_info(api, account)),
            Provider::Microsoft => self.remote(account, |api| graph::account_info(api, account)),
        }
    }

    fn mailboxes(&self, account: &Account) -> ToolResult<Vec<Mailbox>> {
        match Provider::of_id(&account.id) {
            Provider::Apple => self.apple.mailboxes(account),
            Provider::Google => self.remote(account, gmail::mailboxes),
            Provider::Microsoft => self.remote(account, graph::mailboxes),
        }
    }

    fn messages(&self, account: &Account, query: &MessageQuery) -> ToolResult<MessageList> {
        match Provider::of_id(&account.id) {
            Provider::Apple => self.apple.messages(account, query),
            Provider::Google => self.remote(account, |api| gmail::messages(api, query)),
            Provider::Microsoft => self.remote(account, |api| graph::messages(api, query)),
        }
    }

    fn read(
        &self,
        account: &Account,
        mailbox: Option<&str>,
        id: &MessageId,
        max_chars: usize,
    ) -> ToolResult<MessageBody> {
        match Provider::of_id(&account.id) {
            Provider::Apple => self.apple.read(account, mailbox, id, max_chars),
            Provider::Google => self.remote(account, |api| gmail::read(api, id, max_chars)),
            Provider::Microsoft => self.remote(account, |api| graph::read(api, id, max_chars)),
        }
    }

    fn compose(&self, account: &Account, request: &ComposeRequest) -> ToolResult<Preview> {
        let host = self.host.as_ref();
        match Provider::of_id(&account.id) {
            Provider::Apple => self.apple.compose(account, request),
            Provider::Google => {
                self.remote(account, |api| gmail::compose(api, host, account, request))
            }
            Provider::Microsoft => {
                self.remote(account, |api| graph::compose(api, host, account, request))
            }
        }
    }

    fn approval_test(&self, seconds: u64) -> ToolResult<String> {
        dialog::test(self.host.as_ref(), seconds)
    }
}

pub(crate) struct Calendars {
    apple: Box<dyn CalendarBackend>,
    transport: Box<dyn Transport>,
    vault: Box<dyn Vault>,
    host: Box<dyn Host>,
}

fn no_zone() -> ToolError {
    ToolError::msg(
        "The time zone of this Mac could not be determined, so calendars of Google or Outlook.com cannot be used. Ask the user to set the time zone in System Settings.",
    )
}

impl Calendars {
    pub(crate) fn new(
        apple: Box<dyn CalendarBackend>,
        transport: Box<dyn Transport>,
        vault: Box<dyn Vault>,
        host: Box<dyn Host>,
    ) -> Calendars {
        Calendars {
            apple,
            transport,
            vault,
            host,
        }
    }

    pub(crate) fn system() -> Calendars {
        Calendars::new(
            Box::new(RealCalendar::system()),
            Box::new(Curl::system()),
            Box::new(Keychain::system()),
            Box::new(OsascriptHost),
        )
    }

    fn remote<T>(
        &self,
        provider: Provider,
        address: &str,
        action: impl FnOnce(&Api) -> ToolResult<T>,
    ) -> ToolResult<T> {
        let token = auth::access_token(
            self.transport.as_ref(),
            self.vault.as_ref(),
            provider,
            address,
        )?;
        action(&Api::new(self.transport.as_ref(), token, provider))
    }

    fn listed(
        &self,
        provider: Provider,
        address: &str,
        list: impl FnOnce(&Api) -> ToolResult<Vec<CalendarInfo>>,
    ) -> ToolResult<Vec<CalendarInfo>> {
        let found = self.remote(provider, address, list)?;
        Ok(found
            .into_iter()
            .filter_map(|info| {
                Some(CalendarInfo {
                    id: calendar_id(provider, address, &info.id)?,
                    ..info
                })
            })
            .collect())
    }

    fn on_calendar<T>(
        &self,
        calendar: &Calendar,
        action: impl FnOnce(&Api, &str, &str) -> ToolResult<T>,
    ) -> ToolResult<T> {
        let (provider, address, id) = calendar_parts(&calendar.id).ok_or_else(not_linked)?;
        let zone = localtime::zone().ok_or_else(no_zone)?;
        self.remote(provider, address, |api| action(api, id, &zone))
    }
}

impl CalendarBackend for Calendars {
    fn calendars(&self, provider: Provider, address: &str) -> ToolResult<Vec<CalendarInfo>> {
        match provider {
            Provider::Apple => self.apple.calendars(provider, address),
            Provider::Google => self.listed(provider, address, google_calendar::calendars),
            Provider::Microsoft => self.listed(provider, address, graph_calendar::calendars),
        }
    }

    fn events(&self, calendar: &Calendar, window: Window) -> ToolResult<Vec<RawEvent>> {
        match Provider::of_id(&calendar.id) {
            Provider::Apple => self.apple.events(calendar, window),
            Provider::Google => self.on_calendar(calendar, |api, id, zone| {
                google_calendar::events(api, id, window, zone)
            }),
            Provider::Microsoft => self.on_calendar(calendar, |api, id, zone| {
                graph_calendar::events(api, id, window, zone)
            }),
        }
    }

    fn event(&self, calendar: &Calendar, uid: &str) -> ToolResult<EventDetail> {
        match Provider::of_id(&calendar.id) {
            Provider::Apple => self.apple.event(calendar, uid),
            Provider::Google => self.on_calendar(calendar, |api, id, zone| {
                google_calendar::event(api, id, uid, zone)
            }),
            Provider::Microsoft => self.on_calendar(calendar, |api, id, zone| {
                graph_calendar::event(api, id, uid, zone)
            }),
        }
    }

    fn change(
        &self,
        calendar: &Calendar,
        change: &Change,
        approval: &Approval,
    ) -> ToolResult<ChangeResult> {
        let host = self.host.as_ref();
        match Provider::of_id(&calendar.id) {
            Provider::Apple => self.apple.change(calendar, change, approval),
            Provider::Google => self.on_calendar(calendar, |api, id, zone| {
                google_calendar::change(api, host, calendar, id, zone, change, approval)
            }),
            Provider::Microsoft => self.on_calendar(calendar, |api, id, zone| {
                graph_calendar::change(api, host, calendar, id, zone, change, approval)
            }),
        }
    }
}
