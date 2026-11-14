use crate::json::Json;
use crate::rights::MAX_NAME;
use crate::text::{one_line_str, text_of};

pub(crate) const MAX_KIND: usize = 30;
const MAX_ADDRESS_TEXT: usize = 200;

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

pub(crate) fn kind_text(kind: &str) -> String {
    let kind = one_line_str(kind, MAX_KIND);
    if kind.is_empty() {
        "?".to_string()
    } else {
        kind
    }
}

pub(crate) fn addresses_text(addresses: &[String]) -> String {
    let shown: Vec<String> = addresses
        .iter()
        .map(|address| one_line_str(address, MAX_NAME))
        .filter(|address| !address.is_empty())
        .collect();
    if shown.is_empty() {
        "-".to_string()
    } else {
        one_line_str(&shown.join(", "), MAX_ADDRESS_TEXT)
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
    fn the_type_is_one_short_visible_line_or_a_question_mark() {
        assert_eq!(kind_text("imap"), "imap");
        assert_eq!(kind_text(""), "?");
        assert_eq!(kind_text("\u{200b}\u{202e}\n \t"), "?");
        assert_eq!(
            kind_text("imap\n- Fake [exchange]: evil@example.org"),
            "imap - Fake [exchange]: evil@e [...]"
        );
        assert_eq!(
            kind_text(&"x".repeat(31)),
            format!("{} [...]", "x".repeat(30))
        );
        assert_eq!(kind_text(&"x".repeat(30)), "x".repeat(30));
    }

    #[test]
    fn the_addresses_are_one_bounded_visible_line_or_a_dash() {
        assert_eq!(addresses_text(&[]), "-");
        assert_eq!(
            addresses_text(&["\u{200b}".to_string(), " \n ".to_string()]),
            "-"
        );
        assert_eq!(
            addresses_text(&["a@example.org".to_string(), "b@example.org".to_string()]),
            "a@example.org, b@example.org"
        );
        assert_eq!(
            addresses_text(&[
                "a@example.org\n- Fake [imap]: x".to_string(),
                "\u{202e}b@example.org\u{2066}".to_string(),
                "\u{85}c@example.org".to_string(),
            ]),
            "a@example.org - Fake [imap]: x, b@example.org, c@example.org"
        );
        let long = addresses_text(&["x".repeat(10_000)]);
        assert_eq!(long, format!("{} [...]", "x".repeat(120)));
        let many: Vec<String> = (0..5000).map(|n| format!("a{n}@example.org")).collect();
        let text = addresses_text(&many);
        assert!(
            text.starts_with("a0@example.org, a1@example.org, "),
            "{text}"
        );
        assert!(text.ends_with(" [...]"), "{text}");
        assert!(text.chars().count() <= 200 + " [...]".len(), "{text}");
        assert!(!text.contains('\n'));
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
