use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use super::auth::{load_record, Record, MAX_TOKEN};
use crate::base64;
use crate::http::{self, Method, Request, Transport};
use crate::json::{self, Json, Object};
use crate::osascript::run_command;
use crate::provider::{account_id, OAuth, Provider};
use crate::sha256::sha256;
use crate::text::one_line_str;
use crate::vault::Vault;

const OPEN: &str = "/usr/bin/open";
const DEADLINE: Duration = Duration::from_secs(180);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const HEAD_DEADLINE: Duration = Duration::from_secs(3);
const POLL: Duration = Duration::from_millis(50);
const MAX_REQUEST: usize = 8192;
const MAX_CLIENT: usize = 300;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Client {
    pub(crate) id: String,
    pub(crate) secret: Option<String>,
}

impl Client {
    pub(crate) fn new(provider: Provider, id: &str, secret: &str) -> Result<Client, String> {
        let oauth = provider.oauth().ok_or("This provider has no sign-in.")?;
        let id = id.trim();
        let secret = secret.trim();
        let valid = |text: &str| {
            !text.is_empty()
                && text.len() <= MAX_CLIENT
                && text
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-._~".contains(c))
        };
        if !valid(id) {
            return Err("The client id looks wrong (letters, digits, - . _ ~ only).".to_string());
        }
        let secret = if oauth.secret {
            if !valid(secret) {
                return Err(
                    "The client secret looks wrong (letters, digits, - . _ ~ only).".to_string(),
                );
            }
            Some(secret.to_string())
        } else {
            None
        };
        Ok(Client {
            id: id.to_string(),
            secret,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Linked {
    pub(crate) address: String,
    pub(crate) write_mail: bool,
    pub(crate) write_calendar: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Capability {
    pub(crate) write_mail: bool,
    pub(crate) write_calendar: bool,
}

pub(crate) trait Browser {
    fn open(&self, url: &str);
}

pub(crate) struct SystemBrowser;

impl Browser for SystemBrowser {
    fn open(&self, url: &str) {
        let _ = run_command(OPEN, &[url], Duration::from_secs(15));
    }
}

fn random_token(bytes: usize) -> Option<String> {
    let mut buffer = vec![0u8; bytes];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut buffer))
        .ok()?;
    Some(base64::encode_url(&buffer))
}

pub(crate) fn challenge_of(verifier: &str) -> String {
    base64::encode_url(&sha256(verifier.as_bytes()))
}

fn authorize_url(
    oauth: &OAuth,
    client: &Client,
    write: bool,
    port: u16,
    challenge: &str,
    state: &str,
) -> String {
    let scope = oauth.scopes(write).join(" ");
    let redirect = oauth.redirect(port);
    let mut pairs: Vec<(&str, &str)> = vec![
        ("client_id", client.id.as_str()),
        ("redirect_uri", redirect.as_str()),
        ("response_type", "code"),
        ("scope", scope.as_str()),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
    ];
    pairs.extend(oauth.authorize_extras.iter().copied());
    format!("{}?{}", oauth.authorize, http::query(&pairs))
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(byte) = bytes.get(index) {
        match byte {
            b'%' => {
                let pair = text.get(index + 1..index + 3)?;
                out.push(u8::from_str_radix(pair, 16).ok()?);
                index += 3;
            }
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            other => {
                out.push(*other);
                index += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

#[derive(Debug, PartialEq, Eq)]
enum Callback {
    Code(String),
    Failed(String),
    Ignored,
    Wrong,
}

fn parse_callback(request: &str, state: &str) -> Callback {
    let Some(line) = request.lines().next() else {
        return Callback::Wrong;
    };
    let mut parts = line.split(' ');
    let (Some("GET"), Some(target), Some(version)) = (parts.next(), parts.next(), parts.next())
    else {
        return Callback::Wrong;
    };
    if !version.starts_with("HTTP/1.") {
        return Callback::Wrong;
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != "/" {
        return Callback::Ignored;
    }
    let mut code = None;
    let mut found_state = None;
    let mut error = None;
    for pair in query.split('&').take(40) {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        let Some(value) = percent_decode(value) else {
            return Callback::Wrong;
        };
        match name {
            "code" => code = Some(value),
            "state" => found_state = Some(value),
            "error" => error = Some(value),
            _ => {}
        }
    }
    if found_state.as_deref() != Some(state) {
        return Callback::Wrong;
    }
    if let Some(error) = error {
        return Callback::Failed(one_line_str(&error, 60));
    }
    match code {
        Some(code) if !code.is_empty() && code.len() <= 4096 => Callback::Code(code),
        _ => Callback::Wrong,
    }
}

fn respond(stream: &mut TcpStream, status: &str, title: &str, text: &str) {
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title></head><body><h2>{title}</h2><p>{text}</p></body></html>"
    );
    let message = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(message.as_bytes());
    let _ = stream.flush();
}

fn read_request(stream: &mut TcpStream, within: Duration) -> Option<String> {
    let deadline = Instant::now() + within;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    while buffer.len() < MAX_REQUEST && !buffer.windows(4).any(|window| window == b"\r\n\r\n") {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        stream.set_read_timeout(Some(left)).ok()?;
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => buffer.extend_from_slice(chunk.get(..count)?),
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    String::from_utf8(buffer).ok()
}

fn wait_for_code(
    listeners: &[TcpListener],
    state: &str,
    deadline: Instant,
) -> Result<String, String> {
    while Instant::now() < deadline {
        let mut idle = true;
        for listener in listeners {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    idle = false;
                    if stream.set_nonblocking(false).is_err()
                        || stream.set_write_timeout(Some(HEAD_DEADLINE)).is_err()
                    {
                        continue;
                    }
                    let Some(request) = read_request(&mut stream, HEAD_DEADLINE) else {
                        respond(
                            &mut stream,
                            "400 Bad Request",
                            "Avery",
                            "Unreadable request.",
                        );
                        continue;
                    };
                    match parse_callback(&request, state) {
                        Callback::Code(code) => {
                            respond(
                                &mut stream,
                                "200 OK",
                                "Avery",
                                "Sign-in complete. You can close this window and go back to the terminal.",
                            );
                            return Ok(code);
                        }
                        Callback::Failed(error) => {
                            respond(&mut stream, "200 OK", "Avery", "Sign-in was not completed.");
                            return Err(format!("The sign-in was refused ({error})."));
                        }
                        Callback::Ignored => {
                            respond(&mut stream, "404 Not Found", "Avery", "Nothing here.");
                        }
                        Callback::Wrong => {
                            respond(
                                &mut stream,
                                "400 Bad Request",
                                "Avery",
                                "Unexpected request.",
                            );
                        }
                    }
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                Err(_) => return Err("The local sign-in listener failed.".to_string()),
            }
        }
        if idle {
            std::thread::sleep(POLL);
        }
    }
    Err("No sign-in arrived in time. Run the command again.".to_string())
}

fn bind() -> Result<(Vec<TcpListener>, u16), String> {
    let failed = |_| "The local sign-in listener could not be started.".to_string();
    let first = TcpListener::bind(("127.0.0.1", 0)).map_err(failed)?;
    let port = first.local_addr().map_err(failed)?.port();
    let mut listeners = vec![first];
    if let Ok(second) = TcpListener::bind(("::1", port)) {
        listeners.push(second);
    }
    for listener in &listeners {
        listener.set_nonblocking(true).map_err(failed)?;
    }
    Ok((listeners, port))
}

fn text_of(object: &Object, key: &str, limit: usize) -> Option<String> {
    match object.get(key) {
        Some(Json::Str(text)) if !text.is_empty() && text.len() <= limit => Some(text.clone()),
        _ => None,
    }
}

fn exchange(
    transport: &dyn Transport,
    oauth: &OAuth,
    client: &Client,
    code: &str,
    verifier: &str,
    port: u16,
) -> Result<(String, String, String), String> {
    let redirect = oauth.redirect(port);
    let mut pairs: Vec<(&str, &str)> = vec![
        ("client_id", client.id.as_str()),
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect.as_str()),
        ("code_verifier", verifier),
    ];
    if let Some(secret) = &client.secret {
        pairs.push(("client_secret", secret.as_str()));
    }
    let request = Request::new(Method::Post, oauth.token, REQUEST_TIMEOUT)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(&http::query(&pairs));
    let response = transport
        .send(&request)
        .map_err(|_| "The sign-in service could not be reached.".to_string())?;
    let answer = json::parse(&response.body).ok();
    let object = answer.as_ref().and_then(Json::as_object);
    if response.status != 200 {
        let detail = object
            .and_then(|object| object.get("error_description").or(object.get("error")))
            .and_then(Json::as_str)
            .map(|text| one_line_str(text, 120))
            .unwrap_or_default();
        return Err(format!(
            "The sign-in service refused the code ({}): {detail}",
            response.status
        ));
    }
    let object = object.ok_or("The sign-in service gave an unexpected answer.")?;
    let access = text_of(object, "access_token", MAX_TOKEN)
        .ok_or("The sign-in service gave no access token.")?;
    let refresh = text_of(object, "refresh_token", MAX_TOKEN).ok_or(
        "The sign-in service gave no refresh token. Remove the app's access in your account settings and try again.",
    )?;
    let scope = match object.get("scope") {
        Some(Json::Str(scope)) if scope.len() <= MAX_TOKEN => scope.clone(),
        _ => String::new(),
    };
    Ok((access, refresh, scope))
}

fn find_address(transport: &dyn Transport, oauth: &OAuth, access: &str) -> Result<String, String> {
    let request = Request::new(Method::Get, oauth.userinfo, REQUEST_TIMEOUT)
        .header("Authorization", &format!("Bearer {access}"));
    let response = transport
        .send(&request)
        .map_err(|_| "The account could not be identified.".to_string())?;
    let answer = json::parse(&response.body).ok();
    let object = answer
        .as_ref()
        .and_then(Json::as_object)
        .filter(|_| response.status == 200)
        .ok_or("The account could not be identified.")?;
    (oauth.identify)(object)
}

struct Refusing;

impl Transport for Refusing {
    fn send(&self, _request: &Request) -> Result<crate::http::Response, crate::http::HttpError> {
        Err(crate::http::HttpError::Refused)
    }
}

struct Forgetful;

impl Vault for Forgetful {
    fn load(&self, _name: &str) -> Result<Option<String>, crate::vault::VaultError> {
        Ok(None)
    }

    fn store(&self, _name: &str, _secret: &str) -> Result<(), crate::vault::VaultError> {
        Err(crate::vault::VaultError::Refused)
    }

    fn delete(&self, _name: &str) -> Result<(), crate::vault::VaultError> {
        Ok(())
    }
}

struct Closed;

impl Browser for Closed {
    fn open(&self, _url: &str) {}
}

pub(crate) struct Linker {
    transport: Box<dyn Transport>,
    vault: Box<dyn Vault>,
    browser: Box<dyn Browser>,
    deadline: Duration,
}

impl Linker {
    pub(crate) fn new(
        transport: Box<dyn Transport>,
        vault: Box<dyn Vault>,
        browser: Box<dyn Browser>,
    ) -> Linker {
        Linker {
            transport,
            vault,
            browser,
            deadline: DEADLINE,
        }
    }

    pub(crate) fn system() -> Linker {
        Linker::new(
            Box::new(crate::http::Curl::system()),
            Box::new(crate::vault::Keychain::system()),
            Box::new(SystemBrowser),
        )
    }

    pub(crate) fn inert() -> Linker {
        Linker::new(Box::new(Refusing), Box::new(Forgetful), Box::new(Closed))
    }

    #[cfg(test)]
    pub(crate) fn within(mut self, deadline: Duration) -> Linker {
        self.deadline = deadline;
        self
    }

    pub(crate) fn capability(&self, provider: Provider, address: &str) -> Option<Capability> {
        let record = load_record(self.vault.as_ref(), provider, address).ok()?;
        Some(Capability {
            write_mail: record.can_write_mail(provider),
            write_calendar: record.can_write_calendar(provider),
        })
    }

    pub(crate) fn stored(&self, provider: Provider, address: &str) -> bool {
        account_id(provider, address)
            .is_some_and(|name| !matches!(self.vault.load(&name), Ok(None)))
    }

    pub(crate) fn unlink(&self, provider: Provider, address: &str) -> Result<(), String> {
        let name = account_id(provider, address).ok_or("This is not a valid account.")?;
        self.vault
            .delete(&name)
            .map_err(|error| format!("The stored sign-in could not be removed ({error})."))
    }

    pub(crate) fn link(
        &self,
        provider: Provider,
        client: &Client,
        write: bool,
        announce: &mut dyn FnMut(&str),
    ) -> Result<Linked, String> {
        let oauth = provider.oauth().ok_or("This provider has no sign-in.")?;
        let verifier = random_token(32).ok_or("No random numbers available.")?;
        let state = random_token(16).ok_or("No random numbers available.")?;
        let challenge = challenge_of(&verifier);
        let (listeners, port) = bind()?;
        let url = authorize_url(oauth, client, write, port, &challenge, &state);
        announce(&url);
        self.browser.open(&url);
        let code = wait_for_code(&listeners, &state, Instant::now() + self.deadline)?;
        drop(listeners);
        let (access, refresh, scope) = exchange(
            self.transport.as_ref(),
            oauth,
            client,
            &code,
            &verifier,
            port,
        )?;
        let record = Record {
            client_id: client.id.clone(),
            client_secret: client.secret.clone(),
            refresh_token: refresh,
            scope,
        };
        let missing: Vec<&str> = oauth
            .required(write)
            .into_iter()
            .filter(|wanted| !record.has(wanted))
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "The account did not grant everything that was asked for ({}). Run the command again and allow all of it.",
                missing.join(", ")
            ));
        }
        let address = find_address(self.transport.as_ref(), oauth, &access)?;
        let name =
            account_id(provider, &address).ok_or("The account has no usable mail address.")?;
        self.vault
            .store(&name, &record.to_json())
            .map_err(|error| {
                format!("The sign-in could not be stored in the Keychain ({error}).")
            })?;
        Ok(Linked {
            address,
            write_mail: record.can_write_mail(provider),
            write_calendar: record.can_write_calendar(provider),
        })
    }
}

#[cfg(test)]
pub(crate) mod fakes {
    use super::*;
    use crate::http::scripted::Scripted;
    use std::sync::{Arc, Mutex};
    use std::thread;

    pub(crate) fn query_value(url: &str, name: &str) -> Option<String> {
        let query = url.split_once('?')?.1;
        query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key == name).then(|| percent_decode(value)).flatten()
        })
    }

    pub(crate) fn port_of(url: &str) -> Option<u16> {
        let redirect = query_value(url, "redirect_uri")?;
        redirect
            .rsplit(':')
            .next()?
            .trim_end_matches('/')
            .parse()
            .ok()
    }

    #[derive(Clone)]
    pub(crate) enum Visit {
        Good(&'static str),
        WrongStateThenGood(&'static str),
        Denied(&'static str),
        Nothing,
    }

    pub(crate) struct Visitor {
        pub(crate) visit: Visit,
        pub(crate) seen: Arc<Mutex<Vec<String>>>,
    }

    fn get(port: u16, target: &str) -> Option<String> {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
        stream
            .write_all(format!("GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
            .ok()?;
        let mut answer = String::new();
        stream.read_to_string(&mut answer).ok()?;
        Some(answer)
    }

    impl Browser for Visitor {
        fn open(&self, url: &str) {
            if let Ok(mut seen) = self.seen.lock() {
                seen.push(url.to_string());
            }
            let (Some(port), Some(state)) = (port_of(url), query_value(url, "state")) else {
                return;
            };
            let visit = self.visit.clone();
            thread::spawn(move || match visit {
                Visit::Good(code) => {
                    let _ = get(port, &format!("/?code={code}&state={state}"));
                }
                Visit::WrongStateThenGood(code) => {
                    let _ = get(port, "/?code=evil&state=wrong");
                    let _ = get(port, "/favicon.ico");
                    let _ = get(port, &format!("/?code={code}&state={state}"));
                }
                Visit::Denied(error) => {
                    let _ = get(port, &format!("/?error={error}&state={state}"));
                }
                Visit::Nothing => {}
            });
        }
    }
    pub(crate) const GOOGLE_TOKENS: &str = r#"{"access_token": "ya29.a", "refresh_token": "1//r", "expires_in": 3599, "token_type": "Bearer", "scope": "openid https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/gmail.readonly https://www.googleapis.com/auth/calendar.readonly"}"#;

    pub(crate) fn google_transport(tokens: &str) -> Scripted {
        Scripted::new()
            .json(Method::Post, "oauth2.googleapis.com/token", 200, tokens)
            .json(
                Method::Get,
                "openidconnect.googleapis.com",
                200,
                r#"{"sub": "1", "email": "Alice@Gmail.com", "email_verified": true}"#,
            )
    }
    pub(crate) fn record(scope: &str) -> Record {
        Record {
            client_id: "client".to_string(),
            client_secret: None,
            refresh_token: "refresh".to_string(),
            scope: scope.to_string(),
        }
    }

    pub(crate) fn stored_scope(vault: &crate::vault::memory::Memory, name: &str) -> String {
        vault
            .get(name)
            .and_then(|text| Record::parse(&text))
            .map(|record| record.scope)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::fakes::{google_transport, query_value, Visit, Visitor, GOOGLE_TOKENS};
    use super::*;
    use crate::http::scripted::Scripted;
    use crate::http::HttpError;
    use crate::testutil::TestResult;
    use crate::vault::memory::Memory;
    use std::sync::{Arc, Mutex};
    use std::thread;

    fn oauth(provider: Provider) -> Result<&'static OAuth, String> {
        provider.oauth().ok_or_else(|| "oauth".to_string())
    }

    fn google() -> Result<Client, String> {
        Client::new(Provider::Google, "123-abc.apps.example", "GOCSPX-secret_1")
    }

    fn microsoft() -> Result<Client, String> {
        Client::new(
            Provider::Microsoft,
            "0a1b2c3d-1111-2222-3333-444455556666",
            "",
        )
    }

    fn admin(
        transport: Scripted,
        vault: Memory,
        visit: Visit,
    ) -> (Linker, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let browser = Visitor {
            visit,
            seen: Arc::clone(&seen),
        };
        (
            Linker::new(Box::new(transport), Box::new(vault), Box::new(browser))
                .within(Duration::from_secs(10)),
            seen,
        )
    }

    #[test]
    fn the_pkce_challenge_matches_the_rfc_7636_example() {
        assert_eq!(
            challenge_of("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(random_token(32).map(|token| token.len()), Some(43));
        assert_ne!(random_token(16), random_token(16));
    }

    #[test]
    fn clients_are_checked_before_anything_happens() {
        assert!(google().is_ok());
        assert!(microsoft().is_ok());
        assert_eq!(microsoft().map(|client| client.secret), Ok(None));
        assert!(Client::new(Provider::Google, "", "s").is_err());
        assert!(Client::new(Provider::Google, "id", "").is_err());
        assert!(Client::new(Provider::Google, "i d", "s").is_err());
        assert!(Client::new(Provider::Google, "id", "s\"x").is_err());
        assert!(Client::new(Provider::Microsoft, "a&b", "").is_err());
        assert!(Client::new(Provider::Microsoft, &"x".repeat(MAX_CLIENT + 1), "").is_err());
    }

    #[test]
    fn the_authorize_urls_carry_pkce_state_and_the_requested_scopes() -> TestResult {
        let url = authorize_url(
            oauth(Provider::Google)?,
            &google()?,
            false,
            4711,
            "CHAL",
            "STATE",
        );
        assert!(url.starts_with(
            "https://accounts.google.com/o/oauth2/v2/auth?client_id=123-abc.apps.example&"
        ));
        assert_eq!(
            query_value(&url, "redirect_uri").as_deref(),
            Some("http://127.0.0.1:4711/")
        );
        assert_eq!(query_value(&url, "response_type").as_deref(), Some("code"));
        assert_eq!(query_value(&url, "code_challenge").as_deref(), Some("CHAL"));
        assert_eq!(
            query_value(&url, "code_challenge_method").as_deref(),
            Some("S256")
        );
        assert_eq!(query_value(&url, "state").as_deref(), Some("STATE"));
        assert_eq!(query_value(&url, "access_type").as_deref(), Some("offline"));
        assert_eq!(query_value(&url, "prompt").as_deref(), Some("consent"));
        let scope = query_value(&url, "scope").ok_or("scope")?;
        assert!(scope.contains("gmail.readonly") && scope.contains("calendar.readonly"));
        assert!(!scope.contains("compose") && !scope.contains("calendar.events"));
        assert!(!url.contains("client_secret"));
        let write = authorize_url(oauth(Provider::Google)?, &google()?, true, 1, "C", "S");
        let scope = query_value(&write, "scope").ok_or("scope")?;
        assert!(scope.contains("gmail.compose") && scope.contains("calendar.events"));
        let url = authorize_url(
            oauth(Provider::Microsoft)?,
            &microsoft()?,
            true,
            5000,
            "C",
            "S",
        );
        assert!(
            url.starts_with("https://login.microsoftonline.com/consumers/oauth2/v2.0/authorize?")
        );
        assert_eq!(
            query_value(&url, "redirect_uri").as_deref(),
            Some("http://localhost:5000/")
        );
        let scope = query_value(&url, "scope").ok_or("scope")?;
        for wanted in [
            "offline_access",
            "Mail.Read",
            "Calendars.Read",
            "Mail.ReadWrite",
            "Mail.Send",
            "Calendars.ReadWrite",
        ] {
            assert!(
                scope.split(' ').any(|granted| granted == wanted),
                "{wanted}"
            );
        }
        assert!(Provider::Apple.oauth().is_none());
        assert!(Client::new(Provider::Apple, "id", "s").is_err());
        Ok(())
    }

    #[test]
    fn callbacks_are_accepted_only_with_the_right_state() {
        let line = |target: &str| format!("GET {target} HTTP/1.1\r\nHost: x\r\n\r\n");
        assert_eq!(
            parse_callback(&line("/?code=4%2F0AbC&state=S1"), "S1"),
            Callback::Code("4/0AbC".to_string())
        );
        assert_eq!(
            parse_callback(&line("/?state=S1&code=a+b&scope=x"), "S1"),
            Callback::Code("a b".to_string())
        );
        assert_eq!(
            parse_callback(&line("/?error=access_denied&state=S1"), "S1"),
            Callback::Failed("access_denied".to_string())
        );
        assert_eq!(
            parse_callback(&line("/favicon.ico"), "S1"),
            Callback::Ignored
        );
        assert_eq!(
            parse_callback(&line("/other?code=x&state=S1"), "S1"),
            Callback::Ignored
        );
        for wrong in [
            line("/?code=x"),
            line("/?code=x&state=S2"),
            line("/?code=&state=S1"),
            line("/?state=S1"),
            line("/?code=%ZZ&state=S1"),
            line(&format!("/?code={}&state=S1", "a".repeat(5000))),
            "POST /?code=x&state=S1 HTTP/1.1\r\n\r\n".to_string(),
            "GET /?code=x&state=S1 SPDY/3\r\n\r\n".to_string(),
            "GET\r\n\r\n".to_string(),
            String::new(),
        ] {
            assert_eq!(parse_callback(&wrong, "S1"), Callback::Wrong, "{wrong:?}");
        }
        assert_eq!(percent_decode("a%2Fb%20c+d"), Some("a/b c d".to_string()));
        assert_eq!(percent_decode("%"), None);
        assert_eq!(percent_decode("%4"), None);
        assert_eq!(percent_decode("%FF"), None);
    }

    #[test]
    fn a_google_account_is_linked_and_its_sign_in_stored() -> TestResult {
        let (admin, seen) = admin(
            google_transport(GOOGLE_TOKENS),
            Memory::new(),
            Visit::Good("4/0code"),
        );
        let mut announced = Vec::new();
        let linked = admin.link(Provider::Google, &google()?, false, &mut |url| {
            announced.push(url.to_string())
        })?;
        assert_eq!(
            linked,
            Linked {
                address: "alice@gmail.com".to_string(),
                write_mail: false,
                write_calendar: false
            }
        );
        assert_eq!(announced.len(), 1);
        assert_eq!(
            seen.lock().map_err(|_| "lock")?.as_slice(),
            announced.as_slice()
        );
        let verifier_challenge = query_value(&announced[0], "code_challenge").ok_or("challenge")?;
        assert_eq!(verifier_challenge.len(), 43);
        Ok(())
    }

    #[test]
    fn the_code_exchange_sends_the_verifier_and_the_vault_gets_the_record() -> TestResult {
        let transport = google_transport(GOOGLE_TOKENS);
        let vault = Arc::new(Memory::new());
        struct Seen(Arc<Scripted>);
        impl Transport for Seen {
            fn send(&self, request: &Request) -> Result<crate::http::Response, HttpError> {
                self.0.send(request)
            }
        }
        let transport = Arc::new(transport);
        let seen_urls = Arc::new(Mutex::new(Vec::<String>::new()));
        let browser = Visitor {
            visit: Visit::Good("4/0code"),
            seen: Arc::clone(&seen_urls),
        };
        let admin = Linker::new(
            Box::new(Seen(Arc::clone(&transport))),
            Box::new(crate::vault::memory::Shared(Arc::clone(&vault))),
            Box::new(browser),
        )
        .within(Duration::from_secs(10));
        admin.link(Provider::Google, &google()?, false, &mut |_| {})?;
        let requests = transport.seen();
        let token = requests
            .iter()
            .find(|r| r.url.contains("/token"))
            .ok_or("token request")?;
        let body = token.body.clone().ok_or("body")?;
        let url = seen_urls
            .lock()
            .map_err(|_| "lock")?
            .first()
            .cloned()
            .ok_or("url")?;
        assert!(body.contains("grant_type=authorization_code"));
        assert!(body.contains("code=4%2F0code"));
        assert!(body.contains("client_secret=GOCSPX-secret_1"));
        assert!(body.contains(&format!(
            "redirect_uri={}",
            http::encode_component(&query_value(&url, "redirect_uri").ok_or("redirect")?)
        )));
        let verifier = body
            .split('&')
            .find_map(|pair| pair.strip_prefix("code_verifier="))
            .ok_or("verifier")?;
        assert_eq!(
            challenge_of(verifier),
            query_value(&url, "code_challenge").ok_or("challenge")?
        );
        let user = requests
            .iter()
            .find(|r| r.url.contains("openidconnect"))
            .ok_or("userinfo")?;
        assert!(user
            .headers
            .iter()
            .any(|(name, value)| name == "Authorization" && value == "Bearer ya29.a"));
        let stored =
            Record::parse(&vault.get("google:alice@gmail.com").ok_or("stored")?).ok_or("record")?;
        assert_eq!(stored.refresh_token, "1//r");
        assert_eq!(stored.client_id, "123-abc.apps.example");
        assert_eq!(stored.client_secret.as_deref(), Some("GOCSPX-secret_1"));
        Ok(())
    }

    #[test]
    fn write_links_need_the_write_scopes_to_be_granted() -> TestResult {
        let (admin, _) = admin(
            google_transport(GOOGLE_TOKENS),
            Memory::new(),
            Visit::Good("c"),
        );
        let Err(error) = admin.link(Provider::Google, &google()?, true, &mut |_| {}) else {
            return Err("expected an error".into());
        };
        assert!(
            error.contains("gmail.compose") && error.contains("calendar.events"),
            "{error}"
        );
        let full = GOOGLE_TOKENS.replace(
            "calendar.readonly\"",
            "calendar.readonly https://www.googleapis.com/auth/gmail.compose https://www.googleapis.com/auth/calendar.events\"",
        );
        let (admin, _) = admin_with(google_transport(&full), Visit::Good("c"));
        let linked = admin.link(Provider::Google, &google()?, true, &mut |_| {})?;
        assert!(linked.write_mail && linked.write_calendar);
        Ok(())
    }

    fn admin_with(transport: Scripted, visit: Visit) -> (Linker, Arc<Mutex<Vec<String>>>) {
        admin(transport, Memory::new(), visit)
    }

    #[test]
    fn a_microsoft_account_uses_the_graph_profile_and_a_public_client() -> TestResult {
        let transport = Scripted::new()
            .json(
                Method::Post,
                "login.microsoftonline.com/consumers/oauth2/v2.0/token",
                200,
                r#"{"access_token": "EwB", "refresh_token": "M.R3", "scope": "Mail.Read Calendars.Read User.Read openid profile email Mail.ReadWrite Mail.Send Calendars.ReadWrite"}"#,
            )
            .json(
                Method::Get,
                "graph.microsoft.com/v1.0/me",
                200,
                r#"{"mail": null, "userPrincipalName": "Bob@Outlook.example"}"#,
            );
        let (admin, _) = admin_with(transport, Visit::Good("M.code"));
        let linked = admin.link(Provider::Microsoft, &microsoft()?, true, &mut |_| {})?;
        assert_eq!(linked.address, "bob@outlook.example");
        assert!(linked.write_mail && linked.write_calendar);
        Ok(())
    }

    #[test]
    fn wrong_requests_do_not_stop_the_wait_but_never_deliver_a_code() -> TestResult {
        let (admin, _) = admin_with(
            google_transport(GOOGLE_TOKENS),
            Visit::WrongStateThenGood("good"),
        );
        assert!(admin
            .link(Provider::Google, &google()?, false, &mut |_| {})
            .is_ok());
        Ok(())
    }

    fn connect(port: u16) -> Result<TcpStream, Box<dyn std::error::Error>> {
        let stream = TcpStream::connect(("127.0.0.1", port))?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        Ok(stream)
    }

    fn pair() -> Result<(TcpStream, TcpStream), Box<dyn std::error::Error>> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let client = connect(listener.local_addr()?.port())?;
        let (server, _) = listener.accept()?;
        Ok((server, client))
    }

    fn callback(code: &str) -> String {
        format!("GET /?code={code}&state=S1 HTTP/1.1\r\nHost: localhost\r\n\r\n")
    }

    fn answer_of(mut stream: TcpStream) -> String {
        let mut answer = String::new();
        let _ = stream.read_to_string(&mut answer);
        answer
    }

    #[test]
    fn a_request_that_arrives_late_on_an_open_connection_is_still_served() -> TestResult {
        let (listeners, port) = bind()?;
        let mut stream = connect(port)?;
        let sender = thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            let _ = stream.write_all(callback("late").as_bytes());
            answer_of(stream)
        });
        let code = wait_for_code(&listeners, "S1", Instant::now() + Duration::from_secs(5))?;
        assert_eq!(code, "late");
        let answer = sender.join().map_err(|_| "sender")?;
        assert!(answer.starts_with("HTTP/1.1 200 OK"), "{answer}");
        Ok(())
    }

    #[test]
    fn a_request_that_comes_in_pieces_within_the_deadline_is_served() -> TestResult {
        let (listeners, port) = bind()?;
        let mut stream = connect(port)?;
        let sender = thread::spawn(move || {
            for piece in [
                "GET /?code=pie",
                "ces&state=S1 HT",
                "TP/1.1\r\nHost: x\r\n",
                "\r\n",
            ] {
                thread::sleep(Duration::from_millis(150));
                let _ = stream.write_all(piece.as_bytes());
            }
            answer_of(stream)
        });
        let code = wait_for_code(&listeners, "S1", Instant::now() + Duration::from_secs(5))?;
        assert_eq!(code, "pieces");
        let answer = sender.join().map_err(|_| "sender")?;
        assert!(answer.starts_with("HTTP/1.1 200 OK"), "{answer}");
        Ok(())
    }

    #[test]
    fn a_trickling_request_is_cut_off_at_the_overall_deadline_and_the_next_one_is_served(
    ) -> TestResult {
        let (listeners, port) = bind()?;
        let mut slow = connect(port)?;
        let trickler = thread::spawn(move || {
            for byte in callback("slow").as_bytes() {
                if slow.write_all(&[*byte]).is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(250));
            }
            answer_of(slow)
        });
        let mut quick = connect(port)?;
        quick.write_all(callback("good").as_bytes())?;
        let started = Instant::now();
        let code = wait_for_code(&listeners, "S1", started + Duration::from_secs(30))?;
        let took = started.elapsed();
        assert_eq!(code, "good");
        assert!(
            took + Duration::from_millis(100) >= HEAD_DEADLINE,
            "{took:?}"
        );
        assert!(took < HEAD_DEADLINE + Duration::from_secs(2), "{took:?}");
        assert!(answer_of(quick).starts_with("HTTP/1.1 200 OK"));
        let answer = trickler.join().map_err(|_| "trickler")?;
        assert!(!answer.contains("200 OK"), "{answer}");
        Ok(())
    }

    #[test]
    fn the_head_is_read_against_one_deadline_whether_the_client_is_idle_or_trickles() -> TestResult
    {
        let within = Duration::from_millis(600);
        let (mut idle, _kept_open) = pair()?;
        let started = Instant::now();
        assert_eq!(read_request(&mut idle, within), None);
        let took = started.elapsed();
        assert!(took + Duration::from_millis(50) >= within, "{took:?}");
        assert!(took < within + Duration::from_secs(2), "{took:?}");
        let (mut server, mut client) = pair()?;
        let trickler = thread::spawn(move || {
            for byte in callback("slow").as_bytes() {
                if client.write_all(&[*byte]).is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
        });
        let started = Instant::now();
        assert_eq!(read_request(&mut server, within), None);
        let took = started.elapsed();
        assert!(took + Duration::from_millis(50) >= within, "{took:?}");
        assert!(took < within + Duration::from_secs(2), "{took:?}");
        drop(server);
        trickler.join().map_err(|_| "trickler")?;
        Ok(())
    }

    #[test]
    fn a_complete_head_is_returned_at_once_and_a_head_without_an_end_stops_at_the_size_limit(
    ) -> TestResult {
        let (mut server, mut client) = pair()?;
        client.write_all(callback("quick").as_bytes())?;
        let started = Instant::now();
        assert_eq!(
            read_request(&mut server, HEAD_DEADLINE),
            Some(callback("quick"))
        );
        assert!(started.elapsed() < HEAD_DEADLINE);
        let (mut server, mut client) = pair()?;
        let flood = thread::spawn(move || {
            let _ = client.write_all(&vec![b'a'; 4 * MAX_REQUEST]);
        });
        let text = read_request(&mut server, HEAD_DEADLINE).ok_or("head")?;
        assert!(text.len() >= MAX_REQUEST && text.len() < MAX_REQUEST + 1024);
        drop(server);
        flood.join().map_err(|_| "flood")?;
        Ok(())
    }

    #[test]
    fn only_the_first_forty_pairs_of_the_query_are_looked_at() {
        let filler = "x=1&".repeat(40);
        let late = format!("GET /?{filler}code=a&state=S1 HTTP/1.1\r\n\r\n");
        assert_eq!(parse_callback(&late, "S1"), Callback::Wrong);
        let early = format!("GET /?code=a&state=S1&{filler} HTTP/1.1\r\n\r\n");
        assert_eq!(
            parse_callback(&early, "S1"),
            Callback::Code("a".to_string())
        );
    }

    #[test]
    fn refusals_timeouts_and_odd_answers_end_without_storing_anything() -> TestResult {
        let cases: Vec<(Scripted, Visit, &str)> = vec![
            (
                google_transport(GOOGLE_TOKENS),
                Visit::Denied("access_denied"),
                "refused (access_denied)",
            ),
            (
                google_transport(GOOGLE_TOKENS),
                Visit::Nothing,
                "No sign-in arrived in time",
            ),
            (
                Scripted::new().json(
                    Method::Post,
                    "/token",
                    400,
                    r#"{"error": "invalid_grant", "error_description": "Bad\nCode"}"#,
                ),
                Visit::Good("x"),
                "refused the code (400): Bad Code",
            ),
            (
                Scripted::new().reply(Method::Post, "/token", vec![Err(HttpError::TimedOut)]),
                Visit::Good("x"),
                "could not be reached",
            ),
            (
                Scripted::new().json(
                    Method::Post,
                    "/token",
                    200,
                    r#"{"access_token": "a", "scope": "x"}"#,
                ),
                Visit::Good("x"),
                "no refresh token",
            ),
            (
                Scripted::new().json(Method::Post, "/token", 200, "[]"),
                Visit::Good("x"),
                "unexpected answer",
            ),
            (
                google_transport(&GOOGLE_TOKENS.replace("calendar.readonly", "other")),
                Visit::Good("x"),
                "did not grant everything",
            ),
            (
                Scripted::new()
                    .json(Method::Post, "/token", 200, GOOGLE_TOKENS)
                    .json(Method::Get, "openidconnect", 401, "{}"),
                Visit::Good("x"),
                "could not be identified",
            ),
            (
                Scripted::new()
                    .json(Method::Post, "/token", 200, GOOGLE_TOKENS)
                    .json(
                        Method::Get,
                        "openidconnect",
                        200,
                        r#"{"email": "a@b.example", "email_verified": false}"#,
                    ),
                Visit::Good("x"),
                "not verified",
            ),
            (
                Scripted::new()
                    .json(Method::Post, "/token", 200, GOOGLE_TOKENS)
                    .json(
                        Method::Get,
                        "openidconnect",
                        200,
                        r#"{"email": "not an address"}"#,
                    ),
                Visit::Good("x"),
                "no usable mail address",
            ),
        ];
        for (transport, visit, wanted) in cases {
            let vault = Memory::new();
            let (admin, _) = admin(transport, vault, visit);
            let admin = admin.within(Duration::from_secs(if wanted.contains("in time") {
                1
            } else {
                10
            }));
            let Err(error) = admin.link(Provider::Google, &google()?, false, &mut |_| {}) else {
                return Err(format!("{wanted}: expected an error").into());
            };
            assert!(error.contains(wanted), "{wanted}: {error}");
        }
        Ok(())
    }

    #[test]
    fn a_google_account_that_is_not_a_gmail_address_is_refused_before_anything_is_stored(
    ) -> TestResult {
        for email in [
            "alice@firma.example",
            "x@gmail.com.evil.example",
            "x@evilgmail.com",
        ] {
            let transport = Scripted::new()
                .json(Method::Post, "/token", 200, GOOGLE_TOKENS)
                .json(
                    Method::Get,
                    "openidconnect",
                    200,
                    &format!(r#"{{"sub": "1", "email": "{email}", "email_verified": true}}"#),
                );
            let vault = Arc::new(Memory::new());
            let browser = Visitor {
                visit: Visit::Good("x"),
                seen: Arc::new(Mutex::new(Vec::new())),
            };
            let admin = Linker::new(
                Box::new(transport),
                Box::new(crate::vault::memory::Shared(Arc::clone(&vault))),
                Box::new(browser),
            )
            .within(Duration::from_secs(10));
            let Err(error) = admin.link(Provider::Google, &google()?, false, &mut |_| {}) else {
                return Err(format!("{email}: expected an error").into());
            };
            assert!(
                error.contains("nur persoenliche Gmail-Konten")
                    && error.contains("gmail.com oder googlemail.com")
                    && error.contains("keine eigenen Domains"),
                "{error}"
            );
            assert_eq!(vault.count(), 0, "{email}");
        }
        Ok(())
    }

    #[test]
    fn a_failing_keychain_is_reported() -> TestResult {
        let (admin, _) = admin(
            google_transport(GOOGLE_TOKENS),
            Memory::broken(),
            Visit::Good("x"),
        );
        let Err(error) = admin.link(Provider::Google, &google()?, false, &mut |_| {}) else {
            return Err("expected an error".into());
        };
        assert!(error.contains("Keychain"), "{error}");
        Ok(())
    }

    #[test]
    fn a_sign_in_that_does_not_fit_the_keychain_is_refused_and_nothing_is_stored() -> TestResult {
        let tokens = GOOGLE_TOKENS.replace(
            "\"1//r\"",
            &format!("\"{}\"", "r".repeat(crate::vault::MAX_RECORD)),
        );
        let vault = Arc::new(Memory::new());
        let browser = Visitor {
            visit: Visit::Good("x"),
            seen: Arc::new(Mutex::new(Vec::new())),
        };
        let admin = Linker::new(
            Box::new(google_transport(&tokens)),
            Box::new(crate::vault::memory::Shared(Arc::clone(&vault))),
            Box::new(browser),
        )
        .within(Duration::from_secs(10));
        let Err(error) = admin.link(Provider::Google, &google()?, false, &mut |_| {}) else {
            return Err("expected an error".into());
        };
        assert!(
            error.contains("Keychain") && error.contains("too large"),
            "{error}"
        );
        assert_eq!(vault.count(), 0);
        Ok(())
    }

    #[test]
    fn capabilities_follow_the_stored_scopes_and_unlinking_deletes_the_entry() -> TestResult {
        let record = Record {
            client_id: "c".into(),
            client_secret: None,
            refresh_token: "r".into(),
            scope: "Mail.Read Mail.ReadWrite Mail.Send Calendars.Read".into(),
        };
        let vault = Memory::new().with("microsoft:bob@outlook.example", &record.to_json());
        let admin = Linker::new(
            Box::new(Scripted::new()),
            Box::new(vault),
            Box::new(Visitor {
                visit: Visit::Nothing,
                seen: Arc::new(Mutex::new(Vec::new())),
            }),
        );
        assert_eq!(
            admin.capability(Provider::Microsoft, "bob@outlook.example"),
            Some(Capability {
                write_mail: true,
                write_calendar: false
            })
        );
        assert_eq!(
            admin.capability(Provider::Microsoft, "nobody@outlook.example"),
            None
        );
        assert_eq!(
            admin.capability(Provider::Google, "bob@outlook.example"),
            None
        );
        assert!(admin.stored(Provider::Microsoft, "bob@outlook.example"));
        assert!(!admin.stored(Provider::Microsoft, "nobody@outlook.example"));
        assert!(!admin.stored(Provider::Google, "bob@outlook.example"));
        assert!(!admin.stored(Provider::Apple, "bob@outlook.example"));
        assert_eq!(
            admin.unlink(Provider::Microsoft, "bob@outlook.example"),
            Ok(())
        );
        assert_eq!(
            admin.capability(Provider::Microsoft, "bob@outlook.example"),
            None
        );
        assert!(!admin.stored(Provider::Microsoft, "bob@outlook.example"));
        assert_eq!(
            admin.unlink(Provider::Microsoft, "bob@outlook.example"),
            Ok(())
        );
        assert!(admin
            .unlink(Provider::Apple, "bob@outlook.example")
            .is_err());
        Ok(())
    }

    #[test]
    fn an_entry_counts_as_stored_when_it_is_unreadable_or_cannot_be_looked_up() {
        for vault in [
            Memory::new().with("google:alice@gmail.example", "not a record"),
            Memory::broken(),
        ] {
            let admin = Linker::new(
                Box::new(Scripted::new()),
                Box::new(vault),
                Box::new(Visitor {
                    visit: Visit::Nothing,
                    seen: Arc::new(Mutex::new(Vec::new())),
                }),
            );
            assert!(admin.stored(Provider::Google, "alice@gmail.example"));
            assert_eq!(
                admin.capability(Provider::Google, "alice@gmail.example"),
                None
            );
        }
    }
}
