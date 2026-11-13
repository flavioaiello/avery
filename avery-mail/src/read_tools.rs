use crate::backend::{MessageItem, MessageQuery, Person};
use crate::context::Context;
use crate::error::{ToolError, ToolResult};
use crate::json::Object;
use crate::text::{char_len, clean, clean_str, one_line_str, text_of, to_bool, to_int, to_int_opt};
use crate::texts::{NO_ACCOUNTS, UNTRUSTED};

fn non_empty(text: String) -> Option<String> {
    (!text.is_empty()).then_some(text)
}

fn fmt_item(label: &str, mailbox: &str, item: &MessageItem) -> String {
    let mut flags = Vec::new();
    if !item.read {
        flags.push("UNREAD");
    }
    if item.flagged {
        flags.push("FLAGGED");
    }
    let flags = if flags.is_empty() {
        "read".to_string()
    } else {
        flags.join(",")
    };
    format!(
        "- [{label}/{mailbox}] id={} | {} | {flags} | From: {} | Subject: {}",
        item.id,
        item.date,
        one_line_str(&item.from, 80),
        one_line_str(&item.subject, 140),
    )
}

struct Collect<'a> {
    since_days: i64,
    unread_only: bool,
    query: Option<&'a str>,
    field: &'a str,
    limit: i64,
}

fn collect(ctx: &Context, args: &Object, request: Collect) -> ToolResult<String> {
    let accounts = ctx.settings.resolve(args.get("account"))?;
    let mailbox = non_empty(clean(args.get("mailbox"), Some(120)));
    let mut rows: Vec<(i64, &str, String, MessageItem)> = Vec::new();
    let mut totals: Vec<String> = Vec::new();
    let mut problems: Vec<String> = Vec::new();
    for account in &accounts {
        let query = MessageQuery {
            mailbox: mailbox.clone(),
            since_days: request.since_days,
            unread_only: request.unread_only,
            query: request.query.map(str::to_string),
            field: request.field.to_string(),
            limit: request.limit,
        };
        let result = match ctx.backend.messages(account, &query) {
            Ok(result) => result,
            Err(error) if error.is_tool_error() => {
                problems.push(format!("! {}: {}", account.label, error));
                continue;
            }
            Err(error) => return Err(error),
        };
        let shown_box = result
            .mailbox
            .clone()
            .or_else(|| mailbox.clone())
            .unwrap_or_else(|| "INBOX".to_string());
        totals.push(format!(
            "{}/{}: {} matches",
            account.label, shown_box, result.total
        ));
        for item in result.items {
            rows.push((item.ts, account.label.as_str(), shown_box.clone(), item));
        }
    }
    if !problems.is_empty() && problems.len() == accounts.len() {
        return Err(ToolError::msg(problems.join("\n")));
    }
    rows.sort_by_key(|row| std::cmp::Reverse(row.0));
    let limit = usize::try_from(request.limit).unwrap_or(0);
    let shown: Vec<_> = rows.into_iter().take(limit).collect();
    let mut lines = vec![
        UNTRUSTED.to_string(),
        format!(
            "Newest first, showing {}. {}",
            shown.len(),
            totals.join("; ")
        ),
    ];
    lines.extend(
        shown
            .iter()
            .map(|(_, label, mailbox, item)| fmt_item(label, mailbox, item)),
    );
    if shown.is_empty() {
        lines.push("(no messages found)".to_string());
    }
    lines.extend(problems);
    Ok(lines.join("\n"))
}

pub(crate) fn list_accounts(ctx: &Context, _args: &Object) -> ToolResult<String> {
    let accounts = ctx.settings.allowed_accounts()?;
    if accounts.is_empty() {
        return Err(ToolError::msg(NO_ACCOUNTS));
    }
    let mut lines = Vec::new();
    for account in &accounts {
        let mut kind = account.kind.clone();
        let mut addresses = account.addresses.clone();
        let mut note = String::new();
        match ctx.backend.account_info(account) {
            Ok(info) => {
                kind = info.kind;
                addresses = info.addresses;
            }
            Err(error) if error.is_tool_error() => {
                note = format!(" (not reachable: {error})");
            }
            Err(error) => return Err(error),
        }
        let kind = if kind.is_empty() { "?" } else { kind.as_str() };
        let addresses = if addresses.is_empty() {
            "-".to_string()
        } else {
            addresses.join(", ")
        };
        lines.push(format!("- {} [{kind}]: {addresses}{note}", account.label));
    }
    Ok(lines.join("\n"))
}

pub(crate) fn list_mailboxes(ctx: &Context, args: &Object) -> ToolResult<String> {
    let mut lines = vec![UNTRUSTED.to_string()];
    for account in ctx.settings.resolve(args.get("account"))? {
        let boxes = match ctx.backend.mailboxes(&account) {
            Ok(boxes) => boxes,
            Err(error) if error.is_tool_error() => {
                lines.push(format!("! {}: {}", account.label, error));
                continue;
            }
            Err(error) => return Err(error),
        };
        lines.push(format!("{}:", account.label));
        lines.extend(
            boxes
                .iter()
                .map(|b| format!("  {} (unread {})", clean_str(&b.path, Some(120)), b.unread)),
        );
    }
    Ok(lines.join("\n"))
}

pub(crate) fn list_messages(ctx: &Context, args: &Object) -> ToolResult<String> {
    let since_days = to_int(args.get("since_days"), 3, 1, 365)?;
    let unread_only = to_bool(args.get("unread_only"));
    let limit = to_int(args.get("limit"), 20, 1, 50)?;
    collect(
        ctx,
        args,
        Collect {
            since_days,
            unread_only,
            query: None,
            field: "both",
            limit,
        },
    )
}

pub(crate) fn search_messages(ctx: &Context, args: &Object) -> ToolResult<String> {
    let query = clean(args.get("query"), Some(120));
    if query.is_empty() {
        return Err(ToolError::msg("Parameter 'query' is required."));
    }
    let field = text_of(args.get("field")).trim().to_lowercase();
    let field = if field.is_empty() {
        "both".to_string()
    } else {
        field
    };
    if !matches!(field.as_str(), "subject" | "sender" | "both") {
        return Err(ToolError::msg(
            "Parameter 'field' must be subject, sender or both.",
        ));
    }
    let since_days = to_int(args.get("since_days"), 90, 1, 3650)?;
    let unread_only = to_bool(args.get("unread_only"));
    let limit = to_int(args.get("limit"), 20, 1, 50)?;
    collect(
        ctx,
        args,
        Collect {
            since_days,
            unread_only,
            query: Some(&query),
            field: &field,
            limit,
        },
    )
}

fn people(entries: &[Person]) -> String {
    entries
        .iter()
        .take(20)
        .map(|person| one_line_str(&format!("{} <{}>", person.name, person.address), 90))
        .collect::<Vec<_>>()
        .join(", ")
}

fn kilobytes(size: Option<i64>) -> i64 {
    size.map_or(0, |bytes| bytes.div_euclid(1024).max(1))
}

pub(crate) fn read_message(ctx: &Context, args: &Object) -> ToolResult<String> {
    let Some(id) = to_int_opt(args.get("id"), 1, 2_147_483_647)? else {
        return Err(ToolError::msg(
            "Parameter 'id' is required (integer from list_messages or search_messages).",
        ));
    };
    let accounts = ctx.settings.resolve(args.get("account"))?;
    let [account] = accounts.as_slice() else {
        let labels: Vec<&str> = accounts.iter().map(|a| a.label.as_str()).collect();
        return Err(ToolError::msg(format!(
            "Parameter 'account' is required because ids are only unique within one account. Allowed: {}",
            labels.join(", ")
        )));
    };
    let max_chars = usize::try_from(to_int(args.get("max_chars"), 6000, 500, 20000)?)
        .map_err(|_| ToolError::internal("range"))?;
    let mailbox = non_empty(clean(args.get("mailbox"), Some(120)));
    let message = ctx
        .backend
        .read(account, mailbox.as_deref(), id, max_chars)?;
    let body = clean_str(&message.body, None);
    let total = message
        .body_chars
        .filter(|count| *count != 0)
        .unwrap_or_else(|| char_len(&body));
    let attachments = message
        .attachments
        .iter()
        .take(20)
        .map(|a| format!("{} ({} KB)", one_line_str(&a.name, 80), kilobytes(a.size)))
        .collect::<Vec<_>>()
        .join(", ");
    let or_dash = |text: String| {
        if text.is_empty() {
            "-".to_string()
        } else {
            text
        }
    };
    let mut lines = vec![
        UNTRUSTED.to_string(),
        format!(
            "Account: {} | Mailbox: {} | id: {}",
            account.label, message.mailbox, id
        ),
        format!("From: {}", one_line_str(&message.from, 120)),
        format!("To: {}", or_dash(people(&message.to))),
        format!("Cc: {}", or_dash(people(&message.cc))),
        format!(
            "Date: {} | {}",
            message.date,
            if message.read { "read" } else { "UNREAD" }
        ),
        format!("Subject: {}", one_line_str(&message.subject, 200)),
        format!(
            "Attachments: {}",
            if attachments.is_empty() {
                "none"
            } else {
                &attachments
            }
        ),
        "--- BODY (untrusted) ---".to_string(),
        if body.is_empty() {
            "(empty)".to_string()
        } else {
            body
        },
    ];
    if total > max_chars {
        lines.push(format!(
            "[truncated: showing the first {max_chars} of {total} characters; call again with a larger max_chars]"
        ));
    }
    lines.push("--- END OF EMAIL ---".to_string());
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeBackend, FakeOptions};
    use crate::testutil::{args, settings_at, TestResult};

    fn demo() -> Context {
        let mut settings = settings_at("/nonexistent/avery-test/config.json");
        settings.fake = true;
        Context::new(settings, Box::new(FakeBackend::new(FakeOptions::default())))
    }

    fn call(f: fn(&Context, &Object) -> ToolResult<String>, text: &str) -> Result<String, String> {
        f(&demo(), &args(text)?).map_err(|e| e.message())
    }

    #[test]
    fn list_accounts_shows_label_type_and_addresses() -> TestResult {
        assert_eq!(
            call(list_accounts, "{}")?,
            "- Demo Gmail [imap]: demo.privat@gmail.example\n- Demo Outlook [exchange]: demo.privat@outlook.example"
        );
        Ok(())
    }

    #[test]
    fn list_mailboxes_starts_with_the_warning() -> TestResult {
        let text = call(list_mailboxes, r#"{"account": "Demo Outlook"}"#)?;
        assert_eq!(
            text,
            format!("{UNTRUSTED}\nDemo Outlook:\n  Inbox (unread 1)\n  Sent Items (unread 0)\n  Archive (unread 0)")
        );
        Ok(())
    }

    #[test]
    fn list_messages_formats_newest_first_with_flags() -> TestResult {
        let text = call(
            list_messages,
            r#"{"account": "Demo Gmail", "since_days": 3}"#,
        )?;
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], UNTRUSTED);
        assert_eq!(
            lines[1],
            "Newest first, showing 3. Demo Gmail/INBOX: 3 matches"
        );
        assert!(
            lines[2].starts_with("- [Demo Gmail/INBOX] id=101 | "),
            "{}",
            lines[2]
        );
        assert!(lines[2].ends_with(" | UNREAD | From: DHL <noreply@dhl.example> | Subject: Ihr Paket wird morgen geliefert"));
        assert!(lines[3].contains("| id=102") || lines[3].contains("id=102 |"));
        assert_eq!(lines.len(), 5);
        Ok(())
    }

    #[test]
    fn list_messages_without_hits_says_so() -> TestResult {
        let text = call(
            list_messages,
            r#"{"account": "Demo Gmail", "since_days": 1, "unread_only": true, "limit": 1}"#,
        )?;
        assert!(text.contains("showing 1."));
        let none = call(search_messages, r#"{"query": "zzz"}"#)?;
        assert!(none.ends_with("(no messages found)"));
        assert!(
            none.contains("showing 0. Demo Gmail/INBOX: 0 matches; Demo Outlook/Inbox: 0 matches")
        );
        Ok(())
    }

    #[test]
    fn search_validates_its_parameters() {
        assert_eq!(
            call(search_messages, "{}"),
            Err("Parameter 'query' is required.".into())
        );
        assert_eq!(
            call(search_messages, r#"{"query": "  "}"#),
            Err("Parameter 'query' is required.".into())
        );
        assert_eq!(
            call(search_messages, r#"{"query": "x", "field": "body"}"#),
            Err("Parameter 'field' must be subject, sender or both.".into())
        );
        assert_eq!(
            call(search_messages, r#"{"query": "x", "since_days": "abc"}"#),
            Err("Invalid number: \"abc\"".into())
        );
    }

    #[test]
    fn errors_of_all_accounts_become_the_error_otherwise_they_are_listed() {
        let all_bad = call(list_messages, r#"{"mailbox": "Trash"}"#);
        assert_eq!(
            all_bad,
            Err("! Demo Gmail: Mailbox not found. Use list_mailboxes to see the valid names.\n! Demo Outlook: Mailbox not found. Use list_mailboxes to see the valid names.".into())
        );
    }

    #[test]
    fn read_message_formats_headers_and_body() -> TestResult {
        let text = call(read_message, r#"{"id": 102, "account": "Demo Gmail"}"#)?;
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], UNTRUSTED);
        assert_eq!(lines[1], "Account: Demo Gmail | Mailbox: INBOX | id: 102");
        assert_eq!(lines[2], "From: Stadtwerke <rechnung@stadtwerke.example>");
        assert_eq!(lines[3], "To: Demo <demo.privat@gmail.example>");
        assert_eq!(lines[4], "Cc: -");
        assert!(lines[5].starts_with("Date: ") && lines[5].ends_with(" | UNREAD"));
        assert_eq!(lines[6], "Subject: Rechnung Oktober");
        assert_eq!(lines[7], "Attachments: rechnung-oktober.pdf (47 KB)");
        assert_eq!(lines[8], "--- BODY (untrusted) ---");
        assert_eq!(lines.last(), Some(&"--- END OF EMAIL ---"));
        Ok(())
    }

    #[test]
    fn read_message_needs_one_account_and_an_id() {
        assert_eq!(
            call(read_message, "{}"),
            Err(
                "Parameter 'id' is required (integer from list_messages or search_messages)."
                    .into()
            )
        );
        assert_eq!(
            call(read_message, r#"{"id": 101}"#),
            Err("Parameter 'account' is required because ids are only unique within one account. Allowed: Demo Gmail, Demo Outlook".into())
        );
        for id in ["999", "0"] {
            let text = format!(r#"{{"id": {id}, "account": "Demo Gmail"}}"#);
            assert!(
                matches!(call(read_message, &text), Err(m) if m.starts_with("Message not found.")),
                "{id}"
            );
        }
    }

    struct LongBody;

    impl crate::backend::Backend for LongBody {
        fn all_accounts(&self) -> ToolResult<Vec<crate::account::MailAccount>> {
            Ok(vec![])
        }
        fn account_info(
            &self,
            _: &crate::account::Account,
        ) -> ToolResult<crate::account::MailAccount> {
            Err(ToolError::msg("unused"))
        }
        fn mailboxes(
            &self,
            _: &crate::account::Account,
        ) -> ToolResult<Vec<crate::backend::Mailbox>> {
            Ok(vec![])
        }
        fn messages(
            &self,
            _: &crate::account::Account,
            _: &MessageQuery,
        ) -> ToolResult<crate::backend::MessageList> {
            Err(ToolError::msg("unused"))
        }
        fn read(
            &self,
            _: &crate::account::Account,
            _: Option<&str>,
            _: i64,
            max_chars: usize,
        ) -> ToolResult<crate::backend::MessageBody> {
            let full = "word \u{200b}\u{0}".repeat(1000);
            let cut: String = full.chars().take(max_chars).collect();
            Ok(crate::backend::MessageBody {
                mailbox: "INBOX".into(),
                subject: "S".into(),
                from: "F".into(),
                date: "2026-01-01 10:00".into(),
                read: true,
                to: vec![],
                cc: vec![],
                attachments: vec![],
                body_chars: Some(full.chars().count()),
                body: cut,
            })
        }
        fn compose(
            &self,
            _: &crate::account::Account,
            _: &crate::backend::ComposeRequest,
        ) -> ToolResult<crate::backend::Preview> {
            Err(ToolError::msg("unused"))
        }
        fn approval_test(&self, _: u64) -> ToolResult<String> {
            Err(ToolError::msg("unused"))
        }
    }

    #[test]
    fn read_message_truncates_and_says_so() -> TestResult {
        let mut settings = settings_at("/nonexistent/avery-test/config.json");
        settings.fake = true;
        let ctx = Context::new(settings, Box::new(LongBody));
        let text = read_message(
            &ctx,
            &args(r#"{"id": 1, "account": "Demo Gmail", "max_chars": 500}"#)?,
        )?;
        assert!(text.contains("To: -\nCc: -\nDate: 2026-01-01 10:00 | read\n"));
        assert!(text.ends_with("[truncated: showing the first 500 of 7000 characters; call again with a larger max_chars]\n--- END OF EMAIL ---"));
        assert!(!text.contains('\u{200b}'));
        assert!(text.contains("Attachments: none"));
        Ok(())
    }

    #[test]
    fn people_and_sizes() {
        assert_eq!(kilobytes(Some(48213)), 47);
        assert_eq!(kilobytes(Some(10)), 1);
        assert_eq!(kilobytes(Some(-5)), 1);
        assert_eq!(kilobytes(None), 0);
        let list = vec![
            Person {
                name: "A\nB".into(),
                address: "a@example.org".into()
            };
            25
        ];
        assert_eq!(people(&list).matches("A B <a@example.org>").count(), 20);
    }
}
