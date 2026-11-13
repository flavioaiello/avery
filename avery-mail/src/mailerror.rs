use crate::error::ToolError;
use crate::text::clean_str;
use crate::texts::MAX_RECIPIENTS;

pub(crate) fn map_error(text: &str, op: Option<&str>) -> String {
    let low = text.to_lowercase();
    let has = |needle: &str| low.contains(needle);
    if has("-1743") || has("not authorized") || has("nicht berechtigt") {
        return concat!(
            "macOS blocks access to Mail. Ask the user to allow it in System Settings > ",
            "Privacy & Security > Automation (the app that runs this tool, e.g. Goose, ",
            "needs Mail enabled), then retry."
        )
        .to_string();
    }
    if has("approval_dialog_failed") {
        return concat!(
            "The approval dialog could not be shown, so NOTHING was sent. The user must be logged in ",
            "at the Mac with an unlocked screen."
        )
        .to_string();
    }
    if has("sender_mismatch") || has("no_sender_address") {
        return concat!(
            "Mail would send from a different address than the selected account, so NOTHING was sent. ",
            "Ask the user to check the account in Mail."
        )
        .to_string();
    }
    if has("no_recipients") {
        return "Mail did not accept the recipients. NOTHING was sent.".to_string();
    }
    if has("send_state_unknown") {
        return concat!(
            "Mail reported an error while sending. The message may or may not have been sent: ask the ",
            "user to check Sent in Mail before you try again."
        )
        .to_string();
    }
    if has("too_many_recipients") {
        return format!(
            "The message would go to more than {MAX_RECIPIENTS} recipients, so NOTHING was sent. Use create_draft so that the user can send it from Mail, or answer without reply_all."
        );
    }
    if has("recipient_invalid") {
        return concat!(
            "Mail would send to an address that the approval dialog cannot show completely (plain ASCII ",
            "addresses up to 254 characters only), so NOTHING was sent. Use create_draft and let the user ",
            "send it from Mail."
        )
        .to_string();
    }
    if has("content_mismatch") {
        return concat!(
            "The content Mail was about to send differs from the text you gave, so NOTHING was sent. ",
            "Use create_draft and let the user send it from Mail."
        )
        .to_string();
    }
    let timed_out = has("-1712") || has("timed out") || has("timeout");
    if op == Some("compose") && timed_out {
        return concat!(
            "Mail did not answer in time. The message may or may not have been handled: ask the user to ",
            "check Sent and Drafts in Mail before you try again."
        )
        .to_string();
    }
    if timed_out {
        return "Mail did not answer in time. Retry with a smaller range (since_days, limit)."
            .to_string();
    }
    if has("account_not_found") {
        return "The account no longer exists in Mail. Ask the user to run `avery mail setup apple` again."
            .to_string();
    }
    if has("mailbox_not_found") {
        return "Mailbox not found. Use list_mailboxes to see the valid names.".to_string();
    }
    if has("too_many_hits") {
        let count = hit_count(&low).unwrap_or_else(|| "many".to_string());
        return format!(
            "Too many matches ({count}). Narrow down with since_days, unread_only or a query."
        );
    }
    if has("mailbox_changed_during_read") {
        return "The mailbox changed while reading. Please retry.".to_string();
    }
    if matches!(op, Some("read" | "compose"))
        && (has("can't get")
            || has("can\u{2019}t get")
            || has("invalid index")
            || has("message_not_in_account"))
    {
        return concat!(
            "Message not found. Use an id from list_messages or search_messages together ",
            "with the same account and mailbox."
        )
        .to_string();
    }
    format!("Mail error: {}", clean_str(text, Some(200)))
}

fn hit_count(low: &str) -> Option<String> {
    let marker = "too_many_hits:";
    let mut from = 0;
    while let Some(found) = low.get(from..).and_then(|rest| rest.find(marker)) {
        let start = from + found + marker.len();
        let digits: String = low
            .get(start..)
            .unwrap_or("")
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if !digits.is_empty() {
            return Some(digits);
        }
        from = start;
    }
    None
}

pub(crate) fn failure(text: &str, op: Option<&str>, uncertain: bool) -> ToolError {
    let low = text.to_lowercase();
    let timed_out = low.contains("-1712")
        || low.contains("timed out")
        || low.contains("timeout")
        || low.contains("send_state_unknown");
    if op == Some("compose") && (uncertain || timed_out) {
        let mut message = map_error(text, op);
        if !message.contains("may or may not") {
            message.push_str(
                " The message may or may not have been sent: ask the user to check Sent in Mail before you try again.",
            );
        }
        return ToolError::Uncertain(message);
    }
    ToolError::Msg(map_error(text, op))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automation_denied_in_all_its_spellings() {
        for text in [
            "execution error: Not authorized to send Apple events to Mail. (-1743)",
            "Error: -1743",
            "NICHT BERECHTIGT",
        ] {
            assert!(
                map_error(text, None).starts_with("macOS blocks access to Mail."),
                "{text}"
            );
        }
    }

    #[test]
    fn dialog_sender_recipient_and_content_failures_say_nothing_was_sent() {
        assert!(
            map_error("approval_dialog_failed: boom", Some("compose")).contains("NOTHING was sent")
        );
        assert!(map_error("sender_mismatch", Some("compose")).contains("different address"));
        assert!(map_error("no_sender_address", Some("compose")).contains("different address"));
        assert_eq!(
            map_error("no_recipients", Some("compose")),
            "Mail did not accept the recipients. NOTHING was sent."
        );
        assert!(map_error("content_mismatch", Some("compose")).contains("Use create_draft"));
    }

    #[test]
    fn timeouts_differ_for_compose_and_reading() {
        for text in [
            "timed out",
            "Mail got an error: AppleEvent timed out. (-1712)",
            "TIMEOUT",
        ] {
            assert!(map_error(text, Some("compose")).contains("may or may not have been handled"));
            assert_eq!(
                map_error(text, Some("messages")),
                "Mail did not answer in time. Retry with a smaller range (since_days, limit)."
            );
            assert_eq!(map_error(text, None), map_error(text, Some("read")));
        }
    }

    #[test]
    fn script_error_words() {
        assert!(map_error("account_not_found", None).contains("no longer exists"));
        assert!(map_error("account_not_found", None).contains("`avery mail setup apple`"));
        assert!(map_error("mailbox_not_found", None).starts_with("Mailbox not found."));
        assert_eq!(
            map_error("too_many_hits:2500", None),
            "Too many matches (2500). Narrow down with since_days, unread_only or a query."
        );
        assert!(map_error("too_many_hits:", None).contains("Too many matches (many)."));
        assert!(map_error("too_many_hits: x too_many_hits:7", None).contains("(7)"));
        assert_eq!(
            map_error("mailbox_changed_during_read", None),
            "The mailbox changed while reading. Please retry."
        );
    }

    #[test]
    fn message_not_found_only_for_read_and_compose() {
        for text in [
            "Can't get message 5",
            "can\u{2019}t get message",
            "Invalid index.",
        ] {
            assert!(
                map_error(text, Some("read")).starts_with("Message not found."),
                "{text}"
            );
            assert!(
                map_error(text, Some("compose")).starts_with("Message not found."),
                "{text}"
            );
            assert!(
                map_error(text, Some("messages")).starts_with("Mail error: "),
                "{text}"
            );
        }
    }

    #[test]
    fn a_failure_while_sending_after_the_click_is_uncertain() {
        let error = failure(
            "send_state_unknown: Connection is invalid. (-609)",
            Some("compose"),
            false,
        );
        assert!(matches!(
            &error,
            ToolError::Uncertain(m)
                if m.starts_with("Mail reported an error while sending.")
                    && m.contains("may or may not have been sent")
                    && !m.contains("-609")
        ));
        assert!(matches!(
            failure("send_state_unknown: x", Some("read"), false),
            ToolError::Msg(_)
        ));
    }

    #[test]
    fn recipient_rules_say_nothing_was_sent() {
        let too_many = map_error("too_many_recipients", Some("compose"));
        assert!(
            too_many.contains("more than 5 recipients") && too_many.contains("NOTHING was sent")
        );
        assert!(too_many.contains("create_draft"));
        let invalid = map_error("recipient_invalid", Some("compose"));
        assert!(invalid.contains("NOTHING was sent") && invalid.contains("254"));
        assert!(matches!(
            failure("too_many_recipients", Some("compose"), false),
            ToolError::Msg(_)
        ));
    }

    #[test]
    fn a_message_of_another_account_is_reported_as_not_found() {
        for op in ["read", "compose"] {
            assert!(map_error("message_not_in_account", Some(op)).starts_with("Message not found."));
        }
    }

    #[test]
    fn unknown_failures_are_sanitized_and_cut() {
        assert_eq!(map_error("", None), "Mail error: ");
        assert_eq!(
            map_error("odd\u{0}\u{200b}  text", None),
            "Mail error: odd  text"
        );
        let long = "x".repeat(500);
        assert_eq!(
            map_error(&long, None),
            format!("Mail error: {} [...]", "x".repeat(200))
        );
    }

    #[test]
    fn automation_denied_wins_over_other_words() {
        assert!(
            map_error("-1743 timed out content_mismatch", Some("compose"))
                .starts_with("macOS blocks")
        );
    }

    #[test]
    fn compose_failures_with_unknown_outcome_are_uncertain() {
        let timed_out = failure("timed out", Some("compose"), false);
        assert_eq!(
            timed_out,
            ToolError::Uncertain(map_error("timed out", Some("compose")))
        );
        assert!(matches!(
            failure("empty answer from osascript", Some("compose"), true),
            ToolError::Uncertain(message)
                if message.starts_with("Mail error: empty answer from osascript")
                    && message.ends_with("before you try again.")
                    && message.contains("may or may not have been sent")
        ));
        assert!(matches!(
            failure("sender_mismatch", Some("compose"), false),
            ToolError::Msg(_)
        ));
        assert!(matches!(
            failure("timed out", Some("messages"), true),
            ToolError::Msg(_)
        ));
    }
}
