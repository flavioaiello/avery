use crate::backend::Preview;
use crate::calendar::{Approval, Change};
use crate::error::{ToolError, ToolResult};
use crate::json::{Json, Object};
use crate::jxa::exchange;
use crate::osascript::{App, Host};
use crate::text::clean_str;

const TIMEOUT_SLACK: u64 = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Approved,
    Declined,
    TimedOut,
}

fn failure(text: &str, _op: Option<&str>, _uncertain: bool) -> ToolError {
    let low = text.to_lowercase();
    if low.contains("-1743") || low.contains("not authorized") || low.contains("nicht berechtigt") {
        return ToolError::msg(concat!(
            "macOS blocks the approval dialog. Ask the user to allow it in System Settings > ",
            "Privacy & Security > Automation (the app that runs this tool, e.g. Goose), then retry. ",
            "NOTHING was done."
        ));
    }
    if low.contains("approval_dialog_failed") {
        return ToolError::msg(concat!(
            "The approval dialog could not be shown, so NOTHING was done. The user must be logged in ",
            "at the Mac with an unlocked screen."
        ));
    }
    if low.contains("timed out") || low.contains("-1712") {
        return ToolError::msg("The approval dialog did not finish in time. NOTHING was done.");
    }
    ToolError::msg(format!(
        "The approval dialog failed ({}). NOTHING was done.",
        clean_str(text, Some(120))
    ))
}

fn verdict(data: &Json, approved: &str) -> ToolResult<Verdict> {
    match data
        .as_object()
        .and_then(|object| object.get("decision"))
        .and_then(Json::as_str)
    {
        Some(text) if text == approved => Ok(Verdict::Approved),
        Some("declined") => Ok(Verdict::Declined),
        Some("timeout") => Ok(Verdict::TimedOut),
        _ => Err(failure("unexpected answer from osascript", None, true)),
    }
}

fn seconds_json(seconds: u64) -> Json {
    Json::Int(i64::try_from(seconds).unwrap_or(i64::MAX))
}

pub(crate) fn approve_send(
    host: &dyn Host,
    preview: &Preview,
    seconds: u64,
) -> ToolResult<Verdict> {
    let mut request = Object::new();
    request.insert("op", "approve_send");
    request.insert("from", preview.from.as_str());
    request.insert("to", preview.to.as_slice());
    request.insert("cc", preview.cc.as_slice());
    request.insert("subject", preview.subject.as_str());
    request.insert("body", preview.body.as_str());
    request.insert(
        "reply",
        match &preview.reply {
            Some((subject, from)) => Json::object(vec![
                ("subject", Json::from(subject)),
                ("from", Json::from(from)),
            ]),
            None => Json::Null,
        },
    );
    request.insert("seconds", seconds_json(seconds));
    let data = exchange(host, App::Dialog, request, seconds + TIMEOUT_SLACK, failure)?;
    verdict(&data, "send")
}

pub(crate) fn approve_change(
    host: &dyn Host,
    calendar_label: &str,
    change: &Change,
    approval: &Approval,
) -> ToolResult<Verdict> {
    let mut request = Object::new();
    request.insert("op", "approve_change");
    request.insert("action", change.action().as_str());
    request.insert("undo", Json::Bool(approval.undo));
    request.insert("calendarLabel", calendar_label);
    request.insert(
        "before",
        change
            .before()
            .map_or(Json::Null, |fields| fields.to_json()),
    );
    request.insert(
        "after",
        change.after().map_or(Json::Null, |fields| fields.to_json()),
    );
    request.insert("seconds", seconds_json(approval.seconds));
    let data = exchange(
        host,
        App::Dialog,
        request,
        approval.seconds + TIMEOUT_SLACK,
        failure,
    )?;
    verdict(&data, "approved")
}

pub(crate) fn test(host: &dyn Host, seconds: u64) -> ToolResult<String> {
    let mut request = Object::new();
    request.insert("op", "approve_send");
    request.insert("title", "Selbsttest: Freigabedialog");
    request.insert("note", "Nur ein Test. Es wird nichts gesendet.");
    request.insert("sendLabel", "Weiter");
    request.insert("from", "test@example.invalid");
    request.insert("to", Json::Arr(vec![Json::from("test@example.invalid")]));
    request.insert("cc", Json::Arr(Vec::new()));
    request.insert("subject", "Testdialog");
    request.insert(
        "body",
        "Der Dialog funktioniert, wenn du diesen Text lesen kannst.",
    );
    request.insert("reply", Json::Null);
    request.insert("seconds", seconds_json(seconds));
    let data = exchange(host, App::Dialog, request, seconds + TIMEOUT_SLACK, failure)?;
    Ok(match verdict(&data, "send")? {
        Verdict::Approved => "send",
        Verdict::Declined => "declined",
        Verdict::TimedOut => "timeout",
    }
    .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::EventFields;
    use crate::civil::DateTime;
    use crate::json;
    use crate::osascript::Finished;
    use crate::testutil::TestResult;
    use std::sync::Mutex;
    use std::time::Duration;

    struct Dialogs {
        answer: Finished,
        seen: Mutex<Vec<(App, String, Duration)>>,
    }

    impl Dialogs {
        fn saying(decision: &str) -> Dialogs {
            Dialogs {
                answer: Finished::Done {
                    code: Some(0),
                    stdout: format!(
                        "{{\"ok\": true, \"data\": {{\"decision\": \"{decision}\"}}}}\n"
                    ),
                    stderr: String::new(),
                },
                seen: Mutex::new(Vec::new()),
            }
        }

        fn failing(stdout: &str, stderr: &str) -> Dialogs {
            Dialogs {
                answer: Finished::Done {
                    code: Some(1),
                    stdout: stdout.to_string(),
                    stderr: stderr.to_string(),
                },
                seen: Mutex::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<(App, String, Duration)> {
            self.seen
                .lock()
                .map(|seen| seen.clone())
                .unwrap_or_default()
        }
    }

    impl Host for Dialogs {
        fn ensure_running(&self, _app: App) {}

        fn run(&self, app: App, payload: &str, timeout: Duration) -> Finished {
            if let Ok(mut seen) = self.seen.lock() {
                seen.push((app, payload.to_string(), timeout));
            }
            match &self.answer {
                Finished::Done {
                    code,
                    stdout,
                    stderr,
                } => Finished::Done {
                    code: *code,
                    stdout: stdout.clone(),
                    stderr: stderr.clone(),
                },
                Finished::TimedOut => Finished::TimedOut,
                Finished::TooLarge => Finished::TooLarge,
                Finished::NotStarted => Finished::NotStarted,
            }
        }
    }

    fn preview() -> Preview {
        Preview {
            from: "Alice <alice@gmail.example>".into(),
            to: vec!["anna@example.org".into()],
            cc: vec!["bob@example.org".into()],
            subject: "Hallo".into(),
            body: "Kurzer Text.".into(),
            reply: Some(("Re: x".into(), "Anna <anna@example.org>".into())),
            status: String::new(),
            saved: None,
        }
    }

    fn fields(summary: &str) -> Result<EventFields, Box<dyn std::error::Error>> {
        Ok(EventFields {
            summary: summary.into(),
            start: DateTime::parse("2030-05-06 09:00").ok_or("start")?,
            end: DateTime::parse("2030-05-06 10:00").ok_or("end")?,
            all_day: false,
            location: String::new(),
            notes: String::new(),
            url: String::new(),
        })
    }

    #[test]
    fn a_send_dialog_carries_the_message_and_maps_the_decision() -> TestResult {
        for (answer, expected) in [
            ("send", Verdict::Approved),
            ("declined", Verdict::Declined),
            ("timeout", Verdict::TimedOut),
        ] {
            let host = Dialogs::saying(answer);
            assert_eq!(approve_send(&host, &preview(), 75)?, expected);
            let requests = host.requests();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].0, App::Dialog);
            assert_eq!(requests[0].2, Duration::from_secs(135));
            let sent = json::parse(&requests[0].1)?;
            let object = sent.as_object().ok_or("object")?;
            assert_eq!(
                object.get("op").and_then(Json::as_str),
                Some("approve_send")
            );
            assert_eq!(object.get("subject").and_then(Json::as_str), Some("Hallo"));
            assert_eq!(object.get("seconds"), Some(&Json::Int(75)));
            assert_eq!(
                object
                    .get("reply")
                    .and_then(Json::as_object)
                    .and_then(|r| r.get("from"))
                    .and_then(Json::as_str),
                Some("Anna <anna@example.org>")
            );
        }
        assert!(approve_send(&Dialogs::saying("approved"), &preview(), 75).is_err());
        assert!(approve_send(&Dialogs::saying("whatever"), &preview(), 75).is_err());
        Ok(())
    }

    #[test]
    fn a_change_dialog_carries_before_and_after() -> TestResult {
        let change = Change::Update {
            uid: "u1".into(),
            before: fields("Alt")?,
            after: fields("Neu")?,
        };
        let host = Dialogs::saying("approved");
        let approval = Approval {
            seconds: 60,
            undo: true,
        };
        assert_eq!(
            approve_change(&host, "Privat", &change, &approval)?,
            Verdict::Approved
        );
        let sent = json::parse(&host.requests()[0].1)?;
        let object = sent.as_object().ok_or("object")?;
        assert_eq!(object.get("action").and_then(Json::as_str), Some("update"));
        assert_eq!(object.get("undo"), Some(&Json::Bool(true)));
        assert_eq!(
            object.get("calendarLabel").and_then(Json::as_str),
            Some("Privat")
        );
        let summary = |key: &str| {
            object
                .get(key)
                .and_then(Json::as_object)
                .and_then(|fields| fields.get("summary"))
                .and_then(Json::as_str)
                .map(str::to_string)
        };
        assert_eq!(summary("before").as_deref(), Some("Alt"));
        assert_eq!(summary("after").as_deref(), Some("Neu"));
        let create = Change::Create {
            after: fields("Neu")?,
        };
        let host = Dialogs::saying("declined");
        assert_eq!(
            approve_change(&host, "Privat", &create, &approval)?,
            Verdict::Declined
        );
        let sent = json::parse(&host.requests()[0].1)?;
        assert_eq!(
            sent.as_object().and_then(|o| o.get("before")),
            Some(&Json::Null)
        );
        Ok(())
    }

    #[test]
    fn the_dialog_test_reports_the_decision() -> TestResult {
        assert_eq!(test(&Dialogs::saying("send"), 20)?, "send");
        assert_eq!(test(&Dialogs::saying("declined"), 20)?, "declined");
        assert_eq!(test(&Dialogs::saying("timeout"), 20)?, "timeout");
        Ok(())
    }

    #[test]
    fn failures_say_that_nothing_was_done() -> TestResult {
        let cases = [
            (
                Dialogs::failing(
                    "",
                    "execution error: Not authorized to send Apple events. (-1743)",
                ),
                "Automation",
            ),
            (
                Dialogs::failing(
                    "{\"ok\": false, \"error\": \"approval_dialog_failed: x\"}",
                    "",
                ),
                "could not be shown",
            ),
            (
                Dialogs::failing("{\"ok\": false, \"error\": \"boom\"}", ""),
                "failed (boom)",
            ),
            (
                Dialogs {
                    answer: Finished::TimedOut,
                    seen: Mutex::new(Vec::new()),
                },
                "did not finish in time",
            ),
        ];
        for (host, wanted) in cases {
            let Err(error) = approve_send(&host, &preview(), 20) else {
                return Err("expected an error".into());
            };
            assert!(error.message().contains(wanted), "{error}");
            assert!(error.message().contains("NOTHING"), "{error}");
            assert!(matches!(error, ToolError::Msg(_)));
        }
        Ok(())
    }
}
