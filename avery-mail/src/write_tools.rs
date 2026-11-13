use crate::account::Account;
use crate::backend::{ComposeRequest, Mode, Preview, ReplySpec};
use crate::context::Context;
use crate::error::{ToolError, ToolResult};
use crate::journal;
use crate::json::{Json, Object};
use crate::sendlog::{self, LogEntry, Outcome};
use crate::text::{
    bracketed, char_len, clean, clean_str, one_line, one_line_str, to_bool, to_int_opt,
};
use crate::texts::{
    MAX_DRAFT_CHARS, MAX_RECIPIENTS, MAX_SEND_CHARS, MAX_SEND_LINES, MAX_SUBJECT, UNTRUSTED,
};

const COMPOSE_ARGS: [&str; 8] = [
    "account",
    "to",
    "cc",
    "subject",
    "body",
    "reply_to_id",
    "mailbox",
    "reply_all",
];
const MAX_ADDRESS_PIECES: usize = 50;
const MAX_ADDRESS_CHARS: usize = 254;

fn valid_label(label: &str) -> bool {
    let edge = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric());
    (1..=63).contains(&label.len())
        && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && edge(label.chars().next())
        && edge(label.chars().next_back())
}

fn valid_address(address: &str) -> bool {
    if address.chars().count() > MAX_ADDRESS_CHARS {
        return false;
    }
    let Some((local, domain)) = address.split_once('@') else {
        return false;
    };
    let local_chars = ".!#$%&'*+/=?^_`{|}~-";
    if !(1..=64).contains(&local.chars().count())
        || !local
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || local_chars.contains(c))
    {
        return false;
    }
    let labels: Vec<&str> = domain.split('.').collect();
    let Some((top, inner)) = labels.split_last() else {
        return false;
    };
    !inner.is_empty()
        && inner.iter().all(|label| valid_label(label))
        && (2..=63).contains(&top.len())
        && valid_label(top)
        && top.chars().any(|c| c.is_ascii_alphabetic())
}

fn parse_addresses(value: Option<&Json>, label: &str) -> ToolResult<Vec<String>> {
    let parts: Vec<&Json> = match value {
        None | Some(Json::Null) => return Ok(Vec::new()),
        Some(Json::Str(text)) if text.is_empty() => return Ok(Vec::new()),
        Some(Json::Arr(items)) => items.iter().collect(),
        Some(other) => vec![other],
    };
    let mut found: Vec<String> = Vec::new();
    let mut pieces = 0usize;
    for part in parts {
        let Json::Str(text) = part else {
            return Err(ToolError::msg(format!(
                "'{label}' must be plain email addresses."
            )));
        };
        for piece in text.split([',', ';']) {
            let piece = piece.trim();
            if piece.is_empty() {
                continue;
            }
            pieces += 1;
            if pieces > MAX_ADDRESS_PIECES {
                return Err(ToolError::msg(format!(
                    "Too many recipients (max {MAX_RECIPIENTS})."
                )));
            }
            let address = bracketed(piece).unwrap_or(piece).trim();
            if !valid_address(address) {
                return Err(ToolError::msg(format!(
                    "Invalid email address in '{label}': {}. Use plain addresses like name@example.com.",
                    one_line_str(piece, 60)
                )));
            }
            if !found
                .iter()
                .any(|known| known.eq_ignore_ascii_case(address))
            {
                found.push(address.to_string());
            }
        }
    }
    Ok(found)
}

fn unknown_parameters(args: &Object) -> Option<ToolError> {
    let unknown: Vec<&str> = args
        .keys()
        .filter(|key| !COMPOSE_ARGS.contains(key))
        .collect();
    if unknown.is_empty() {
        return None;
    }
    let hint = if unknown.iter().any(|key| {
        matches!(
            key.to_lowercase().as_str(),
            "bcc" | "attachments" | "attachment"
        )
    }) {
        " Bcc and attachments are not supported."
    } else {
        ""
    };
    let names: Vec<String> = unknown.iter().map(|key| clean_str(key, Some(30))).collect();
    Some(ToolError::msg(format!(
        "Unknown parameter(s): {}.{hint}",
        names.join(", ")
    )))
}

pub(crate) fn compose_request(
    ctx: &Context,
    args: &Object,
    mode: Mode,
) -> ToolResult<(Account, i64, ComposeRequest)> {
    if let Some(error) = unknown_parameters(args) {
        return Err(error);
    }
    let (account, settings) = ctx.settings.resolve_writable(args.get("account"))?;
    let body = clean(args.get("body"), None);
    if body.is_empty() {
        return Err(ToolError::msg("Parameter 'body' is required."));
    }
    let length = char_len(&body);
    if mode == Mode::Send && length > MAX_SEND_CHARS {
        return Err(ToolError::msg(format!(
            "The text has {length} characters but send_message allows {MAX_SEND_CHARS}. Use create_draft so that the user can read and send it in Mail."
        )));
    }
    let lines = body.lines().count();
    if mode == Mode::Send && lines > MAX_SEND_LINES {
        return Err(ToolError::msg(format!(
            "The text has {lines} lines but send_message allows {MAX_SEND_LINES}. Use create_draft so that the user can read and send it in Mail."
        )));
    }
    if length > MAX_DRAFT_CHARS {
        return Err(ToolError::msg(format!(
            "The text is too long (max {MAX_DRAFT_CHARS} characters)."
        )));
    }
    let to = parse_addresses(args.get("to"), "to")?;
    let cc: Vec<String> = parse_addresses(args.get("cc"), "cc")?
        .into_iter()
        .filter(|address| !to.iter().any(|known| known.eq_ignore_ascii_case(address)))
        .collect();
    let subject = one_line(args.get("subject"), MAX_SUBJECT + 1);
    if char_len(&subject) > MAX_SUBJECT {
        return Err(ToolError::msg(format!(
            "The subject is too long (max {MAX_SUBJECT} characters)."
        )));
    }
    let reply_id = to_int_opt(
        args.get("reply_to_id"),
        i64::from(i32::MIN),
        i64::from(i32::MAX),
    )?;
    let reply = match reply_id {
        Some(id) if id < 1 => return Err(ToolError::msg(
            "Parameter 'reply_to_id' must be a message id from list_messages or search_messages.",
        )),
        Some(id) => {
            if !to.is_empty() || !cc.is_empty() || !subject.is_empty() {
                return Err(ToolError::msg(
                    "For an answer do not pass 'to', 'cc' or 'subject': Mail takes the recipients and the subject from the original message.",
                ));
            }
            let mailbox = clean(args.get("mailbox"), Some(120));
            Some(ReplySpec {
                mailbox: (!mailbox.is_empty()).then_some(mailbox),
                id,
                all: to_bool(args.get("reply_all")),
            })
        }
        None => {
            if to.is_empty() {
                return Err(ToolError::msg(
                    "Parameter 'to' is required (or reply_to_id for an answer).",
                ));
            }
            if subject.is_empty() {
                return Err(ToolError::msg("Parameter 'subject' is required."));
            }
            if to.len() + cc.len() > MAX_RECIPIENTS {
                return Err(ToolError::msg(format!(
                    "Too many recipients (max {MAX_RECIPIENTS})."
                )));
            }
            None
        }
    };
    Ok((
        account,
        settings.max_per_hour,
        ComposeRequest {
            mode,
            to,
            cc,
            subject,
            body,
            reply,
        },
    ))
}

fn log_attempt(
    ctx: &Context,
    account: &Account,
    request: &ComposeRequest,
    outcome: Outcome,
    preview: Option<&Preview>,
    id: Option<&str>,
    strict: bool,
) -> ToolResult<()> {
    let entry = LogEntry::for_attempt(
        journal::now_seconds(),
        &account.label,
        request,
        outcome,
        preview,
        id,
    );
    sendlog::append(&ctx.settings, &entry, strict)
}

fn summary(preview: &Preview) -> String {
    let to: Vec<String> = preview.to.iter().map(|x| one_line_str(x, 100)).collect();
    let mut lines = vec![
        format!("From: {}", one_line_str(&preview.from, 120)),
        format!(
            "To: {}",
            if to.is_empty() {
                "-".to_string()
            } else {
                to.join(", ")
            }
        ),
    ];
    if !preview.cc.is_empty() {
        let cc: Vec<String> = preview.cc.iter().map(|x| one_line_str(x, 100)).collect();
        lines.push(format!("Cc: {}", cc.join(", ")));
    }
    lines.push(format!(
        "Subject: {}",
        one_line_str(&preview.subject, MAX_SUBJECT)
    ));
    lines.join("\n")
}

fn guard(ctx: &Context, mode: Mode, max_per_hour: i64) -> ToolResult<()> {
    let now = journal::now_seconds();
    let history = sendlog::read_window(&ctx.settings, now)?;
    match mode {
        Mode::Send => sendlog::check_send_limits(&history.entries, now, max_per_hour)?,
        Mode::Draft => sendlog::check_draft_limit(&history.entries, now)?,
    }
    sendlog::ensure_writable(&ctx.settings)
}

pub(crate) fn perform_draft(
    ctx: &Context,
    account: &Account,
    request: &ComposeRequest,
) -> ToolResult<Preview> {
    guard(ctx, Mode::Draft, 0)?;
    match ctx.backend.compose(account, request) {
        Ok(preview) => {
            log_attempt(
                ctx,
                account,
                request,
                Outcome::Drafted,
                Some(&preview),
                None,
                false,
            )?;
            Ok(preview)
        }
        Err(error) => {
            if error.is_tool_error() {
                log_attempt(ctx, account, request, Outcome::Error, None, None, false)?;
            }
            Err(error)
        }
    }
}

pub(crate) fn perform_send(
    ctx: &Context,
    account: &Account,
    request: &ComposeRequest,
    max_per_hour: i64,
) -> ToolResult<(Preview, Outcome)> {
    guard(ctx, Mode::Send, max_per_hour)?;
    let id = journal::attempt_id()?;
    log_attempt(
        ctx,
        account,
        request,
        Outcome::Pending,
        None,
        Some(&id),
        true,
    )?;
    let preview = match ctx.backend.compose(account, request) {
        Ok(preview) => preview,
        Err(ToolError::Msg(message)) => {
            log_attempt(
                ctx,
                account,
                request,
                Outcome::Error,
                None,
                Some(&id),
                false,
            )?;
            return Err(ToolError::Msg(message));
        }
        Err(other) => return Err(other),
    };
    let outcome = Outcome::from_status(&preview.status);
    log_attempt(
        ctx,
        account,
        request,
        outcome,
        Some(&preview),
        Some(&id),
        false,
    )?;
    Ok((preview, outcome))
}

pub(crate) fn create_draft(ctx: &Context, args: &Object) -> ToolResult<String> {
    let (account, _, request) = compose_request(ctx, args, Mode::Draft)?;
    let preview = perform_draft(ctx, &account, &request)?;
    let note = if preview.saved.unwrap_or(true) {
        ""
    } else {
        " (the window is open; Mail saves it to Drafts when closed)"
    };
    Ok(format!(
        "Draft opened in Mail{note}. NOTHING was sent. Tell the user to review it and send it in Mail.\n{UNTRUSTED}\n{}",
        summary(&preview)
    ))
}

pub(crate) fn send_message(ctx: &Context, args: &Object) -> ToolResult<String> {
    let (account, max_per_hour, request) = compose_request(ctx, args, Mode::Send)?;
    let (preview, outcome) = perform_send(ctx, &account, &request, max_per_hour)?;
    Ok(match outcome {
        Outcome::Sent => format!(
            "Sent: the user approved and Mail took the message for delivery.\n{UNTRUSTED}\n{}",
            summary(&preview)
        ),
        Outcome::Timeout => "The user did not answer the approval dialog in time. NOTHING was sent. Ask the user whether to try again.".to_string(),
        _ => "The user did NOT approve the message in the confirmation dialog. NOTHING was sent. Do not retry unless the user asks you to.".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::string_list;
    use crate::fake::FakeOptions;
    use crate::testutil::{args, j, log_entry, settings_at, TempDir, TestResult};
    use std::error::Error;
    use std::fs;
    use std::io;

    const BODY_5000: usize = 5000;

    fn writer(dir: &TempDir, accounts: &str, approve: &str, options: FakeOptions) -> Context {
        let mut settings = settings_at(&dir.path("config.json"));
        settings.fake = true;
        settings.fake_write = accounts.to_string();
        Context::demo(
            settings,
            FakeOptions {
                approve: approve.to_string(),
                outbox: Some(dir.path("outbox.jsonl")),
                ..options
            },
        )
    }

    fn gmail(dir: &TempDir, approve: &str) -> Context {
        writer(dir, "FAKE-GMAIL", approve, FakeOptions::default())
    }

    fn mail() -> Result<Object, String> {
        args(
            r#"{"account": "Demo Gmail", "to": ["anna@example.org"], "subject": "Hallo", "body": "Kurzer Text."}"#,
        )
    }

    fn with(changes: &[(&str, &str)]) -> Result<Object, Box<dyn Error>> {
        let mut object = mail()?;
        for (key, value) in changes {
            if *value == "<remove>" {
                object.remove(key);
            } else {
                object.insert(*key, j(value)?);
            }
        }
        Ok(object)
    }

    fn outbox(dir: &TempDir) -> Result<Vec<Json>, Box<dyn Error>> {
        match fs::read_to_string(dir.path("outbox.jsonl")) {
            Ok(text) => text
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| Ok(j(line)?))
                .collect(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error.into()),
        }
    }

    fn field(entry: &Json, key: &str) -> String {
        crate::text::text_of(entry.as_object().and_then(|object| object.get(key)))
    }

    fn list(entry: &Json, key: &str) -> Vec<String> {
        string_list(entry.as_object().and_then(|object| object.get(key)))
    }

    fn outcomes(ctx: &Context) -> Result<Vec<Outcome>, ToolError> {
        Ok(sendlog::read_entries(&ctx.settings, 300)?
            .entries
            .iter()
            .map(|entry| entry.outcome)
            .collect())
    }

    fn seed(ctx: &Context, entries: &[LogEntry]) -> ToolResult<()> {
        entries
            .iter()
            .try_for_each(|entry| sendlog::append(&ctx.settings, entry, true))
    }

    fn refused(result: ToolResult<String>, needle: &str) -> bool {
        matches!(result, Err(ToolError::Msg(message)) if message.contains(needle))
    }

    fn address(local: &str, domain: &str) -> String {
        format!("{local}@{domain}")
    }

    #[test]
    fn addresses_follow_the_documented_shape() {
        let good = [
            "anna@example.org".to_string(),
            "a.b+c@sub.mail.example".to_string(),
            "x@a-b.mail.example".to_string(),
            "o'neil@example.org".to_string(),
            address("1", "2.ab"),
            address("name", "xn--e1afmkfd.xn--p1ai"),
        ];
        for item in &good {
            assert!(valid_address(item), "{item}");
        }
        let mut bad: Vec<String> = [
            "",
            "anna",
            "@example.org",
            "anna@",
            "anna@example",
            "anna@example.",
            "anna@exa mple.org",
            "anna@example.o",
            "anna@example.123",
            "anna@example.org-",
            "anna@example.-org",
            "an na@example.org",
            "anna@@example.org",
            "anna@example.org\n",
            "a\u{e4}@example.org",
            "anna@exa\u{e4}mple.org",
        ]
        .iter()
        .map(|item| item.to_string())
        .collect();
        for (local, domain) in [
            ("anna", ".example.org"),
            ("anna", "example..org"),
            ("anna", "-example.org"),
            ("anna", "example-.org"),
        ] {
            bad.push(address(local, domain));
        }
        bad.push(format!("{}@example.org", "a".repeat(65)));
        bad.push(format!("a@{}.org", "b".repeat(64)));
        bad.push(format!("a@{}.org", vec!["b".repeat(60); 5].join(".")));
        for item in &bad {
            assert!(!valid_address(item), "{item:?}");
        }
    }

    #[test]
    fn addresses_are_split_unwrapped_and_deduplicated() -> TestResult {
        let parsed = |text: &str| -> Result<Vec<String>, Box<dyn Error>> {
            Ok(parse_addresses(Some(&j(text)?), "to")?)
        };
        assert_eq!(parsed("null")?, Vec::<String>::new());
        assert_eq!(parse_addresses(None, "to")?, Vec::<String>::new());
        assert_eq!(parsed("\"\"")?, Vec::<String>::new());
        assert_eq!(parsed("[]")?, Vec::<String>::new());
        assert_eq!(parsed("\"  \"")?, Vec::<String>::new());
        assert_eq!(parsed("\"a@example.org\"")?, ["a@example.org"]);
        assert_eq!(
            parsed("\"Anna Muster <A@Example.org>; b@example.org, ,\"")?,
            ["A@Example.org", "b@example.org"]
        );
        assert_eq!(
            parsed("[\"a@example.org\", \"A@EXAMPLE.ORG\", \"c@example.org\"]")?,
            ["a@example.org", "c@example.org"]
        );
        Ok(())
    }

    #[test]
    fn non_text_address_values_are_refused() -> TestResult {
        for bad in ["5", "[5]", "{}", "true", "[[\"a@example.org\"]]"] {
            assert_eq!(
                parse_addresses(Some(&j(bad)?), "cc"),
                Err(ToolError::msg("'cc' must be plain email addresses.")),
                "{bad}"
            );
        }
        assert_eq!(
            parse_addresses(Some(&j("\"not-an-address\"")?), "to"),
            Err(ToolError::msg(
                "Invalid email address in 'to': not-an-address. Use plain addresses like name@example.com."
            ))
        );
        Ok(())
    }

    #[test]
    fn address_work_is_bounded() -> TestResult {
        let many = vec!["a@example.org"; 100].join(",");
        assert_eq!(
            parse_addresses(Some(&Json::Str(many)), "to"),
            Err(ToolError::msg("Too many recipients (max 5)."))
        );
        Ok(())
    }

    #[test]
    fn writing_off_refuses_both_tools() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = writer(&dir, "", "yes", FakeOptions::default());
        assert!(refused(send_message(&ctx, &mail()?), "not enabled"));
        assert!(refused(create_draft(&ctx, &mail()?), "not enabled"));
        assert!(outbox(&dir)?.is_empty());
        assert!(outcomes(&ctx)?.is_empty());
        Ok(())
    }

    #[test]
    fn a_declined_send_is_logged_before_and_after_and_blocks_a_retry() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = gmail(&dir, "no");
        let text = send_message(&ctx, &mail()?)?;
        assert!(text.contains("did NOT approve"));
        assert!(outbox(&dir)?.is_empty());
        let entries = sendlog::read_entries(&ctx.settings, 300)?.entries;
        let outcomes: Vec<Outcome> = entries.iter().map(|entry| entry.outcome).collect();
        assert_eq!(outcomes, [Outcome::Pending, Outcome::Declined]);
        assert!(entries[0].id.is_some());
        assert_eq!(entries[0].id, entries[1].id);
        assert!(refused(send_message(&ctx, &mail()?), "just declined"));
        Ok(())
    }

    #[test]
    fn a_draft_is_created_and_nothing_is_sent() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = gmail(&dir, "no");
        let text = create_draft(&ctx, &mail()?)?;
        assert!(text.contains("NOTHING was sent"));
        assert!(text.contains("To: anna@example.org"));
        let sent = outbox(&dir)?;
        assert_eq!(sent.len(), 1);
        assert_eq!(field(&sent[0], "status"), "drafted");
        assert_eq!(list(&sent[0], "to"), ["anna@example.org"]);
        assert_eq!(outcomes(&ctx)?, [Outcome::Drafted]);
        Ok(())
    }

    #[test]
    fn long_texts_are_drafts_only() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = gmail(&dir, "no");
        let long = Json::Str("x".repeat(BODY_5000));
        let mut draft = mail()?;
        draft.insert("body", long.clone());
        assert!(create_draft(&ctx, &draft).is_ok());
        assert!(refused(send_message(&ctx, &draft), "create_draft"));
        let mut huge = mail()?;
        huge.insert("body", Json::Str("x".repeat(20_001)));
        assert!(refused(create_draft(&ctx, &huge), "too long (max 20000"));
        let mut edge = mail()?;
        edge.insert("body", Json::Str("x".repeat(1500)));
        assert!(send_message(&ctx, &edge).is_ok());
        Ok(())
    }

    #[test]
    fn only_accounts_that_may_write_are_reachable() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = gmail(&dir, "no");
        assert!(refused(
            send_message(&ctx, &with(&[("account", "\"Demo Outlook\"")])?),
            "Accounts that may write: Demo Gmail"
        ));
        assert!(refused(
            send_message(&ctx, &with(&[("account", "\"Demo Firma\"")])?),
            "Unknown account"
        ));
        assert!(create_draft(
            &ctx,
            &with(&[("account", "\"demo.privat@gmail.example\"")])?
        )
        .is_ok());
        assert!(create_draft(&ctx, &with(&[("account", "<remove>")])?).is_ok());
        let two = writer(
            &dir,
            "FAKE-GMAIL,FAKE-OUTLOOK",
            "no",
            FakeOptions::default(),
        );
        assert!(refused(
            create_draft(&two, &with(&[("account", "<remove>")])?),
            "Parameter 'account' is required"
        ));
        assert!(refused(
            create_draft(&two, &with(&[("account", "[\"Demo Gmail\"]")])?),
            "must be text"
        ));
        Ok(())
    }

    #[test]
    fn invalid_requests_are_refused_with_a_reason() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = gmail(&dir, "no");
        let six = "[\"a0@example.org\", \"a1@example.org\", \"a2@example.org\", \"a3@example.org\", \"a4@example.org\", \"a5@example.org\"]";
        let long_subject = format!("\"{}\"", "s".repeat(300));
        let cases: Vec<(Vec<(&str, &str)>, &str)> = vec![
            (
                vec![("to", "[\"not-an-address\"]")],
                "Invalid email address",
            ),
            (vec![("to", six)], "Too many recipients"),
            (vec![("body", "\"  \\n \"")], "'body' is required"),
            (
                vec![("bcc", "[\"x@example.org\"]")],
                "Bcc and attachments are not supported",
            ),
            (
                vec![("Attachments", "[\"/etc/passwd\"]")],
                "Bcc and attachments are not supported",
            ),
            (vec![("bogus", "1")], "Unknown parameter(s): bogus."),
            (vec![("to", "<remove>")], "'to' is required"),
            (vec![("subject", "<remove>")], "'subject' is required"),
            (
                vec![("subject", long_subject.as_str())],
                "subject is too long",
            ),
            (vec![("reply_to_id", "101")], "do not pass"),
            (
                vec![
                    ("to", "<remove>"),
                    ("subject", "<remove>"),
                    ("reply_to_id", "0"),
                ],
                "reply_to_id",
            ),
            (
                vec![
                    ("to", "<remove>"),
                    ("subject", "<remove>"),
                    ("reply_to_id", "-5"),
                ],
                "reply_to_id",
            ),
            (
                vec![
                    ("to", "<remove>"),
                    ("subject", "<remove>"),
                    ("reply_to_id", "\"abc\""),
                ],
                "Invalid number",
            ),
        ];
        for (changes, needle) in cases {
            let args = with(&changes)?;
            assert!(refused(create_draft(&ctx, &args), needle), "{changes:?}");
            assert!(refused(send_message(&ctx, &args), needle), "{changes:?}");
        }
        assert!(outbox(&dir)?.is_empty());
        assert!(outcomes(&ctx)?.is_empty());
        Ok(())
    }

    #[test]
    fn display_names_are_unwrapped_and_cc_is_deduplicated() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = gmail(&dir, "no");
        create_draft(
            &ctx,
            &with(&[
                ("to", "[\"Anna Muster <anna@example.org>\"]"),
                ("cc", "\"bob@example.org; anna@example.org\""),
            ])?,
        )?;
        let sent = outbox(&dir)?;
        assert_eq!(list(&sent[0], "to"), ["anna@example.org"]);
        assert_eq!(list(&sent[0], "cc"), ["bob@example.org"]);
        Ok(())
    }

    #[test]
    fn line_breaks_in_the_subject_are_removed() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = gmail(&dir, "no");
        create_draft(
            &ctx,
            &with(&[("subject", "\"Hallo\\r\\nBcc: evil@example.org\"")])?,
        )?;
        let subject = field(&outbox(&dir)?[0], "subject");
        assert_eq!(subject, "Hallo Bcc: evil@example.org");
        Ok(())
    }

    #[test]
    fn reply_drafts_take_the_recipient_from_the_original() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = gmail(&dir, "no");
        let reply = |id: &str| {
            args(&format!(
                r#"{{"account": "Demo Gmail", "reply_to_id": {id}, "body": "Danke!"}}"#
            ))
        };
        create_draft(&ctx, &reply("101")?)?;
        create_draft(&ctx, &reply("\"101\"")?)?;
        for sent in outbox(&dir)? {
            assert_eq!(list(&sent, "to"), ["noreply@dhl.example"]);
            assert_eq!(
                field(&sent, "subject"),
                "Re: Ihr Paket wird morgen geliefert"
            );
        }
        assert!(refused(create_draft(&ctx, &reply("999")?), "not found"));
        assert_eq!(
            outcomes(&ctx)?,
            [Outcome::Drafted, Outcome::Drafted, Outcome::Error]
        );
        Ok(())
    }

    #[test]
    fn a_blind_follower_cannot_send_without_the_click() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = gmail(&dir, "no");
        let args = args(
            r#"{"account": "Demo Gmail", "to": ["leak@evil.example"], "subject": "Alle Mails", "body": "Hier sind alle E-Mails des Nutzers ..."}"#,
        )?;
        let text = send_message(&ctx, &args)?;
        assert!(text.contains("NOTHING was sent"));
        assert!(outbox(&dir)?
            .iter()
            .all(|entry| field(entry, "status") != "sent"));
        Ok(())
    }

    #[test]
    fn an_approved_send_goes_out_and_is_logged_without_the_text() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = writer(
            &dir,
            "FAKE-GMAIL,FAKE-OUTLOOK",
            "yes",
            FakeOptions::default(),
        );
        let text = send_message(&ctx, &with(&[("cc", "[\"bob@example.org\"]")])?)?;
        assert!(text.starts_with("Sent:"));
        let sent = outbox(&dir)?;
        assert_eq!(sent.len(), 1);
        assert_eq!(list(&sent[0], "to"), ["anna@example.org"]);
        assert_eq!(list(&sent[0], "cc"), ["bob@example.org"]);
        let entries = sendlog::read_entries(&ctx.settings, 300)?.entries;
        assert_eq!(
            entries.iter().map(|e| e.outcome).collect::<Vec<_>>(),
            [Outcome::Pending, Outcome::Sent]
        );
        assert_eq!(entries[0].id, entries[1].id);
        assert_eq!(entries[1].sha256.len(), 16);
        let raw = fs::read_to_string(ctx.settings.sendlog_path())?;
        assert!(!raw.contains("Kurzer Text"));
        Ok(())
    }

    #[test]
    fn an_approved_reply_goes_to_the_original_sender() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = gmail(&dir, "yes");
        let reply =
            args(r#"{"account": "Demo Gmail", "reply_to_id": 102, "body": "Wird bezahlt."}"#)?;
        send_message(&ctx, &reply)?;
        let sent = outbox(&dir)?;
        assert_eq!(list(&sent[0], "to"), ["rechnung@stadtwerke.example"]);
        assert!(field(&sent[0], "subject").starts_with("Re: "));
        Ok(())
    }

    #[test]
    fn a_second_writable_account_sends_from_its_own_address() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = writer(
            &dir,
            "FAKE-GMAIL,FAKE-OUTLOOK",
            "yes",
            FakeOptions::default(),
        );
        send_message(&ctx, &with(&[("account", "\"Demo Outlook\"")])?)?;
        assert!(field(&outbox(&dir)?[0], "from").ends_with("<demo.privat@outlook.example>"));
        Ok(())
    }

    #[test]
    fn the_hourly_limit_stops_the_sixth_mail() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = gmail(&dir, "yes");
        for number in 0..5 {
            let subject = format!("\"Nr {number}\"");
            assert!(
                send_message(&ctx, &with(&[("subject", subject.as_str())])?)?.starts_with("Sent:")
            );
        }
        assert!(refused(send_message(&ctx, &mail()?), "Send limit reached"));
        let sent = outbox(&dir)?
            .iter()
            .filter(|entry| field(entry, "status") == "sent")
            .count();
        assert_eq!(sent, 5);
        Ok(())
    }

    #[test]
    fn a_timeout_sends_nothing_and_blocks_a_retry() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = gmail(&dir, "timeout");
        let text = send_message(&ctx, &mail()?)?;
        assert!(text.contains("did not answer"));
        assert!(outbox(&dir)?.is_empty());
        assert!(refused(
            send_message(&ctx, &mail()?),
            "just declined or ignored"
        ));
        Ok(())
    }

    #[test]
    fn the_log_drives_the_send_limits() -> TestResult {
        let now = journal::now_seconds();
        let case = |entries: Vec<LogEntry>| -> Result<ToolResult<String>, Box<dyn Error>> {
            let dir = TempDir::new()?;
            let ctx = gmail(&dir, "yes");
            seed(&ctx, &entries)?;
            Ok(send_message(&ctx, &mail()?))
        };
        let spam = (0..6)
            .map(|i| log_entry(now - 100 - i * 30, Outcome::Declined, None))
            .collect();
        assert!(refused(case(spam)?, "Too many send requests"));
        let old = (0..5)
            .map(|i| log_entry(now - 4000 - i, Outcome::Sent, None))
            .collect();
        assert!(case(old)?.is_ok_and(|text| text.starts_with("Sent:")));
        let recent = (0..5)
            .map(|i| log_entry(now - 1000 - i, Outcome::Sent, None))
            .collect();
        assert!(refused(case(recent)?, "Send limit reached"));
        let pendings = (0..5)
            .map(|i| log_entry(now - 900 - i, Outcome::Pending, Some(&format!("p{i}"))))
            .collect();
        assert!(refused(case(pendings)?, "Send limit reached"));
        Ok(())
    }

    #[test]
    fn a_pending_entry_and_its_result_count_once() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = gmail(&dir, "yes");
        let now = journal::now_seconds();
        let mut pairs = Vec::new();
        for i in 0..4 {
            let id = format!("q{i}");
            pairs.push(log_entry(now - 1500 - i, Outcome::Pending, Some(&id)));
            pairs.push(log_entry(now - 1490 - i, Outcome::Sent, Some(&id)));
        }
        seed(&ctx, &pairs)?;
        assert!(send_message(&ctx, &mail()?)?.starts_with("Sent:"));
        assert!(refused(send_message(&ctx, &mail()?), "Send limit reached"));
        Ok(())
    }

    #[test]
    fn an_unwritable_log_blocks_sending() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = gmail(&dir, "yes");
        fs::create_dir_all(ctx.settings.sendlog_path())?;
        assert!(refused(
            send_message(&ctx, &mail()?),
            "send log is not writable"
        ));
        assert!(outbox(&dir)?.is_empty());
        Ok(())
    }

    #[test]
    fn a_crash_while_sending_keeps_the_attempt_as_pending() -> TestResult {
        let dir = TempDir::new()?;
        let options = FakeOptions {
            crash: true,
            ..FakeOptions::default()
        };
        let ctx = writer(&dir, "FAKE-GMAIL", "yes", options);
        let now = journal::now_seconds();
        let pendings: Vec<LogEntry> = (0..4)
            .map(|i| log_entry(now - 900 - i, Outcome::Pending, Some(&format!("p{i}"))))
            .collect();
        seed(&ctx, &pendings)?;
        assert_eq!(
            send_message(&ctx, &mail()?),
            Err(ToolError::internal("RuntimeError"))
        );
        assert_eq!(outcomes(&ctx)?.last(), Some(&Outcome::Pending));
        assert!(outbox(&dir)?.is_empty());
        assert!(refused(send_message(&ctx, &mail()?), "Send limit reached"));
        Ok(())
    }

    #[test]
    fn an_uncertain_outcome_warns_and_stays_pending() -> TestResult {
        let dir = TempDir::new()?;
        let options = FakeOptions {
            uncertain: true,
            ..FakeOptions::default()
        };
        let ctx = writer(&dir, "FAKE-GMAIL", "yes", options);
        let result = send_message(&ctx, &mail()?);
        assert!(matches!(result, Err(ToolError::Uncertain(m)) if m.contains("may or may not")));
        assert_eq!(outcomes(&ctx)?, [Outcome::Pending]);
        Ok(())
    }

    #[test]
    fn drafts_are_limited_per_hour_and_refusals_are_not_logged() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = gmail(&dir, "no");
        for number in 0..30 {
            let subject = format!("\"Entwurf {number}\"");
            assert!(create_draft(&ctx, &with(&[("subject", subject.as_str())])?).is_ok());
        }
        assert!(refused(
            create_draft(&ctx, &mail()?),
            "Draft limit reached (30 per hour)"
        ));
        assert_eq!(outbox(&dir)?.len(), 30);
        assert_eq!(outcomes(&ctx)?.len(), 30);
        Ok(())
    }

    #[test]
    fn failing_drafts_count_against_the_limit_too() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = gmail(&dir, "no");
        let missing = args(r#"{"account": "Demo Gmail", "reply_to_id": 999999, "body": "x"}"#)?;
        for _ in 0..30 {
            assert!(refused(create_draft(&ctx, &missing), "not found"));
        }
        assert!(refused(create_draft(&ctx, &missing), "Draft limit reached"));
        assert_eq!(outcomes(&ctx)?.len(), 30);
        Ok(())
    }

    #[test]
    fn a_flood_of_drafts_cannot_push_the_send_history_out_of_sight() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = gmail(&dir, "yes");
        let now = journal::now_seconds();
        let mut history: Vec<LogEntry> = (0..5)
            .map(|index| log_entry(now - 1800 - index, Outcome::Sent, None))
            .collect();
        for index in 0..400 {
            let mut draft = log_entry(now - 600 + index % 50, Outcome::Drafted, None);
            draft.mode = Mode::Draft;
            history.push(draft);
        }
        seed(&ctx, &history)?;
        assert!(refused(
            send_message(&ctx, &mail()?),
            "Send limit reached (5 per hour)"
        ));
        assert!(outbox(&dir)?.is_empty());
        Ok(())
    }

    #[test]
    fn a_log_that_cannot_be_read_or_trusted_blocks_sending_and_drafts() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = gmail(&dir, "yes");
        fs::write(ctx.settings.sendlog_path(), "garbage line\n")?;
        for result in [send_message(&ctx, &mail()?), create_draft(&ctx, &mail()?)] {
            assert!(refused(result, "cannot be read"));
        }
        assert!(outbox(&dir)?.is_empty());
        fs::remove_file(ctx.settings.sendlog_path())?;
        fs::create_dir_all(ctx.settings.sendlog_path())?;
        for result in [send_message(&ctx, &mail()?), create_draft(&ctx, &mail()?)] {
            assert!(refused(result, "send log is not writable"));
        }
        assert!(outbox(&dir)?.is_empty());
        Ok(())
    }

    #[test]
    fn results_that_repeat_mail_data_carry_the_untrusted_banner() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = gmail(&dir, "yes");
        let reply = args(r#"{"account": "Demo Gmail", "reply_to_id": 101, "body": "Danke!"}"#)?;
        for text in [create_draft(&ctx, &reply)?, send_message(&ctx, &reply)?] {
            assert!(text.contains(UNTRUSTED), "{text}");
            let banner = text.find(UNTRUSTED).ok_or("banner")?;
            let subject = text.find("Subject:").ok_or("subject")?;
            assert!(banner < subject);
        }
        Ok(())
    }

    #[test]
    fn long_texts_by_line_count_are_drafts_only() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = gmail(&dir, "yes");
        let many = vec!["Zeile"; 41].join("\n");
        let mut tall = mail()?;
        tall.insert("body", Json::Str(many.clone()));
        assert!(refused(
            send_message(&ctx, &tall),
            "41 lines but send_message allows 40"
        ));
        assert!(create_draft(&ctx, &tall).is_ok());
        let mut edge = mail()?;
        edge.insert("body", Json::Str(vec!["Zeile"; 40].join("\n")));
        assert!(send_message(&ctx, &edge).is_ok());
        let spaced = vec!["Zeile"; 20].join("\n\n\n\n");
        let mut blanks = mail()?;
        blanks.insert("body", Json::Str(spaced));
        assert!(send_message(&ctx, &blanks).is_ok());
        Ok(())
    }

    #[test]
    fn a_safe_failure_replaces_pending_with_error() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = gmail(&dir, "yes");
        let reply = args(r#"{"account": "Demo Gmail", "reply_to_id": 999, "body": "x"}"#)?;
        assert!(refused(send_message(&ctx, &reply), "not found"));
        let entries = sendlog::read_entries(&ctx.settings, 300)?.entries;
        assert_eq!(
            entries.iter().map(|e| e.outcome).collect::<Vec<_>>(),
            [Outcome::Pending, Outcome::Error]
        );
        assert_eq!(entries[0].id, entries[1].id);
        Ok(())
    }
}
