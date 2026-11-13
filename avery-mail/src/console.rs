use std::io::{BufRead, Write};

use crate::error::ToolError;
use crate::rights::Level;
use crate::server::{self, Line};

const MAX_ANSWER_BYTES: usize = 8192;
const LEVEL_ATTEMPTS: usize = 3;

pub(crate) struct Console<'a> {
    pub(crate) input: &'a mut dyn BufRead,
    pub(crate) out: &'a mut dyn Write,
}

impl Console<'_> {
    pub(crate) fn say(&mut self, text: &str) {
        let _ = writeln!(self.out, "{text}");
    }

    pub(crate) fn ask(&mut self, prompt: &str) -> Option<String> {
        let _ = write!(self.out, "{prompt}");
        let _ = self.out.flush();
        match server::next_line(self.input, MAX_ANSWER_BYTES) {
            Ok(Line::Text(bytes)) => String::from_utf8(bytes).ok(),
            _ => None,
        }
    }

    pub(crate) fn confirm(&mut self, prompt: &str) -> bool {
        self.ask(prompt).is_some_and(|answer| {
            matches!(
                answer.trim().to_lowercase().as_str(),
                "j" | "ja" | "y" | "yes"
            )
        })
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

pub(crate) fn ask_levels(
    console: &mut Console,
    choices: &[Choice],
) -> Result<Vec<Option<Level>>, u8> {
    let mut chosen = Vec::with_capacity(choices.len());
    for choice in choices {
        let prompt = format!(
            "{}  (aktuell: {})  Stufe 0=aus 1=lesen 2=schreiben, Enter=unveraendert: ",
            choice.title,
            level_word(choice.current)
        );
        let mut answer = None;
        for _ in 0..LEVEL_ATTEMPTS {
            let Some(line) = console.ask(&prompt) else {
                console.say("Ungueltige Eingabe.");
                return Err(1);
            };
            match line.trim() {
                "" => answer = Some(choice.current),
                "0" => answer = Some(None),
                "1" => answer = Some(Some(Level::Read)),
                "2" if choice.highest == Level::Write => answer = Some(Some(Level::Write)),
                "2" => console.say("Dieser Eintrag ist schreibgeschuetzt (hoechstens Stufe 1)."),
                _ => console.say("Ungueltige Eingabe."),
            }
            if answer.is_some() {
                break;
            }
        }
        match answer {
            Some(level) => chosen.push(level),
            None => {
                console.say("Zu viele ungueltige Eingaben.");
                return Err(1);
            }
        }
    }
    Ok(chosen)
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
            let mut console = Console {
                input: &mut reader,
                out: &mut out,
            };
            ask_levels(&mut console, &choices())
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

    #[test]
    fn level_words_are_german() {
        assert_eq!(level_word(None), "aus");
        assert_eq!(level_word(Some(Level::Read)), "lesen");
        assert_eq!(level_word(Some(Level::Write)), "schreiben");
    }
}
