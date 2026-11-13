mod dialogs;

use std::io::{BufRead, Write};

use crate::error::ToolError;
use crate::osascript::Host;
use crate::provider::Provider;
use crate::rights::Level;
use crate::server::{self, Line};

pub(crate) use dialogs::{Buttons, Dialogs, Icon, List, Stop, Talk};

const MAX_ANSWER_BYTES: usize = 8192;
const LEVEL_ATTEMPTS: usize = 3;
const INVALID: &str = "Ungueltige Eingabe.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Topic {
    Registration(Provider),
    AppleAccess,
}

pub(crate) struct Screen<'a> {
    pub(crate) host: &'a dyn Host,
    pub(crate) gui: fn() -> bool,
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

    pub(crate) fn explain(&mut self, topic: Topic) -> bool {
        match self {
            Console::Terminal(terminal) => terminal.explain(topic),
            Console::Dialogs(dialogs) => dialogs.explain(topic),
        }
    }

    pub(crate) fn ask_client(&mut self, provider: Provider) -> Option<(String, String)> {
        match self {
            Console::Terminal(terminal) => terminal.ask_client(provider),
            Console::Dialogs(dialogs) => dialogs.ask_client(provider),
        }
    }

    pub(crate) fn ask_write(&mut self) -> Option<bool> {
        match self {
            Console::Terminal(terminal) => terminal.ask_write(),
            Console::Dialogs(dialogs) => dialogs.ask_write(),
        }
    }

    pub(crate) fn browser(&mut self, url: &str) {
        match self {
            Console::Terminal(terminal) => terminal.browser(url),
            Console::Dialogs(dialogs) => dialogs.browser(),
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

    fn explain(&mut self, topic: Topic) -> bool {
        match topic {
            Topic::Registration(provider) => {
                for line in provider.oauth().map_or(&[][..], |oauth| oauth.registration) {
                    self.say(line);
                }
                self.say(&format!(
                    "Details: README, Abschnitt '{}'.",
                    Provider::remote_names(" and ")
                ));
                self.say("Avery speichert die Anmeldung im macOS-Schluesselbund (Dienst avery-mail), nie in einer Datei.");
                self.say("Der Agent bekommt nur das, was du danach mit 'avery mail setup' oder hier freigibst.");
            }
            Topic::AppleAccess => {
                self.say(&format!(
                    "{} einbeziehen (optional). Dazu startet Avery Mail und Kalender;",
                    Provider::Apple.display()
                ));
                self.say("macOS fragt beim ersten Mal nach der Automation-Berechtigung. Avery braucht dafuer keine Anmeldung.");
            }
        }
        true
    }

    fn ask_client(&mut self, provider: Provider) -> Option<(String, String)> {
        let Some(id) = self.ask("Client-ID: ") else {
            self.say(INVALID);
            return None;
        };
        if !provider.oauth().is_some_and(|oauth| oauth.secret) {
            return Some((id, String::new()));
        }
        let Some(secret) = self.ask("Client-Secret: ") else {
            self.say(INVALID);
            return None;
        };
        Some((id, secret))
    }

    fn ask_write(&mut self) -> Option<bool> {
        let answer = self.ask("Stufe: 1 = lesen, 2 = schreiben (Entwuerfe, Senden, Termine aendern; jedes Mal mit Klick) [1]: ");
        match answer.as_deref().map(str::trim) {
            Some("" | "1") => Some(false),
            Some("2") => Some(true),
            _ => {
                self.say(INVALID);
                None
            }
        }
    }

    fn browser(&mut self, url: &str) {
        self.say("\nDer Browser oeffnet sich. Melde dich an und erlaube den Zugriff. Falls nicht, oeffne diese Adresse:");
        self.say(&format!("  {url}"));
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
    fn the_client_is_asked_with_a_secret_only_where_the_provider_needs_one() {
        let (mut out, mut reader) = terminal("id-1\nsecret-1\n");
        let mut console = Console::terminal(&mut reader, &mut out);
        assert_eq!(
            console.ask_client(Provider::Google),
            Some(("id-1".to_string(), "secret-1".to_string()))
        );
        assert_eq!(String::from_utf8_lossy(&out), "Client-ID: Client-Secret: ");
        let (mut out, mut reader) = terminal("id-2\nignored\n");
        let mut console = Console::terminal(&mut reader, &mut out);
        assert_eq!(
            console.ask_client(Provider::Microsoft),
            Some(("id-2".to_string(), String::new()))
        );
        assert_eq!(String::from_utf8_lossy(&out), "Client-ID: ");
        for (input, shown) in [("", "Client-ID: "), ("id\n", "Client-ID: Client-Secret: ")] {
            let (mut out, mut reader) = terminal(input);
            let mut console = Console::terminal(&mut reader, &mut out);
            assert_eq!(console.ask_client(Provider::Google), None, "{input:?}");
            assert_eq!(String::from_utf8_lossy(&out), format!("{shown}{INVALID}\n"));
        }
    }

    #[test]
    fn the_write_level_defaults_to_read_and_refuses_nonsense() {
        for (input, expected) in [
            ("\n", Some(false)),
            ("1\n", Some(false)),
            (" 2 \n", Some(true)),
            ("3\n", None),
            ("x\n", None),
            ("", None),
        ] {
            let (mut out, mut reader) = terminal(input);
            let mut console = Console::terminal(&mut reader, &mut out);
            assert_eq!(console.ask_write(), expected, "{input:?}");
            assert_eq!(
                String::from_utf8_lossy(&out).contains(INVALID),
                expected.is_none(),
                "{input:?}"
            );
        }
    }

    #[test]
    fn topics_are_explained_in_lines_and_never_stop_the_command_line() {
        for provider in Provider::remote() {
            let (mut out, mut reader) = terminal("");
            let mut console = Console::terminal(&mut reader, &mut out);
            assert!(console.explain(Topic::Registration(provider)));
            let text = String::from_utf8_lossy(&out).into_owned();
            let registration = provider.oauth().map_or(&[][..], |oauth| oauth.registration);
            assert!(!registration.is_empty());
            for line in registration {
                assert!(text.contains(line), "{text}");
            }
            assert!(text.contains("Schluesselbund"), "{text}");
        }
        let (mut out, mut reader) = terminal("");
        let mut console = Console::terminal(&mut reader, &mut out);
        assert!(console.explain(Topic::AppleAccess));
        let text = String::from_utf8_lossy(&out).into_owned();
        assert!(text.contains("Automation-Berechtigung"), "{text}");
        assert!(
            text.contains("Apple Mail und Kalender einbeziehen"),
            "{text}"
        );
    }

    #[test]
    fn the_browser_hint_names_the_address() {
        let (mut out, mut reader) = terminal("");
        let mut console = Console::terminal(&mut reader, &mut out);
        console.browser("https://accounts.example/auth?x=1");
        let text = String::from_utf8_lossy(&out).into_owned();
        assert!(text.contains("Der Browser oeffnet sich"), "{text}");
        assert!(
            text.ends_with("  https://accounts.example/auth?x=1\n"),
            "{text}"
        );
    }

    #[test]
    fn level_words_are_german() {
        assert_eq!(level_word(None), "aus");
        assert_eq!(level_word(Some(Level::Read)), "lesen");
        assert_eq!(level_word(Some(Level::Write)), "schreiben");
    }
}
