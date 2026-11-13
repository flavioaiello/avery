use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

use crate::account::{self, Account};
use crate::calendar::{self, Calendar};
use crate::error::{ToolError, ToolResult};
use crate::fake;
use crate::json::{self, Json, Object};
use crate::rights::{Level, Rights, CONFIG_VERSION};
use crate::text::{clean, text_of};
use crate::texts::{
    CALENDAR_WRITE_OFF, DEFAULT_CHANGES_PER_HOUR, DEFAULT_SENDS_PER_HOUR, NO_ACCOUNTS,
    NO_CALENDARS, WRITE_OFF,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Settings {
    pub(crate) config_path: String,
    pub(crate) fake: bool,
    pub(crate) fake_write: String,
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
        }
    }

    pub(crate) fn sendlog_path(&self) -> String {
        join_path(&dirname(&self.config_path), "sendlog.jsonl")
    }

    pub(crate) fn changes_path(&self) -> String {
        join_path(&dirname(&self.config_path), "changes.jsonl")
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
        let bytes = match fs::read(&self.config_path) {
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

    pub(crate) fn save_config(&self, data: &Object) -> io::Result<()> {
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

    pub(crate) fn save_rights(&self, rights: &Rights) -> io::Result<()> {
        self.save_config(&rights.to_config())
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
    use crate::testutil::{j, settings_at, TempDir, TestResult};
    use std::collections::HashMap;
    use std::os::unix::fs::PermissionsExt;

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

    const TWO_ACCOUNTS: &str = r#"{"version": 3, "mail": {"accounts": [
        {"id": "ACC-1", "name": "Privat Gmail", "type": "imap", "addresses": ["me@gmail.example"], "level": "read"},
        {"id": "ACC-2", "name": "Outlook", "type": "exchange", "addresses": ["me@outlook.example", "alias@outlook.example"], "level": "read"}]}}"#;

    const CALENDARS: &str = r#"{"version": 3, "calendar": {"calendars": [
        {"id": "CAL-1", "name": "Privat", "level": "write"},
        {"id": "CAL-2", "name": "Familie", "level": "read"}], "changes_per_hour": 4}}"#;

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
        Ok(())
    }

    #[test]
    fn a_config_with_unexpected_content_is_invalid_as_a_whole() -> TestResult {
        let dir = TempDir::new()?;
        for text in [
            r#"{"version": 3, "allowed_accounts": []}"#,
            r#"{"version": 3, "mail": {"accounts": [{"id": 7, "level": "read"}]}}"#,
            r#"{"version": 3, "mail": {"accounts": [{"id": "A", "level": "admin"}]}}"#,
            r#"{"version": 3, "mail": {"sends_per_hour": 0}}"#,
            r#"{"version": 3, "calendar": {"calendars": [{"id": "C"}]}}"#,
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
            r#"{"version": 3, "mail": {"accounts": [
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
                r#"{{"version": 3, "mail": {{"accounts": [{{"id": "A", "name": "A", "level": "write"}}], {limit}}}}}"#
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
            r#"{"version": 3, "mail": {"accounts": [{"id": "A", "level": "write"}]}}"#,
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
            r#"{"version": 3, "calendar": {"calendars": [{"id": "C", "name": "N", "level": "read"}]}}"#,
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
        rights.mail.push((
            crate::account::MailAccount {
                id: "A".into(),
                name: "N".into(),
                kind: "imap".into(),
                addresses: vec!["a@example.org".into()],
            },
            Level::Write,
        ));
        rights.calendars.push((
            crate::calendar::CalendarEntry {
                id: "C".into(),
                name: "K".into(),
            },
            Level::Read,
        ));
        s.save_rights(&rights)?;
        assert_eq!(s.rights()?, rights);
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
}
