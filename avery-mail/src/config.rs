use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use crate::account::{self, Account};
use crate::calendar::{self, Calendar};
use crate::error::{ToolError, ToolResult};
use crate::fake;
use crate::json::{self, Json, Object};
use crate::rights::{Apple, Level, Refusal, Rights, CONFIG_VERSION};
use crate::text::{clean, text_of};
use crate::texts::{
    CALENDAR_WRITE_OFF, DEFAULT_CHANGES_PER_HOUR, DEFAULT_SENDS_PER_HOUR, NO_ACCOUNTS,
    NO_CALENDARS, WRITE_OFF,
};

const LOCK_WAIT: Duration = Duration::from_secs(5);
const LOCK_POLL: Duration = Duration::from_millis(50);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Settings {
    pub(crate) config_path: String,
    pub(crate) fake: bool,
    pub(crate) fake_write: String,
    pub(crate) lock_wait: Duration,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum UpdateError {
    Busy,
    Lock(io::ErrorKind),
    Unreadable(ToolError),
    Refused(Refusal),
    Write(io::ErrorKind),
}

struct ConfigLock {
    file: File,
}

impl Drop for ConfigLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WriteSettings {
    pub(crate) accounts: Vec<Account>,
    pub(crate) max_per_hour: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CalendarWriteSettings {
    pub(crate) calendars: Vec<Calendar>,
    pub(crate) max_per_hour: i64,
}

impl Settings {
    pub(crate) fn from_env() -> Settings {
        Settings::from_lookup(&|key| {
            std::env::var_os(key).map(|value| value.to_string_lossy().into_owned())
        })
    }

    pub(crate) fn from_lookup(lookup: &dyn Fn(&str) -> Option<String>) -> Settings {
        let non_empty = |key: &str| lookup(key).filter(|value| !value.is_empty());
        let raw = match non_empty("MAIL_MCP_CONFIG") {
            Some(path) => path,
            None => {
                let home = non_empty("AVERY_HOME").unwrap_or_else(|| "~/.avery".to_string());
                join_path(&join_path(&home, "mail"), "config.json")
            }
        };
        Settings {
            config_path: expanduser(&raw, lookup("HOME").as_deref()),
            fake: lookup("MAIL_MCP_FAKE").as_deref() == Some("1"),
            fake_write: lookup("MAIL_MCP_FAKE_WRITE").unwrap_or_default(),
            lock_wait: LOCK_WAIT,
        }
    }

    pub(crate) fn sendlog_path(&self) -> String {
        join_path(&dirname(&self.config_path), "sendlog.jsonl")
    }

    pub(crate) fn changes_path(&self) -> String {
        join_path(&dirname(&self.config_path), "changes.jsonl")
    }

    pub(crate) fn lock_path(&self) -> String {
        join_path(&dirname(&self.config_path), "config.lock")
    }

    pub(crate) fn config_exists(&self) -> bool {
        Path::new(&self.config_path).exists()
    }

    pub(crate) fn load_config(&self) -> ToolResult<Object> {
        let unreadable = || {
            ToolError::msg(
                "config.json is unreadable. Ask the user to move the file away and run `avery mail setup` again.",
            )
        };
        let bytes = match read_bounded(&self.config_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let mut empty = Object::new();
                empty.insert("version", Json::Int(CONFIG_VERSION));
                return Ok(empty);
            }
            Err(_) => return Err(unreadable()),
        };
        let text = String::from_utf8(bytes).map_err(|_| unreadable())?;
        let data = json::parse(&text).map_err(|_| unreadable())?;
        let Json::Obj(object) = data else {
            return Err(ToolError::msg(
                "config.json has an unexpected format. Ask the user to move the file away and run `avery mail setup` again.",
            ));
        };
        if object.get("version") != Some(&Json::Int(CONFIG_VERSION)) {
            return Err(ToolError::msg(format!(
                "config.json has an unsupported format version (expected {CONFIG_VERSION}). Ask the user to move the file away and run `avery mail setup` again."
            )));
        }
        Ok(object)
    }

    fn save_config(&self, data: &Object) -> io::Result<()> {
        let directory = dirname(&self.config_path);
        make_dirs(&directory, 0o700)?;
        let temporary = format!("{}.tmp", self.config_path);
        match fs::remove_file(&temporary) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(json::dumps_pretty(&Json::Obj(data.clone()), 2, false).as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, &self.config_path)
    }

    pub(crate) fn rights(&self) -> ToolResult<Rights> {
        if self.fake {
            return Ok(self.fake_rights());
        }
        self.stored_rights()
    }

    pub(crate) fn stored_rights(&self) -> ToolResult<Rights> {
        let config = self.load_config()?;
        Rights::parse(&config).map_err(|what| {
            ToolError::msg(format!(
                "config.json is invalid ({what}). Ask the user to fix it or to move it away and run `avery mail setup` again."
            ))
        })
    }

    fn lock(&self) -> Result<ConfigLock, UpdateError> {
        let failed = |error: io::Error| UpdateError::Lock(error.kind());
        make_dirs(&dirname(&self.config_path), 0o700).map_err(failed)?;
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(self.lock_path())
            .map_err(failed)?;
        let started = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(ConfigLock { file }),
                Err(TryLockError::Error(error)) => return Err(failed(error)),
                Err(TryLockError::WouldBlock) => {
                    let left = self.lock_wait.saturating_sub(started.elapsed());
                    if left.is_zero() {
                        return Err(UpdateError::Busy);
                    }
                    thread::sleep(LOCK_POLL.min(left));
                }
            }
        }
    }

    pub(crate) fn update_rights<T>(
        &self,
        change: impl FnOnce(&mut Rights) -> Result<T, Refusal>,
    ) -> Result<T, UpdateError> {
        let _lock = self.lock()?;
        let stored = self.stored_rights().map_err(UpdateError::Unreadable)?;
        let mut rights = stored.clone();
        let value = change(&mut rights).map_err(UpdateError::Refused)?;
        if rights != stored {
            self.save_config(&rights.to_config())
                .map_err(|error| UpdateError::Write(error.kind()))?;
        }
        Ok(value)
    }

    fn fake_rights(&self) -> Rights {
        let writable: Vec<&str> = self
            .fake_write
            .split(',')
            .filter(|piece| !piece.is_empty())
            .collect();
        let level = |id: &str| {
            if writable.contains(&id) {
                Level::Write
            } else {
                Level::Read
            }
        };
        Rights {
            accounts: Vec::new(),
            apple: Apple {
                mail: fake::accounts()
                    .into_iter()
                    .filter(|entry| entry.id != "FAKE-WORK")
                    .map(|entry| {
                        let granted = level(&entry.id);
                        (entry, granted)
                    })
                    .collect(),
                calendars: calendar::demo_entries()
                    .into_iter()
                    .filter(|entry| entry.id != "FAKE-CAL-FIRMA")
                    .map(|entry| {
                        let granted = level(&entry.id);
                        (entry, granted)
                    })
                    .collect(),
            },
            sends_per_hour: DEFAULT_SENDS_PER_HOUR,
            changes_per_hour: DEFAULT_CHANGES_PER_HOUR,
        }
    }

    pub(crate) fn allowed_accounts(&self) -> ToolResult<Vec<Account>> {
        Ok(account::with_labels(self.rights()?.mail_at(Level::Read)))
    }

    pub(crate) fn write_settings(&self) -> ToolResult<WriteSettings> {
        let rights = self.rights()?;
        let accounts = account::with_labels(rights.mail_at(Level::Read))
            .into_iter()
            .filter(|account| rights.mail_level(&account.id) == Some(Level::Write))
            .collect();
        Ok(WriteSettings {
            accounts,
            max_per_hour: rights.sends_per_hour,
        })
    }

    pub(crate) fn allowed_calendars(&self) -> ToolResult<Vec<Calendar>> {
        Ok(calendar::with_labels(
            self.rights()?.calendars_at(Level::Read),
        ))
    }

    pub(crate) fn calendar_write_settings(&self) -> ToolResult<CalendarWriteSettings> {
        let rights = self.rights()?;
        let calendars = calendar::with_labels(rights.calendars_at(Level::Read))
            .into_iter()
            .filter(|calendar| rights.calendar_level(&calendar.id) == Some(Level::Write))
            .collect();
        Ok(CalendarWriteSettings {
            calendars,
            max_per_hour: rights.changes_per_hour,
        })
    }

    pub(crate) fn resolve(&self, account_name: Option<&Json>) -> ToolResult<Vec<Account>> {
        let accounts = self.allowed_accounts()?;
        if accounts.is_empty() {
            return Err(ToolError::msg(NO_ACCOUNTS));
        }
        let wanted = name_filter(account_name, "account")?;
        if wanted.is_empty() {
            return Ok(accounts);
        }
        let hits = by_name(&accounts, &wanted, |a| &a.label, Account::is_named);
        if hits.is_empty() {
            let labels: Vec<&str> = accounts.iter().map(|a| a.label.as_str()).collect();
            return Err(ToolError::msg(format!(
                "Unknown account '{}'. Allowed accounts: {}",
                clean(account_name, Some(60)),
                labels.join(", ")
            )));
        }
        Ok(hits)
    }

    pub(crate) fn resolve_writable(
        &self,
        account_name: Option<&Json>,
    ) -> ToolResult<(Account, WriteSettings)> {
        let settings = self.write_settings()?;
        if settings.accounts.is_empty() {
            return Err(ToolError::msg(WRITE_OFF));
        }
        let labels: Vec<&str> = settings.accounts.iter().map(|a| a.label.as_str()).collect();
        let names = labels.join(", ");
        let wanted = name_filter(account_name, "account")?;
        if wanted.is_empty() {
            return match settings.accounts.as_slice() {
                [only] => Ok((only.clone(), settings.clone())),
                _ => Err(ToolError::msg(format!(
                    "Parameter 'account' is required. Accounts that may write: {names}"
                ))),
            };
        }
        let hits: Vec<&Account> = settings
            .accounts
            .iter()
            .filter(|account| account.is_named(&wanted))
            .collect();
        match hits.as_slice() {
            [hit] => Ok(((*hit).clone(), settings.clone())),
            _ => Err(ToolError::msg(format!(
                "Unknown account '{}'. Accounts that may write: {names}",
                clean(account_name, Some(60))
            ))),
        }
    }

    pub(crate) fn resolve_calendars(
        &self,
        calendar_name: Option<&Json>,
    ) -> ToolResult<Vec<Calendar>> {
        let calendars = self.allowed_calendars()?;
        if calendars.is_empty() {
            return Err(ToolError::msg(NO_CALENDARS));
        }
        let wanted = name_filter(calendar_name, "calendar")?;
        if wanted.is_empty() {
            return Ok(calendars);
        }
        let hits = by_name(&calendars, &wanted, |c| &c.label, Calendar::is_named);
        if hits.is_empty() {
            let labels: Vec<&str> = calendars.iter().map(|c| c.label.as_str()).collect();
            return Err(ToolError::msg(format!(
                "Unknown calendar '{}'. Allowed calendars: {}",
                clean(calendar_name, Some(60)),
                labels.join(", ")
            )));
        }
        Ok(hits)
    }

    pub(crate) fn resolve_writable_calendar(
        &self,
        calendar_name: Option<&Json>,
    ) -> ToolResult<(Calendar, CalendarWriteSettings)> {
        let settings = self.calendar_write_settings()?;
        if settings.calendars.is_empty() {
            return Err(ToolError::msg(CALENDAR_WRITE_OFF));
        }
        let labels: Vec<&str> = settings
            .calendars
            .iter()
            .map(|c| c.label.as_str())
            .collect();
        let names = labels.join(", ");
        let wanted = name_filter(calendar_name, "calendar")?;
        if wanted.is_empty() {
            return match settings.calendars.as_slice() {
                [only] => Ok((only.clone(), settings.clone())),
                _ => Err(ToolError::msg(format!(
                    "Parameter 'calendar' is required. Calendars that may be changed: {names}"
                ))),
            };
        }
        let hits: Vec<&Calendar> = settings
            .calendars
            .iter()
            .filter(|calendar| calendar.is_named(&wanted))
            .collect();
        match hits.as_slice() {
            [hit] => Ok(((*hit).clone(), settings.clone())),
            _ => Err(ToolError::msg(format!(
                "Unknown calendar '{}'. Calendars that may be changed: {names}",
                clean(calendar_name, Some(60))
            ))),
        }
    }
}

fn by_name<T: Clone>(
    items: &[T],
    wanted: &str,
    label: impl Fn(&T) -> &str,
    named: impl Fn(&T, &str) -> bool,
) -> Vec<T> {
    let exact: Vec<T> = items
        .iter()
        .filter(|item| named(item, wanted))
        .cloned()
        .collect();
    if !exact.is_empty() {
        return exact;
    }
    items
        .iter()
        .filter(|item| label(item).to_lowercase().contains(wanted))
        .cloned()
        .collect()
}

fn name_filter(value: Option<&Json>, parameter: &str) -> ToolResult<String> {
    match value {
        Some(Json::Arr(_) | Json::Obj(_)) => Err(ToolError::msg(format!(
            "Parameter '{parameter}' must be text."
        ))),
        other => Ok(text_of(other).trim().to_lowercase()),
    }
}

const MAX_CONFIG_BYTES: usize = 1024 * 1024;

fn read_bounded(path: &str) -> io::Result<Vec<u8>> {
    let limit = u64::try_from(MAX_CONFIG_BYTES + 1).unwrap_or(u64::MAX);
    let mut bytes = Vec::new();
    fs::File::open(path)?.take(limit).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "too large"));
    }
    Ok(bytes)
}

pub(crate) fn join_path(base: &str, name: &str) -> String {
    if base.is_empty() {
        name.to_string()
    } else if base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

pub(crate) fn dirname(path: &str) -> String {
    let end = path.rfind('/').map_or(0, |index| index + 1);
    let head = path.get(..end).unwrap_or("");
    if head.chars().all(|c| c == '/') {
        head.to_string()
    } else {
        head.trim_end_matches('/').to_string()
    }
}

pub(crate) fn expanduser(path: &str, home: Option<&str>) -> String {
    if !path.starts_with('~') {
        return path.to_string();
    }
    let end = path
        .get(1..)
        .and_then(|rest| rest.find('/'))
        .map_or(path.len(), |index| index + 1);
    if end != 1 {
        return path.to_string();
    }
    let Some(home) = home else {
        return path.to_string();
    };
    let combined = format!(
        "{}{}",
        home.trim_end_matches('/'),
        path.get(end..).unwrap_or("")
    );
    if combined.is_empty() {
        "/".to_string()
    } else {
        combined
    }
}

pub(crate) fn make_dirs(path: &str, mode: u32) -> io::Result<()> {
    if path.is_empty() || Path::new(path).is_dir() {
        return Ok(());
    }
    if let Some(parent) = Path::new(path).parent() {
        if let Some(text) = parent.to_str() {
            make_dirs(text, mode)?;
        }
    }
    match DirBuilder::new().mode(mode).create(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && Path::new(path).is_dir() => {
            Ok(())
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Provider;
    use crate::rights::RemoteAccount;
    use crate::testutil::{j, settings_at, TempDir, TestResult};
    use std::collections::HashMap;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::sync::mpsc;

    fn account(provider: Provider, address: &str) -> Result<RemoteAccount, String> {
        RemoteAccount::new(provider, address).ok_or_else(|| format!("account {address}"))
    }

    fn updated<T>(
        settings: &Settings,
        change: impl FnOnce(&mut Rights) -> Result<T, Refusal>,
    ) -> Result<T, String> {
        settings
            .update_rights(change)
            .map_err(|error| format!("{error:?}"))
    }

    fn hold(settings: &Settings) -> Result<ConfigLock, String> {
        settings.lock().map_err(|error| format!("{error:?}"))
    }

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key| map.get(key).cloned()
    }

    fn write_config(dir: &TempDir, text: &str) -> io::Result<Settings> {
        let path = dir.path("config.json");
        fs::write(&path, text)?;
        Ok(settings_at(&path))
    }

    const TWO_ACCOUNTS: &str = r#"{"version": 3, "apple": {"mail": [
        {"id": "ACC-1", "name": "Privat Gmail", "type": "imap", "addresses": ["me@gmail.example"], "level": "read"},
        {"id": "ACC-2", "name": "Outlook", "type": "exchange", "addresses": ["me@outlook.example", "alias@outlook.example"], "level": "read"}]}}"#;

    const CALENDARS: &str = r#"{"version": 3, "apple": {"calendars": [
        {"id": "CAL-1", "name": "Privat", "level": "write"},
        {"id": "CAL-2", "name": "Familie", "level": "read"}]}, "changes_per_hour": 4}"#;

    const REMOTE: &str = r#"{"version": 3, "accounts": [
        {"provider": "google", "address": "alice@gmail.example", "mail": "write",
         "calendars": [
            {"id": "alice@gmail.example", "name": "alice@gmail.example", "level": "write"},
            {"id": "family@group", "name": "Familie (alice@gmail.example)", "level": "read"}]},
        {"provider": "microsoft", "address": "bob@outlook.example", "mail": "read"}],
        "sends_per_hour": 2}"#;

    #[test]
    fn paths_follow_the_environment() {
        let s = Settings::from_lookup(&lookup(&[("HOME", "/home/x")]));
        assert_eq!(s.config_path, "/home/x/.avery/mail/config.json");
        assert_eq!(s.sendlog_path(), "/home/x/.avery/mail/sendlog.jsonl");
        assert_eq!(s.changes_path(), "/home/x/.avery/mail/changes.jsonl");
        assert!(!s.fake);
        let s = Settings::from_lookup(&lookup(&[("HOME", "/home/x"), ("AVERY_HOME", "/opt/av/")]));
        assert_eq!(s.config_path, "/opt/av/mail/config.json");
        let s = Settings::from_lookup(&lookup(&[("HOME", "/home/x"), ("AVERY_HOME", "~/own")]));
        assert_eq!(s.config_path, "/home/x/own/mail/config.json");
        let s = Settings::from_lookup(&lookup(&[
            ("HOME", "/home/x"),
            ("AVERY_HOME", "/opt/av"),
            ("MAIL_MCP_CONFIG", "/etc/c.json"),
        ]));
        assert_eq!(s.config_path, "/etc/c.json");
        assert_eq!(s.sendlog_path(), "/etc/sendlog.jsonl");
        let s = Settings::from_lookup(&lookup(&[
            ("HOME", "/home/x"),
            ("MAIL_MCP_CONFIG", ""),
            ("AVERY_HOME", ""),
        ]));
        assert_eq!(s.config_path, "/home/x/.avery/mail/config.json");
    }

    #[test]
    fn only_the_exact_value_one_enables_demo_mode() {
        for (value, expected) in [
            ("1", true),
            ("true", false),
            ("0", false),
            ("", false),
            ("yes", false),
        ] {
            let s = Settings::from_lookup(&lookup(&[("MAIL_MCP_FAKE", value)]));
            assert_eq!(s.fake, expected, "{value:?}");
        }
    }

    #[test]
    fn expanduser_and_dirname_resolve_paths() {
        assert_eq!(expanduser("~", Some("/h")), "/h");
        assert_eq!(expanduser("~/a/b", Some("/h/")), "/h/a/b");
        assert_eq!(expanduser("~/a", Some("")), "/a");
        assert_eq!(expanduser("~", Some("")), "/");
        assert_eq!(expanduser("~bob/a", Some("/h")), "~bob/a");
        assert_eq!(expanduser("a/~", Some("/h")), "a/~");
        assert_eq!(expanduser("~/a", None), "~/a");
        assert_eq!(dirname("/a/b/c.json"), "/a/b");
        assert_eq!(dirname("c.json"), "");
        assert_eq!(dirname("/c.json"), "/");
        assert_eq!(dirname("a//b"), "a");
        assert_eq!(dirname("/a/b/"), "/a/b");
        assert_eq!(join_path("", "x"), "x");
        assert_eq!(join_path("a", "x"), "a/x");
        assert_eq!(join_path("a/", "x"), "a/x");
    }

    #[test]
    fn a_missing_config_grants_nothing() -> TestResult {
        let dir = TempDir::new()?;
        let s = settings_at(&dir.path("none.json"));
        assert_eq!(s.rights(), Ok(Rights::empty()));
        assert_eq!(s.allowed_accounts(), Ok(vec![]));
        assert_eq!(s.allowed_calendars(), Ok(vec![]));
        assert_eq!(s.resolve(None), Err(ToolError::msg(NO_ACCOUNTS)));
        assert_eq!(s.resolve_calendars(None), Err(ToolError::msg(NO_CALENDARS)));
        Ok(())
    }

    #[test]
    fn unreadable_and_malformed_configs_are_refused() -> TestResult {
        let dir = TempDir::new()?;
        for text in ["", "{", "nope", "\u{feff}{}", "{\"mail\": NaN}"] {
            let s = write_config(&dir, text)?;
            assert!(
                matches!(s.load_config(), Err(ToolError::Msg(m)) if m.contains("unreadable")),
                "{text:?}"
            );
        }
        for text in ["[]", "\"x\"", "5"] {
            let s = write_config(&dir, text)?;
            assert!(
                matches!(s.load_config(), Err(ToolError::Msg(m)) if m.contains("unexpected format")),
                "{text:?}"
            );
        }
        for text in [
            "{}",
            "{\"allowed_accounts\": []}",
            "{\"version\": 1, \"allowed_accounts\": []}",
            "{\"version\": 2, \"allowed_accounts\": [], \"write\": {\"accounts\": []}}",
            "{\"version\": 4}",
            "{\"version\": \"3\"}",
            "{\"version\": 3.0}",
            "{\"version\": null}",
        ] {
            let s = write_config(&dir, text)?;
            assert!(
                matches!(s.load_config(), Err(ToolError::Msg(m)) if m.contains("unsupported format version (expected 3)")),
                "{text:?}"
            );
            assert!(s.rights().is_err(), "{text:?}");
            assert!(s.allowed_accounts().is_err(), "{text:?}");
            assert!(s.allowed_calendars().is_err(), "{text:?}");
            assert_eq!(fs::read_to_string(&s.config_path)?, text);
        }
        fs::write(dir.path("bytes.json"), [0xff, 0xfe, 0x7b, 0x7d])?;
        let s = settings_at(&dir.path("bytes.json"));
        assert!(matches!(s.load_config(), Err(ToolError::Msg(m)) if m.contains("unreadable")));
        let s = settings_at(&dir.path(""));
        assert!(matches!(s.load_config(), Err(ToolError::Msg(m)) if m.contains("unreadable")));
        let padding = " ".repeat(MAX_CONFIG_BYTES);
        let s = write_config(&dir, &format!("{{\"version\": 3}}{padding}"))?;
        assert!(matches!(s.load_config(), Err(ToolError::Msg(m)) if m.contains("unreadable")));
        let padding = " ".repeat(MAX_CONFIG_BYTES - 14);
        let s = write_config(&dir, &format!("{{\"version\": 3}}{padding}"))?;
        assert!(s.load_config().is_ok());
        Ok(())
    }

    #[test]
    fn a_config_with_unexpected_content_is_invalid_as_a_whole() -> TestResult {
        let dir = TempDir::new()?;
        for text in [
            r#"{"version": 3, "allowed_accounts": []}"#,
            r#"{"version": 3, "mail": {"accounts": []}}"#,
            r#"{"version": 3, "apple": {"mail": [{"id": 7, "level": "read"}]}}"#,
            r#"{"version": 3, "apple": {"mail": [{"id": "A", "level": "admin"}]}}"#,
            r#"{"version": 3, "sends_per_hour": 0}"#,
            r#"{"version": 3, "apple": {"calendars": [{"id": "C"}]}}"#,
            r#"{"version": 3, "accounts": [{"provider": "google", "address": "Alice@Gmail.example"}]}"#,
        ] {
            let s = write_config(&dir, text)?;
            assert!(
                matches!(s.rights(), Err(ToolError::Msg(m)) if m.starts_with("config.json is invalid (")),
                "{text}"
            );
            assert!(s.allowed_accounts().is_err());
            assert!(s.calendar_write_settings().is_err());
            assert_eq!(fs::read_to_string(&s.config_path)?, text);
        }
        Ok(())
    }

    #[test]
    fn a_config_without_sections_grants_nothing() -> TestResult {
        let dir = TempDir::new()?;
        let s = write_config(&dir, "{\"version\": 3}")?;
        assert_eq!(s.allowed_accounts(), Ok(vec![]));
        assert_eq!(s.allowed_calendars(), Ok(vec![]));
        Ok(())
    }

    #[test]
    fn allowed_accounts_are_read_with_labels() -> TestResult {
        let dir = TempDir::new()?;
        let s = write_config(&dir, TWO_ACCOUNTS)?;
        let accounts = s.allowed_accounts()?;
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[0].label, "Privat Gmail");
        assert_eq!(
            accounts[1].addresses,
            ["me@outlook.example", "alias@outlook.example"]
        );
        assert_eq!(accounts[1].kind, "exchange");
        Ok(())
    }

    #[test]
    fn demo_mode_never_exposes_the_company_account_or_calendar() -> TestResult {
        let mut s = settings_at("/nonexistent/config.json");
        s.fake = true;
        let ids: Vec<String> = s.allowed_accounts()?.into_iter().map(|a| a.id).collect();
        assert_eq!(ids, ["FAKE-GMAIL", "FAKE-OUTLOOK"]);
        let calendars: Vec<String> = s.allowed_calendars()?.into_iter().map(|c| c.id).collect();
        assert_eq!(
            calendars,
            ["FAKE-CAL-PRIVAT", "FAKE-CAL-FAMILIE", "FAKE-CAL-FEIERTAGE"]
        );
        Ok(())
    }

    #[test]
    fn resolve_finds_accounts_by_label_name_address_or_part_of_a_label() -> TestResult {
        let dir = TempDir::new()?;
        let s = write_config(&dir, TWO_ACCOUNTS)?;
        let names = |value: &str| -> Result<Vec<String>, Box<dyn std::error::Error>> {
            Ok(s.resolve(Some(&j(value)?))?
                .into_iter()
                .map(|a| a.id)
                .collect())
        };
        assert_eq!(names("\"privat gmail\"")?, ["ACC-1"]);
        assert_eq!(names("\" OUTLOOK \"")?, ["ACC-2"]);
        assert_eq!(names("\"alias@outlook.example\"")?, ["ACC-2"]);
        assert_eq!(names("\"gmail\"")?, ["ACC-1"]);
        assert_eq!(names("\"   \"")?, ["ACC-1", "ACC-2"]);
        assert_eq!(s.resolve(None)?.len(), 2);
        assert_eq!(s.resolve(Some(&j("\"\"")?))?.len(), 2);
        assert_eq!(
            s.resolve(Some(&j("\"Firma\"")?)),
            Err(ToolError::msg(
                "Unknown account 'Firma'. Allowed accounts: Privat Gmail, Outlook"
            ))
        );
        assert_eq!(
            s.resolve(Some(&j("0")?)),
            Err(ToolError::msg(
                "Unknown account '0'. Allowed accounts: Privat Gmail, Outlook"
            ))
        );
        for container in ["[\"a\"]", "[]", "{}"] {
            assert_eq!(
                s.resolve(Some(&j(container)?)),
                Err(ToolError::msg("Parameter 'account' must be text.")),
                "{container}"
            );
        }
        Ok(())
    }

    #[test]
    fn writing_needs_the_write_level_for_the_account() -> TestResult {
        let dir = TempDir::new()?;
        let off = write_config(&dir, TWO_ACCOUNTS)?;
        assert_eq!(off.write_settings()?.accounts, vec![]);
        assert_eq!(off.resolve_writable(None), Err(ToolError::msg(WRITE_OFF)));
        let writing = write_config(
            &dir,
            r#"{"version": 3, "apple": {"mail": [
                {"id": "ACC-1", "name": "A", "level": "read"},
                {"id": "ACC-2", "name": "B", "level": "write"}]}}"#,
        )?;
        let settings = writing.write_settings()?;
        let ids: Vec<&str> = settings.accounts.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["ACC-2"]);
        assert_eq!(writing.allowed_accounts()?.len(), 2);
        let (only, _) = writing.resolve_writable(None)?;
        assert_eq!(only.id, "ACC-2");
        assert_eq!(
            writing.resolve_writable(Some(&j("\"A\"")?)),
            Err(ToolError::msg(
                "Unknown account 'A'. Accounts that may write: B"
            ))
        );
        Ok(())
    }

    #[test]
    fn write_settings_carry_the_hourly_limit() -> TestResult {
        let dir = TempDir::new()?;
        let with = |limit: &str| {
            format!(
                r#"{{"version": 3, "apple": {{"mail": [{{"id": "A", "name": "A", "level": "write"}}]}}, {limit}}}"#
            )
        };
        let s = write_config(&dir, &with(r#""sends_per_hour": 3"#))?;
        let settings = s.write_settings()?;
        assert_eq!(settings.accounts.len(), 1);
        assert_eq!(settings.max_per_hour, 3);
        let s = write_config(&dir, &with(r#""x": 1"#))?;
        assert!(s.write_settings().is_err());
        let s = write_config(
            &dir,
            r#"{"version": 3, "apple": {"mail": [{"id": "A", "level": "write"}]}}"#,
        )?;
        assert_eq!(s.write_settings()?.max_per_hour, 5);
        for bad in ["0", "31", "\"7\"", "null", "true"] {
            let s = write_config(&dir, &with(&format!(r#""sends_per_hour": {bad}"#)))?;
            assert!(s.write_settings().is_err(), "{bad}");
        }
        Ok(())
    }

    #[test]
    fn demo_write_ids_come_from_the_environment_variable() -> TestResult {
        let mut s = settings_at("/nonexistent/config.json");
        s.fake = true;
        s.fake_write = "FAKE-GMAIL,,FAKE-WORK,FAKE-OUTLOOK,FAKE-CAL-PRIVAT,FAKE-CAL-FIRMA".into();
        let settings = s.write_settings()?;
        let ids: Vec<&str> = settings.accounts.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["FAKE-GMAIL", "FAKE-OUTLOOK"]);
        assert_eq!(settings.max_per_hour, 5);
        let calendars = s.calendar_write_settings()?;
        let ids: Vec<&str> = calendars.calendars.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["FAKE-CAL-PRIVAT"]);
        assert_eq!(calendars.max_per_hour, 10);
        Ok(())
    }

    #[test]
    fn resolve_writable_picks_exactly_one_account() -> TestResult {
        let mut s = settings_at("/nonexistent/config.json");
        s.fake = true;
        s.fake_write = "FAKE-GMAIL".into();
        let (only, _) = s.resolve_writable(None)?;
        assert_eq!(only.id, "FAKE-GMAIL");
        let (by_address, _) = s.resolve_writable(Some(&j("\"demo.privat@gmail.example\"")?))?;
        assert_eq!(by_address.id, "FAKE-GMAIL");
        assert_eq!(
            s.resolve_writable(Some(&j("\"Demo Outlook\"")?)),
            Err(ToolError::msg(
                "Unknown account 'Demo Outlook'. Accounts that may write: Demo Gmail"
            ))
        );
        assert_eq!(
            s.resolve_writable(Some(&j("\"gmail\"")?)),
            Err(ToolError::msg(
                "Unknown account 'gmail'. Accounts that may write: Demo Gmail"
            ))
        );
        s.fake_write = "FAKE-GMAIL,FAKE-OUTLOOK".into();
        assert_eq!(
            s.resolve_writable(None),
            Err(ToolError::msg("Parameter 'account' is required. Accounts that may write: Demo Gmail, Demo Outlook"))
        );
        s.fake_write = String::new();
        assert_eq!(s.resolve_writable(None), Err(ToolError::msg(WRITE_OFF)));
        Ok(())
    }

    #[test]
    fn calendars_are_read_with_levels_limit_and_labels() -> TestResult {
        let dir = TempDir::new()?;
        let s = write_config(&dir, CALENDARS)?;
        let all = s.allowed_calendars()?;
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].label, "Privat");
        let writing = s.calendar_write_settings()?;
        let ids: Vec<&str> = writing.calendars.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["CAL-1"]);
        assert_eq!(writing.max_per_hour, 4);
        let (only, _) = s.resolve_writable_calendar(None)?;
        assert_eq!(only.id, "CAL-1");
        assert_eq!(
            s.resolve_writable_calendar(Some(&j("\"Familie\"")?)),
            Err(ToolError::msg(
                "Unknown calendar 'Familie'. Calendars that may be changed: Privat"
            ))
        );
        let read_only = write_config(
            &dir,
            r#"{"version": 3, "apple": {"calendars": [{"id": "C", "name": "N", "level": "read"}]}}"#,
        )?;
        assert_eq!(
            read_only.resolve_writable_calendar(None),
            Err(ToolError::msg(CALENDAR_WRITE_OFF))
        );
        assert_eq!(read_only.calendar_write_settings()?.max_per_hour, 10);
        Ok(())
    }

    #[test]
    fn resolve_calendars_finds_by_label_or_part_and_names_the_allowed_ones() -> TestResult {
        let dir = TempDir::new()?;
        let s = write_config(&dir, CALENDARS)?;
        let ids = |value: &str| -> Result<Vec<String>, Box<dyn std::error::Error>> {
            Ok(s.resolve_calendars(Some(&j(value)?))?
                .into_iter()
                .map(|c| c.id)
                .collect())
        };
        assert_eq!(ids("\"privat\"")?, ["CAL-1"]);
        assert_eq!(ids("\" FAMILIE \"")?, ["CAL-2"]);
        assert_eq!(ids("\"fam\"")?, ["CAL-2"]);
        assert_eq!(ids("\"\"")?, ["CAL-1", "CAL-2"]);
        assert_eq!(s.resolve_calendars(None)?.len(), 2);
        assert_eq!(
            s.resolve_calendars(Some(&j("\"Firma\"")?)),
            Err(ToolError::msg(
                "Unknown calendar 'Firma'. Allowed calendars: Privat, Familie"
            ))
        );
        assert_eq!(
            s.resolve_calendars(Some(&j("[]")?)),
            Err(ToolError::msg("Parameter 'calendar' must be text."))
        );
        let mut demo = settings_at("/nonexistent/config.json");
        demo.fake = true;
        demo.fake_write = "FAKE-CAL-PRIVAT,FAKE-CAL-FAMILIE".into();
        assert_eq!(
            demo.resolve_writable_calendar(None),
            Err(ToolError::msg("Parameter 'calendar' is required. Calendars that may be changed: Demo Privat, Demo Familie"))
        );
        Ok(())
    }

    #[test]
    fn rights_survive_a_save_and_load() -> TestResult {
        let dir = TempDir::new()?;
        let s = settings_at(&dir.path("sub/dir/config.json"));
        let mut rights = Rights::empty();
        rights.sends_per_hour = 7;
        rights.apple.mail.push((
            crate::account::MailAccount {
                id: "A".into(),
                name: "N".into(),
                kind: "imap".into(),
                addresses: vec!["a@example.org".into()],
            },
            Level::Write,
        ));
        rights.apple.calendars.push((
            crate::calendar::CalendarEntry {
                id: "C".into(),
                name: "K".into(),
            },
            Level::Read,
        ));
        let mut account = crate::rights::RemoteAccount::new(
            crate::provider::Provider::Microsoft,
            "bob@outlook.example",
        )
        .ok_or("account")?;
        account.mail = Some(Level::Read);
        rights.accounts.push(account);
        let saved = rights.clone();
        updated(&s, |stored| {
            *stored = saved;
            Ok(())
        })?;
        assert_eq!(s.rights()?, rights);
        Ok(())
    }

    #[test]
    fn remote_accounts_are_read_with_runtime_ids_labels_and_limits() -> TestResult {
        let dir = TempDir::new()?;
        let s = write_config(&dir, REMOTE)?;
        let accounts = s.allowed_accounts()?;
        let ids: Vec<&str> = accounts.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "google:alice@gmail.example",
                "microsoft:bob@outlook.example"
            ]
        );
        assert_eq!(accounts[0].label, "alice@gmail.example");
        assert_eq!(accounts[0].kind, "gmail");
        assert_eq!(accounts[1].kind, "outlook");
        let writing = s.write_settings()?;
        let ids: Vec<&str> = writing.accounts.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["google:alice@gmail.example"]);
        assert_eq!(writing.max_per_hour, 2);
        let calendars = s.allowed_calendars()?;
        let ids: Vec<&str> = calendars.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "google:alice@gmail.example:alice@gmail.example",
                "google:alice@gmail.example:family@group"
            ]
        );
        assert_eq!(
            s.resolve(Some(&j("\"bob@outlook.example\"")?))?[0].id,
            "microsoft:bob@outlook.example"
        );
        let changing = s.calendar_write_settings()?;
        let ids: Vec<&str> = changing.calendars.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["google:alice@gmail.example:alice@gmail.example"]);
        let (only, _) = s.resolve_writable_calendar(None)?;
        assert_eq!(only.label, "alice@gmail.example");
        Ok(())
    }

    #[test]
    fn save_config_writes_indented_json_with_private_permissions() -> TestResult {
        let dir = TempDir::new()?;
        let s = settings_at(&dir.path("sub/dir/config.json"));
        let mut config = Object::new();
        config.insert("version", 3i64);
        s.save_config(&config)?;
        let text = fs::read_to_string(dir.path("sub/dir/config.json"))?;
        assert_eq!(text, "{\n  \"version\": 3\n}");
        let file_mode = fs::metadata(dir.path("sub/dir/config.json"))?
            .permissions()
            .mode()
            & 0o777;
        let dir_mode = fs::metadata(dir.path("sub/dir"))?.permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600);
        assert_eq!(dir_mode, 0o700);
        assert!(!Path::new(&dir.path("sub/dir/config.json.tmp")).exists());
        assert_eq!(s.load_config()?, config);
        Ok(())
    }

    #[test]
    fn make_dirs_gives_every_directory_it_creates_the_private_mode() -> TestResult {
        let dir = TempDir::new()?;
        make_dirs(&dir.path("p/q/r"), 0o700)?;
        let mode = |sub: &str| -> io::Result<u32> {
            Ok(fs::metadata(dir.path(sub))?.permissions().mode() & 0o777)
        };
        for sub in ["p", "p/q", "p/q/r"] {
            assert_eq!(mode(sub)?, 0o700, "{sub}");
        }
        assert_ne!(mode("")?, 0o700);
        make_dirs(&dir.path("p/q/r"), 0o700)?;
        fs::write(dir.path("file"), "x")?;
        assert!(make_dirs(&dir.path("file"), 0o700).is_err());
        assert!(make_dirs(&dir.path("file/sub"), 0o700).is_err());
        Ok(())
    }

    #[test]
    fn save_config_replaces_a_planted_temporary_file_instead_of_following_it() -> TestResult {
        let dir = TempDir::new()?;
        let s = settings_at(&dir.path("config.json"));
        fs::write(dir.path("victim"), "keep")?;
        std::os::unix::fs::symlink(dir.path("victim"), format!("{}.tmp", s.config_path))?;
        let mut config = Object::new();
        config.insert("version", 3i64);
        s.save_config(&config)?;
        assert_eq!(fs::read_to_string(dir.path("victim"))?, "keep");
        assert!(fs::read_to_string(&s.config_path)?.contains("\"version\": 3"));
        Ok(())
    }

    #[test]
    fn an_update_changes_what_is_stored_and_returns_what_the_change_returns() -> TestResult {
        let dir = TempDir::new()?;
        let s = write_config(&dir, REMOTE)?;
        let seen = updated(&s, |rights| {
            let seen = rights.accounts.len();
            rights.sends_per_hour = 4;
            Ok(seen)
        })?;
        assert_eq!(seen, 2);
        let rights = s.stored_rights()?;
        assert_eq!((rights.accounts.len(), rights.sends_per_hour), (2, 4));
        Ok(())
    }

    #[test]
    fn the_change_runs_on_what_another_writer_saved_after_the_snapshot() -> TestResult {
        let dir = TempDir::new()?;
        let s = write_config(&dir, REMOTE)?;
        let snapshot = s.stored_rights()?;
        updated(&s, |rights| {
            rights.sends_per_hour = 9;
            Ok(())
        })?;
        let outcome = s.update_rights(|fresh| {
            if *fresh != snapshot {
                return Err(Refusal::Changed);
            }
            fresh.sends_per_hour = 1;
            Ok(())
        });
        assert_eq!(outcome, Err(UpdateError::Refused(Refusal::Changed)));
        assert_eq!(s.stored_rights()?.sends_per_hour, 9);
        let carried = s.update_rights(|fresh| Ok(fresh.sends_per_hour));
        assert_eq!(carried, Ok(9));
        Ok(())
    }

    #[test]
    fn a_refused_change_and_an_unreadable_config_leave_the_file_alone() -> TestResult {
        let dir = TempDir::new()?;
        let s = write_config(&dir, REMOTE)?;
        assert_eq!(
            s.update_rights(|_| Err::<(), _>(Refusal::Full)),
            Err(UpdateError::Refused(Refusal::Full))
        );
        assert_eq!(fs::read_to_string(&s.config_path)?, REMOTE);
        for text in [
            "not json",
            "{\"version\": 2}",
            "[]",
            "{\"version\": 3, \"x\": 1}",
        ] {
            fs::write(&s.config_path, text)?;
            let mut ran = false;
            let outcome = s.update_rights(|_| {
                ran = true;
                Ok(())
            });
            assert!(
                matches!(outcome, Err(UpdateError::Unreadable(_))),
                "{text}: {outcome:?}"
            );
            assert!(!ran, "{text}");
            assert_eq!(fs::read_to_string(&s.config_path)?, text);
        }
        Ok(())
    }

    #[test]
    fn an_update_that_changes_nothing_writes_nothing() -> TestResult {
        let dir = TempDir::new()?;
        let s = settings_at(&dir.path("config.json"));
        updated(&s, |_| Ok(()))?;
        assert!(!Path::new(&s.config_path).exists());
        let handwritten = r#"{"version": 3}"#;
        fs::write(&s.config_path, handwritten)?;
        updated(&s, |_| Ok(()))?;
        assert_eq!(fs::read_to_string(&s.config_path)?, handwritten);
        Ok(())
    }

    #[test]
    fn the_lock_file_is_private_stays_in_place_and_is_not_the_config_file() -> TestResult {
        let dir = TempDir::new()?;
        let s = settings_at(&dir.path("sub/config.json"));
        assert_eq!(s.lock_path(), dir.path("sub/config.lock"));
        assert_ne!(s.lock_path(), s.config_path);
        updated(&s, |rights| {
            rights.sends_per_hour = 2;
            Ok(())
        })?;
        let lock = fs::metadata(s.lock_path())?;
        assert_eq!(lock.permissions().mode() & 0o777, 0o600);
        assert_eq!(lock.len(), 0);
        let directory = fs::metadata(dir.path("sub"))?;
        assert_eq!(directory.permissions().mode() & 0o777, 0o700);
        let (lock_inode, config_inode) = (lock.ino(), fs::metadata(&s.config_path)?.ino());
        updated(&s, |rights| {
            rights.sends_per_hour = 3;
            Ok(())
        })?;
        assert_eq!(fs::metadata(s.lock_path())?.ino(), lock_inode);
        assert_ne!(fs::metadata(&s.config_path)?.ino(), config_inode);
        assert_eq!(s.stored_rights()?.sends_per_hour, 3);
        Ok(())
    }

    #[test]
    fn the_lock_path_follows_the_directory_of_the_config_file() {
        assert_eq!(
            settings_at("/etc/avery/other.json").lock_path(),
            "/etc/avery/config.lock"
        );
        assert_eq!(settings_at("config.json").lock_path(), "config.lock");
        assert_eq!(settings_at("/config.json").lock_path(), "/config.lock");
    }

    #[test]
    fn two_handles_never_hold_the_lock_at_once() -> TestResult {
        let dir = TempDir::new()?;
        let mut s = settings_at(&dir.path("config.json"));
        s.lock_wait = Duration::ZERO;
        let first = hold(&s)?;
        assert!(matches!(s.lock(), Err(UpdateError::Busy)));
        let mut other = settings_at(&dir.path("config.json"));
        other.lock_wait = Duration::ZERO;
        assert!(matches!(other.lock(), Err(UpdateError::Busy)));
        drop(first);
        let second = hold(&other)?;
        assert!(matches!(s.lock(), Err(UpdateError::Busy)));
        drop(second);
        drop(hold(&s)?);
        Ok(())
    }

    #[test]
    fn dropping_the_lock_releases_it_even_while_a_copy_of_the_descriptor_lives_on() -> TestResult {
        let dir = TempDir::new()?;
        let mut s = settings_at(&dir.path("config.json"));
        s.lock_wait = Duration::ZERO;
        let first = hold(&s)?;
        let inherited = first.file.try_clone()?;
        drop(first);
        drop(hold(&s)?);
        drop(inherited);
        Ok(())
    }

    #[test]
    fn an_update_waits_for_a_lock_that_is_released_in_time() -> TestResult {
        let dir = TempDir::new()?;
        let mut s = write_config(&dir, REMOTE)?;
        s.lock_wait = Duration::from_secs(10);
        let holder = s.clone();
        let (locked, ready) = mpsc::channel();
        let thread = thread::spawn(move || -> Result<(), String> {
            let held = hold(&holder)?;
            locked.send(()).map_err(|_| "send".to_string())?;
            thread::sleep(Duration::from_millis(250));
            drop(held);
            Ok(())
        });
        ready.recv_timeout(Duration::from_secs(5))?;
        let started = Instant::now();
        updated(&s, |rights| {
            rights.sends_per_hour = 2;
            Ok(())
        })?;
        assert!(started.elapsed() >= Duration::from_millis(100));
        thread.join().map_err(|_| "thread")??;
        assert_eq!(s.stored_rights()?.sends_per_hour, 2);
        Ok(())
    }

    #[test]
    fn an_update_gives_up_when_the_lock_stays_taken_and_changes_nothing() -> TestResult {
        let dir = TempDir::new()?;
        let mut s = write_config(&dir, REMOTE)?;
        s.lock_wait = Duration::from_millis(200);
        let held = hold(&s)?;
        let started = Instant::now();
        let mut ran = false;
        let outcome = s.update_rights(|rights| {
            ran = true;
            rights.sends_per_hour = 1;
            Ok(())
        });
        let waited = started.elapsed();
        assert_eq!(outcome, Err(UpdateError::Busy));
        assert!(!ran);
        assert!(
            waited >= Duration::from_millis(200) && waited < Duration::from_secs(5),
            "{waited:?}"
        );
        assert_eq!(fs::read_to_string(&s.config_path)?, REMOTE);
        drop(held);
        Ok(())
    }

    #[test]
    fn the_default_wait_for_the_lock_is_bounded() {
        let s = Settings::from_lookup(&lookup(&[("HOME", "/home/x")]));
        assert_eq!(s.lock_wait, Duration::from_secs(5));
    }

    #[test]
    fn racing_updates_lose_nothing_and_release_the_lock() -> TestResult {
        let dir = TempDir::new()?;
        let mut s = settings_at(&dir.path("config.json"));
        s.lock_wait = Duration::from_secs(30);
        let threads: Vec<_> = (0..12)
            .map(|n| {
                let own = s.clone();
                thread::spawn(move || -> Result<(), String> {
                    let fresh = account(Provider::Google, &format!("u{n}@gmail.example"))?;
                    updated(&own, |rights| rights.add_account(fresh))
                })
            })
            .collect();
        for handle in threads {
            handle.join().map_err(|_| "thread")??;
        }
        assert_eq!(s.stored_rights()?.accounts.len(), 12);
        s.lock_wait = Duration::ZERO;
        drop(hold(&s)?);
        Ok(())
    }

    #[test]
    fn a_lock_that_cannot_be_created_is_an_error_and_not_a_wait() -> TestResult {
        let dir = TempDir::new()?;
        let s = settings_at(&dir.path("config.json"));
        fs::create_dir(s.lock_path())?;
        let started = Instant::now();
        assert!(matches!(
            s.update_rights(|_| Ok(())),
            Err(UpdateError::Lock(_))
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
        fs::write(dir.path("file"), "x")?;
        let blocked = settings_at(&dir.path("file/config.json"));
        assert!(matches!(
            blocked.update_rights(|_| Ok(())),
            Err(UpdateError::Lock(_))
        ));
        Ok(())
    }

    #[test]
    fn a_failed_write_is_reported_and_releases_the_lock() -> TestResult {
        let dir = TempDir::new()?;
        let mut s = write_config(&dir, REMOTE)?;
        fs::create_dir(format!("{}.tmp", s.config_path))?;
        let outcome = s.update_rights(|rights| {
            rights.sends_per_hour = 3;
            Ok(())
        });
        assert!(matches!(outcome, Err(UpdateError::Write(_))), "{outcome:?}");
        assert_eq!(fs::read_to_string(&s.config_path)?, REMOTE);
        s.lock_wait = Duration::ZERO;
        drop(hold(&s)?);
        Ok(())
    }
}
