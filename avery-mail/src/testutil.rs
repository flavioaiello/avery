use crate::account::{Account, MailAccount};
use crate::backend::{
    Backend, ComposeRequest, Mailbox, MessageBody, MessageId, MessageList, MessageQuery, Mode,
    Preview,
};
use crate::calendar::{
    Approval, Calendar, CalendarBackend, CalendarInfo, Change, ChangeResult, EventDetail, RawEvent,
    Window,
};
use crate::config::Settings;
use crate::console::Screen;
use crate::error::{ToolError, ToolResult};
use crate::json::{self, Json, Object, ParseError};
use crate::osascript::{App, Finished, Host};
use crate::sendlog::{LogEntry, Outcome};
use std::collections::VecDeque;
use std::error::Error;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

pub(crate) type TestResult = Result<(), Box<dyn Error>>;

pub(crate) fn j(text: &str) -> Result<Json, ParseError> {
    json::parse(text)
}

pub(crate) fn mid(text: &str) -> Result<crate::backend::MessageId, Box<dyn Error>> {
    crate::backend::MessageId::parse(text).ok_or_else(|| "message id".into())
}

pub(crate) struct TempDir {
    root: PathBuf,
}

impl TempDir {
    pub(crate) fn new() -> io::Result<TempDir> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let name = format!(
            "avery-mail-test-{}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst),
            nanos
        );
        let root = std::env::temp_dir().join(name);
        fs::create_dir_all(&root)?;
        Ok(TempDir { root })
    }

    pub(crate) fn path(&self, sub: &str) -> String {
        if sub.is_empty() {
            self.root.to_string_lossy().into_owned()
        } else {
            self.root.join(sub).to_string_lossy().into_owned()
        }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub(crate) fn settings_at(path: &str) -> Settings {
    Settings {
        config_path: path.to_string(),
        fake: false,
        fake_write: String::new(),
        lock_wait: Duration::from_millis(300),
        no_onboard: false,
    }
}

pub(crate) fn args(text: &str) -> Result<Object, String> {
    match json::parse(text) {
        Ok(Json::Obj(object)) => Ok(object),
        Ok(other) => Err(format!("invalid test input: {other:?}")),
        Err(error) => Err(format!("invalid test input: {error}")),
    }
}

pub(crate) fn log_entry(ts: i64, outcome: Outcome, id: Option<&str>) -> LogEntry {
    LogEntry {
        ts,
        mode: Mode::Send,
        outcome,
        by: crate::journal::By::Model,
        account: "A".into(),
        from: String::new(),
        to: vec![],
        cc: vec![],
        subject: String::new(),
        reply_to_id: None,
        recipients: 0,
        chars: 0,
        sha256: String::new(),
        id: id.map(str::to_string),
    }
}

#[derive(Clone, Default)]
pub(crate) struct Listing {
    pub(crate) accounts: Vec<MailAccount>,
    pub(crate) calendars: Vec<CalendarInfo>,
}

impl Backend for Listing {
    fn all_accounts(&self) -> ToolResult<Vec<MailAccount>> {
        Ok(self.accounts.clone())
    }

    fn account_info(&self, _: &Account) -> ToolResult<MailAccount> {
        Err(ToolError::msg("only listed"))
    }

    fn mailboxes(&self, _: &Account) -> ToolResult<Vec<Mailbox>> {
        Err(ToolError::msg("only listed"))
    }

    fn messages(&self, _: &Account, _: &MessageQuery) -> ToolResult<MessageList> {
        Err(ToolError::msg("only listed"))
    }

    fn read(
        &self,
        _: &Account,
        _: Option<&str>,
        _: &MessageId,
        _: usize,
    ) -> ToolResult<MessageBody> {
        Err(ToolError::msg("only listed"))
    }

    fn compose(&self, _: &Account, _: &ComposeRequest) -> ToolResult<Preview> {
        Err(ToolError::msg("only listed"))
    }

    fn approval_test(&self, _: u64) -> ToolResult<String> {
        Err(ToolError::msg("only listed"))
    }
}

impl CalendarBackend for Listing {
    fn calendars(&self) -> ToolResult<Vec<CalendarInfo>> {
        Ok(self.calendars.clone())
    }

    fn events(&self, _: &Calendar, _: Window) -> ToolResult<Vec<RawEvent>> {
        Err(ToolError::msg("only listed"))
    }

    fn event(&self, _: &Calendar, _: &str) -> ToolResult<EventDetail> {
        Err(ToolError::msg("only listed"))
    }

    fn change(&self, _: &Calendar, _: &Change, _: &Approval) -> ToolResult<ChangeResult> {
        Err(ToolError::msg("only listed"))
    }
}

#[derive(Clone, Default)]
pub(crate) struct Untouchable {
    calls: Arc<AtomicUsize>,
}

impl Untouchable {
    pub(crate) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn touched<T>(&self) -> ToolResult<T> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(ToolError::msg("BACKEND TOUCHED"))
    }
}

impl Backend for Untouchable {
    fn all_accounts(&self) -> ToolResult<Vec<MailAccount>> {
        self.touched()
    }

    fn account_info(&self, _: &Account) -> ToolResult<MailAccount> {
        self.touched()
    }

    fn mailboxes(&self, _: &Account) -> ToolResult<Vec<Mailbox>> {
        self.touched()
    }

    fn messages(&self, _: &Account, _: &MessageQuery) -> ToolResult<MessageList> {
        self.touched()
    }

    fn read(
        &self,
        _: &Account,
        _: Option<&str>,
        _: &MessageId,
        _: usize,
    ) -> ToolResult<MessageBody> {
        self.touched()
    }

    fn compose(&self, _: &Account, _: &ComposeRequest) -> ToolResult<Preview> {
        self.touched()
    }

    fn approval_test(&self, _: u64) -> ToolResult<String> {
        self.touched()
    }
}

impl CalendarBackend for Untouchable {
    fn calendars(&self) -> ToolResult<Vec<CalendarInfo>> {
        self.touched()
    }

    fn events(&self, _: &Calendar, _: Window) -> ToolResult<Vec<RawEvent>> {
        self.touched()
    }

    fn event(&self, _: &Calendar, _: &str) -> ToolResult<EventDetail> {
        self.touched()
    }

    fn change(&self, _: &Calendar, _: &Change, _: &Approval) -> ToolResult<ChangeResult> {
        self.touched()
    }
}

pub(crate) struct Dark;

impl Host for Dark {
    fn ensure_running(&self, _app: App) {}

    fn run(&self, _app: App, _payload: &str, _timeout: Duration) -> Finished {
        Finished::NotStarted
    }
}

pub(crate) static DARK: Dark = Dark;

pub(crate) fn dark() -> Screen<'static> {
    Screen {
        host: &DARK,
        gui: &|| false,
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Reply {
    Button(usize),
    List(Vec<usize>),
    Cancel,
    Timeout,
    Done,
    Data(String),
    Silent,
    Hung,
    Flood,
    Raw {
        code: Option<i32>,
        stdout: String,
        stderr: String,
    },
}

impl Reply {
    fn finished(&self) -> Finished {
        let data = match self {
            Reply::Button(index) => format!("{{\"result\": \"button\", \"index\": {index}}}"),
            Reply::List(selected) => format!(
                "{{\"result\": \"list\", \"selected\": [{}]}}",
                selected
                    .iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Reply::Cancel => "{\"result\": \"cancel\"}".to_string(),
            Reply::Timeout => "{\"result\": \"timeout\"}".to_string(),
            Reply::Done => "{\"result\": \"ok\"}".to_string(),
            Reply::Data(text) => text.clone(),
            Reply::Silent => return Finished::NotStarted,
            Reply::Hung => return Finished::TimedOut,
            Reply::Flood => return Finished::TooLarge,
            Reply::Raw {
                code,
                stdout,
                stderr,
            } => {
                return Finished::Done {
                    code: *code,
                    stdout: stdout.clone(),
                    stderr: stderr.clone(),
                }
            }
        };
        Finished::Done {
            code: Some(0),
            stdout: format!("{{\"ok\": true, \"data\": {data}}}\n"),
            stderr: String::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Seen {
    pub(crate) payload: String,
    pub(crate) wait: Duration,
}

impl Seen {
    pub(crate) fn op(&self) -> String {
        match json::parse(&self.payload) {
            Ok(Json::Obj(object)) => object
                .get("op")
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_string(),
            _ => String::new(),
        }
    }

    pub(crate) fn field(&self, key: &str) -> Option<Json> {
        match json::parse(&self.payload) {
            Ok(Json::Obj(object)) => object.get(key).cloned(),
            _ => None,
        }
    }

    pub(crate) fn text(&self, key: &str) -> String {
        self.field(key)
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_default()
    }
}

type Hook = Box<dyn FnOnce()>;

pub(crate) struct Script {
    steps: Mutex<VecDeque<(&'static str, Reply)>>,
    seen: Mutex<Vec<Seen>>,
    hooks: Mutex<Vec<(usize, Hook)>>,
    mismatches: Mutex<Vec<String>>,
}

impl Script {
    pub(crate) fn new() -> Script {
        Script {
            steps: Mutex::new(VecDeque::new()),
            seen: Mutex::new(Vec::new()),
            hooks: Mutex::new(Vec::new()),
            mismatches: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn then(self, op: &'static str, reply: Reply) -> Script {
        if let Ok(mut steps) = self.steps.lock() {
            steps.push_back((op, reply));
        }
        self
    }

    pub(crate) fn after(self, calls: usize, hook: impl FnOnce() + 'static) -> Script {
        if let Ok(mut hooks) = self.hooks.lock() {
            hooks.push((calls, Box::new(hook)));
        }
        self
    }

    pub(crate) fn seen(&self) -> Vec<Seen> {
        self.seen
            .lock()
            .map(|seen| seen.clone())
            .unwrap_or_default()
    }

    pub(crate) fn ops(&self) -> Vec<String> {
        self.seen().iter().map(Seen::op).collect()
    }

    pub(crate) fn left(&self) -> usize {
        self.steps.lock().map(|steps| steps.len()).unwrap_or(0)
    }

    pub(crate) fn mismatches(&self) -> Vec<String> {
        self.mismatches
            .lock()
            .map(|found| found.clone())
            .unwrap_or_default()
    }
}

impl Host for Script {
    fn ensure_running(&self, _app: App) {}

    fn run(&self, app: App, payload: &str, timeout: Duration) -> Finished {
        let seen = Seen {
            payload: payload.to_string(),
            wait: timeout,
        };
        let op = seen.op();
        let calls = match self.seen.lock() {
            Ok(mut all) => {
                all.push(seen);
                all.len()
            }
            Err(_) => 0,
        };
        let ready: Vec<Hook> = match self.hooks.lock() {
            Ok(mut hooks) => {
                let mut ready = Vec::new();
                let mut index = 0;
                while index < hooks.len() {
                    if hooks[index].0 <= calls {
                        ready.push(hooks.remove(index).1);
                    } else {
                        index += 1;
                    }
                }
                ready
            }
            Err(_) => Vec::new(),
        };
        let next = self
            .steps
            .lock()
            .ok()
            .and_then(|mut steps| steps.pop_front());
        let finished = match next {
            Some((wanted, reply)) if wanted == op && app == App::Dialog => reply.finished(),
            Some((wanted, _)) => {
                self.note(&format!("wanted {wanted} for {app:?}, got {op}"));
                Finished::NotStarted
            }
            None => {
                self.note(&format!("no answer left for {op}"));
                Finished::NotStarted
            }
        };
        for hook in ready {
            hook();
        }
        finished
    }
}

impl Script {
    fn note(&self, text: &str) {
        if let Ok(mut found) = self.mismatches.lock() {
            found.push(text.to_string());
        }
    }
}
