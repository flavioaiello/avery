use crate::account::Account;
use crate::backend::{MessageId, MoveReport, MoveRequest};
use crate::changelog::Outcome;
use crate::context::Context;
use crate::error::{ToolError, ToolResult};
use crate::journal;
use crate::json::{Json, Object};
use crate::movelog::{self, MoveEntry};
use crate::text::{char_len, clean_str, one_line_str, text_of, to_bool};
use crate::texts::{MAX_MAILBOX_NAME, MAX_MOVE_MESSAGES};

const MOVE_ARGS: [&str; 5] = ["account", "mailbox", "ids", "to_mailbox", "create_mailbox"];
const SHOWN_PATH: usize = 120;
const SPECIAL_MAILBOXES: &[&str] = &[
    "trash",
    "bin",
    "deleted",
    "deleted items",
    "deleted messages",
    "papierkorb",
    "gelöschte objekte",
    "gelöschte elemente",
    "gelöschte nachrichten",
    "junk",
    "junk email",
    "junk e-mail",
    "spam",
    "drafts",
    "entwürfe",
    "sent",
    "sent items",
    "sent messages",
    "sent mail",
    "gesendet",
    "gesendete objekte",
    "gesendete elemente",
    "gesendete nachrichten",
    "outbox",
    "postausgang",
];

fn unknown_parameters(args: &Object) -> Option<ToolError> {
    let unknown: Vec<&str> = args.keys().filter(|key| !MOVE_ARGS.contains(key)).collect();
    if unknown.is_empty() {
        return None;
    }
    let hint = if unknown.iter().any(|key| {
        matches!(
            key.to_lowercase().as_str(),
            "delete" | "trash" | "flag" | "flagged" | "read" | "mark" | "archive"
        )
    }) {
        " Only moving is supported: nothing can be deleted, flagged or marked."
    } else {
        ""
    };
    let names: Vec<String> = unknown.iter().map(|key| clean_str(key, Some(30))).collect();
    Some(ToolError::msg(format!(
        "Unknown parameter(s): {}.{hint}",
        names.join(", ")
    )))
}

fn ids_argument(value: Option<&Json>) -> ToolResult<Vec<MessageId>> {
    let invalid = || {
        ToolError::msg(format!(
            "Parameter 'ids' must be a list of 1 to {MAX_MOVE_MESSAGES} message ids (whole numbers) from list_messages or search_messages."
        ))
    };
    let Some(Json::Arr(items)) = value else {
        return Err(invalid());
    };
    if items.is_empty() || items.len() > MAX_MOVE_MESSAGES {
        return Err(invalid());
    }
    let mut ids: Vec<MessageId> = Vec::with_capacity(items.len());
    for item in items {
        let id = MessageId::argument(Some(item), "ids")?
            .filter(|id| id.number().is_some())
            .ok_or_else(invalid)?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    Ok(ids)
}

fn mailbox_name(value: Option<&Json>, parameter: &str) -> ToolResult<Option<String>> {
    let raw = text_of(value);
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let name = one_line_str(&raw, MAX_MAILBOX_NAME + 1);
    if char_len(&name) > MAX_MAILBOX_NAME {
        return Err(ToolError::msg(format!(
            "Parameter '{parameter}' must be one line of at most {MAX_MAILBOX_NAME} characters."
        )));
    }
    Ok(Some(name))
}

fn is_special(name: &str) -> bool {
    name.split('/')
        .any(|part| SPECIAL_MAILBOXES.contains(&part.trim().to_lowercase().as_str()))
}

fn creatable(name: &str) -> bool {
    !name.starts_with('.')
        && !name
            .chars()
            .any(|c| matches!(c, '/' | '\\' | ':' | '[' | ']' | '*' | '%' | '"'))
}

fn request_from(args: &Object) -> ToolResult<MoveRequest> {
    let ids = ids_argument(args.get("ids"))?;
    let target = mailbox_name(args.get("to_mailbox"), "to_mailbox")?
        .ok_or_else(|| ToolError::msg("Parameter 'to_mailbox' is required."))?;
    let mailbox = mailbox_name(args.get("mailbox"), "mailbox")?;
    let create = to_bool(args.get("create_mailbox"));
    if is_special(&target) {
        return Err(ToolError::msg(
            "Trash, Junk, Spam, Drafts and Sent cannot be the target of a move: nothing is deleted or filed away with this tool. Choose another mailbox.",
        ));
    }
    if create && !creatable(&target) {
        return Err(ToolError::msg(
            "A new mailbox is created at the top level of the account: its name must not contain / \\ : [ ] * % or quotes and must not start with a dot.",
        ));
    }
    Ok(MoveRequest {
        mailbox,
        ids,
        target,
        create,
    })
}

fn shown(path: &str) -> String {
    one_line_str(path, SHOWN_PATH)
}

fn describe(report: &MoveReport) -> String {
    match report {
        MoveReport::Moved {
            source,
            target,
            created,
            count,
            kept,
        } => {
            let plural = if *count == 1 { "" } else { "s" };
            let (source, target) = (shown(source), shown(target));
            let made = if *created {
                " (the mailbox was created)"
            } else {
                ""
            };
            if *kept == 0 {
                format!(
                    "Moved {count} message{plural} from '{source}' to '{target}'{made} after the user approved it. Nothing was deleted. The messages have new ids in the target mailbox: use list_messages with that mailbox to see them."
                )
            } else {
                format!(
                    "Added {count} message{plural} to '{target}'{made} after the user approved it, but {kept} of them also remain in '{source}': this account works with labels (Gmail), and Mail cannot take a message out of a mailbox through its scripting interface. Nothing was deleted. If they should leave '{source}', the user has to archive them in Mail."
                )
            }
        }
        MoveReport::Timeout => "The user did not answer the approval dialog in time. NOTHING was moved. Ask the user whether to try again.".to_string(),
        MoveReport::Declined => "The user did NOT approve moving the messages. NOTHING was moved. Do not retry unless the user asks you to.".to_string(),
    }
}

struct Attempt<'a> {
    ctx: &'a Context,
    account: &'a Account,
    request: &'a MoveRequest,
    id: &'a str,
}

impl Attempt<'_> {
    fn log(&self, outcome: Outcome, report: Option<&MoveReport>, strict: bool) -> ToolResult<()> {
        let (from, to, count, created) = match report {
            Some(MoveReport::Moved {
                source,
                target,
                created,
                count,
                ..
            }) => (source.as_str(), target.as_str(), *count, *created),
            _ => (
                self.request.mailbox.as_deref().unwrap_or_default(),
                self.request.target.as_str(),
                self.request.ids.len(),
                self.request.create,
            ),
        };
        let entry = MoveEntry {
            ts: journal::now_seconds(),
            id: self.id.to_string(),
            outcome,
            account_id: self.account.id.clone(),
            account: one_line_str(&self.account.label, SHOWN_PATH),
            from: one_line_str(from, SHOWN_PATH),
            to: one_line_str(to, SHOWN_PATH),
            count,
            created,
        };
        movelog::append(&self.ctx.settings, &entry, strict)
    }
}

fn outcome_of(report: &MoveReport) -> Outcome {
    match report {
        MoveReport::Moved { .. } => Outcome::Done,
        MoveReport::Declined => Outcome::Declined,
        MoveReport::Timeout => Outcome::Timeout,
    }
}

pub(crate) fn move_messages(ctx: &Context, args: &Object) -> ToolResult<String> {
    if let Some(error) = unknown_parameters(args) {
        return Err(error);
    }
    let (account, _) = ctx.settings.resolve_writable(args.get("account"))?;
    let request = request_from(args)?;
    let now = journal::now_seconds();
    let history = movelog::read_window(&ctx.settings, now)?;
    movelog::check_limits(&history.entries, now)?;
    movelog::ensure_writable(&ctx.settings)?;
    let id = journal::attempt_id()?;
    let attempt = Attempt {
        ctx,
        account: &account,
        request: &request,
        id: &id,
    };
    attempt.log(Outcome::Pending, None, true)?;
    match ctx.backend.move_messages(&account, &request) {
        Ok(report) => {
            attempt.log(outcome_of(&report), Some(&report), false)?;
            Ok(describe(&report))
        }
        Err(ToolError::Msg(message)) => {
            attempt.log(Outcome::Error, None, false)?;
            Err(ToolError::Msg(message))
        }
        Err(other) => Err(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeOptions;
    use crate::testutil::{args, j, settings_at, TempDir, TestResult};
    use crate::texts::{MAX_ATTEMPTS_PER_10_MIN, MAX_MOVES_PER_HOUR};
    use std::error::Error;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn writer(dir: &TempDir, accounts: &str, approve: &str) -> Context {
        let mut settings = settings_at(&dir.path("config.json"));
        settings.fake = true;
        settings.fake_write = accounts.to_string();
        Context::demo(
            settings,
            FakeOptions {
                approve: approve.to_string(),
                ..FakeOptions::default()
            },
        )
    }

    fn gmail(dir: &TempDir, approve: &str) -> Context {
        writer(dir, "FAKE-GMAIL", approve)
    }

    fn run(ctx: &Context, text: &str) -> ToolResult<String> {
        move_messages(ctx, &args(text).map_err(ToolError::msg)?)
    }

    fn refused(result: ToolResult<String>, needle: &str) -> bool {
        matches!(result, Err(ToolError::Msg(message)) if message.contains(needle))
    }

    const MOVE: &str = r#"{"ids": [101, 102], "to_mailbox": "Werbung", "create_mailbox": true}"#;

    #[test]
    fn an_approved_move_into_a_new_mailbox_says_so() -> TestResult {
        let dir = TempDir::new()?;
        let outlook = writer(&dir, "FAKE-OUTLOOK", "yes");
        let text = run(
            &outlook,
            r#"{"ids": [201, 202], "to_mailbox": "Werbung", "create_mailbox": true}"#,
        )?;
        assert!(text
            .starts_with("Moved 2 messages from 'Inbox' to 'Werbung' (the mailbox was created)"));
        assert!(text.contains("Nothing was deleted") && text.contains("new ids"));
        let text = run(
            &outlook,
            r#"{"ids": ["201"], "to_mailbox": "Archive", "mailbox": "inbox"}"#,
        )?;
        assert!(text.starts_with("Moved 1 message from 'Inbox' to 'Archive' after"));
        Ok(())
    }

    #[test]
    fn in_gmail_the_move_adds_a_label_and_the_model_is_told_the_mail_stays() -> TestResult {
        let dir = TempDir::new()?;
        let text = run(&gmail(&dir, "yes"), MOVE)?;
        assert!(text.starts_with(
            "Added 2 messages to 'Werbung' (the mailbox was created) after the user approved it, but 2 of them also remain in 'INBOX'"
        ));
        assert!(text.contains("archive them in Mail") && text.contains("Nothing was deleted"));
        let text = run(
            &gmail(&dir, "yes"),
            r#"{"ids": ["101"], "to_mailbox": "[Gmail]/All Mail", "mailbox": "inbox"}"#,
        )?;
        assert!(text.starts_with("Added 1 message to '[Gmail]/All Mail' after"));
        Ok(())
    }

    #[test]
    fn a_decline_or_a_timeout_moves_nothing_and_tells_the_model_not_to_retry() -> TestResult {
        let dir = TempDir::new()?;
        let declined = run(&gmail(&dir, "no"), MOVE)?;
        assert!(declined.contains("did NOT approve") && declined.contains("NOTHING was moved"));
        assert!(declined.contains("Do not retry"));
        let other = TempDir::new()?;
        let late = run(&gmail(&other, "timeout"), MOVE)?;
        assert!(late.contains("did not answer") && late.contains("NOTHING was moved"));
        Ok(())
    }

    fn entries(ctx: &Context) -> Result<Vec<MoveEntry>, Box<dyn Error>> {
        let text = fs::read_to_string(ctx.settings.moves_path())?;
        text.lines()
            .map(|line| MoveEntry::from_json(&j(line)?).ok_or_else(|| "unreadable line".into()))
            .collect()
    }

    #[test]
    fn every_attempt_is_logged_first_as_pending_and_then_with_its_outcome() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = gmail(&dir, "yes");
        run(&ctx, MOVE)?;
        let logged = entries(&ctx)?;
        assert_eq!(logged.len(), 2);
        let (pending, done) = (&logged[0], &logged[1]);
        assert_eq!(
            (pending.outcome, done.outcome),
            (Outcome::Pending, Outcome::Done)
        );
        assert_eq!(pending.id, done.id);
        assert_eq!(done.id.len(), 12);
        assert_eq!((pending.count, done.count), (2, 2));
        assert_eq!((done.from.as_str(), done.to.as_str()), ("INBOX", "Werbung"));
        assert_eq!(
            (pending.to.as_str(), pending.from.as_str()),
            ("Werbung", "")
        );
        assert!(done.created && pending.created);
        assert_eq!(done.account, "Demo Gmail");
        let mode = fs::metadata(ctx.settings.moves_path())?
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        Ok(())
    }

    #[test]
    fn a_decline_pauses_the_model_and_the_refused_retry_leaves_no_trace() -> TestResult {
        for (approve, outcome) in [("no", Outcome::Declined), ("timeout", Outcome::Timeout)] {
            let dir = TempDir::new()?;
            let ctx = gmail(&dir, approve);
            run(&ctx, MOVE)?;
            assert!(refused(run(&ctx, MOVE), "just declined or ignored"));
            let outcomes: Vec<Outcome> = entries(&ctx)?.iter().map(|e| e.outcome).collect();
            assert_eq!(outcomes, [Outcome::Pending, outcome]);
        }
        Ok(())
    }

    #[test]
    fn a_failure_with_an_unknown_outcome_leaves_the_attempt_pending_and_counted() -> TestResult {
        let dir = TempDir::new()?;
        let mut settings = settings_at(&dir.path("config.json"));
        settings.fake = true;
        settings.fake_write = "FAKE-GMAIL".to_string();
        let ctx = Context::demo(
            settings,
            FakeOptions {
                approve: "yes".to_string(),
                uncertain: true,
                ..FakeOptions::default()
            },
        );
        let now = journal::now_seconds();
        for n in 0..MAX_MOVES_PER_HOUR - 1 {
            let earlier = MoveEntry {
                ts: now - 1000 + i64::try_from(n)?,
                id: format!("d{n}"),
                outcome: Outcome::Done,
                account_id: "FAKE-GMAIL".to_string(),
                account: "Demo Gmail".to_string(),
                from: "INBOX".to_string(),
                to: "Werbung".to_string(),
                count: 1,
                created: false,
            };
            movelog::append(&ctx.settings, &earlier, true)?;
        }
        assert!(matches!(
            run(&ctx, MOVE),
            Err(ToolError::Uncertain(message)) if message.contains("may have been moved")
        ));
        let logged = entries(&ctx)?;
        let last = logged.last().ok_or("no entry")?;
        assert_eq!(
            (logged.len(), last.outcome),
            (MAX_MOVES_PER_HOUR, Outcome::Pending)
        );
        assert!(refused(run(&ctx, MOVE), "Move limit reached"));
        Ok(())
    }

    #[test]
    fn a_model_that_keeps_failing_is_stopped_after_six_attempts_in_ten_minutes() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = gmail(&dir, "yes");
        let missing = r#"{"ids": [999], "to_mailbox": "Werbung", "create_mailbox": true}"#;
        for _ in 0..MAX_ATTEMPTS_PER_10_MIN {
            assert!(refused(run(&ctx, missing), "A message id was not found"));
        }
        assert!(refused(run(&ctx, MOVE), "Too many move requests"));
        let errors = entries(&ctx)?
            .iter()
            .filter(|entry| entry.outcome == Outcome::Error)
            .count();
        assert_eq!(errors, MAX_ATTEMPTS_PER_10_MIN);
        Ok(())
    }

    #[test]
    fn a_log_that_cannot_be_trusted_blocks_moves_and_mistakes_leave_no_trace() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = gmail(&dir, "yes");
        assert!(refused(
            run(&ctx, r#"{"ids": [], "to_mailbox": "Werbung"}"#),
            "'ids'"
        ));
        assert!(!std::path::Path::new(&ctx.settings.moves_path()).exists());
        fs::write(ctx.settings.moves_path(), "garbage\n")?;
        assert!(refused(run(&ctx, MOVE), "cannot be read"));
        fs::remove_file(ctx.settings.moves_path())?;
        fs::create_dir(ctx.settings.moves_path())?;
        assert!(refused(run(&ctx, MOVE), "not writable"));
        Ok(())
    }

    #[test]
    fn only_accounts_with_the_write_level_can_move_and_several_need_a_name() -> TestResult {
        let dir = TempDir::new()?;
        assert!(refused(run(&writer(&dir, "", "yes"), MOVE), "not enabled"));
        let two = writer(&dir, "FAKE-GMAIL,FAKE-OUTLOOK", "yes");
        assert!(refused(run(&two, MOVE), "'account' is required"));
        let text = run(
            &two,
            r#"{"account": "Demo Outlook", "ids": [201], "to_mailbox": "Archive"}"#,
        )?;
        assert!(text.starts_with("Moved 1 message from 'Inbox' to 'Archive'"));
        assert!(refused(
            run(
                &gmail(&dir, "yes"),
                r#"{"account": "Demo Outlook", "ids": [201], "to_mailbox": "Archive"}"#
            ),
            "Unknown account"
        ));
        Ok(())
    }

    #[test]
    fn the_ids_are_a_bounded_list_of_whole_numbers_without_duplicates() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = gmail(&dir, "yes");
        let many: Vec<String> = (1..=MAX_MOVE_MESSAGES + 1).map(|n| n.to_string()).collect();
        let too_many = format!(
            r#"{{"ids": [{}], "to_mailbox": "Werbung", "create_mailbox": true}}"#,
            many.join(",")
        );
        for text in [
            r#"{"to_mailbox": "Werbung"}"#,
            r#"{"ids": [], "to_mailbox": "Werbung"}"#,
            r#"{"ids": "101", "to_mailbox": "Werbung"}"#,
            r#"{"ids": [0], "to_mailbox": "Werbung"}"#,
            r#"{"ids": [-4], "to_mailbox": "Werbung"}"#,
            r#"{"ids": ["18c3f"], "to_mailbox": "Werbung"}"#,
            r#"{"ids": [null], "to_mailbox": "Werbung"}"#,
            r#"{"ids": [1.5], "to_mailbox": "Werbung"}"#,
            r#"{"ids": [[101]], "to_mailbox": "Werbung"}"#,
            too_many.as_str(),
        ] {
            assert!(refused(run(&ctx, text), "'ids'"), "{text}");
        }
        let text = run(
            &ctx,
            r#"{"ids": [101, "101", " 101 ", 102], "to_mailbox": "Werbung", "create_mailbox": true}"#,
        )?;
        assert!(text.starts_with("Added 2 messages"), "{text}");
        let exactly = format!(
            r#"{{"ids": [{}], "to_mailbox": "Werbung", "create_mailbox": true}}"#,
            many[..MAX_MOVE_MESSAGES].join(",")
        );
        assert!(refused(run(&ctx, &exactly), "A message id was not found"));
        Ok(())
    }

    #[test]
    fn the_target_is_one_clean_line_and_special_mailboxes_are_refused() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = gmail(&dir, "yes");
        let target = |name: &str, create: bool| {
            run(
                &ctx,
                &format!(r#"{{"ids": [101], "to_mailbox": "{name}", "create_mailbox": {create}}}"#),
            )
        };
        for name in [
            "Trash",
            "trash",
            "[Gmail]/Papierkorb",
            "[Google Mail]/Spam",
            "Deleted Items",
            "Junk Email",
            "Drafts",
            "Entw\u{fc}rfe",
            "Sent Items",
            "Gesendet",
            "Outbox",
            "Archive/Bin",
        ] {
            for create in [false, true] {
                assert!(
                    refused(target(name, create), "cannot be the target"),
                    "{name} {create}"
                );
            }
        }
        assert!(target("Trash talk", true)?.contains("'Trash talk'"));
        assert!(target("Wer\\u200bbung", true)?.contains("'Werbung'"));
        assert!(target("Wer\\nbung", true)?.contains("'Wer bung'"));
        for name in [
            "a/b", "a\\\\b", "a:b", "[x]", "a*", "a%", ".hidden", "a\\\"b",
        ] {
            assert!(
                refused(target(name, true), "top level of the account"),
                "{name}"
            );
        }
        assert!(refused(target("", true), "'to_mailbox' is required"));
        assert!(refused(target("  ", true), "'to_mailbox' is required"));
        let long = "x".repeat(MAX_MAILBOX_NAME + 1);
        assert!(refused(target(&long, true), "at most 100 characters"));
        assert!(target(&"x".repeat(MAX_MAILBOX_NAME), true).is_ok());
        Ok(())
    }

    #[test]
    fn a_missing_target_needs_create_mailbox_and_other_mistakes_move_nothing() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = gmail(&dir, "yes");
        assert!(refused(
            run(&ctx, r#"{"ids": [101], "to_mailbox": "Werbung"}"#),
            "create_mailbox=true"
        ));
        assert!(refused(
            run(&ctx, r#"{"ids": [101], "to_mailbox": "inbox"}"#),
            "already in the target mailbox"
        ));
        assert!(refused(
            run(
                &ctx,
                r#"{"ids": [999], "to_mailbox": "Werbung", "create_mailbox": true}"#
            ),
            "A message id was not found"
        ));
        assert!(refused(
            run(
                &ctx,
                r#"{"ids": [101], "to_mailbox": "Werbung", "create_mailbox": true, "mailbox": "Nope"}"#
            ),
            "Mailbox not found"
        ));
        Ok(())
    }

    #[test]
    fn unknown_parameters_are_refused_with_a_hint_that_nothing_can_be_deleted() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = gmail(&dir, "yes");
        let hinted = run(
            &ctx,
            r#"{"ids": [101], "to_mailbox": "Werbung", "delete": true}"#,
        );
        assert!(refused(
            hinted,
            "Unknown parameter(s): delete. Only moving is supported"
        ));
        let plain = run(&ctx, r#"{"ids": [101], "to_mailbox": "Werbung", "x": 1}"#);
        assert!(matches!(&plain, Err(ToolError::Msg(m)) if m == "Unknown parameter(s): x."));
        Ok(())
    }

    #[test]
    fn a_crash_in_the_backend_stays_an_internal_error() -> TestResult {
        let dir = TempDir::new()?;
        let mut settings = settings_at(&dir.path("config.json"));
        settings.fake = true;
        settings.fake_write = "FAKE-GMAIL".to_string();
        let ctx = Context::demo(
            settings,
            FakeOptions {
                crash: true,
                ..FakeOptions::default()
            },
        );
        assert!(matches!(run(&ctx, MOVE), Err(ToolError::Internal(_))));
        Ok(())
    }
}
