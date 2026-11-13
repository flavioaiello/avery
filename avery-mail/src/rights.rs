use crate::account::{string_list, MailAccount};
use crate::calendar::CalendarEntry;
use crate::json::{Json, Object};
use crate::provider::{account_id, calendar_id, Provider};
use crate::text::{one_line_str, plain_address};
use crate::texts::{DEFAULT_CHANGES_PER_HOUR, DEFAULT_SENDS_PER_HOUR, MAX_LIMIT_PER_HOUR};

pub(crate) const CONFIG_VERSION: i64 = 3;
pub(crate) const MAX_ACCOUNTS: usize = 16;
pub(crate) const MAX_CALENDARS: usize = 100;
pub(crate) const MAX_APPLE: usize = 200;
pub(crate) const MAX_NAME: usize = 120;
const APPLE: &str = Provider::Apple.as_str();

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
pub(crate) enum Refusal {
    Changed,
    Full,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CalendarGrant {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) level: Level,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RemoteAccount {
    provider: Provider,
    address: String,
    id: String,
    pub(crate) mail: Option<Level>,
    pub(crate) calendars: Vec<CalendarGrant>,
}

impl RemoteAccount {
    pub(crate) fn new(provider: Provider, address: &str) -> Option<RemoteAccount> {
        let id = account_id(provider, address)?;
        (address == address.to_lowercase()).then(|| RemoteAccount {
            provider,
            address: address.to_string(),
            id,
            mail: None,
            calendars: Vec::new(),
        })
    }

    pub(crate) fn provider(&self) -> Provider {
        self.provider
    }

    pub(crate) fn address(&self) -> &str {
        &self.address
    }

    pub(crate) fn mail_account(&self) -> MailAccount {
        MailAccount {
            id: self.id.clone(),
            name: self.address.clone(),
            kind: self.provider.kind().to_string(),
            addresses: vec![self.address.clone()],
        }
    }

    pub(crate) fn calendar_id(&self, provider_id: &str) -> Option<String> {
        calendar_id(self.provider, &self.address, provider_id)
    }

    pub(crate) fn lower(&mut self, write_mail: bool, write_calendar: bool) {
        if !write_mail {
            self.mail = self.mail.map(|level| level.min(Level::Read));
        }
        if !write_calendar {
            for grant in &mut self.calendars {
                grant.level = grant.level.min(Level::Read);
            }
        }
    }

    fn calendar_entries(&self) -> impl Iterator<Item = (CalendarEntry, Level)> + '_ {
        self.calendars.iter().filter_map(|grant| {
            Some((
                CalendarEntry {
                    id: self.calendar_id(&grant.id)?,
                    name: grant.name.clone(),
                },
                grant.level,
            ))
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Apple {
    pub(crate) mail: Vec<(MailAccount, Level)>,
    pub(crate) calendars: Vec<(CalendarEntry, Level)>,
}

impl Apple {
    pub(crate) fn enabled(&self) -> bool {
        !self.mail.is_empty() || !self.calendars.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Rights {
    pub(crate) accounts: Vec<RemoteAccount>,
    pub(crate) apple: Apple,
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

fn list_of<'a>(
    section: &'a Object,
    key: &str,
    place: &str,
    most: usize,
) -> Result<&'a [Json], String> {
    match section.get(key) {
        None => Ok(&[]),
        Some(Json::Arr(items)) if items.len() <= most => Ok(items.as_slice()),
        Some(Json::Arr(_)) => Err(format!("{place}{key} has more than {most} entries")),
        Some(_) => Err(format!("{place}{key} must be a list")),
    }
}

fn parse_name(entry: &Object, id: &str, place: &str) -> Result<String, String> {
    Ok(match optional_text(entry, "name", place)? {
        name if name.is_empty() => id.to_string(),
        name => name,
    })
}

fn parse_remote_calendars(
    account: &RemoteAccount,
    entry: &Object,
    place: &str,
) -> Result<Vec<CalendarGrant>, String> {
    let mut found: Vec<CalendarGrant> = Vec::new();
    let prefix = format!("{place}.");
    for (index, item) in list_of(entry, "calendars", &prefix, MAX_CALENDARS)?
        .iter()
        .enumerate()
    {
        let place = format!("{place}.calendars[{index}]");
        let calendar = object_of(item, &place)?;
        only_keys(calendar, &["id", "name", "level"], &place)?;
        let id = id_of(calendar, &place)?;
        if account.calendar_id(&id).is_none() {
            return Err(format!("{place}.id is not a valid calendar id"));
        }
        if found.iter().any(|known| known.id == id) {
            return Err(format!("{place}.id appears twice"));
        }
        found.push(CalendarGrant {
            name: parse_name(calendar, &id, &place)?,
            id,
            level: Level::parse(calendar.get("level"), &format!("{place}.level"))?,
        });
    }
    Ok(found)
}

fn parse_accounts(config: &Object) -> Result<Vec<RemoteAccount>, String> {
    let mut found: Vec<RemoteAccount> = Vec::new();
    for (index, item) in list_of(config, "accounts", "", MAX_ACCOUNTS)?
        .iter()
        .enumerate()
    {
        let place = format!("accounts[{index}]");
        let entry = object_of(item, &place)?;
        only_keys(entry, &["provider", "address", "mail", "calendars"], &place)?;
        let provider = entry
            .get("provider")
            .and_then(Json::as_str)
            .and_then(Provider::parse_remote)
            .ok_or_else(|| {
                let choices: Vec<String> = Provider::remote()
                    .map(|provider| format!("\"{}\"", provider.as_str()))
                    .collect();
                format!("{place}.provider must be {}", choices.join(" or "))
            })?;
        let address = entry
            .get("address")
            .and_then(Json::as_str)
            .filter(|address| plain_address(address))
            .ok_or_else(|| format!("{place}.address must be a plain lower-case mail address"))?;
        let mut account = RemoteAccount::new(provider, address)
            .ok_or_else(|| format!("{place}.address must be a plain lower-case mail address"))?;
        if found.iter().any(|known| known.id == account.id) {
            return Err(format!("{place} appears twice"));
        }
        if let Some(level) = entry.get("mail") {
            account.mail = Some(Level::parse(Some(level), &format!("{place}.mail"))?);
        }
        account.calendars = parse_remote_calendars(&account, entry, &place)?;
        found.push(account);
    }
    Ok(found)
}

fn apple_id(entry: &Object, place: &str) -> Result<String, String> {
    let id = id_of(entry, place)?;
    if !Provider::is_apple_id(&id) {
        return Err(format!(
            "{place}.id must not look like a {} id",
            Provider::remote_names(" or ")
        ));
    }
    Ok(id)
}

fn parse_apple_mail(section: &Object) -> Result<Vec<(MailAccount, Level)>, String> {
    let mut found: Vec<(MailAccount, Level)> = Vec::new();
    for (index, item) in list_of(section, "mail", &format!("{APPLE}."), MAX_APPLE)?
        .iter()
        .enumerate()
    {
        let place = format!("{APPLE}.mail[{index}]");
        let entry = object_of(item, &place)?;
        only_keys(entry, &["id", "name", "type", "addresses", "level"], &place)?;
        let id = apple_id(entry, &place)?;
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

fn parse_apple_calendars(section: &Object) -> Result<Vec<(CalendarEntry, Level)>, String> {
    let mut found: Vec<(CalendarEntry, Level)> = Vec::new();
    for (index, item) in list_of(section, "calendars", &format!("{APPLE}."), MAX_APPLE)?
        .iter()
        .enumerate()
    {
        let place = format!("{APPLE}.calendars[{index}]");
        let entry = object_of(item, &place)?;
        only_keys(entry, &["id", "name", "level"], &place)?;
        let id = apple_id(entry, &place)?;
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

fn parse_apple(section: &Object) -> Result<Apple, String> {
    only_keys(section, &["mail", "calendars"], APPLE)?;
    Ok(Apple {
        mail: parse_apple_mail(section)?,
        calendars: parse_apple_calendars(section)?,
    })
}

fn account_json(account: &RemoteAccount) -> Json {
    let mut entry = Object::new();
    entry.insert("provider", account.provider.as_str());
    entry.insert("address", account.address.as_str());
    if let Some(level) = account.mail {
        entry.insert("mail", level.as_str());
    }
    if !account.calendars.is_empty() {
        let calendars: Vec<Json> = account
            .calendars
            .iter()
            .map(|grant| {
                let mut calendar = Object::new();
                calendar.insert("id", grant.id.as_str());
                calendar.insert("name", grant.name.as_str());
                calendar.insert("level", grant.level.as_str());
                Json::Obj(calendar)
            })
            .collect();
        entry.insert("calendars", Json::Arr(calendars));
    }
    Json::Obj(entry)
}

fn apple_json(apple: &Apple) -> Json {
    let mut section = Object::new();
    if !apple.mail.is_empty() {
        let accounts: Vec<Json> = apple
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
        section.insert("mail", Json::Arr(accounts));
    }
    if !apple.calendars.is_empty() {
        let calendars: Vec<Json> = apple
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
        section.insert("calendars", Json::Arr(calendars));
    }
    Json::Obj(section)
}

impl Rights {
    #[cfg(test)]
    pub(crate) fn empty() -> Rights {
        Rights {
            accounts: Vec::new(),
            apple: Apple::default(),
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
                "accounts",
                APPLE,
                "sends_per_hour",
                "changes_per_hour",
            ],
            "config",
        )?;
        let apple = match config.get(APPLE) {
            Some(value) => parse_apple(object_of(value, APPLE)?)?,
            None => Apple::default(),
        };
        Ok(Rights {
            accounts: parse_accounts(config)?,
            apple,
            sends_per_hour: limit_of(config, "sends_per_hour", DEFAULT_SENDS_PER_HOUR)?,
            changes_per_hour: limit_of(config, "changes_per_hour", DEFAULT_CHANGES_PER_HOUR)?,
        })
    }

    pub(crate) fn to_config(&self) -> Object {
        let mut config = Object::new();
        config.insert("version", Json::Int(CONFIG_VERSION));
        if !self.accounts.is_empty() {
            let accounts: Vec<Json> = self.accounts.iter().map(account_json).collect();
            config.insert("accounts", Json::Arr(accounts));
        }
        if self.apple.enabled() {
            config.insert(APPLE, apple_json(&self.apple));
        }
        config.insert("sends_per_hour", Json::Int(self.sends_per_hour));
        config.insert("changes_per_hour", Json::Int(self.changes_per_hour));
        config
    }

    pub(crate) fn account(&self, provider: Provider, address: &str) -> Option<&RemoteAccount> {
        self.accounts
            .iter()
            .find(|account| account.provider == provider && account.address == address)
    }

    pub(crate) fn account_mut(
        &mut self,
        provider: Provider,
        address: &str,
    ) -> Option<&mut RemoteAccount> {
        self.accounts
            .iter_mut()
            .find(|account| account.provider == provider && account.address == address)
    }

    pub(crate) fn add_account(&mut self, account: RemoteAccount) -> Result<(), Refusal> {
        if self.accounts.len() >= MAX_ACCOUNTS {
            return Err(Refusal::Full);
        }
        self.accounts.push(account);
        Ok(())
    }

    pub(crate) fn remove_account(&mut self, provider: Provider, address: &str) -> bool {
        let known = self.accounts.len();
        self.accounts
            .retain(|account| !(account.provider == provider && account.address == address));
        self.accounts.len() != known
    }

    pub(crate) fn apply(&mut self, before: &Rights, after: &Rights) -> Result<(), Refusal> {
        for wanted in &after.accounts {
            let Some(old) = before.account(wanted.provider, &wanted.address) else {
                continue;
            };
            if old == wanted {
                continue;
            }
            match self.account_mut(wanted.provider, &wanted.address) {
                Some(current) if *current == *old => *current = wanted.clone(),
                _ => return Err(Refusal::Changed),
            }
        }
        if after.apple != before.apple {
            if self.apple != before.apple {
                return Err(Refusal::Changed);
            }
            self.apple = after.apple.clone();
        }
        Ok(())
    }

    pub(crate) fn revoke(&mut self) {
        for account in &mut self.accounts {
            account.mail = None;
            account.calendars.clear();
        }
        self.apple = Apple::default();
    }

    pub(crate) fn mail_entries(&self) -> Vec<(MailAccount, Level)> {
        self.accounts
            .iter()
            .filter_map(|account| Some((account.mail_account(), account.mail?)))
            .chain(self.apple.mail.iter().cloned())
            .collect()
    }

    pub(crate) fn calendar_entries(&self) -> Vec<(CalendarEntry, Level)> {
        self.accounts
            .iter()
            .flat_map(RemoteAccount::calendar_entries)
            .chain(self.apple.calendars.iter().cloned())
            .collect()
    }

    pub(crate) fn mail_at(&self, least: Level) -> Vec<MailAccount> {
        self.mail_entries()
            .into_iter()
            .filter(|(_, level)| *level >= least)
            .map(|(account, _)| account)
            .collect()
    }

    pub(crate) fn calendars_at(&self, least: Level) -> Vec<CalendarEntry> {
        self.calendar_entries()
            .into_iter()
            .filter(|(_, level)| *level >= least)
            .map(|(calendar, _)| calendar)
            .collect()
    }

    pub(crate) fn mail_level(&self, id: &str) -> Option<Level> {
        self.mail_entries()
            .into_iter()
            .find(|(account, _)| account.id == id)
            .map(|(_, level)| level)
    }

    pub(crate) fn calendar_level(&self, id: &str) -> Option<Level> {
        self.calendar_entries()
            .into_iter()
            .find(|(calendar, _)| calendar.id == id)
            .map(|(_, level)| level)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{args, TestResult};

    const FULL: &str = r#"{"version": 3,
        "accounts": [
            {"provider": "google", "address": "alice@gmail.example", "mail": "write",
             "calendars": [
                {"id": "alice@gmail.example", "name": "alice@gmail.example", "level": "write"},
                {"id": "family123@group.calendar.google.example", "level": "read"}]},
            {"provider": "microsoft", "address": "bob@outlook.example", "mail": "read",
             "calendars": [{"id": "AQMk-_=", "name": "Calendar (bob@outlook.example)", "level": "read"}]},
            {"provider": "google", "address": "carol@gmail.example"}],
        "apple": {
            "mail": [
                {"id": "ACC-1", "name": "Privat", "type": "imap", "addresses": ["me@example.org"], "level": "write"},
                {"id": "ACC-2", "name": "Arbeit", "level": "read"}],
            "calendars": [
                {"id": "CAL-1", "name": "Privat", "level": "write"},
                {"id": "CAL-2", "level": "read"}]},
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
    fn a_full_file_is_read_with_accounts_the_apple_section_and_limits() -> TestResult {
        let rights = parsed(FULL)?;
        assert_eq!(rights.accounts.len(), 3);
        let alice = rights
            .account(Provider::Google, "alice@gmail.example")
            .ok_or("alice")?;
        assert_eq!(alice.provider(), Provider::Google);
        assert_eq!(alice.address(), "alice@gmail.example");
        assert_eq!(alice.mail, Some(Level::Write));
        assert_eq!(alice.calendars.len(), 2);
        assert_eq!(
            alice.calendars[1].name,
            "family123@group.calendar.google.example"
        );
        assert_eq!(alice.calendars[1].level, Level::Read);
        let carol = rights
            .account(Provider::Google, "carol@gmail.example")
            .ok_or("carol")?;
        assert_eq!(carol.mail, None);
        assert!(carol.calendars.is_empty());
        assert!(rights
            .account(Provider::Microsoft, "alice@gmail.example")
            .is_none());
        assert_eq!(rights.apple.mail.len(), 2);
        assert_eq!(rights.apple.mail[0].0.addresses, ["me@example.org"]);
        assert_eq!(rights.apple.mail[1].0.name, "Arbeit");
        assert_eq!(rights.apple.mail[1].0.kind, "");
        assert_eq!(rights.apple.calendars[1].0.name, "CAL-2");
        assert!(rights.apple.enabled());
        assert_eq!((rights.sends_per_hour, rights.changes_per_hour), (3, 7));
        Ok(())
    }

    #[test]
    fn an_empty_file_grants_nothing_and_uses_the_default_limits() -> TestResult {
        let rights = parsed(r#"{"version": 3}"#)?;
        assert_eq!(rights, Rights::empty());
        assert!(rights.mail_at(Level::Read).is_empty());
        assert!(rights.calendars_at(Level::Read).is_empty());
        assert!(!rights.apple.enabled());
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
                "accounts",
                "apple",
                "sends_per_hour",
                "changes_per_hour"
            ]
        );
        let carol = RemoteAccount::new(Provider::Google, "carol@gmail.example").ok_or("carol")?;
        let mut only = Rights::empty();
        only.accounts.push(carol);
        let config = only.to_config();
        let accounts = config
            .get("accounts")
            .and_then(Json::as_array)
            .ok_or("accounts")?;
        let entry = accounts[0].as_object().ok_or("entry")?;
        let keys: Vec<&str> = entry.keys().collect();
        assert_eq!(keys, ["provider", "address"]);
        assert!(config.get("apple").is_none());
        Ok(())
    }

    #[test]
    fn the_flat_views_use_the_runtime_ids_and_levels() -> TestResult {
        let rights = parsed(FULL)?;
        let mail: Vec<String> = rights
            .mail_at(Level::Read)
            .into_iter()
            .map(|account| account.id)
            .collect();
        assert_eq!(
            mail,
            [
                "google:alice@gmail.example",
                "microsoft:bob@outlook.example",
                "ACC-1",
                "ACC-2"
            ]
        );
        let writing: Vec<String> = rights
            .mail_at(Level::Write)
            .into_iter()
            .map(|account| account.id)
            .collect();
        assert_eq!(writing, ["google:alice@gmail.example", "ACC-1"]);
        let first = rights.mail_at(Level::Read).remove(0);
        assert_eq!(first.name, "alice@gmail.example");
        assert_eq!(first.kind, "gmail");
        assert_eq!(first.addresses, ["alice@gmail.example"]);
        let calendars: Vec<String> = rights
            .calendars_at(Level::Read)
            .into_iter()
            .map(|calendar| calendar.id)
            .collect();
        assert_eq!(
            calendars,
            [
                "google:alice@gmail.example:alice@gmail.example",
                "google:alice@gmail.example:family123@group.calendar.google.example",
                "microsoft:bob@outlook.example:AQMk-_=",
                "CAL-1",
                "CAL-2"
            ]
        );
        assert_eq!(rights.calendars_at(Level::Write).len(), 2);
        assert!(Level::Read < Level::Write);
        assert_eq!(
            rights.mail_level("microsoft:bob@outlook.example"),
            Some(Level::Read)
        );
        assert_eq!(rights.mail_level("google:carol@gmail.example"), None);
        assert_eq!(rights.mail_level("ACC-2"), Some(Level::Read));
        assert_eq!(rights.mail_level("nope"), None);
        assert_eq!(
            rights.calendar_level("google:alice@gmail.example:alice@gmail.example"),
            Some(Level::Write)
        );
        assert_eq!(rights.calendar_level("CAL-1"), Some(Level::Write));
        assert_eq!(rights.calendar_level("ACC-1"), None);
        assert_eq!(rights.calendar_level("google:alice@gmail.example"), None);
        Ok(())
    }

    #[test]
    fn apple_is_enabled_only_by_entries() -> TestResult {
        let off = parsed(r#"{"version": 3, "apple": {}}"#)?;
        assert!(!off.apple.enabled());
        assert!(off.to_config().get("apple").is_none());
        let off = parsed(r#"{"version": 3, "apple": {"mail": [], "calendars": []}}"#)?;
        assert!(!off.apple.enabled());
        let calendars_only =
            parsed(r#"{"version": 3, "apple": {"calendars": [{"id": "C", "level": "read"}]}}"#)?;
        assert!(calendars_only.apple.enabled());
        assert!(calendars_only.to_config().get("apple").is_some());
        let mail_only =
            parsed(r#"{"version": 3, "apple": {"mail": [{"id": "A", "level": "read"}]}}"#)?;
        assert!(mail_only.apple.enabled());
        assert!(Rights::empty().mail_entries().is_empty());
        Ok(())
    }

    #[test]
    fn revoking_clears_grants_and_the_apple_section_but_keeps_the_accounts() -> TestResult {
        let mut rights = parsed(FULL)?;
        rights.revoke();
        assert_eq!(rights.accounts.len(), 3);
        for account in &rights.accounts {
            assert_eq!(account.mail, None);
            assert!(account.calendars.is_empty());
        }
        assert!(!rights.apple.enabled());
        assert!(rights.mail_at(Level::Read).is_empty());
        assert!(rights.calendars_at(Level::Read).is_empty());
        assert_eq!((rights.sends_per_hour, rights.changes_per_hour), (3, 7));
        assert_eq!(Rights::parse(&rights.to_config())?, rights);
        Ok(())
    }

    #[test]
    fn remote_accounts_need_a_remote_provider_and_a_plain_lower_case_address() {
        assert!(RemoteAccount::new(Provider::Google, "alice@gmail.example").is_some());
        assert!(RemoteAccount::new(Provider::Microsoft, "bob@outlook.example").is_some());
        assert!(RemoteAccount::new(Provider::Apple, "alice@gmail.example").is_none());
        assert!(RemoteAccount::new(Provider::Google, "Alice@gmail.example").is_none());
        assert!(RemoteAccount::new(Provider::Google, "nonsense").is_none());
        assert!(RemoteAccount::new(Provider::Google, "a b@gmail.example").is_none());
    }

    #[test]
    fn names_and_addresses_must_be_visible_single_lines() {
        for text in [
            r#"{"version": 3, "apple": {"mail": [{"id": "A", "name": "Privat\u200b", "level": "read"}]}}"#,
            r#"{"version": 3, "apple": {"mail": [{"id": "A", "name": "Pri\nvat", "level": "read"}]}}"#,
            r#"{"version": 3, "apple": {"mail": [{"id": "A", "name": "Privat\u202e", "level": "read"}]}}"#,
            r#"{"version": 3, "apple": {"mail": [{"id": "A", "addresses": ["a@x.example\u200b"], "level": "read"}]}}"#,
            r#"{"version": 3, "apple": {"calendars": [{"id": "C", "name": "Feiertage\u{e0041}", "level": "read"}]}}"#,
            r#"{"version": 3, "accounts": [{"provider": "google", "address": "a@gmail.example", "calendars": [{"id": "c", "name": "Fa\nmilie", "level": "read"}]}]}"#,
            r#"{"version": 3, "accounts": [{"provider": "google", "address": "a@gmail.example", "calendars": [{"id": "c", "name": "Fa\u200bmilie", "level": "read"}]}]}"#,
        ] {
            assert!(parsed(text).is_err(), "{text}");
        }
        assert!(parsed(
            r#"{"version": 3, "apple": {"mail": [{"id": "A", "name": "Privat Gmail", "addresses": ["a@x.example"], "level": "read"}]}}"#
        )
        .is_ok());
    }

    #[test]
    fn malformed_remote_accounts_are_rejected_with_their_place() {
        let account = |inner: &str| format!(r#"{{"version": 3, "accounts": [{{{inner}}}]}}"#);
        for (text, place) in [
            (r#"{"version": 3, "accounts": {}}"#.to_string(), "accounts must be a list"),
            (r#"{"version": 3, "accounts": [5]}"#.to_string(), "accounts[0] must be an object"),
            (account(r#""address": "a@gmail.example""#), "accounts[0].provider must be"),
            (account(r#""provider": "apple", "address": "a@gmail.example""#), "accounts[0].provider must be"),
            (account(r#""provider": "yahoo", "address": "a@gmail.example""#), "accounts[0].provider must be"),
            (account(r#""provider": 5, "address": "a@gmail.example""#), "accounts[0].provider must be"),
            (account(r#""provider": "google""#), "accounts[0].address must be"),
            (account(r#""provider": "google", "address": "A@Gmail.example""#), "accounts[0].address must be a plain lower-case mail address"),
            (account(r#""provider": "google", "address": "nonsense""#), "accounts[0].address must be"),
            (account(r#""provider": "google", "address": 5"#), "accounts[0].address must be"),
            (account(r#""provider": "google", "address": "a@gmail.example", "x": 1"#), "accounts[0] has the unknown key"),
            (account(r#""provider": "google", "address": "a@gmail.example", "mail": "off""#), "accounts[0].mail must be"),
            (account(r#""provider": "google", "address": "a@gmail.example", "mail": "WRITE""#), "accounts[0].mail must be"),
            (account(r#""provider": "google", "address": "a@gmail.example", "mail": true"#), "accounts[0].mail must be"),
            (account(r#""provider": "google", "address": "a@gmail.example", "calendars": {}"#), "accounts[0].calendars must be a list"),
            (account(r#""provider": "google", "address": "a@gmail.example", "calendars": [5]"#), "accounts[0].calendars[0] must be an object"),
            (account(r#""provider": "google", "address": "a@gmail.example", "calendars": [{"level": "read"}]"#), "accounts[0].calendars[0].id"),
            (account(r#""provider": "google", "address": "a@gmail.example", "calendars": [{"id": "", "level": "read"}]"#), "accounts[0].calendars[0].id"),
            (account(r#""provider": "google", "address": "a@gmail.example", "calendars": [{"id": "x y", "level": "read"}]"#), "accounts[0].calendars[0].id is not a valid calendar id"),
            (account(r#""provider": "google", "address": "a@gmail.example", "calendars": [{"id": "a/b", "level": "read"}]"#), "is not a valid calendar id"),
            (account(r#""provider": "google", "address": "a@gmail.example", "calendars": [{"id": "c"}]"#), "accounts[0].calendars[0].level must be"),
            (account(r#""provider": "google", "address": "a@gmail.example", "calendars": [{"id": "c", "level": "none"}]"#), "accounts[0].calendars[0].level must be"),
            (account(r#""provider": "google", "address": "a@gmail.example", "calendars": [{"id": "c", "level": "read", "x": 1}]"#), "accounts[0].calendars[0] has the unknown key"),
            (account(r#""provider": "google", "address": "a@gmail.example", "calendars": [{"id": "c", "level": "read", "name": 5}]"#), "accounts[0].calendars[0].name must be text"),
            (account(r#""provider": "google", "address": "a@gmail.example", "calendars": [{"id": "c", "level": "read"}, {"id": "c", "level": "write"}]"#), "accounts[0].calendars[1].id appears twice"),
            (
                r#"{"version": 3, "accounts": [{"provider": "google", "address": "a@gmail.example"}, {"provider": "google", "address": "a@gmail.example"}]}"#.to_string(),
                "accounts[1] appears twice",
            ),
        ] {
            rejected(&text, place);
        }
        assert!(parsed(
            r#"{"version": 3, "accounts": [{"provider": "google", "address": "a@example.org"}, {"provider": "microsoft", "address": "a@example.org"}]}"#
        )
        .is_ok());
        assert!(parsed(
            r#"{"version": 3, "accounts": [{"provider": "google", "address": "a@gmail.example", "calendars": [{"id": "c", "level": "read"}]}, {"provider": "microsoft", "address": "b@outlook.example", "calendars": [{"id": "c", "level": "read"}]}]}"#
        )
        .is_ok());
    }

    #[test]
    fn the_names_in_the_error_texts_come_from_the_providers() {
        rejected(
            r#"{"version": 3, "accounts": [{"provider": "yahoo", "address": "a@gmail.example"}]}"#,
            r#"accounts[0].provider must be "google" or "microsoft""#,
        );
        rejected(
            r#"{"version": 3, "apple": {"mail": [{"id": "google:a@gmail.example", "level": "read"}]}}"#,
            "apple.mail[0].id must not look like a Gmail or Outlook.com id",
        );
    }

    #[test]
    fn malformed_apple_sections_are_rejected_with_their_place() {
        for (text, place) in [
            (r#"{"version": 3, "apple": []}"#, "apple must be an object"),
            (
                r#"{"version": 3, "apple": {"write": []}}"#,
                "apple has the unknown key",
            ),
            (
                r#"{"version": 3, "apple": {"mail": {}}}"#,
                "apple.mail must be a list",
            ),
            (
                r#"{"version": 3, "apple": {"mail": [5]}}"#,
                "apple.mail[0] must be an object",
            ),
            (
                r#"{"version": 3, "apple": {"mail": [{"level": "read"}]}}"#,
                "apple.mail[0].id",
            ),
            (
                r#"{"version": 3, "apple": {"mail": [{"id": "", "level": "read"}]}}"#,
                "apple.mail[0].id",
            ),
            (
                r#"{"version": 3, "apple": {"mail": [{"id": 7, "level": "read"}]}}"#,
                "apple.mail[0].id",
            ),
            (
                r#"{"version": 3, "apple": {"mail": [{"id": "A"}]}}"#,
                "apple.mail[0].level must be",
            ),
            (
                r#"{"version": 3, "apple": {"mail": [{"id": "A", "level": "off"}]}}"#,
                "apple.mail[0].level must be",
            ),
            (
                r#"{"version": 3, "apple": {"mail": [{"id": "A", "level": "WRITE"}]}}"#,
                "apple.mail[0].level must be",
            ),
            (
                r#"{"version": 3, "apple": {"mail": [{"id": "A", "level": "read", "x": 1}]}}"#,
                "apple.mail[0] has the unknown key",
            ),
            (
                r#"{"version": 3, "apple": {"mail": [{"id": "A", "level": "read", "name": 5}]}}"#,
                "apple.mail[0].name must be text",
            ),
            (
                r#"{"version": 3, "apple": {"mail": [{"id": "A", "level": "read", "addresses": [5]}]}}"#,
                "apple.mail[0].addresses",
            ),
            (
                r#"{"version": 3, "apple": {"mail": [{"id": "A", "level": "read", "addresses": "a@example.org"}]}}"#,
                "apple.mail[0].addresses",
            ),
            (
                r#"{"version": 3, "apple": {"mail": [{"id": "A", "level": "read"}, {"id": "A", "level": "write"}]}}"#,
                "apple.mail[1].id appears twice",
            ),
            (
                r#"{"version": 3, "apple": {"mail": [{"id": "google:a@gmail.example", "level": "read"}]}}"#,
                "apple.mail[0].id must not look like",
            ),
            (
                r#"{"version": 3, "apple": {"calendars": [{"id": "microsoft:a@outlook.example:c", "level": "read"}]}}"#,
                "apple.calendars[0].id must not look like",
            ),
            (
                r#"{"version": 3, "apple": {"calendars": "x"}}"#,
                "apple.calendars must be a list",
            ),
            (
                r#"{"version": 3, "apple": {"calendars": [{"id": "C", "level": "read", "addresses": []}]}}"#,
                "apple.calendars[0] has the unknown key",
            ),
            (
                r#"{"version": 3, "apple": {"calendars": [{"id": "C", "level": "none"}]}}"#,
                "apple.calendars[0].level must be",
            ),
            (
                r#"{"version": 3, "apple": {"calendars": [{"id": "C", "level": "read"}, {"id": "C", "level": "read"}]}}"#,
                "apple.calendars[1].id appears twice",
            ),
        ] {
            rejected(text, place);
        }
    }

    #[test]
    fn versions_unknown_keys_and_limits_are_rejected() {
        for (text, place) in [
            (r#"{"version": 2}"#, "format version"),
            (r#"{}"#, "format version"),
            (r#"{"version": "3"}"#, "format version"),
            (
                r#"{"version": 3, "allowed_accounts": []}"#,
                "allowed_accounts",
            ),
            (
                r#"{"version": 3, "mail": {"accounts": []}}"#,
                "config has the unknown key \"mail\"",
            ),
            (
                r#"{"version": 3, "calendar": {"calendars": []}}"#,
                "config has the unknown key \"calendar\"",
            ),
            (
                r#"{"version": 3, "cloud": {"accounts": []}}"#,
                "config has the unknown key \"cloud\"",
            ),
            (
                r#"{"version": 3, "sends_per_hour": 0}"#,
                "sends_per_hour must be",
            ),
            (
                r#"{"version": 3, "sends_per_hour": 31}"#,
                "sends_per_hour must be",
            ),
            (
                r#"{"version": 3, "sends_per_hour": "5"}"#,
                "sends_per_hour must be",
            ),
            (
                r#"{"version": 3, "sends_per_hour": 2.5}"#,
                "sends_per_hour must be",
            ),
            (
                r#"{"version": 3, "changes_per_hour": true}"#,
                "changes_per_hour must be",
            ),
            (
                r#"{"version": 3, "changes_per_hour": 0}"#,
                "changes_per_hour must be",
            ),
        ] {
            rejected(text, place);
        }
        let limits = parsed(r#"{"version": 3, "sends_per_hour": 30, "changes_per_hour": 1}"#);
        assert_eq!(
            limits.map(|rights| (rights.sends_per_hour, rights.changes_per_hour)),
            Ok((30, 1))
        );
    }

    #[test]
    fn the_number_of_entries_is_bounded() {
        let accounts = |count: usize| {
            let entries: Vec<String> = (0..count)
                .map(|n| format!(r#"{{"provider": "google", "address": "a{n}@gmail.example"}}"#))
                .collect();
            format!(r#"{{"version": 3, "accounts": [{}]}}"#, entries.join(","))
        };
        assert!(parsed(&accounts(MAX_ACCOUNTS)).is_ok());
        rejected(
            &accounts(MAX_ACCOUNTS + 1),
            "accounts has more than 16 entries",
        );
        let calendars = |count: usize| {
            let entries: Vec<String> = (0..count)
                .map(|n| format!(r#"{{"id": "c{n}", "level": "read"}}"#))
                .collect();
            format!(
                r#"{{"version": 3, "accounts": [{{"provider": "google", "address": "a@gmail.example", "calendars": [{}]}}]}}"#,
                entries.join(",")
            )
        };
        assert!(parsed(&calendars(MAX_CALENDARS)).is_ok());
        rejected(
            &calendars(MAX_CALENDARS + 1),
            "accounts[0].calendars has more than 100 entries",
        );
        let apple = |count: usize| {
            let entries: Vec<String> = (0..count)
                .map(|n| format!(r#"{{"id": "A{n}", "level": "read"}}"#))
                .collect();
            format!(
                r#"{{"version": 3, "apple": {{"mail": [{}]}}}}"#,
                entries.join(",")
            )
        };
        assert!(parsed(&apple(MAX_APPLE)).is_ok());
        rejected(
            &apple(MAX_APPLE + 1),
            "apple.mail has more than 200 entries",
        );
    }

    #[test]
    fn a_change_is_applied_only_to_entries_that_are_still_as_they_were() -> TestResult {
        let before = parsed(FULL)?;
        let mut after = before.clone();
        after
            .account_mut(Provider::Google, "alice@gmail.example")
            .ok_or("alice")?
            .mail = Some(Level::Read);
        let mut fresh = before.clone();
        assert_eq!(fresh.apply(&before, &after), Ok(()));
        assert_eq!(fresh, after);

        let mut fresh = before.clone();
        fresh
            .account_mut(Provider::Microsoft, "bob@outlook.example")
            .ok_or("bob")?
            .mail = None;
        fresh.sends_per_hour = 11;
        assert_eq!(fresh.apply(&before, &after), Ok(()));
        assert_eq!(
            fresh.mail_level("google:alice@gmail.example"),
            Some(Level::Read)
        );
        assert_eq!(fresh.mail_level("microsoft:bob@outlook.example"), None);
        assert_eq!(fresh.sends_per_hour, 11);

        let mut fresh = before.clone();
        fresh.revoke();
        let revoked = fresh.clone();
        assert_eq!(fresh.apply(&before, &after), Err(Refusal::Changed));
        assert_eq!(fresh.mail_level("google:alice@gmail.example"), None);
        let mut untouched = revoked.clone();
        assert_eq!(untouched.apply(&before, &before), Ok(()));
        assert_eq!(untouched, revoked);

        let mut fresh = before.clone();
        assert!(fresh.remove_account(Provider::Google, "alice@gmail.example"));
        assert_eq!(fresh.apply(&before, &after), Err(Refusal::Changed));
        assert!(fresh
            .account(Provider::Google, "alice@gmail.example")
            .is_none());
        Ok(())
    }

    #[test]
    fn the_apple_section_is_replaced_only_when_it_is_still_as_it_was() -> TestResult {
        let before = parsed(FULL)?;
        let mut after = before.clone();
        after.apple.mail.truncate(1);
        let mut fresh = before.clone();
        assert_eq!(fresh.apply(&before, &after), Ok(()));
        assert_eq!(fresh.apple.mail.len(), 1);

        let mut fresh = before.clone();
        fresh.apple = Apple::default();
        assert_eq!(fresh.apply(&before, &after), Err(Refusal::Changed));
        assert!(!fresh.apple.enabled());

        let mut other = before.clone();
        other.apple.calendars.truncate(1);
        assert_eq!(other.apply(&before, &after), Err(Refusal::Changed));

        let mut unrelated = before.clone();
        unrelated.apple = Apple::default();
        let mut changes_accounts_only = before.clone();
        changes_accounts_only
            .account_mut(Provider::Google, "carol@gmail.example")
            .ok_or("carol")?
            .mail = Some(Level::Read);
        assert_eq!(unrelated.apply(&before, &changes_accounts_only), Ok(()));
        assert!(!unrelated.apple.enabled());
        assert_eq!(
            unrelated.mail_level("google:carol@gmail.example"),
            Some(Level::Read)
        );
        Ok(())
    }

    #[test]
    fn accounts_are_added_up_to_the_limit_and_removed_by_provider_and_address() -> TestResult {
        let mut rights = Rights::empty();
        for n in 0..MAX_ACCOUNTS {
            let account = RemoteAccount::new(Provider::Google, &format!("u{n}@gmail.example"))
                .ok_or("account")?;
            assert_eq!(rights.add_account(account), Ok(()));
        }
        let one_more =
            RemoteAccount::new(Provider::Microsoft, "x@outlook.example").ok_or("account")?;
        assert_eq!(rights.add_account(one_more), Err(Refusal::Full));
        assert_eq!(rights.accounts.len(), MAX_ACCOUNTS);
        assert!(!rights.remove_account(Provider::Microsoft, "u0@gmail.example"));
        assert!(!rights.remove_account(Provider::Google, "nobody@gmail.example"));
        assert!(rights.remove_account(Provider::Google, "u0@gmail.example"));
        assert!(!rights.remove_account(Provider::Google, "u0@gmail.example"));
        assert_eq!(rights.accounts.len(), MAX_ACCOUNTS - 1);
        Ok(())
    }

    #[test]
    fn lowering_never_raises_and_follows_the_two_write_capabilities() -> TestResult {
        let full = parsed(FULL)?;
        let alice = full
            .account(Provider::Google, "alice@gmail.example")
            .ok_or("alice")?;
        let mut same = alice.clone();
        same.lower(true, true);
        assert_eq!(&same, alice);
        let mut mail = alice.clone();
        mail.lower(false, true);
        assert_eq!(mail.mail, Some(Level::Read));
        assert_eq!(mail.calendars, alice.calendars);
        let mut calendar = alice.clone();
        calendar.lower(true, false);
        assert_eq!(calendar.mail, Some(Level::Write));
        assert!(calendar
            .calendars
            .iter()
            .all(|grant| grant.level == Level::Read));
        let carol = full
            .account(Provider::Google, "carol@gmail.example")
            .ok_or("carol")?;
        let mut none = carol.clone();
        none.lower(false, false);
        assert_eq!(&none, carol);
        let bob = full
            .account(Provider::Microsoft, "bob@outlook.example")
            .ok_or("bob")?;
        let mut read = bob.clone();
        read.lower(false, false);
        assert_eq!(&read, bob);
        Ok(())
    }
}
