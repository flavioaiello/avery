use crate::account::{string_list, MailAccount};
use crate::calendar::CalendarEntry;
use crate::json::{Json, Object};
use crate::text::one_line_str;
use crate::texts::{DEFAULT_CHANGES_PER_HOUR, DEFAULT_SENDS_PER_HOUR, MAX_LIMIT_PER_HOUR};

pub(crate) const CONFIG_VERSION: i64 = 4;
pub(crate) const MAX_ENTRIES: usize = 200;
pub(crate) const MAX_NAME: usize = 120;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Level {
    Read,
    Write,
}

impl Level {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Level::Read => "read",
            Level::Write => "write",
        }
    }

    fn parse(value: Option<&Json>, place: &str) -> Result<Level, String> {
        match value.and_then(Json::as_str) {
            Some("read") => Ok(Level::Read),
            Some("write") => Ok(Level::Write),
            _ => Err(format!("{place} must be \"read\" or \"write\"")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Changed;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Rights {
    pub(crate) mail: Vec<(MailAccount, Level)>,
    pub(crate) calendars: Vec<(CalendarEntry, Level)>,
    pub(crate) sends_per_hour: i64,
    pub(crate) changes_per_hour: i64,
}

fn object_of<'a>(value: &'a Json, place: &str) -> Result<&'a Object, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{place} must be an object"))
}

fn only_keys(object: &Object, allowed: &[&str], place: &str) -> Result<(), String> {
    match object.keys().find(|key| !allowed.contains(key)) {
        Some(key) => Err(format!(
            "{place} has the unknown key \"{}\"",
            printable(key)
        )),
        None => Ok(()),
    }
}

fn printable(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_ascii_graphic())
        .take(30)
        .collect()
}

fn id_of(object: &Object, place: &str) -> Result<String, String> {
    match object.get("id").and_then(Json::as_str) {
        Some(id) if !id.is_empty() => Ok(id.to_string()),
        _ => Err(format!("{place}.id must be a non-empty text")),
    }
}

fn optional_text(object: &Object, key: &str, place: &str) -> Result<String, String> {
    match object.get(key) {
        None => Ok(String::new()),
        Some(Json::Str(text)) if *text == one_line_str(text, MAX_NAME) => Ok(text.clone()),
        Some(Json::Str(_)) => Err(format!(
            "{place}.{key} must be one line of visible text up to {MAX_NAME} characters"
        )),
        Some(_) => Err(format!("{place}.{key} must be text")),
    }
}

fn limit_of(config: &Object, key: &str, default: i64) -> Result<i64, String> {
    match config.get(key) {
        None => Ok(default),
        Some(Json::Int(value)) if (1..=MAX_LIMIT_PER_HOUR).contains(value) => Ok(*value),
        Some(_) => Err(format!(
            "{key} must be a whole number from 1 to {MAX_LIMIT_PER_HOUR}"
        )),
    }
}

fn list_of<'a>(config: &'a Object, key: &str) -> Result<&'a [Json], String> {
    match config.get(key) {
        None => Ok(&[]),
        Some(Json::Arr(items)) if items.len() <= MAX_ENTRIES => Ok(items.as_slice()),
        Some(Json::Arr(_)) => Err(format!("{key} has more than {MAX_ENTRIES} entries")),
        Some(_) => Err(format!("{key} must be a list")),
    }
}

pub(crate) fn name_or_id(name: String, id: &str) -> String {
    if name.is_empty() {
        one_line_str(id, MAX_NAME)
    } else {
        name
    }
}

fn parse_name(entry: &Object, id: &str, place: &str) -> Result<String, String> {
    Ok(name_or_id(optional_text(entry, "name", place)?, id))
}

fn parse_mail(config: &Object) -> Result<Vec<(MailAccount, Level)>, String> {
    let mut found: Vec<(MailAccount, Level)> = Vec::new();
    for (index, item) in list_of(config, "mail")?.iter().enumerate() {
        let place = format!("mail[{index}]");
        let entry = object_of(item, &place)?;
        only_keys(entry, &["id", "name", "type", "addresses", "level"], &place)?;
        let id = id_of(entry, &place)?;
        if found.iter().any(|(known, _)| known.id == id) {
            return Err(format!("{place}.id appears twice"));
        }
        if let Some(addresses) = entry.get("addresses") {
            let valid = matches!(addresses, Json::Arr(items) if items.iter().all(|item| item.as_str().is_some_and(|text| text == one_line_str(text, MAX_NAME))));
            if !valid {
                return Err(format!(
                    "{place}.addresses must be a list of one-line texts"
                ));
            }
        }
        let account = MailAccount {
            name: parse_name(entry, &id, &place)?,
            id,
            kind: optional_text(entry, "type", &place)?,
            addresses: string_list(entry.get("addresses")),
        };
        found.push((
            account,
            Level::parse(entry.get("level"), &format!("{place}.level"))?,
        ));
    }
    Ok(found)
}

fn parse_calendars(config: &Object) -> Result<Vec<(CalendarEntry, Level)>, String> {
    let mut found: Vec<(CalendarEntry, Level)> = Vec::new();
    for (index, item) in list_of(config, "calendars")?.iter().enumerate() {
        let place = format!("calendars[{index}]");
        let entry = object_of(item, &place)?;
        only_keys(entry, &["id", "name", "level"], &place)?;
        let id = id_of(entry, &place)?;
        if found.iter().any(|(known, _)| known.id == id) {
            return Err(format!("{place}.id appears twice"));
        }
        found.push((
            CalendarEntry {
                name: parse_name(entry, &id, &place)?,
                id,
            },
            Level::parse(entry.get("level"), &format!("{place}.level"))?,
        ));
    }
    Ok(found)
}

fn mail_json(entries: &[(MailAccount, Level)]) -> Json {
    Json::Arr(
        entries
            .iter()
            .map(|(account, level)| {
                let mut entry = Object::new();
                entry.insert("id", account.id.as_str());
                entry.insert("name", account.name.as_str());
                entry.insert("type", account.kind.as_str());
                entry.insert("addresses", account.addresses.as_slice());
                entry.insert("level", level.as_str());
                Json::Obj(entry)
            })
            .collect(),
    )
}

fn calendars_json(entries: &[(CalendarEntry, Level)]) -> Json {
    Json::Arr(
        entries
            .iter()
            .map(|(calendar, level)| {
                let mut entry = Object::new();
                entry.insert("id", calendar.id.as_str());
                entry.insert("name", calendar.name.as_str());
                entry.insert("level", level.as_str());
                Json::Obj(entry)
            })
            .collect(),
    )
}

impl Rights {
    #[cfg(test)]
    pub(crate) fn empty() -> Rights {
        Rights {
            mail: Vec::new(),
            calendars: Vec::new(),
            sends_per_hour: DEFAULT_SENDS_PER_HOUR,
            changes_per_hour: DEFAULT_CHANGES_PER_HOUR,
        }
    }

    pub(crate) fn parse(config: &Object) -> Result<Rights, String> {
        if config.get("version") != Some(&Json::Int(CONFIG_VERSION)) {
            return Err(format!("the format version must be {CONFIG_VERSION}"));
        }
        only_keys(
            config,
            &[
                "version",
                "mail",
                "calendars",
                "sends_per_hour",
                "changes_per_hour",
            ],
            "config",
        )?;
        Ok(Rights {
            mail: parse_mail(config)?,
            calendars: parse_calendars(config)?,
            sends_per_hour: limit_of(config, "sends_per_hour", DEFAULT_SENDS_PER_HOUR)?,
            changes_per_hour: limit_of(config, "changes_per_hour", DEFAULT_CHANGES_PER_HOUR)?,
        })
    }

    pub(crate) fn to_config(&self) -> Object {
        let mut config = Object::new();
        config.insert("version", Json::Int(CONFIG_VERSION));
        if !self.mail.is_empty() {
            config.insert("mail", mail_json(&self.mail));
        }
        if !self.calendars.is_empty() {
            config.insert("calendars", calendars_json(&self.calendars));
        }
        config.insert("sends_per_hour", Json::Int(self.sends_per_hour));
        config.insert("changes_per_hour", Json::Int(self.changes_per_hour));
        config
    }

    pub(crate) fn enabled(&self) -> bool {
        !self.mail.is_empty() || !self.calendars.is_empty()
    }

    pub(crate) fn apply(&mut self, before: &Rights, after: &Rights) -> Result<(), Changed> {
        if after.mail != before.mail || after.calendars != before.calendars {
            if self.mail != before.mail || self.calendars != before.calendars {
                return Err(Changed);
            }
            self.mail = after.mail.clone();
            self.calendars = after.calendars.clone();
        }
        Ok(())
    }

    pub(crate) fn revoke(&mut self) {
        self.mail.clear();
        self.calendars.clear();
    }

    pub(crate) fn mail_at(&self, least: Level) -> Vec<MailAccount> {
        self.mail
            .iter()
            .filter(|(_, level)| *level >= least)
            .map(|(account, _)| account.clone())
            .collect()
    }

    pub(crate) fn calendars_at(&self, least: Level) -> Vec<CalendarEntry> {
        self.calendars
            .iter()
            .filter(|(_, level)| *level >= least)
            .map(|(calendar, _)| calendar.clone())
            .collect()
    }

    pub(crate) fn mail_level(&self, id: &str) -> Option<Level> {
        self.mail
            .iter()
            .find(|(account, _)| account.id == id)
            .map(|(_, level)| *level)
    }

    pub(crate) fn calendar_level(&self, id: &str) -> Option<Level> {
        self.calendars
            .iter()
            .find(|(calendar, _)| calendar.id == id)
            .map(|(_, level)| *level)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{args, TestResult};

    const FULL: &str = r#"{"version": 4,
        "mail": [
            {"id": "ACC-1", "name": "Privat", "type": "imap", "addresses": ["me@example.org"], "level": "write"},
            {"id": "ACC-2", "name": "Arbeit", "level": "read"}],
        "calendars": [
            {"id": "CAL-1", "name": "Privat", "level": "write"},
            {"id": "CAL-2", "level": "read"}],
        "sends_per_hour": 3, "changes_per_hour": 7}"#;

    fn parsed(text: &str) -> Result<Rights, String> {
        Rights::parse(&args(text)?)
    }

    fn rejected(text: &str, place: &str) {
        let outcome = parsed(text);
        assert!(
            matches!(&outcome, Err(message) if message.contains(place)),
            "{text}: {outcome:?}"
        );
    }

    #[test]
    fn a_full_file_is_read_with_mail_calendars_and_limits() -> TestResult {
        let rights = parsed(FULL)?;
        assert_eq!(rights.mail.len(), 2);
        assert_eq!(rights.mail[0].0.addresses, ["me@example.org"]);
        assert_eq!(rights.mail[0].0.kind, "imap");
        assert_eq!(rights.mail[1].0.name, "Arbeit");
        assert_eq!(rights.mail[1].0.kind, "");
        assert_eq!(rights.calendars[1].0.name, "CAL-2");
        assert!(rights.enabled());
        assert_eq!((rights.sends_per_hour, rights.changes_per_hour), (3, 7));
        Ok(())
    }

    #[test]
    fn an_empty_file_grants_nothing_and_uses_the_default_limits() -> TestResult {
        let rights = parsed(r#"{"version": 4}"#)?;
        assert_eq!(rights, Rights::empty());
        assert!(rights.mail_at(Level::Read).is_empty());
        assert!(rights.calendars_at(Level::Read).is_empty());
        assert!(!rights.enabled());
        assert_eq!(rights.sends_per_hour, DEFAULT_SENDS_PER_HOUR);
        assert_eq!(rights.changes_per_hour, DEFAULT_CHANGES_PER_HOUR);
        Ok(())
    }

    #[test]
    fn rights_round_trip_through_the_config_object() -> TestResult {
        let rights = parsed(FULL)?;
        assert_eq!(Rights::parse(&rights.to_config())?, rights);
        let empty = Rights::empty().to_config();
        let keys: Vec<&str> = empty.keys().collect();
        assert_eq!(keys, ["version", "sends_per_hour", "changes_per_hour"]);
        assert_eq!(Rights::parse(&empty)?, Rights::empty());
        let mut custom = Rights::empty();
        custom.sends_per_hour = 9;
        assert_eq!(Rights::parse(&custom.to_config())?, custom);
        let config = rights.to_config();
        let keys: Vec<&str> = config.keys().collect();
        assert_eq!(
            keys,
            [
                "version",
                "mail",
                "calendars",
                "sends_per_hour",
                "changes_per_hour"
            ]
        );
        let mail_only = parsed(r#"{"version": 4, "mail": [{"id": "A", "level": "read"}]}"#)?;
        let keys: Vec<String> = mail_only.to_config().keys().map(str::to_string).collect();
        assert_eq!(
            keys,
            ["version", "mail", "sends_per_hour", "changes_per_hour"]
        );
        Ok(())
    }

    #[test]
    fn the_views_use_the_ids_and_levels() -> TestResult {
        let rights = parsed(FULL)?;
        let mail: Vec<String> = rights
            .mail_at(Level::Read)
            .into_iter()
            .map(|account| account.id)
            .collect();
        assert_eq!(mail, ["ACC-1", "ACC-2"]);
        let writing: Vec<String> = rights
            .mail_at(Level::Write)
            .into_iter()
            .map(|account| account.id)
            .collect();
        assert_eq!(writing, ["ACC-1"]);
        let calendars: Vec<String> = rights
            .calendars_at(Level::Read)
            .into_iter()
            .map(|calendar| calendar.id)
            .collect();
        assert_eq!(calendars, ["CAL-1", "CAL-2"]);
        assert_eq!(rights.calendars_at(Level::Write).len(), 1);
        assert!(Level::Read < Level::Write);
        assert_eq!(rights.mail_level("ACC-1"), Some(Level::Write));
        assert_eq!(rights.mail_level("ACC-2"), Some(Level::Read));
        assert_eq!(rights.mail_level("nope"), None);
        assert_eq!(rights.mail_level("CAL-1"), None);
        assert_eq!(rights.calendar_level("CAL-1"), Some(Level::Write));
        assert_eq!(rights.calendar_level("CAL-2"), Some(Level::Read));
        assert_eq!(rights.calendar_level("ACC-1"), None);
        Ok(())
    }

    #[test]
    fn rights_are_enabled_exactly_when_mail_or_calendars_has_an_entry() -> TestResult {
        let off = parsed(r#"{"version": 4, "mail": [], "calendars": []}"#)?;
        assert!(!off.enabled());
        assert!(off.to_config().get("mail").is_none());
        assert!(off.to_config().get("calendars").is_none());
        let calendars_only =
            parsed(r#"{"version": 4, "calendars": [{"id": "C", "level": "read"}]}"#)?;
        assert!(calendars_only.enabled());
        assert!(calendars_only.to_config().get("calendars").is_some());
        let mail_only = parsed(r#"{"version": 4, "mail": [{"id": "A", "level": "read"}]}"#)?;
        assert!(mail_only.enabled());
        assert!(!Rights::empty().enabled());
        Ok(())
    }

    #[test]
    fn revoking_clears_the_grants_and_keeps_the_limits() -> TestResult {
        let mut rights = parsed(FULL)?;
        rights.revoke();
        assert!(!rights.enabled());
        assert!(rights.mail_at(Level::Read).is_empty());
        assert!(rights.calendars_at(Level::Read).is_empty());
        assert_eq!((rights.sends_per_hour, rights.changes_per_hour), (3, 7));
        assert_eq!(Rights::parse(&rights.to_config())?, rights);
        Ok(())
    }

    #[test]
    fn names_and_addresses_must_be_visible_single_lines() {
        for text in [
            r#"{"version": 4, "mail": [{"id": "A", "name": "Privat\u200b", "level": "read"}]}"#,
            r#"{"version": 4, "mail": [{"id": "A", "name": "Pri\nvat", "level": "read"}]}"#,
            r#"{"version": 4, "mail": [{"id": "A", "name": "Privat\u202e", "level": "read"}]}"#,
            r#"{"version": 4, "mail": [{"id": "A", "addresses": ["a@x.example\u200b"], "level": "read"}]}"#,
            r#"{"version": 4, "calendars": [{"id": "C", "name": "Feiertage\u{e0041}", "level": "read"}]}"#,
            r#"{"version": 4, "calendars": [{"id": "C", "name": "Fa\nmilie", "level": "read"}]}"#,
        ] {
            assert!(parsed(text).is_err(), "{text}");
        }
        assert!(parsed(
            r#"{"version": 4, "mail": [{"id": "A", "name": "Privat Gmail", "addresses": ["a@x.example"], "level": "read"}]}"#
        )
        .is_ok());
    }

    #[test]
    fn malformed_entries_are_rejected_with_their_place() {
        for (text, place) in [
            (r#"{"version": 4, "mail": {}}"#, "mail must be a list"),
            (
                r#"{"version": 4, "mail": [5]}"#,
                "mail[0] must be an object",
            ),
            (
                r#"{"version": 4, "mail": [{"level": "read"}]}"#,
                "mail[0].id",
            ),
            (
                r#"{"version": 4, "mail": [{"id": "", "level": "read"}]}"#,
                "mail[0].id",
            ),
            (
                r#"{"version": 4, "mail": [{"id": 7, "level": "read"}]}"#,
                "mail[0].id",
            ),
            (
                r#"{"version": 4, "mail": [{"id": "A"}]}"#,
                "mail[0].level must be",
            ),
            (
                r#"{"version": 4, "mail": [{"id": "A", "level": "off"}]}"#,
                "mail[0].level must be",
            ),
            (
                r#"{"version": 4, "mail": [{"id": "A", "level": "WRITE"}]}"#,
                "mail[0].level must be",
            ),
            (
                r#"{"version": 4, "mail": [{"id": "A", "level": "read", "x": 1}]}"#,
                "mail[0] has the unknown key",
            ),
            (
                r#"{"version": 4, "mail": [{"id": "A", "level": "read", "name": 5}]}"#,
                "mail[0].name must be text",
            ),
            (
                r#"{"version": 4, "mail": [{"id": "A", "level": "read", "type": 5}]}"#,
                "mail[0].type must be text",
            ),
            (
                r#"{"version": 4, "mail": [{"id": "A", "level": "read", "addresses": [5]}]}"#,
                "mail[0].addresses",
            ),
            (
                r#"{"version": 4, "mail": [{"id": "A", "level": "read", "addresses": "a@example.org"}]}"#,
                "mail[0].addresses",
            ),
            (
                r#"{"version": 4, "mail": [{"id": "A", "level": "read"}, {"id": "A", "level": "write"}]}"#,
                "mail[1].id appears twice",
            ),
            (
                r#"{"version": 4, "calendars": "x"}"#,
                "calendars must be a list",
            ),
            (
                r#"{"version": 4, "calendars": [{"id": "C", "level": "read", "addresses": []}]}"#,
                "calendars[0] has the unknown key",
            ),
            (
                r#"{"version": 4, "calendars": [{"id": "C", "level": "none"}]}"#,
                "calendars[0].level must be",
            ),
            (
                r#"{"version": 4, "calendars": [{"id": "C", "level": "read"}, {"id": "C", "level": "read"}]}"#,
                "calendars[1].id appears twice",
            ),
        ] {
            rejected(text, place);
        }
    }

    #[test]
    fn versions_unknown_keys_and_limits_are_rejected() {
        for (text, place) in [
            (r#"{"version": 2}"#, "format version"),
            (r#"{"version": 3}"#, "format version"),
            (
                r#"{"version": 3, "apple": {"mail": [{"id": "A", "level": "read"}]}}"#,
                "format version",
            ),
            (r#"{"version": 5}"#, "format version"),
            (r#"{}"#, "format version"),
            (r#"{"version": "4"}"#, "format version"),
            (
                r#"{"version": 4, "allowed_accounts": []}"#,
                "allowed_accounts",
            ),
            (
                r#"{"version": 4, "apple": {"mail": []}}"#,
                "config has the unknown key \"apple\"",
            ),
            (
                r#"{"version": 4, "accounts": []}"#,
                "config has the unknown key \"accounts\"",
            ),
            (
                r#"{"version": 4, "calendar": {"calendars": []}}"#,
                "config has the unknown key \"calendar\"",
            ),
            (
                r#"{"version": 4, "sends_per_hour": 0}"#,
                "sends_per_hour must be",
            ),
            (
                r#"{"version": 4, "sends_per_hour": 31}"#,
                "sends_per_hour must be",
            ),
            (
                r#"{"version": 4, "sends_per_hour": "5"}"#,
                "sends_per_hour must be",
            ),
            (
                r#"{"version": 4, "sends_per_hour": 2.5}"#,
                "sends_per_hour must be",
            ),
            (
                r#"{"version": 4, "changes_per_hour": true}"#,
                "changes_per_hour must be",
            ),
            (
                r#"{"version": 4, "changes_per_hour": 0}"#,
                "changes_per_hour must be",
            ),
        ] {
            rejected(text, place);
        }
        let limits = parsed(r#"{"version": 4, "sends_per_hour": 30, "changes_per_hour": 1}"#);
        assert_eq!(
            limits.map(|rights| (rights.sends_per_hour, rights.changes_per_hour)),
            Ok((30, 1))
        );
    }

    #[test]
    fn the_number_of_entries_is_bounded() {
        for key in ["mail", "calendars"] {
            let list = |count: usize| {
                let entries: Vec<String> = (0..count)
                    .map(|n| format!(r#"{{"id": "A{n}", "level": "read"}}"#))
                    .collect();
                format!(r#"{{"version": 4, "{key}": [{}]}}"#, entries.join(","))
            };
            assert!(parsed(&list(MAX_ENTRIES)).is_ok(), "{key}");
            rejected(
                &list(MAX_ENTRIES + 1),
                &format!("{key} has more than 200 entries"),
            );
        }
    }

    #[test]
    fn a_change_is_applied_only_when_the_entries_are_still_as_they_were() -> TestResult {
        let before = parsed(FULL)?;
        let mut after = before.clone();
        after.mail.truncate(1);
        let mut fresh = before.clone();
        assert_eq!(fresh.apply(&before, &after), Ok(()));
        assert_eq!(fresh.mail.len(), 1);
        assert_eq!(fresh.calendars, before.calendars);

        let mut fresh = before.clone();
        fresh.revoke();
        assert_eq!(fresh.apply(&before, &after), Err(Changed));
        assert!(!fresh.enabled());

        let mut other = before.clone();
        other.calendars.truncate(1);
        assert_eq!(other.apply(&before, &after), Err(Changed));
        assert_eq!(other.mail, before.mail);

        let mut limits = before.clone();
        limits.sends_per_hour = 11;
        assert_eq!(limits.apply(&before, &after), Ok(()));
        assert_eq!(limits.sends_per_hour, 11);
        assert_eq!(limits.mail.len(), 1);

        let mut untouched = before.clone();
        untouched.revoke();
        let revoked = untouched.clone();
        assert_eq!(untouched.apply(&before, &before), Ok(()));
        assert_eq!(untouched, revoked);
        Ok(())
    }

    fn names_of(rights: &Rights) -> Vec<String> {
        let mail = rights.mail.iter().map(|(account, _)| &account.name);
        let calendars = rights.calendars.iter().map(|(entry, _)| &entry.name);
        mail.chain(calendars).cloned().collect()
    }

    #[test]
    fn a_name_less_entry_takes_its_id_as_a_one_line_name() -> TestResult {
        let rights = parsed(
            r#"{"version": 4,
                "mail": [{"id": " ACC-1 ", "level": "read"}],
                "calendars": [{"id": "CAL\u200b 1", "level": "read"}]}"#,
        )?;
        assert_eq!(rights.mail[0].0.id, " ACC-1 ");
        assert_eq!(rights.mail[0].0.name, "ACC-1");
        assert_eq!(rights.calendars[0].0.id, "CAL\u{200b} 1");
        assert_eq!(rights.calendars[0].0.name, "CAL 1");
        let long = "L".repeat(300);
        let clipped = one_line_str(&long, MAX_NAME);
        assert!(clipped.len() < long.len());
        assert_eq!(one_line_str(&clipped, MAX_NAME), clipped);
        let rights = parsed(&format!(
            r#"{{"version": 4, "mail": [{{"id": "{long}", "level": "read"}}]}}"#
        ))?;
        assert_eq!(rights.mail[0].0.id, long);
        assert_eq!(rights.mail[0].0.name, clipped);
        Ok(())
    }

    #[test]
    fn what_the_reader_accepts_is_always_read_back_equal_from_what_the_writer_writes() -> TestResult
    {
        let long = "x".repeat(300);
        let ids = [
            " x",
            "x ",
            "a\\u200b b",
            "a \\u200b b",
            "\\u202eevil\\u202c",
            "tab\\there",
            "line\\nbreak",
            "\\u200b",
            "\\u0000",
            "  \\u00a0  ",
            "AB\\u200d\\u200dCD",
            long.as_str(),
        ];
        for id in ids {
            for section in ["mail", "calendars"] {
                let text = format!(
                    r#"{{"version": 4, "{section}": [{{"id": "{id}", "level": "read"}}]}}"#
                );
                let rights = parsed(&text)?;
                let again = Rights::parse(&rights.to_config())
                    .map_err(|what| format!("{id:?} in {section}: {what}"))?;
                assert_eq!(again, rights, "{id:?} in {section}");
                for name in names_of(&rights) {
                    assert_eq!(one_line_str(&name, MAX_NAME), name, "{id:?} in {section}");
                }
            }
        }
        Ok(())
    }

    #[test]
    fn an_id_that_is_nothing_but_hidden_text_keeps_an_empty_name_that_reads_back_equal(
    ) -> TestResult {
        let rights = parsed(r#"{"version": 4, "calendars": [{"id": "\u200b", "level": "read"}]}"#)?;
        assert_eq!(rights.calendars[0].0.name, "");
        assert_eq!(Rights::parse(&rights.to_config())?, rights);
        Ok(())
    }
}
