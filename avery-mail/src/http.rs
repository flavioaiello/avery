use std::time::Duration;

use crate::osascript::{run_fed, Finished};

pub(crate) const CURL: &str = "/usr/bin/curl";
pub(crate) const RESPONSE_LIMIT: usize = 8 * 1024 * 1024;
pub(crate) const REQUEST_LIMIT: usize = 4 * 1024 * 1024;
const ERROR_LIMIT: usize = 2048;
const MAX_URL: usize = 4096;
const CONNECT_SECONDS: u64 = 10;
const GRACE_SECONDS: u64 = 5;
const CURL_TIMED_OUT: i32 = 28;
const HOSTS: [&str; 7] = [
    "gmail.googleapis.com",
    "www.googleapis.com",
    "oauth2.googleapis.com",
    "openidconnect.googleapis.com",
    "accounts.google.com",
    "graph.microsoft.com",
    "login.microsoftonline.com",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Method {
    Get,
    Post,
    Patch,
    Delete,
}

impl Method {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HttpError {
    Refused,
    NotStarted,
    TimedOut,
    TooLarge,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Request {
    pub(crate) method: Method,
    pub(crate) url: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Option<String>,
    pub(crate) timeout: Duration,
    refused: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Response {
    pub(crate) status: u16,
    pub(crate) body: String,
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HttpError::Refused => f.write_str("request refused"),
            HttpError::NotStarted => f.write_str("curl could not be started"),
            HttpError::TimedOut => f.write_str("timed out"),
            HttpError::TooLarge => f.write_str("answer too large"),
            HttpError::Failed(text) => f.write_str(text),
        }
    }
}

impl std::error::Error for HttpError {}

pub(crate) trait Transport: Sync {
    fn send(&self, request: &Request) -> Result<Response, HttpError>;
}

fn dot_segment(segment: &str) -> bool {
    let plain = segment.to_ascii_lowercase().replace("%2e", ".");
    plain == "." || plain == ".."
}

pub(crate) fn url_allowed(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let target = rest.split_once('?').map_or(rest, |(path, _)| path);
    let mut segments = target.split('/');
    let host = segments.next().unwrap_or_default();
    url.len() <= MAX_URL
        && HOSTS.contains(&host)
        && !segments.any(dot_segment)
        && rest
            .chars()
            .all(|c| c.is_ascii_graphic() && !"\"\\<>#{}[]|^`".contains(c))
}

fn header_ok(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii() && !c.is_ascii_control())
}

impl Request {
    pub(crate) fn new(method: Method, url: &str, timeout: Duration) -> Request {
        Request {
            method,
            url: url.to_string(),
            headers: Vec::new(),
            body: None,
            timeout,
            refused: !url_allowed(url),
        }
    }

    pub(crate) fn header(mut self, name: &str, value: &str) -> Request {
        if !header_ok(name) || name.contains(':') || !header_ok(value) {
            self.refused = true;
        }
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    pub(crate) fn body(mut self, body: &str) -> Request {
        if body.len() > REQUEST_LIMIT
            || body
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
        {
            self.refused = true;
        }
        self.body = Some(body.to_string());
        self
    }

    pub(crate) fn refused(&self) -> bool {
        self.refused
    }
}

pub(crate) fn encode_component(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

pub(crate) fn query(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(name, value)| format!("{name}={}", encode_component(value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn config_for(request: &Request) -> Option<String> {
    if request.refused() {
        return None;
    }
    let seconds = request.timeout.as_secs().max(1);
    let mut lines = vec![
        "silent".to_string(),
        "show-error".to_string(),
        "globoff".to_string(),
        "tlsv1.2".to_string(),
        "proto = \"=https\"".to_string(),
        "proto-redir = \"=https\"".to_string(),
        "max-redirs = 0".to_string(),
        format!("connect-timeout = {CONNECT_SECONDS}"),
        format!("max-time = {seconds}"),
        "user-agent = \"avery-mail\"".to_string(),
        "write-out = \"\\n%{http_code}\"".to_string(),
        format!("request = {}", quoted(request.method.as_str())),
        format!("url = {}", quoted(&request.url)),
        "header = \"Accept: application/json\"".to_string(),
        "header = \"Expect:\"".to_string(),
    ];
    for (name, value) in &request.headers {
        lines.push(format!("header = {}", quoted(&format!("{name}: {value}"))));
    }
    if let Some(body) = &request.body {
        lines.push(format!("data-binary = {}", quoted(body)));
    }
    lines.push(String::new());
    Some(lines.join("\n"))
}

fn one_line(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(200)
        .collect()
}

pub(crate) struct Curl {
    program: String,
}

impl Curl {
    pub(crate) fn system() -> Curl {
        Curl {
            program: CURL.to_string(),
        }
    }

    #[cfg(test)]
    pub(crate) fn at(program: &str) -> Curl {
        Curl {
            program: program.to_string(),
        }
    }
}

impl Transport for Curl {
    fn send(&self, request: &Request) -> Result<Response, HttpError> {
        let config = config_for(request).ok_or(HttpError::Refused)?;
        let limit = Duration::from_secs(request.timeout.as_secs().max(1) + GRACE_SECONDS);
        let finished = run_fed(
            &self.program,
            &["-q", "--config", "-"],
            config.as_bytes(),
            limit,
            RESPONSE_LIMIT + 16,
            ERROR_LIMIT,
        );
        let (code, stdout, stderr) = match finished {
            Finished::NotStarted => return Err(HttpError::NotStarted),
            Finished::TimedOut => return Err(HttpError::TimedOut),
            Finished::TooLarge => return Err(HttpError::TooLarge),
            Finished::Done {
                code,
                stdout,
                stderr,
            } => (code, stdout, stderr),
        };
        match code {
            Some(0) => {}
            Some(CURL_TIMED_OUT) => return Err(HttpError::TimedOut),
            Some(other) => {
                return Err(HttpError::Failed(format!(
                    "curl exit {other}: {}",
                    one_line(&stderr)
                )))
            }
            None => return Err(HttpError::Failed("curl was stopped".to_string())),
        }
        let (body, status) = stdout
            .rsplit_once('\n')
            .ok_or_else(|| HttpError::Failed("curl gave no status".to_string()))?;
        let status = status
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|status| (100..=599).contains(status))
            .ok_or_else(|| HttpError::Failed("curl gave no status".to_string()))?;
        if body.len() > RESPONSE_LIMIT {
            return Err(HttpError::TooLarge);
        }
        Ok(Response {
            status,
            body: body.to_string(),
        })
    }
}

#[cfg(test)]
pub(crate) mod scripted {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    struct Rule {
        method: Method,
        needle: String,
        replies: VecDeque<Result<Response, HttpError>>,
    }

    #[derive(Default)]
    pub(crate) struct Scripted {
        rules: Mutex<Vec<Rule>>,
        seen: Mutex<Vec<Request>>,
    }

    impl Scripted {
        pub(crate) fn new() -> Scripted {
            Scripted::default()
        }

        pub(crate) fn reply(
            self,
            method: Method,
            needle: &str,
            replies: Vec<Result<Response, HttpError>>,
        ) -> Scripted {
            if let Ok(mut rules) = self.rules.lock() {
                rules.push(Rule {
                    method,
                    needle: needle.to_string(),
                    replies: replies.into(),
                });
            }
            self
        }

        pub(crate) fn json(
            self,
            method: Method,
            needle: &str,
            status: u16,
            body: &str,
        ) -> Scripted {
            self.reply(
                method,
                needle,
                vec![Ok(Response {
                    status,
                    body: body.to_string(),
                })],
            )
        }

        pub(crate) fn seen(&self) -> Vec<Request> {
            self.seen
                .lock()
                .map(|seen| seen.clone())
                .unwrap_or_default()
        }
    }

    impl Transport for Scripted {
        fn send(&self, request: &Request) -> Result<Response, HttpError> {
            if request.refused() {
                return Err(HttpError::Refused);
            }
            if let Ok(mut seen) = self.seen.lock() {
                seen.push(request.clone());
            }
            let Ok(mut rules) = self.rules.lock() else {
                return Err(HttpError::Failed("poisoned".to_string()));
            };
            for rule in rules.iter_mut() {
                if rule.method == request.method && request.url.contains(&rule.needle) {
                    let reply = if rule.replies.len() > 1 {
                        rule.replies.pop_front()
                    } else {
                        rule.replies.front().cloned()
                    };
                    if let Some(reply) = reply {
                        return reply;
                    }
                }
            }
            Err(HttpError::Failed(format!(
                "unscripted request: {} {}",
                request.method.as_str(),
                request.url
            )))
        }
    }

    pub(crate) struct Shared(pub(crate) std::sync::Arc<Scripted>);

    impl Transport for Shared {
        fn send(&self, request: &Request) -> Result<Response, HttpError> {
            self.0.send(request)
        }
    }

    pub(crate) fn ok(body: &str) -> Result<Response, HttpError> {
        Ok(Response {
            status: 200,
            body: body.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{TempDir, TestResult};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    const SECONDS: Duration = Duration::from_secs(20);

    fn get(url: &str) -> Request {
        Request::new(Method::Get, url, SECONDS)
    }

    #[test]
    fn only_https_requests_to_the_known_hosts_are_accepted() {
        for url in [
            "https://gmail.googleapis.com/gmail/v1/users/me/messages?maxResults=5",
            "https://www.googleapis.com/calendar/v3/users/me/calendarList",
            "https://oauth2.googleapis.com/token",
            "https://graph.microsoft.com/v1.0/me/messages?$top=5&$select=id,subject",
            "https://login.microsoftonline.com/consumers/oauth2/v2.0/token",
        ] {
            assert!(!get(url).refused(), "{url}");
        }
        for url in [
            "http://gmail.googleapis.com/x",
            "https://evil.example/x",
            "https://gmail.googleapis.com.evil.example/x",
            "https://gmail.googleapis.com@evil.example/x",
            "https://gmail.googleapis.com:8443/x",
            "https://graph.microsoft.com\\@evil.example/",
            "https://graph.microsoft.com/a b",
            "https://graph.microsoft.com/a\"b",
            "https://graph.microsoft.com/a#frag",
            "https://graph.microsoft.com/a\nb",
            "https://graph.microsoft.com/ä",
            "ftp://graph.microsoft.com/",
            "",
        ] {
            assert!(get(url).refused(), "{url:?}");
        }
        let long = format!("https://graph.microsoft.com/{}", "a".repeat(MAX_URL));
        assert!(get(&long).refused());
    }

    #[test]
    fn brackets_and_braces_cannot_make_curl_expand_one_url_into_many() {
        for url in [
            "https://graph.microsoft.com/v1.0/me/messages?$skip=[1-9999]",
            "https://graph.microsoft.com/v1.0/me/messages?$skip=[a-z]&$top=[1-3]",
            "https://graph.microsoft.com/v1.0/[1-3]/messages",
            "https://graph.microsoft.com/v1.0/me/messages?x=a[b",
            "https://graph.microsoft.com/v1.0/me/messages?x=a]b",
            "https://graph.microsoft.com/v1.0/me/messages?x=%5B1-3%5D&y=[",
            "https://graph.microsoft.com/v1.0/me/messages?$skip={1,2}",
            "https://graph.microsoft.com/v1.0/{a,b}/messages",
            "https://graph.microsoft.com/v1.0/me/messages?x=}",
            "https://gmail.googleapis.com/gmail/v1/users/me/messages?q=[1-5]",
            "https://www.googleapis.com/calendar/v3/calendars/[a-b]/events",
        ] {
            assert!(get(url).refused(), "{url}");
            assert!(!url_allowed(url), "{url}");
            assert!(config_for(&get(url)).is_none(), "{url}");
        }
    }

    #[test]
    fn dot_segments_cannot_retarget_a_request_even_when_percent_encoded() {
        for url in [
            "https://graph.microsoft.com/v1.0/me/../users",
            "https://graph.microsoft.com/v1.0/me/messages/../../users/other",
            "https://graph.microsoft.com/v1.0/./me/messages",
            "https://graph.microsoft.com/v1.0/me/messages/.",
            "https://graph.microsoft.com/v1.0/me/messages/..",
            "https://graph.microsoft.com/v1.0/me/messages/..?$top=1",
            "https://graph.microsoft.com/v1.0/me/messages/.?$top=1",
            "https://graph.microsoft.com/../x",
            "https://graph.microsoft.com/./x",
            "https://graph.microsoft.com/..",
            "https://graph.microsoft.com/v1.0/me/%2e%2e/users",
            "https://graph.microsoft.com/v1.0/me/%2E%2E/users",
            "https://graph.microsoft.com/v1.0/me/%2e%2E/users",
            "https://graph.microsoft.com/v1.0/me/.%2e/users",
            "https://graph.microsoft.com/v1.0/me/%2E./users",
            "https://graph.microsoft.com/v1.0/me/%2e/users",
            "https://graph.microsoft.com/v1.0/me/%2E",
            "https://www.googleapis.com/calendar/v3/calendars/../events",
            "https://gmail.googleapis.com/gmail/v1/users/me/messages/%2e%2e",
        ] {
            assert!(get(url).refused(), "{url}");
            assert!(config_for(&get(url)).is_none(), "{url}");
        }
    }

    #[test]
    fn ordinary_urls_and_percent_encoded_ids_still_pass() {
        for url in [
            "https://graph.microsoft.com/v1.0/me/messages/AAMkAD%3D%3D?$select=id%2Csubject",
            "https://graph.microsoft.com/v1.0/me/messages?$top=50&$skip=50&$skiptoken=abc%2Fdef%2B%3D",
            "https://graph.microsoft.com/v1.0/users('0000')/calendars('AQMk')/calendarView?startDateTime=2026-11-19T00%3A00%3A00.0000000Z&%24top=100",
            "https://graph.microsoft.com/v1.0/me/mailFolders('AAMk')/messages?$filter=isRead%20eq%20false",
            "https://www.googleapis.com/calendar/v3/calendars/en.usa%23holiday%40group.v.calendar.google.com/events?timeMin=2026-01-01T00%3A00%3A00Z",
            "https://gmail.googleapis.com/gmail/v1/users/me/messages/18c2f%2Fabc%2E",
            "https://gmail.googleapis.com/gmail/v1/users/me/messages/a.b/..c/c../...",
            "https://graph.microsoft.com/v1.0/me/%2e%2e%2e/%252e%252e",
            "https://graph.microsoft.com/v1.0/me/messages?next=../..&dot=.",
            "https://graph.microsoft.com/v1.0/me/messages/",
            "https://graph.microsoft.com?x=1",
            "https://graph.microsoft.com",
        ] {
            assert!(!get(url).refused(), "{url}");
            assert!(url_allowed(url), "{url}");
            assert!(config_for(&get(url)).is_some(), "{url}");
        }
    }

    #[test]
    fn headers_and_bodies_are_checked() {
        let url = "https://graph.microsoft.com/v1.0/me";
        assert!(!get(url).header("Authorization", "Bearer abc").refused());
        assert!(get(url)
            .header("Authorization", "Bearer a\r\nX: y")
            .refused());
        assert!(get(url).header("Bad:Name", "x").refused());
        assert!(get(url).header("", "x").refused());
        assert!(get(url).header("X", "").refused());
        assert!(get(url).header("X", "ä").refused());
        assert!(!get(url).body("x").refused());
        assert!(get(url).body(&"x".repeat(REQUEST_LIMIT + 1)).refused());
        assert!(!get(url).body("a\nb\r\tc ä").refused());
        for control in ["\u{0}", "\u{1b}", "\u{7f}", "\u{85}"] {
            assert!(
                get(url).body(&format!("a{control}b")).refused(),
                "{control:?}"
            );
        }
    }

    #[test]
    fn components_and_queries_are_percent_encoded() {
        assert_eq!(
            encode_component("a b&c=d/é~-._"),
            "a%20b%26c%3Dd%2F%C3%A9~-._"
        );
        assert_eq!(
            query(&[("q", "in:inbox is:unread"), ("$top", "5")]),
            "q=in%3Ainbox%20is%3Aunread&$top=5"
        );
    }

    #[test]
    fn the_config_carries_everything_and_escapes_quotes_and_newlines() -> TestResult {
        let request = Request::new(
            Method::Post,
            "https://oauth2.googleapis.com/token",
            Duration::from_secs(30),
        )
        .header("Authorization", "Bearer tok\"en")
        .header("Content-Type", "application/json")
        .body("{\"a\": \"x\\ny\"}\n");
        let config = config_for(&request).ok_or("config")?;
        assert!(config.contains("request = \"POST\"\n"));
        assert!(config.contains("url = \"https://oauth2.googleapis.com/token\"\n"));
        assert!(config.contains("header = \"Authorization: Bearer tok\\\"en\"\n"));
        assert!(config.contains("data-binary = \"{\\\"a\\\": \\\"x\\\\ny\\\"}\\n\"\n"));
        assert!(config.contains("max-time = 30\n"));
        assert!(config.contains("proto = \"=https\"\n"));
        assert!(config.contains("max-redirs = 0\n"));
        assert!(config.contains("tlsv1.2\n"));
        assert!(config.lines().any(|line| line == "globoff"));
        assert!(config.ends_with('\n'));
        assert_eq!(config.matches('\n').count(), config.lines().count());
        assert!(config_for(&get("https://evil.example/")).is_none());
        for request in [
            get("https://graph.microsoft.com/v1.0/me/messages?$skip=50"),
            Request::new(
                Method::Delete,
                "https://graph.microsoft.com/v1.0/me/messages/AAMk%3D",
                SECONDS,
            ),
        ] {
            let config = config_for(&request).ok_or("config")?;
            assert_eq!(config.lines().filter(|line| *line == "globoff").count(), 1);
        }
        Ok(())
    }

    fn fake_curl(dir: &TempDir, script: &str) -> Result<String, Box<dyn std::error::Error>> {
        let path = dir.path("curl.sh");
        fs::write(&path, format!("#!/bin/sh\n{script}\n"))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
        Ok(path)
    }

    #[test]
    fn curl_receives_the_request_on_stdin_only() -> TestResult {
        let dir = TempDir::new()?;
        let record = dir.path("config.txt");
        let args = dir.path("args.txt");
        let program = fake_curl(
            &dir,
            &format!(
                "printf '%s ' \"$@\" > '{args}'\ncat > '{record}'\nprintf '{{\"ok\": true}}\\n200'"
            ),
        )?;
        let request =
            get("https://graph.microsoft.com/v1.0/me").header("Authorization", "Bearer SECRET");
        let response = Curl::at(&program).send(&request)?;
        assert_eq!(response.status, 200);
        assert_eq!(response.body, "{\"ok\": true}");
        let seen_args = fs::read_to_string(&args)?;
        assert_eq!(seen_args.trim(), "-q --config -");
        assert!(!seen_args.contains("SECRET"));
        let config = fs::read_to_string(&record)?;
        assert!(config.contains("Bearer SECRET"));
        assert!(config.lines().any(|line| line == "globoff"));
        Ok(())
    }

    #[test]
    fn curl_results_are_mapped_to_responses_and_errors() -> TestResult {
        let dir = TempDir::new()?;
        let request = get("https://graph.microsoft.com/v1.0/me");
        let cases: [(&str, Result<Response, HttpError>); 7] = [
            (
                "cat >/dev/null; printf '{\"error\": 1}\\n404'",
                Ok(Response {
                    status: 404,
                    body: "{\"error\": 1}".into(),
                }),
            ),
            (
                "cat >/dev/null; printf 'line1\\nline2\\n\\n204'",
                Ok(Response {
                    status: 204,
                    body: "line1\nline2\n".into(),
                }),
            ),
            ("cat >/dev/null; exit 28", Err(HttpError::TimedOut)),
            (
                "cat >/dev/null; echo 'Could not   resolve host' >&2; exit 6",
                Err(HttpError::Failed(
                    "curl exit 6: Could not resolve host".into(),
                )),
            ),
            (
                "cat >/dev/null; printf 'no status here'",
                Err(HttpError::Failed("curl gave no status".into())),
            ),
            (
                "cat >/dev/null; printf 'x\\n999'",
                Err(HttpError::Failed("curl gave no status".into())),
            ),
            (
                "cat >/dev/null; kill -9 $$",
                Err(HttpError::Failed("curl was stopped".into())),
            ),
        ];
        for (script, expected) in cases {
            let program = fake_curl(&dir, script)?;
            assert_eq!(Curl::at(&program).send(&request), expected, "{script}");
        }
        Ok(())
    }

    #[test]
    fn curl_that_cannot_start_or_talks_too_much_is_reported() -> TestResult {
        let dir = TempDir::new()?;
        let request = Request::new(
            Method::Get,
            "https://graph.microsoft.com/v1.0/me",
            Duration::from_secs(10),
        );
        assert_eq!(
            Curl::at("/nonexistent/curl").send(&request),
            Err(HttpError::NotStarted)
        );
        let program = fake_curl(
            &dir,
            "cat >/dev/null; /usr/bin/yes aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )?;
        assert_eq!(Curl::at(&program).send(&request), Err(HttpError::TooLarge));
        assert_eq!(
            Curl::at("/nonexistent/curl").send(&get("https://evil.example/")),
            Err(HttpError::Refused)
        );
        Ok(())
    }

    #[test]
    fn the_scripted_transport_answers_by_rule_and_records_requests() -> TestResult {
        use scripted::{ok, Scripted};
        let transport = Scripted::new()
            .reply(Method::Get, "/a", vec![ok("first"), ok("second")])
            .json(Method::Post, "/b", 201, "made");
        let url = |path: &str| format!("https://graph.microsoft.com{path}");
        assert_eq!(transport.send(&get(&url("/a")))?.body, "first");
        assert_eq!(transport.send(&get(&url("/a")))?.body, "second");
        assert_eq!(transport.send(&get(&url("/a")))?.body, "second");
        let post = Request::new(Method::Post, &url("/b"), SECONDS);
        assert_eq!(transport.send(&post)?.status, 201);
        assert!(matches!(
            transport.send(&get(&url("/b"))),
            Err(HttpError::Failed(text)) if text.contains("unscripted")
        ));
        assert_eq!(transport.seen().len(), 5);
        assert_eq!(
            transport.send(&get("https://evil.example/")),
            Err(HttpError::Refused)
        );
        Ok(())
    }
}
