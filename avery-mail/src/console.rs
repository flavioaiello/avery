mod dialogs;

use std::io::{BufRead, Write};

use crate::error::ToolError;
use crate::osascript::Host;
use crate::rights::Level;
use crate::server::{self, Line};

pub(crate) use dialogs::{Buttons, Dialogs, Icon, Stop, Talk};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Area {
    Mail,
    Calendar,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GrantChange {
    pub(crate) area: Area,
    pub(crate) title: String,
    pub(crate) was: Option<Level>,
    pub(crate) now: Option<Level>,
}

pub(crate) struct Recap {
    pub(crate) mail: String,
    pub(crate) calendars: String,
    pub(crate) changes: Vec<GrantChange>,
}

const MAX_ANSWER_BYTES: usize = 8192;
const LEVEL_ATTEMPTS: usize = 3;
const INVALID: &str = "Ungueltige Eingabe.";
pub(crate) const PRIVATE_ONLY: &str =
    "Nur PRIVATE Konten und Kalender freigeben, niemals geschaeftliche.";
pub(crate) const RESTART_GOOSE: &str = "Goose danach neu starten.";

pub(crate) struct Screen<'a> {
    pub(crate) host: &'a dyn Host,
    pub(crate) gui: &'a dyn Fn() -> bool,
}

pub(crate) enum Console<'a> {
    Terminal(Terminal<'a>),
    Dialogs(Dialogs<'a>),
}

pub(crate) struct Terminal<'a> {
    input: &'a mut dyn BufRead,
    out: &'a mut dyn Write,
}

impl<'a> Console<'a> {
    pub(crate) fn terminal(input: &'a mut dyn BufRead, out: &'a mut dyn Write) -> Console<'a> {
        Console::Terminal(Terminal { input, out })
    }

    pub(crate) fn say(&mut self, text: &str) {
        match self {
            Console::Terminal(terminal) => terminal.say(text),
            Console::Dialogs(dialogs) => dialogs.say(text),
        }
    }

    pub(crate) fn hint(&mut self, text: &str) {
        match self {
            Console::Terminal(terminal) => terminal.say(text),
            Console::Dialogs(_) => {}
        }
    }

    pub(crate) fn ask(&mut self, prompt: &str) -> Option<String> {
        match self {
            Console::Terminal(terminal) => terminal.ask(prompt),
            Console::Dialogs(dialogs) => dialogs.ask(prompt),
        }
    }

    pub(crate) fn confirm(&mut self, question: &str) -> bool {
        match self {
            Console::Terminal(terminal) => terminal.confirm(question),
            Console::Dialogs(dialogs) => dialogs.confirm(question),
        }
    }

    pub(crate) fn confirm_grants(&mut self, recap: &Recap) -> bool {
        match self {
            Console::Terminal(terminal) => terminal.confirm_grants(recap),
            Console::Dialogs(dialogs) => dialogs.confirm_grants(recap),
        }
    }

    pub(crate) fn explain(&mut self) -> bool {
        match self {
            Console::Terminal(terminal) => terminal.explain(),
            Console::Dialogs(dialogs) => dialogs.explain(),
        }
    }

    pub(crate) fn ask_levels(&mut self, choices: &[Choice]) -> Result<Vec<Option<Level>>, u8> {
        match self {
            Console::Terminal(terminal) => terminal.ask_levels(choices),
            Console::Dialogs(dialogs) => dialogs.ask_levels(choices),
        }
    }
}

impl Terminal<'_> {
    fn say(&mut self, text: &str) {
        let _ = writeln!(self.out, "{text}");
    }

    fn ask(&mut self, prompt: &str) -> Option<String> {
        let _ = write!(self.out, "{prompt}");
        let _ = self.out.flush();
        match server::next_line(self.input, MAX_ANSWER_BYTES) {
            Ok(Line::Text(bytes)) => String::from_utf8(bytes).ok(),
            _ => None,
        }
    }

    fn confirm(&mut self, question: &str) -> bool {
        self.ask(&format!("{question} [j/N] "))
            .is_some_and(|answer| {
                matches!(
                    answer.trim().to_lowercase().as_str(),
                    "j" | "ja" | "y" | "yes"
                )
            })
    }

    fn confirm_grants(&mut self, recap: &Recap) -> bool {
        self.say("\nNeue Freigaben:");
        self.say(&format!("  Mail:     {}", recap.mail));
        self.say(&format!("  Kalender: {}", recap.calendars));
        self.confirm("Korrekt?")
    }

    fn explain(&mut self) -> bool {
        self.say("Avery liest und aendert Mail und Kalender ueber Apple Mail und Kalender. Dazu startet Avery beide Programme;");
        self.say("macOS fragt beim ersten Mal nach der Automation-Berechtigung (bitte erlauben).");
        true
    }

    fn ask_levels(&mut self, choices: &[Choice]) -> Result<Vec<Option<Level>>, u8> {
        let mut chosen = Vec::with_capacity(choices.len());
        for choice in choices {
            let prompt = format!(
                "{}  (aktuell: {})  Stufe 0=aus 1=lesen 2=schreiben, Enter=unveraendert: ",
                choice.title,
                level_word(choice.current)
            );
            let mut answer = None;
            for _ in 0..LEVEL_ATTEMPTS {
                let Some(line) = self.ask(&prompt) else {
                    self.say(INVALID);
                    return Err(1);
                };
                match line.trim() {
                    "" => answer = Some(choice.current),
                    "0" => answer = Some(None),
                    "1" => answer = Some(Some(Level::Read)),
                    "2" if choice.highest == Level::Write => answer = Some(Some(Level::Write)),
                    "2" => self.say("Dieser Eintrag ist schreibgeschuetzt (hoechstens Stufe 1)."),
                    _ => self.say(INVALID),
                }
                if answer.is_some() {
                    break;
                }
            }
            match answer {
                Some(level) => chosen.push(level),
                None => {
                    self.say("Zu viele ungueltige Eingaben.");
                    return Err(1);
                }
            }
        }
        Ok(chosen)
    }
}

pub(crate) fn failed(console: &mut Console, error: &ToolError) -> u8 {
    console.say(&format!("Fehler: {error}"));
    1
}

pub(crate) fn level_word(level: Option<Level>) -> &'static str {
    match level {
        None => "aus",
        Some(Level::Read) => "lesen",
        Some(Level::Write) => "schreiben",
    }
}

pub(crate) struct Choice {
    pub(crate) title: String,
    pub(crate) current: Option<Level>,
    pub(crate) highest: Level,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestResult;
    use std::io::Cursor;

    fn choices() -> Vec<Choice> {
        vec![
            Choice {
                title: "A".into(),
                current: None,
                highest: Level::Write,
            },
            Choice {
                title: "B".into(),
                current: Some(Level::Read),
                highest: Level::Read,
            },
        ]
    }

    fn asked(input: &str) -> (Result<Vec<Option<Level>>, u8>, String) {
        let mut out = Vec::new();
        let result = {
            let mut reader = Cursor::new(input.as_bytes().to_vec());
            let mut console = Console::terminal(&mut reader, &mut out);
            console.ask_levels(&choices())
        };
        (result, String::from_utf8_lossy(&out).into_owned())
    }

    #[test]
    fn levels_are_asked_per_item_and_enter_keeps_the_current_one() {
        assert_eq!(
            asked("2\n\n").0,
            Ok(vec![Some(Level::Write), Some(Level::Read)])
        );
        assert_eq!(asked("\n0\n").0, Ok(vec![None, None]));
        assert_eq!(
            asked(" 1 \n1\n").0,
            Ok(vec![Some(Level::Read), Some(Level::Read)])
        );
    }

    #[test]
    fn read_only_items_refuse_the_write_level_and_ask_again() {
        let (result, out) = asked("\n2\n1\n");
        assert_eq!(result, Ok(vec![None, Some(Level::Read)]));
        assert!(out.contains("schreibgeschuetzt"));
    }

    #[test]
    fn nonsense_missing_input_and_endless_retries_abort() {
        for input in ["", "\n", "x\n", "3\n\n", "-1\n\n"] {
            assert!(asked(input).0.is_err(), "{input:?}");
        }
        let (result, out) = asked("x\ny\nz\n");
        assert_eq!(result, Err(1));
        assert!(out.contains("Zu viele ungueltige Eingaben."));
        let (result, out) = asked("x\n3\n\n\n");
        assert_eq!(result, Ok(vec![None, Some(Level::Read)]));
        assert!(out.contains("Ungueltige Eingabe."));
        let long = format!("{}\n", "1".repeat(20_000));
        let (result, out) = asked(&long);
        assert_eq!(result, Err(1));
        assert!(out.contains("Ungueltige Eingabe."));
    }

    fn terminal(input: &str) -> (Vec<u8>, Cursor<Vec<u8>>) {
        (Vec::new(), Cursor::new(input.as_bytes().to_vec()))
    }

    #[test]
    fn a_confirmation_adds_the_choice_marker_and_accepts_only_yes_words() {
        for (input, expected) in [
            ("j\n", true),
            (" Ja \n", true),
            ("y\n", true),
            ("YES\n", true),
            ("\n", false),
            ("n\n", false),
            ("jein\n", false),
            ("", false),
        ] {
            let (mut out, mut reader) = terminal(input);
            let mut console = Console::terminal(&mut reader, &mut out);
            assert_eq!(console.confirm("Korrekt?"), expected, "{input:?}");
            assert_eq!(String::from_utf8_lossy(&out), "Korrekt? [j/N] ");
        }
    }

    #[test]
    fn a_hint_is_printed_like_a_line_of_text() {
        let (mut out, mut reader) = terminal("");
        let mut console = Console::terminal(&mut reader, &mut out);
        console.hint("eins");
        console.say("zwei");
        assert_eq!(String::from_utf8_lossy(&out), "eins\nzwei\n");
    }

    #[test]
    fn the_terminal_explains_the_automation_permission_and_never_stops_the_command() {
        let (mut out, mut reader) = terminal("");
        let mut console = Console::terminal(&mut reader, &mut out);
        assert!(console.explain());
        let text = String::from_utf8_lossy(&out).into_owned();
        assert!(text.contains("Automation-Berechtigung"), "{text}");
        assert!(text.contains("Apple Mail und Kalender"), "{text}");
    }

    #[test]
    fn the_dialog_console_shows_lines_with_the_next_question_and_drops_the_hints() -> TestResult {
        use crate::testutil::{Reply, Script};
        let script = Script::new()
            .then("ask_buttons", Reply::Button(1))
            .then("ask_text", Reply::text("42"));
        let mut talk = Talk::new();
        let mut console = Console::Dialogs(Dialogs::new(&script, &mut talk));
        console.say("Neue Freigaben:");
        console.hint("Goose danach neu starten.");
        assert!(console.confirm("Korrekt?"));
        assert_eq!(console.ask("Nummer?"), Some("42".to_string()));
        let seen = script.seen();
        assert_eq!(seen[0].text("text"), "Neue Freigaben:\n\nKorrekt?");
        assert_eq!(seen[1].text("text"), "Nummer?");
        assert!(script.mismatches().is_empty());
        Ok(())
    }

    #[test]
    fn level_words_are_german() {
        assert_eq!(level_word(None), "aus");
        assert_eq!(level_word(Some(Level::Read)), "lesen");
        assert_eq!(level_word(Some(Level::Write)), "schreiben");
    }
}
