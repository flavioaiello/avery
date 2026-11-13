use crate::json::Json;
use crate::text::text_of;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MailAccount {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) addresses: Vec<String>,
}

impl MailAccount {
    pub(crate) fn from_json(value: &Json) -> Option<MailAccount> {
        let object = value.as_object()?;
        Some(MailAccount {
            id: text_of(object.get("id")),
            name: text_of(object.get("name")),
            kind: text_of(object.get("type")),
            addresses: string_list(object.get("addresses")),
        })
    }
}

pub(crate) fn string_list(value: Option<&Json>) -> Vec<String> {
    match value {
        Some(Json::Arr(items)) => items
            .iter()
            .filter_map(Json::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Account {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) addresses: Vec<String>,
    pub(crate) label: String,
}

impl Account {
    pub(crate) fn is_named(&self, wanted: &str) -> bool {
        self.label.to_lowercase() == wanted
            || self.name.to_lowercase() == wanted
            || self.addresses.iter().any(|a| a.to_lowercase() == wanted)
    }
}

pub(crate) fn with_labels(entries: Vec<MailAccount>) -> Vec<Account> {
    let names: Vec<String> = entries.iter().map(|e| e.name.to_lowercase()).collect();
    entries
        .into_iter()
        .map(|entry| {
            let own = entry.name.to_lowercase();
            let duplicate = names.iter().filter(|name| **name == own).count() > 1;
            let address = match entry.addresses.first() {
                Some(address) => address.clone(),
                None => entry.id.chars().take(8).collect(),
            };
            let label = if duplicate {
                format!("{} <{}>", entry.name, address)
            } else {
                entry.name.clone()
            };
            Account {
                id: entry.id,
                name: entry.name,
                kind: entry.kind,
                addresses: entry.addresses,
                label,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestResult;

    fn mail(id: &str, name: &str, addresses: &[&str]) -> MailAccount {
        MailAccount {
            id: id.into(),
            name: name.into(),
            kind: "imap".into(),
            addresses: addresses.iter().map(|a| a.to_string()).collect(),
        }
    }

    #[test]
    fn labels_are_names_unless_two_accounts_share_one() {
        let accounts = with_labels(vec![
            mail("A1", "Privat", &["a@example.org"]),
            mail("B2", "Firma", &["b@example.org"]),
        ]);
        assert_eq!(accounts[0].label, "Privat");
        assert_eq!(accounts[1].label, "Firma");
    }

    #[test]
    fn duplicate_names_get_the_address_or_the_id_prefix() {
        let accounts = with_labels(vec![
            mail("A1", "Gmail", &["a@example.org"]),
            mail("0123456789ABC", "gmail", &[]),
            mail("C3", "Other", &[]),
        ]);
        assert_eq!(accounts[0].label, "Gmail <a@example.org>");
        assert_eq!(accounts[1].label, "gmail <01234567>");
        assert_eq!(accounts[2].label, "Other");
    }

    #[test]
    fn accounts_are_found_by_label_name_or_address_ignoring_case() {
        let account = with_labels(vec![mail(
            "A1",
            "Demo Gmail",
            &["Demo.Privat@Gmail.example"],
        )])
        .remove(0);
        assert!(account.is_named("demo gmail"));
        assert!(account.is_named("demo.privat@gmail.example"));
        assert!(!account.is_named("gmail"));
    }

    #[test]
    fn decodes_the_script_answer_and_skips_non_text_addresses() -> TestResult {
        let value =
            crate::json::parse(r#"{"id": "X", "name": "N", "addresses": ["a@example.org", 5]}"#)?;
        let account = MailAccount::from_json(&value).ok_or("account")?;
        assert_eq!(account.id, "X");
        assert_eq!(account.kind, "");
        assert_eq!(account.addresses, ["a@example.org"]);
        assert!(MailAccount::from_json(&Json::Null).is_none());
        Ok(())
    }
}
