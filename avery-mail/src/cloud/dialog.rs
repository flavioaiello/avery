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
    #[cfg(target_os = "macos")]
    const MOCK_DIALOG: &str = r#"
var LOG = {text: null, title: null, button: null, calls: 0};
var DECISION = 'approve';
function MOCKAPP() {
  return {
    includeStandardAdditions: false,
    activate: function () {},
    displayDialog: function (text, opts) {
      LOG.calls++; LOG.text = text; LOG.title = opts.withTitle; LOG.button = opts.buttons[1];
      if (DECISION === 'timeout') { return {gaveUp: true, buttonReturned: ''}; }
      if (DECISION === 'approve') { return {gaveUp: false, buttonReturned: opts.buttons[1]}; }
      throw new Error('User canceled. (-128)');
    }
  };
}
function run(argv) {
  var input = JSON.parse(argv[0]), out;
  DECISION = input.decision || 'approve';
  try { out = {ok: true, data: dispatch(input.req)}; }
  catch (e) { out = {ok: false, error: String((e && e.message) || e)}; }
  out.log = LOG;
  return JSON.stringify(out);
}
"#;

    #[cfg(target_os = "macos")]
    fn probe(decision: &str, request: &Json) -> Result<Json, Box<dyn std::error::Error>> {
        let script = format!(
            "{}\n{MOCK_DIALOG}",
            crate::osascript::dialog_script()
                .replace("Application.currentApplication()", "MOCKAPP()")
        );
        let payload = format!(
            r#"{{"decision": "{decision}", "req": {}}}"#,
            json::dumps(request, true)
        );
        let output = std::process::Command::new(crate::osascript::OSASCRIPT)
            .args(["-l", "JavaScript", "-e", &script, &payload])
            .output()?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
        }
        Ok(json::parse(String::from_utf8_lossy(&output.stdout).trim())?)
    }

    #[cfg(target_os = "macos")]
    fn text_at(value: &Json, path: &[&str]) -> String {
        let mut current = value;
        for key in path {
            match current.as_object().and_then(|object| object.get(key)) {
                Some(next) => current = next,
                None => return String::new(),
            }
        }
        current.as_str().unwrap_or_default().to_string()
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_send_dialog_script_shows_the_message_and_returns_the_decision() -> TestResult {
        let request = json::parse(
            r#"{"op": "approve_send", "from": "Alice <alice@gmail.example>", "to": ["anna@example.org", "bob@example.org"], "cc": [],
                "subject": "Hallo\u200b Welt", "body": "Zeile 1\nZeile 2", "reply": {"subject": "Re: x", "from": "Anna <anna@example.org>"}, "seconds": 75}"#,
        )?;
        for (decision, wanted) in [
            ("approve", "send"),
            ("decline", "declined"),
            ("timeout", "timeout"),
        ] {
            let answer = probe(decision, &request)?;
            assert_eq!(
                text_at(&answer, &["data", "decision"]),
                wanted,
                "{decision}"
            );
            let text = text_at(&answer, &["log", "text"]);
            assert!(
                text.contains("Von:        Alice <alice@gmail.example>"),
                "{text}"
            );
            assert!(
                text.contains("An:         anna@example.org, bob@example.org"),
                "{text}"
            );
            assert!(!text.contains("Cc:"), "{text}");
            assert!(text.contains("Betreff:    Hallo Welt"), "{text}");
            assert!(
                text.contains("Antwort auf: Re: x (von Anna <anna@example.org>)"),
                "{text}"
            );
            assert!(text.contains("Zeile 1\nZeile 2"), "{text}");
            assert_eq!(
                text_at(&answer, &["log", "title"]),
                "Goose möchte eine E-Mail senden"
            );
            assert_eq!(text_at(&answer, &["log", "button"]), "Senden");
        }
        let unknown = probe("approve", &json::parse(r#"{"op": "nope"}"#)?)?;
        assert_eq!(
            unknown.as_object().and_then(|o| o.get("ok")),
            Some(&Json::Bool(false))
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_change_dialog_script_shows_before_and_after_for_every_action() -> TestResult {
        let fields = |summary: &str, start: &str, end: &str| {
            format!(
                r#"{{"summary": "{summary}", "start": "{start}", "end": "{end}", "allDay": false, "location": "Büro", "notes": "", "url": ""}}"#
            )
        };
        let before = fields("Alt", "2030-05-06 09:00", "2030-05-06 10:00");
        let after = fields("Neu", "2030-05-06 11:00", "2030-05-06 12:00");
        for (action, undo, before, after, title, button, lines) in [
            ("create", false, "null", after.as_str(), "Goose möchte einen Termin anlegen", "Anlegen", vec!["Titel:      Neu"]),
            ("update", false, before.as_str(), after.as_str(), "Goose möchte einen Termin ändern", "Ändern", vec!["Titel:      Alt  →  Neu", "Wann:       2030-05-06 09:00 bis 2030-05-06 10:00  →  2030-05-06 11:00 bis 2030-05-06 12:00"]),
            ("delete", false, before.as_str(), "null", "Goose möchte einen Termin löschen", "Löschen", vec!["Titel:      Alt"]),
            ("update", true, before.as_str(), after.as_str(), "Goose: Änderung rückgängig machen", "Ändern", vec!["RÜCKGÄNGIG"]),
        ] {
            let request = json::parse(&format!(
                r#"{{"op": "approve_change", "action": "{action}", "undo": {undo}, "calendarLabel": "Privat", "before": {before}, "after": {after}, "seconds": 60}}"#
            ))?;
            let answer = probe("approve", &request)?;
            assert_eq!(text_at(&answer, &["data", "decision"]), "approved", "{action}");
            let text = text_at(&answer, &["log", "text"]);
            assert!(text.contains("Kalender:   Privat"), "{text}");
            for line in lines {
                assert!(text.contains(line), "{action}: {text}");
            }
            assert_eq!(text_at(&answer, &["log", "title"]), title);
            assert_eq!(text_at(&answer, &["log", "button"]), button);
            let declined = probe("decline", &request)?;
            assert_eq!(text_at(&declined, &["data", "decision"]), "declined");
        }
        Ok(())
    }
}
