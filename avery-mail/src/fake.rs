//! Synthetic demo backend (`MAIL_MCP_FAKE=1`). No Mail access.

use crate::account::MailAccount;

fn account(id: &str, name: &str, kind: &str, address: &str) -> MailAccount {
    MailAccount {
        id: id.to_string(),
        name: name.to_string(),
        kind: kind.to_string(),
        addresses: vec![address.to_string()],
    }
}

/// The demo accounts. `FAKE-WORK` must never be exposed.
pub fn accounts() -> Vec<MailAccount> {
    vec![
        account(
            "FAKE-GMAIL",
            "Demo Gmail",
            "imap",
            "demo.privat@gmail.example",
        ),
        account(
            "FAKE-OUTLOOK",
            "Demo Outlook",
            "exchange",
            "demo.privat@outlook.example",
        ),
        account("FAKE-WORK", "Demo Firma", "exchange", "demo@firma.example"),
    ]
}
