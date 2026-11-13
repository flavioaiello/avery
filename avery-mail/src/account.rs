//! Accounts as Mail reports them and accounts on the allowlist.

use crate::json::Json;
use crate::pycompat;

/// An account as reported by Mail (or by the fake backend).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailAccount {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub addresses: Vec<String>,
}

impl MailAccount {
    /// Reads the answer of the JXA script. Missing fields become empty.
    pub fn from_json(value: &Json) -> Option<MailAccount> {
        let object = value.as_object()?;
        Some(MailAccount {
            id: pycompat::text_of(object.get("id")),
            name: pycompat::text_of(object.get("name")),
            kind: pycompat::text_of(object.get("type")),
            addresses: string_list(object.get("addresses")),
        })
    }

    pub fn to_json(&self) -> Json {
        Json::object(vec![
            ("id", Json::from(&self.id)),
            ("name", Json::from(&self.name)),
            ("type", Json::from(&self.kind)),
            ("addresses", Json::from(self.addresses.as_slice())),
        ])
    }
}

/// The elements of a JSON array as text; anything else is an empty list.
pub fn string_list(value: Option<&Json>) -> Vec<String> {
    match value {
        Some(Json::Arr(items)) => items.iter().map(pycompat::str_of).collect(),
        _ => Vec::new(),
    }
}

/// An account the agent may read (and perhaps write). `label` is the name shown
/// to the model: the account name, or `name <address>` when two accounts share a name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub addresses: Vec<String>,
    pub label: String,
}

impl Account {
    /// Whether `wanted` (already stripped and lower-cased) names this account:
    /// its label, its name or one of its addresses.
    pub fn is_named(&self, wanted: &str) -> bool {
        pycompat::lower(&self.label) == wanted
            || pycompat::lower(&self.name) == wanted
            || self.addresses.iter().any(|a| pycompat::lower(a) == wanted)
    }
}

/// Gives every account its label.
pub fn with_labels(entries: Vec<MailAccount>) -> Vec<Account> {
    let names: Vec<String> = entries.iter().map(|e| pycompat::lower(&e.name)).collect();
    entries
        .into_iter()
        .map(|entry| {
            let own = pycompat::lower(&entry.name);
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
    fn decodes_the_script_answer_leniently() {
        let value =
            crate::json::parse(r#"{"id": "X", "name": "N", "addresses": ["a@example.org", 5]}"#)
                .expect("json");
        let account = MailAccount::from_json(&value).expect("account");
        assert_eq!(account.id, "X");
        assert_eq!(account.kind, "");
        assert_eq!(account.addresses, ["a@example.org", "5"]);
        assert!(MailAccount::from_json(&Json::Null).is_none());
    }
}
