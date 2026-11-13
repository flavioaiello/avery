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
use crate::error::{ToolError, ToolResult};
use crate::json::{self, Json, Object, ParseError};
use crate::provider::Provider;
use crate::sendlog::{LogEntry, Outcome};
use std::error::Error;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

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
    fn calendars(&self, _: Provider, _: &str) -> ToolResult<Vec<CalendarInfo>> {
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
