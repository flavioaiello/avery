//! config.json: where it lives, how it is read and written, the allowlist of
//! accounts and the write settings.
//!
//! Only accounts listed in config.json are ever exposed (fail closed). Writing is
//! off unless the human enabled it per account with `write-enable`; the model can
//! neither change this nor see the file.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

use crate::account::{self, Account, MailAccount};
use crate::error::{ToolError, ToolResult};
use crate::fake;
use crate::json::{self, Json, Object};
use crate::pycompat;
use crate::text::{self, clean};
use crate::texts::{DEFAULT_SENDS_PER_HOUR, NO_ACCOUNTS, WRITE_OFF};

/// Where the program finds its files and whether it runs on demo data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub config_path: String,
    /// `MAIL_MCP_FAKE=1`: synthetic accounts and messages, no Mail access.
    pub fake: bool,
    /// `MAIL_MCP_FAKE_WRITE`: comma separated ids of the demo accounts that may write.
    pub fake_write: String,
}

/// Accounts that may write (a subset of the readable ones) and the hourly send limit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteSettings {
    pub accounts: Vec<Account>,
    pub max_per_hour: i64,
}

impl Settings {
    pub fn from_env() -> Settings {
        Settings::from_lookup(&|key| {
            std::env::var_os(key).map(|value| value.to_string_lossy().into_owned())
        })
    }

    /// `MAIL_MCP_CONFIG`, or `$AVERY_HOME/mail/config.json`, default home `~/.avery`.
    pub fn from_lookup(lookup: &dyn Fn(&str) -> Option<String>) -> Settings {
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

    pub fn sendlog_path(&self) -> String {
        join_path(&dirname(&self.config_path), "sendlog.jsonl")
    }

    pub fn config_exists(&self) -> bool {
        Path::new(&self.config_path).exists()
    }

    /// Reads config.json. A missing file is an empty allowlist.
    pub fn load_config(&self) -> ToolResult<Object> {
        let unreadable = || {
            ToolError::msg(
                "config.json is unreadable. Ask the user to run `avery mail setup` again.",
            )
        };
        let bytes = match fs::read(&self.config_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let mut empty = Object::new();
                empty.insert("allowed_accounts", Json::Arr(Vec::new()));
                return Ok(empty);
            }
            Err(_) => return Err(unreadable()),
        };
        let text = String::from_utf8(bytes).map_err(|_| unreadable())?;
        let data = json::parse(&text).map_err(|_| unreadable())?;
        match data {
            Json::Obj(object)
                if object
                    .get("allowed_accounts")
                    .is_none_or(|value| matches!(value, Json::Arr(_))) =>
            {
                Ok(object)
            }
            _ => Err(ToolError::msg(
                "config.json has an unexpected format. Ask the user to run `avery mail setup` again.",
            )),
        }
    }

    /// Writes config.json atomically: the directory is private (0700), the file
    /// is private (0600) and replaced as a whole.
    pub fn save_config(&self, data: &Object) -> io::Result<()> {
        let directory = dirname(&self.config_path);
        make_dirs(&directory, 0o700)?;
        let temporary = format!("{}.tmp", self.config_path);
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(json::dumps_pretty(&Json::Obj(data.clone()), 2, false).as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, &self.config_path)
    }

    /// Accounts the agent may read. An empty list means nothing is exposed.
    pub fn allowed_accounts(&self) -> ToolResult<Vec<Account>> {
        let entries: Vec<MailAccount> = if self.fake {
            fake::accounts()
                .into_iter()
                .filter(|entry| entry.id != "FAKE-WORK")
                .collect()
        } else {
            let config = self.load_config()?;
            let mut entries = Vec::new();
            if let Some(Json::Arr(list)) = config.get("allowed_accounts") {
                for item in list {
                    let Some(object) = item.as_object() else {
                        continue;
                    };
                    if !json::truthy(object.get("id")) {
                        continue;
                    }
                    let id = pycompat::text_of(object.get("id"));
                    let name = match object.get("name") {
                        Some(name) if name.is_truthy() => pycompat::str_of(name),
                        _ => id.clone(),
                    };
                    let kind = match object.get("type") {
                        Some(kind) if kind.is_truthy() => pycompat::str_of(kind),
                        _ => String::new(),
                    };
                    entries.push(MailAccount {
                        id,
                        name,
                        kind,
                        addresses: account::string_list(object.get("addresses")),
                    });
                }
            }
            entries
        };
        Ok(account::with_labels(entries))
    }

    /// Accounts that may write and the send limit. Off unless enabled per account.
    pub fn write_settings(&self) -> ToolResult<WriteSettings> {
        let (ids, limit): (Vec<String>, i64) = if self.fake {
            let ids = self
                .fake_write
                .split(',')
                .filter(|piece| !piece.is_empty())
                .map(str::to_string)
                .collect();
            (ids, DEFAULT_SENDS_PER_HOUR)
        } else {
            let config = self.load_config()?;
            let section = match config.get("write") {
                Some(Json::Obj(section)) => Some(section),
                _ => None,
            };
            let ids = match section.and_then(|section| section.get("accounts")) {
                Some(Json::Arr(items)) => items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect(),
                _ => Vec::new(),
            };
            let limit = text::to_int(
                section.and_then(|section| section.get("max_per_hour")),
                DEFAULT_SENDS_PER_HOUR,
                1,
                30,
            )?;
            (ids, limit)
        };
        let accounts = self
            .allowed_accounts()?
            .into_iter()
            .filter(|account| ids.contains(&account.id))
            .collect();
        Ok(WriteSettings {
            accounts,
            max_per_hour: limit,
        })
    }

    /// The accounts a tool call refers to: all of them without a name, otherwise
    /// the ones the name matches (label, name or address; then part of a label).
    pub fn resolve(&self, account_name: Option<&Json>) -> ToolResult<Vec<Account>> {
        let accounts = self.allowed_accounts()?;
        if accounts.is_empty() {
            return Err(ToolError::msg(NO_ACCOUNTS));
        }
        if !json::truthy(account_name) {
            return Ok(accounts);
        }
        let wanted = pycompat::lower(pycompat::strip(&pycompat::text_of(account_name)));
        let mut hits: Vec<Account> = accounts
            .iter()
            .filter(|account| account.is_named(&wanted))
            .cloned()
            .collect();
        if hits.is_empty() {
            hits = accounts
                .iter()
                .filter(|account| pycompat::lower(&account.label).contains(&wanted))
                .cloned()
                .collect();
        }
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

    /// The one account a write tool works with.
    pub fn resolve_writable(
        &self,
        account_name: Option<&Json>,
    ) -> ToolResult<(Account, WriteSettings)> {
        let settings = self.write_settings()?;
        if settings.accounts.is_empty() {
            return Err(ToolError::msg(WRITE_OFF));
        }
        let labels: Vec<&str> = settings.accounts.iter().map(|a| a.label.as_str()).collect();
        let names = labels.join(", ");
        if !json::truthy(account_name) {
            return match settings.accounts.as_slice() {
                [only] => Ok((only.clone(), settings.clone())),
                _ => Err(ToolError::msg(format!(
                    "Parameter 'account' is required. Accounts that may write: {names}"
                ))),
            };
        }
        let wanted = pycompat::lower(pycompat::strip(&pycompat::text_of(account_name)));
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
}

/// `os.path.join(base, name)` for two path pieces.
pub fn join_path(base: &str, name: &str) -> String {
    if base.is_empty() {
        name.to_string()
    } else if base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

/// `os.path.dirname`.
pub fn dirname(path: &str) -> String {
    let end = path.rfind('/').map_or(0, |index| index + 1);
    let head = path.get(..end).unwrap_or("");
    if head.chars().all(|c| c == '/') {
        head.to_string()
    } else {
        head.trim_end_matches('/').to_string()
    }
}

/// `os.path.expanduser` for `~` and `~/...`. Other `~user` forms are left alone.
pub fn expanduser(path: &str, home: Option<&str>) -> String {
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

/// `os.makedirs(path, mode, exist_ok=True)`: parents get the default mode, only
/// the last directory gets `mode`.
pub fn make_dirs(path: &str, mode: u32) -> io::Result<()> {
    if path.is_empty() || Path::new(path).is_dir() {
        return Ok(());
    }
    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            fs::create_dir_all(parent)?;
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
pub mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::collections::HashMap;
    use std::os::unix::fs::PermissionsExt;

    pub fn settings_at(path: &str) -> Settings {
        Settings {
            config_path: path.to_string(),
            fake: false,
            fake_write: String::new(),
        }
    }

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key| map.get(key).cloned()
    }

    fn j(text: &str) -> Json {
        json::parse(text).expect("valid json")
    }

    fn write_config(dir: &TempDir, text: &str) -> Settings {
        let path = dir.path("config.json");
        fs::write(&path, text).expect("write config");
        settings_at(&path)
    }

    const TWO_ACCOUNTS: &str = r#"{"version": 2, "allowed_accounts": [
        {"id": "ACC-1", "name": "Privat Gmail", "type": "imap", "addresses": ["me@gmail.example"]},
        {"id": "ACC-2", "name": "Outlook", "type": "exchange", "addresses": ["me@outlook.example", "alias@outlook.example"]}]}"#;

    #[test]
    fn paths_follow_the_environment() {
        let s = Settings::from_lookup(&lookup(&[("HOME", "/Users/x")]));
        assert_eq!(s.config_path, "/Users/x/.avery/mail/config.json");
        assert_eq!(s.sendlog_path(), "/Users/x/.avery/mail/sendlog.jsonl");
        assert!(!s.fake);
        let s = Settings::from_lookup(&lookup(&[("HOME", "/Users/x"), ("AVERY_HOME", "/opt/av/")]));
        assert_eq!(s.config_path, "/opt/av/mail/config.json");
        let s = Settings::from_lookup(&lookup(&[("HOME", "/Users/x"), ("AVERY_HOME", "~/own")]));
        assert_eq!(s.config_path, "/Users/x/own/mail/config.json");
        let s = Settings::from_lookup(&lookup(&[
            ("HOME", "/Users/x"),
            ("AVERY_HOME", "/opt/av"),
            ("MAIL_MCP_CONFIG", "/etc/c.json"),
        ]));
        assert_eq!(s.config_path, "/etc/c.json");
        assert_eq!(s.sendlog_path(), "/etc/sendlog.jsonl");
        let s = Settings::from_lookup(&lookup(&[
            ("HOME", "/Users/x"),
            ("MAIL_MCP_CONFIG", ""),
            ("AVERY_HOME", ""),
        ]));
        assert_eq!(s.config_path, "/Users/x/.avery/mail/config.json");
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
    fn expanduser_and_dirname_follow_python() {
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
    fn a_missing_config_is_an_empty_allowlist() {
        let dir = TempDir::new();
        let s = settings_at(&dir.path("none.json"));
        assert_eq!(s.allowed_accounts(), Ok(vec![]));
        assert_eq!(s.resolve(None), Err(ToolError::msg(NO_ACCOUNTS)));
    }

    #[test]
    fn unreadable_and_malformed_configs_are_refused() {
        let dir = TempDir::new();
        for text in ["", "{", "nope", "\u{feff}{}", "{\"allowed_accounts\": NaN}"] {
            let s = write_config(&dir, text);
            match s.load_config() {
                Err(ToolError::Msg(message)) => assert!(message.contains("unreadable"), "{text:?}"),
                other => panic!("{text:?}: {other:?}"),
            }
        }
        for text in [
            "[]",
            "\"x\"",
            "5",
            "{\"allowed_accounts\": 5}",
            "{\"allowed_accounts\": null}",
            "{\"allowed_accounts\": {}}",
        ] {
            let s = write_config(&dir, text);
            match s.load_config() {
                Err(ToolError::Msg(message)) => {
                    assert!(message.contains("unexpected format"), "{text:?}")
                }
                other => panic!("{text:?}: {other:?}"),
            }
        }
        fs::write(dir.path("bytes.json"), [0xff, 0xfe, 0x7b, 0x7d]).expect("write");
        let s = settings_at(&dir.path("bytes.json"));
        assert!(matches!(s.load_config(), Err(ToolError::Msg(m)) if m.contains("unreadable")));
        let s = settings_at(&dir.path("")); // a directory
        assert!(matches!(s.load_config(), Err(ToolError::Msg(m)) if m.contains("unreadable")));
    }

    #[test]
    fn a_config_without_allowed_accounts_is_accepted_and_empty() {
        let dir = TempDir::new();
        let s = write_config(&dir, "{\"version\": 2}");
        assert_eq!(s.allowed_accounts(), Ok(vec![]));
    }

    #[test]
    fn allowed_accounts_are_read_with_labels() {
        let dir = TempDir::new();
        let s = write_config(&dir, TWO_ACCOUNTS);
        let accounts = s.allowed_accounts().expect("accounts");
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[0].label, "Privat Gmail");
        assert_eq!(
            accounts[1].addresses,
            ["me@outlook.example", "alias@outlook.example"]
        );
        assert_eq!(accounts[1].kind, "exchange");
    }

    #[test]
    fn odd_entries_are_skipped_or_converted_like_python() {
        let dir = TempDir::new();
        let s = write_config(
            &dir,
            r#"{"allowed_accounts": [5, "x", {"name": "no id"}, {"id": ""}, {"id": 7},
                {"id": "A", "name": null, "type": null, "addresses": "abc"}]}"#,
        );
        let accounts = s.allowed_accounts().expect("accounts");
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[0].id, "7");
        assert_eq!(accounts[0].name, "7");
        assert_eq!(accounts[1].name, "A");
        assert_eq!(accounts[1].kind, "");
        assert!(accounts[1].addresses.is_empty());
    }

    #[test]
    fn demo_mode_never_exposes_the_company_account() {
        let mut s = settings_at("/nonexistent/config.json");
        s.fake = true;
        let ids: Vec<String> = s
            .allowed_accounts()
            .expect("accounts")
            .into_iter()
            .map(|a| a.id)
            .collect();
        assert_eq!(ids, ["FAKE-GMAIL", "FAKE-OUTLOOK"]);
    }

    #[test]
    fn resolve_finds_accounts_by_label_name_address_or_part_of_a_label() {
        let dir = TempDir::new();
        let s = write_config(&dir, TWO_ACCOUNTS);
        let names = |value: &str| -> Vec<String> {
            s.resolve(Some(&j(value)))
                .expect("hits")
                .into_iter()
                .map(|a| a.id)
                .collect()
        };
        assert_eq!(names("\"privat gmail\""), ["ACC-1"]);
        assert_eq!(names("\" OUTLOOK \""), ["ACC-2"]);
        assert_eq!(names("\"alias@outlook.example\""), ["ACC-2"]);
        assert_eq!(names("\"gmail\""), ["ACC-1"]);
        assert_eq!(names("\"   \""), ["ACC-1", "ACC-2"]);
        assert_eq!(s.resolve(None).expect("all").len(), 2);
        assert_eq!(s.resolve(Some(&j("\"\""))).expect("all").len(), 2);
        assert_eq!(s.resolve(Some(&j("0"))).expect("all").len(), 2);
        match s.resolve(Some(&j("\"Firma\""))) {
            Err(ToolError::Msg(message)) => {
                assert_eq!(
                    message,
                    "Unknown account 'Firma'. Allowed accounts: Privat Gmail, Outlook"
                );
            }
            other => panic!("{other:?}"),
        }
        match s.resolve(Some(&j("[\"a\"]"))) {
            Err(ToolError::Msg(message)) => {
                assert!(message.starts_with("Unknown account '['a']'."))
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn writing_is_off_unless_enabled_for_a_readable_account() {
        let dir = TempDir::new();
        let off = write_config(&dir, TWO_ACCOUNTS);
        assert_eq!(off.write_settings().expect("settings").accounts, vec![]);
        assert_eq!(off.resolve_writable(None), Err(ToolError::msg(WRITE_OFF)));

        for text in [
            // write account outside the read allowlist
            r#"{"allowed_accounts": [{"id": "ACC-1", "name": "A"}], "write": {"accounts": ["ACC-OTHER"]}}"#,
            // nothing readable at all
            r#"{"allowed_accounts": [], "write": {"accounts": ["ACC-1"]}}"#,
            // broken write section
            r#"{"allowed_accounts": [{"id": "ACC-1", "name": "A"}], "write": "kaputt"}"#,
            r#"{"allowed_accounts": [{"id": "ACC-1", "name": "A"}], "write": {"accounts": "ACC-1"}}"#,
            r#"{"allowed_accounts": [{"id": "ACC-1", "name": "A"}], "write": {"accounts": [1, null]}}"#,
        ] {
            let s = write_config(&dir, text);
            assert_eq!(
                s.write_settings().expect("settings").accounts,
                vec![],
                "{text}"
            );
        }
    }

    #[test]
    fn write_settings_carry_the_hourly_limit() {
        let dir = TempDir::new();
        let s = write_config(
            &dir,
            r#"{"allowed_accounts": [{"id": "A", "name": "A"}], "write": {"accounts": ["A"], "max_per_hour": 3}}"#,
        );
        let settings = s.write_settings().expect("settings");
        assert_eq!(settings.accounts.len(), 1);
        assert_eq!(settings.max_per_hour, 3);
        for (value, expected) in [("100", 30), ("0", 1), ("\"7\"", 7), ("null", 5)] {
            let s = write_config(
                &dir,
                &format!(
                    r#"{{"allowed_accounts": [{{"id": "A", "name": "A"}}], "write": {{"accounts": ["A"], "max_per_hour": {value}}}}}"#
                ),
            );
            assert_eq!(
                s.write_settings().expect("settings").max_per_hour,
                expected,
                "{value}"
            );
        }
        let s = write_config(
            &dir,
            r#"{"allowed_accounts": [{"id": "A", "name": "A"}], "write": {"accounts": ["A"], "max_per_hour": true}}"#,
        );
        assert_eq!(
            s.write_settings(),
            Err(ToolError::msg("Invalid number: True"))
        );
    }

    #[test]
    fn demo_write_accounts_come_from_the_environment_variable() {
        let mut s = settings_at("/nonexistent/config.json");
        s.fake = true;
        s.fake_write = "FAKE-GMAIL,,FAKE-WORK,FAKE-OUTLOOK".into();
        let settings = s.write_settings().expect("settings");
        let ids: Vec<&str> = settings.accounts.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["FAKE-GMAIL", "FAKE-OUTLOOK"]);
        assert_eq!(settings.max_per_hour, 5);
    }

    #[test]
    fn resolve_writable_follows_the_python_rules() {
        let mut s = settings_at("/nonexistent/config.json");
        s.fake = true;
        s.fake_write = "FAKE-GMAIL".into();
        let (only, _) = s
            .resolve_writable(None)
            .expect("single account is the default");
        assert_eq!(only.id, "FAKE-GMAIL");
        let (by_address, _) = s
            .resolve_writable(Some(&j("\"demo.privat@gmail.example\"")))
            .expect("by address");
        assert_eq!(by_address.id, "FAKE-GMAIL");
        assert_eq!(
            s.resolve_writable(Some(&j("\"Demo Outlook\""))),
            Err(ToolError::msg(
                "Unknown account 'Demo Outlook'. Accounts that may write: Demo Gmail"
            ))
        );
        assert_eq!(
            s.resolve_writable(Some(&j("\"gmail\""))),
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
    }

    #[test]
    fn save_config_writes_python_style_json_with_private_permissions() {
        let dir = TempDir::new();
        let s = settings_at(&dir.path("sub/dir/config.json"));
        let mut config = Object::new();
        config.insert("version", 2i64);
        config.insert("allowed_accounts", Json::Arr(vec![]));
        s.save_config(&config).expect("save");
        let text = fs::read_to_string(dir.path("sub/dir/config.json")).expect("read");
        assert_eq!(text, "{\n  \"version\": 2,\n  \"allowed_accounts\": []\n}");
        let file_mode = fs::metadata(dir.path("sub/dir/config.json"))
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        let dir_mode = fs::metadata(dir.path("sub/dir"))
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600);
        assert_eq!(dir_mode, 0o700);
        assert!(!Path::new(&dir.path("sub/dir/config.json.tmp")).exists());
        assert_eq!(s.load_config().expect("load"), config);
    }

    #[test]
    fn make_dirs_gives_only_the_last_directory_the_private_mode() {
        let dir = TempDir::new();
        make_dirs(&dir.path("p/q/r"), 0o700).expect("make");
        let mode = |sub: &str| {
            fs::metadata(dir.path(sub))
                .expect("meta")
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode("p/q/r"), 0o700);
        assert_ne!(mode("p"), 0o700);
        make_dirs(&dir.path("p/q/r"), 0o700).expect("existing directory is fine");
        fs::write(dir.path("file"), "x").expect("write");
        assert!(make_dirs(&dir.path("file"), 0o700).is_err());
    }
}
