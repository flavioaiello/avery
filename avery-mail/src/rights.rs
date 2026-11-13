use crate::account::{string_list, MailAccount};
use crate::calendar::CalendarEntry;
use crate::json::{Json, Object};
use crate::source::{account_address, calendar_parts, plain_address, Source};
use crate::text::one_line_str;
use crate::texts::{DEFAULT_CHANGES_PER_HOUR, DEFAULT_SENDS_PER_HOUR, MAX_LIMIT_PER_HOUR};

pub(crate) const CONFIG_VERSION: i64 = 3;

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
            _ => Err(format!("{place}.level must be \"read\" or \"write\"")),
        }
    }
}

pub(crate) const MAX_LINKS: usize = 16;
pub(crate) const MAX_NAME: usize = 120;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Link {
    pub(crate) source: Source,
    pub(crate) address: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Rights {
    pub(crate) mail: Vec<(MailAccount, Level)>,
    pub(crate) calendars: Vec<(CalendarEntry, Level)>,
    pub(crate) linked: Vec<Link>,
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

fn limit_of(section: &Object, key: &str, default: i64, place: &str) -> Result<i64, String> {
    match section.get(key) {
        None => Ok(default),
        Some(Json::Int(value)) if (1..=MAX_LIMIT_PER_HOUR).contains(value) => Ok(*value),
        Some(_) => Err(format!(
            "{place}.{key} must be a whole number from 1 to {MAX_LIMIT_PER_HOUR}"
        )),
    }
}

fn list_of<'a>(section: &'a Object, key: &str, place: &str) -> Result<&'a [Json], String> {
    match section.get(key) {
        None => Ok(&[]),
        Some(Json::Arr(items)) => Ok(items.as_slice()),
        Some(_) => Err(format!("{place}.{key} must be a list")),
    }
}

fn parse_mail(section: &Object) -> Result<Vec<(MailAccount, Level)>, String> {
    let mut found: Vec<(MailAccount, Level)> = Vec::new();
    for (index, item) in list_of(section, "accounts", "mail")?.iter().enumerate() {
        let place = format!("mail.accounts[{index}]");
        let entry = object_of(item, &place)?;
        only_keys(entry, &["id", "name", "type", "addresses", "level"], &place)?;
        let id = id_of(entry, &place)?;
        if found.iter().any(|(known, _)| known.id == id) {
            return Err(format!("{place}.id appears twice"));
        }
        let name = match optional_text(entry, "name", &place)? {
            name if name.is_empty() => id.clone(),
            name => name,
        };
        if let Some(addresses) = entry.get("addresses") {
            let valid = matches!(addresses, Json::Arr(items) if items.iter().all(|item| item.as_str().is_some_and(|text| text == one_line_str(text, MAX_NAME))));
            if !valid {
                return Err(format!(
                    "{place}.addresses must be a list of one-line texts"
                ));
            }
        }
        let account = MailAccount {
            id,
            name,
            kind: optional_text(entry, "type", &place)?,
            addresses: string_list(entry.get("addresses")),
        };
        found.push((account, Level::parse(entry.get("level"), &place)?));
    }
    Ok(found)
}

fn parse_calendars(section: &Object) -> Result<Vec<(CalendarEntry, Level)>, String> {
    let mut found: Vec<(CalendarEntry, Level)> = Vec::new();
    for (index, item) in list_of(section, "calendars", "calendar")?
        .iter()
        .enumerate()
    {
        let place = format!("calendar.calendars[{index}]");
        let entry = object_of(item, &place)?;
        only_keys(entry, &["id", "name", "level"], &place)?;
        let id = id_of(entry, &place)?;
        if found.iter().any(|(known, _)| known.id == id) {
            return Err(format!("{place}.id appears twice"));
        }
        let name = match optional_text(entry, "name", &place)? {
            name if name.is_empty() => id.clone(),
            name => name,
        };
        found.push((
            CalendarEntry { id, name },
            Level::parse(entry.get("level"), &place)?,
        ));
    }
    Ok(found)
}

fn parse_links(section: &Object) -> Result<Vec<Link>, String> {
    let mut found: Vec<Link> = Vec::new();
    let items = list_of(section, "accounts", "cloud")?;
    if items.len() > MAX_LINKS {
        return Err(format!("cloud.accounts has more than {MAX_LINKS} entries"));
    }
    for (index, item) in items.iter().enumerate() {
        let place = format!("cloud.accounts[{index}]");
        let entry = object_of(item, &place)?;
        only_keys(entry, &["source", "address"], &place)?;
        let source = entry
            .get("source")
            .and_then(Json::as_str)
            .and_then(Source::parse_cloud)
            .ok_or_else(|| format!("{place}.source must be \"google\" or \"microsoft\""))?;
        let address = match entry.get("address").and_then(Json::as_str) {
            Some(address) if plain_address(address) && address == address.to_lowercase() => {
                address.to_string()
            }
            _ => {
                return Err(format!(
                    "{place}.address must be a plain lower-case mail address"
                ))
            }
        };
        if found
            .iter()
            .any(|link| link.source == source && link.address == address)
        {
            return Err(format!("{place} appears twice"));
        }
        found.push(Link { source, address });
    }
    Ok(found)
}

fn check_grants(
    mail: &[(MailAccount, Level)],
    calendars: &[(CalendarEntry, Level)],
    linked: &[Link],
) -> Result<(), String> {
    let is_linked = |source: Source, address: &str| {
        linked
            .iter()
            .any(|link| link.source == source && link.address == address)
    };
    for (index, (account, _)) in mail.iter().enumerate() {
        if Source::of_id(&account.id) == Source::Apple {
            continue;
        }
        match account_address(&account.id) {
            Some((source, address)) if is_linked(source, address) => {}
            Some(_) => {
                return Err(format!(
                    "mail.accounts[{index}] belongs to an account that is not linked"
                ))
            }
            None => {
                return Err(format!(
                    "mail.accounts[{index}].id is not a valid cloud account"
                ))
            }
        }
    }
    for (index, (calendar, _)) in calendars.iter().enumerate() {
        if Source::of_id(&calendar.id) == Source::Apple {
            continue;
        }
        match calendar_parts(&calendar.id) {
            Some((source, address, _)) if is_linked(source, address) => {}
            Some(_) => {
                return Err(format!(
                    "calendar.calendars[{index}] belongs to an account that is not linked"
                ))
            }
            None => {
                return Err(format!(
                    "calendar.calendars[{index}].id is not a valid cloud calendar"
                ))
            }
        }
    }
    Ok(())
}

impl Rights {
    pub(crate) fn empty() -> Rights {
        Rights {
            mail: Vec::new(),
            calendars: Vec::new(),
            linked: Vec::new(),
            sends_per_hour: DEFAULT_SENDS_PER_HOUR,
            changes_per_hour: DEFAULT_CHANGES_PER_HOUR,
        }
    }

    pub(crate) fn parse(config: &Object) -> Result<Rights, String> {
        if config.get("version") != Some(&Json::Int(CONFIG_VERSION)) {
            return Err(format!("the format version must be {CONFIG_VERSION}"));
        }
        only_keys(config, &["version", "mail", "calendar", "cloud"], "config")?;
        let mut rights = Rights::empty();
        if let Some(value) = config.get("mail") {
            let section = object_of(value, "mail")?;
            only_keys(section, &["accounts", "sends_per_hour"], "mail")?;
            rights.mail = parse_mail(section)?;
            rights.sends_per_hour =
                limit_of(section, "sends_per_hour", DEFAULT_SENDS_PER_HOUR, "mail")?;
        }
        if let Some(value) = config.get("calendar") {
            let section = object_of(value, "calendar")?;
            only_keys(section, &["calendars", "changes_per_hour"], "calendar")?;
            rights.calendars = parse_calendars(section)?;
            rights.changes_per_hour = limit_of(
                section,
                "changes_per_hour",
                DEFAULT_CHANGES_PER_HOUR,
                "calendar",
            )?;
        }
        if let Some(value) = config.get("cloud") {
            let section = object_of(value, "cloud")?;
            only_keys(section, &["accounts"], "cloud")?;
            rights.linked = parse_links(section)?;
        }
        check_grants(&rights.mail, &rights.calendars, &rights.linked)?;
        Ok(rights)
    }

    pub(crate) fn to_config(&self) -> Object {
        let mut config = Object::new();
        config.insert("version", Json::Int(CONFIG_VERSION));
        if !self.mail.is_empty() || self.sends_per_hour != DEFAULT_SENDS_PER_HOUR {
            let accounts: Vec<Json> = self
                .mail
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
                .collect();
            let mut section = Object::new();
            section.insert("accounts", Json::Arr(accounts));
            section.insert("sends_per_hour", Json::Int(self.sends_per_hour));
            config.insert("mail", Json::Obj(section));
        }
        if !self.calendars.is_empty() || self.changes_per_hour != DEFAULT_CHANGES_PER_HOUR {
            let calendars: Vec<Json> = self
                .calendars
                .iter()
                .map(|(calendar, level)| {
                    let mut entry = Object::new();
                    entry.insert("id", calendar.id.as_str());
                    entry.insert("name", calendar.name.as_str());
                    entry.insert("level", level.as_str());
                    Json::Obj(entry)
                })
                .collect();
            let mut section = Object::new();
            section.insert("calendars", Json::Arr(calendars));
            section.insert("changes_per_hour", Json::Int(self.changes_per_hour));
            config.insert("calendar", Json::Obj(section));
        }
        if !self.linked.is_empty() {
            let accounts: Vec<Json> = self
                .linked
                .iter()
                .map(|link| {
                    let mut entry = Object::new();
                    entry.insert("source", link.source.as_str());
                    entry.insert("address", link.address.as_str());
                    Json::Obj(entry)
                })
                .collect();
            let mut section = Object::new();
            section.insert("accounts", Json::Arr(accounts));
            config.insert("cloud", Json::Obj(section));
        }
        config
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

    const FULL: &str = r#"{"version": 3,
        "mail": {"accounts": [
            {"id": "ACC-1", "name": "Privat", "type": "imap", "addresses": ["me@example.org"], "level": "write"},
            {"id": "ACC-2", "name": "Arbeit", "level": "read"}], "sends_per_hour": 3},
        "calendar": {"calendars": [
            {"id": "CAL-1", "name": "Privat", "level": "write"},
            {"id": "CAL-2", "level": "read"}], "changes_per_hour": 7}}"#;

    fn parsed(text: &str) -> Result<Rights, String> {
        Rights::parse(&args(text)?)
    }

    #[test]
    fn a_full_file_is_read_with_levels_names_and_limits() -> TestResult {
        let rights = parsed(FULL)?;
        assert_eq!(rights.mail.len(), 2);
        assert_eq!(rights.mail[0].0.addresses, ["me@example.org"]);
        assert_eq!(rights.mail[0].1, Level::Write);
        assert_eq!(rights.mail[1].0.name, "Arbeit");
        assert_eq!(rights.mail[1].0.kind, "");
        assert_eq!(rights.mail[1].1, Level::Read);
        assert_eq!(rights.calendars[1].0.name, "CAL-2");
        assert_eq!(rights.calendars[1].1, Level::Read);
        assert_eq!((rights.sends_per_hour, rights.changes_per_hour), (3, 7));
        Ok(())
    }

    #[test]
    fn an_empty_file_grants_nothing_and_uses_the_default_limits() -> TestResult {
        let rights = parsed(r#"{"version": 3}"#)?;
        assert_eq!(rights, Rights::empty());
        assert!(rights.mail_at(Level::Read).is_empty());
        assert!(rights.calendars_at(Level::Read).is_empty());
        assert_eq!(rights.sends_per_hour, DEFAULT_SENDS_PER_HOUR);
        assert_eq!(rights.changes_per_hour, DEFAULT_CHANGES_PER_HOUR);
        Ok(())
    }

    #[test]
    fn rights_round_trip_through_the_config_object() -> TestResult {
        let rights = parsed(FULL)?;
        assert_eq!(Rights::parse(&rights.to_config())?, rights);
        let empty = Rights::empty().to_config();
        assert_eq!(empty.len(), 1);
        assert_eq!(Rights::parse(&empty)?, Rights::empty());
        let mut custom = Rights::empty();
        custom.sends_per_hour = 9;
        assert_eq!(Rights::parse(&custom.to_config())?, custom);
        Ok(())
    }

    const CLOUD: &str = r#"{"version": 3,
        "mail": {"accounts": [
            {"id": "google:alice@gmail.example", "name": "alice@gmail.example", "type": "gmail", "addresses": ["alice@gmail.example"], "level": "read"},
            {"id": "ACC-1", "name": "Privat", "level": "write"}]},
        "calendar": {"calendars": [
            {"id": "google:alice@gmail.example:alice@gmail.example", "name": "alice@gmail.example", "level": "read"},
            {"id": "microsoft:bob@outlook.example:AQMk-_=", "name": "Calendar (bob@outlook.example)", "level": "write"}]},
        "cloud": {"accounts": [
            {"source": "google", "address": "alice@gmail.example"},
            {"source": "microsoft", "address": "bob@outlook.example"}]}}"#;

    #[test]
    fn linked_cloud_accounts_are_read_and_written_back() -> TestResult {
        let rights = parsed(CLOUD)?;
        assert_eq!(
            rights.linked,
            [
                Link {
                    source: Source::Google,
                    address: "alice@gmail.example".into()
                },
                Link {
                    source: Source::Microsoft,
                    address: "bob@outlook.example".into()
                }
            ]
        );
        assert_eq!(
            rights.mail_level("google:alice@gmail.example"),
            Some(Level::Read)
        );
        assert_eq!(Rights::parse(&rights.to_config())?, rights);
        let linked_only = parsed(
            r#"{"version": 3, "cloud": {"accounts": [{"source": "google", "address": "a@gmail.example"}]}}"#,
        )?;
        assert!(linked_only.mail.is_empty());
        assert_eq!(linked_only.linked.len(), 1);
        assert_eq!(Rights::parse(&linked_only.to_config())?, linked_only);
        Ok(())
    }

    #[test]
    fn names_and_addresses_must_be_visible_single_lines() {
        for text in [
            r#"{"version": 3, "mail": {"accounts": [{"id": "A", "name": "Privat\u200b", "level": "read"}]}}"#,
            r#"{"version": 3, "mail": {"accounts": [{"id": "A", "name": "Pri\nvat", "level": "read"}]}}"#,
            r#"{"version": 3, "mail": {"accounts": [{"id": "A", "name": "Privat\u202e", "level": "read"}]}}"#,
            r#"{"version": 3, "mail": {"accounts": [{"id": "A", "addresses": ["a@x.example\u200b"], "level": "read"}]}}"#,
            r#"{"version": 3, "calendar": {"calendars": [{"id": "C", "name": "Feiertage\u{e0041}", "level": "read"}]}}"#,
        ] {
            assert!(parsed(text).is_err(), "{text}");
        }
        assert!(parsed(
            r#"{"version": 3, "mail": {"accounts": [{"id": "A", "name": "Privat Gmail", "addresses": ["a@x.example"], "level": "read"}]}}"#
        )
        .is_ok());
    }

    #[test]
    fn grants_for_cloud_accounts_need_a_link_and_a_valid_id() {
        let link = r#""cloud": {"accounts": [{"source": "google", "address": "a@gmail.example"}]}"#;
        for (text, place) in [
            (
                r#"{"version": 3, "mail": {"accounts": [{"id": "google:a@gmail.example", "level": "read"}]}}"#.to_string(),
                "mail.accounts[0] belongs to an account that is not linked",
            ),
            (
                format!(r#"{{"version": 3, "mail": {{"accounts": [{{"id": "microsoft:a@gmail.example", "level": "read"}}]}}, {link}}}"#),
                "mail.accounts[0] belongs to an account that is not linked",
            ),
            (
                format!(r#"{{"version": 3, "mail": {{"accounts": [{{"id": "google:nonsense", "level": "read"}}]}}, {link}}}"#),
                "mail.accounts[0].id is not a valid cloud account",
            ),
            (
                format!(r#"{{"version": 3, "calendar": {{"calendars": [{{"id": "google:b@gmail.example:x", "level": "read"}}]}}, {link}}}"#),
                "calendar.calendars[0] belongs to an account that is not linked",
            ),
            (
                format!(r#"{{"version": 3, "calendar": {{"calendars": [{{"id": "google:a@gmail.example", "level": "read"}}]}}, {link}}}"#),
                "calendar.calendars[0].id is not a valid cloud calendar",
            ),
        ] {
            assert_eq!(parsed(&text).err().as_deref(), Some(place), "{text}");
        }
    }

    #[test]
    fn malformed_cloud_sections_are_rejected() {
        let many: Vec<String> = (0..=MAX_LINKS)
            .map(|n| format!(r#"{{"source": "google", "address": "a{n}@gmail.example"}}"#))
            .collect();
        let too_many = format!(
            r#"{{"version": 3, "cloud": {{"accounts": [{}]}}}}"#,
            many.join(",")
        );
        for (text, place) in [
            (r#"{"version": 3, "cloud": []}"#.to_string(), "cloud must be an object"),
            (r#"{"version": 3, "cloud": {"keys": []}}"#.to_string(), "cloud has the unknown key"),
            (r#"{"version": 3, "cloud": {"accounts": {}}}"#.to_string(), "cloud.accounts must be a list"),
            (r#"{"version": 3, "cloud": {"accounts": [5]}}"#.to_string(), "cloud.accounts[0] must be an object"),
            (
                r#"{"version": 3, "cloud": {"accounts": [{"source": "apple", "address": "a@gmail.example"}]}}"#.to_string(),
                "cloud.accounts[0].source must be",
            ),
            (
                r#"{"version": 3, "cloud": {"accounts": [{"address": "a@gmail.example"}]}}"#.to_string(),
                "cloud.accounts[0].source must be",
            ),
            (
                r#"{"version": 3, "cloud": {"accounts": [{"source": "google", "address": "A@Gmail.example"}]}}"#.to_string(),
                "cloud.accounts[0].address must be a plain lower-case mail address",
            ),
            (
                r#"{"version": 3, "cloud": {"accounts": [{"source": "google", "address": "nonsense"}]}}"#.to_string(),
                "cloud.accounts[0].address must be",
            ),
            (
                r#"{"version": 3, "cloud": {"accounts": [{"source": "google", "address": "a@gmail.example", "x": 1}]}}"#.to_string(),
                "cloud.accounts[0] has the unknown key",
            ),
            (
                r#"{"version": 3, "cloud": {"accounts": [{"source": "google", "address": "a@gmail.example"}, {"source": "google", "address": "a@gmail.example"}]}}"#.to_string(),
                "cloud.accounts[1] appears twice",
            ),
            (too_many, "more than 16 entries"),
        ] {
            let error = parsed(&text).err().unwrap_or_default();
            assert!(error.contains(place), "{text}: {error}");
        }
    }

    #[test]
    fn levels_are_ordered_and_filter_the_lists() -> TestResult {
        let rights = parsed(FULL)?;
        assert!(Level::Read < Level::Write);
        assert_eq!(rights.mail_at(Level::Read).len(), 2);
        assert_eq!(rights.mail_at(Level::Write).len(), 1);
        assert_eq!(rights.calendars_at(Level::Write)[0].id, "CAL-1");
        assert_eq!(rights.mail_level("ACC-2"), Some(Level::Read));
        assert_eq!(rights.mail_level("nope"), None);
        assert_eq!(rights.calendar_level("CAL-1"), Some(Level::Write));
        assert_eq!(rights.calendar_level("ACC-1"), None);
        Ok(())
    }

    #[test]
    fn anything_unexpected_is_rejected_with_its_place() {
        for (text, place) in [
            (r#"{"version": 2}"#, "format version"),
            (r#"{}"#, "format version"),
            (r#"{"version": "3"}"#, "format version"),
            (
                r#"{"version": 3, "allowed_accounts": []}"#,
                "allowed_accounts",
            ),
            (r#"{"version": 3, "mail": []}"#, "mail must be an object"),
            (
                r#"{"version": 3, "mail": {"write": []}}"#,
                "mail has the unknown key",
            ),
            (
                r#"{"version": 3, "mail": {"accounts": {}}}"#,
                "mail.accounts must be a list",
            ),
            (
                r#"{"version": 3, "mail": {"accounts": [5]}}"#,
                "mail.accounts[0] must be an object",
            ),
            (
                r#"{"version": 3, "mail": {"accounts": [{"level": "read"}]}}"#,
                "mail.accounts[0].id",
            ),
            (
                r#"{"version": 3, "mail": {"accounts": [{"id": "", "level": "read"}]}}"#,
                "mail.accounts[0].id",
            ),
            (
                r#"{"version": 3, "mail": {"accounts": [{"id": 7, "level": "read"}]}}"#,
                "mail.accounts[0].id",
            ),
            (
                r#"{"version": 3, "mail": {"accounts": [{"id": "A"}]}}"#,
                "mail.accounts[0].level",
            ),
            (
                r#"{"version": 3, "mail": {"accounts": [{"id": "A", "level": "off"}]}}"#,
                "mail.accounts[0].level",
            ),
            (
                r#"{"version": 3, "mail": {"accounts": [{"id": "A", "level": "WRITE"}]}}"#,
                "mail.accounts[0].level",
            ),
            (
                r#"{"version": 3, "mail": {"accounts": [{"id": "A", "level": "read", "x": 1}]}}"#,
                "mail.accounts[0] has the unknown key",
            ),
            (
                r#"{"version": 3, "mail": {"accounts": [{"id": "A", "level": "read", "name": 5}]}}"#,
                "mail.accounts[0].name",
            ),
            (
                r#"{"version": 3, "mail": {"accounts": [{"id": "A", "level": "read", "addresses": [5]}]}}"#,
                "mail.accounts[0].addresses",
            ),
            (
                r#"{"version": 3, "mail": {"accounts": [{"id": "A", "level": "read", "addresses": "a@example.org"}]}}"#,
                "mail.accounts[0].addresses",
            ),
            (
                r#"{"version": 3, "mail": {"accounts": [{"id": "A", "level": "read"}, {"id": "A", "level": "write"}]}}"#,
                "mail.accounts[1].id appears twice",
            ),
            (
                r#"{"version": 3, "mail": {"sends_per_hour": 0}}"#,
                "mail.sends_per_hour",
            ),
            (
                r#"{"version": 3, "mail": {"sends_per_hour": 31}}"#,
                "mail.sends_per_hour",
            ),
            (
                r#"{"version": 3, "mail": {"sends_per_hour": "5"}}"#,
                "mail.sends_per_hour",
            ),
            (
                r#"{"version": 3, "mail": {"sends_per_hour": 2.5}}"#,
                "mail.sends_per_hour",
            ),
            (
                r#"{"version": 3, "calendar": "x"}"#,
                "calendar must be an object",
            ),
            (
                r#"{"version": 3, "calendar": {"calendars": [{"id": "C", "level": "read", "addresses": []}]}}"#,
                "calendar.calendars[0] has the unknown key",
            ),
            (
                r#"{"version": 3, "calendar": {"calendars": [{"id": "C", "level": "none"}]}}"#,
                "calendar.calendars[0].level",
            ),
            (
                r#"{"version": 3, "calendar": {"calendars": [{"id": "C", "level": "read"}, {"id": "C", "level": "read"}]}}"#,
                "calendar.calendars[1].id appears twice",
            ),
            (
                r#"{"version": 3, "calendar": {"changes_per_hour": true}}"#,
                "calendar.changes_per_hour",
            ),
        ] {
            let outcome = parsed(text);
            assert!(
                matches!(&outcome, Err(message) if message.contains(place)),
                "{text}: {outcome:?}"
            );
        }
    }
}
