use std::fmt;

use super::{Choice, Topic};
use crate::error::ToolError;
use crate::json::{Json, Object};
use crate::jxa::exchange;
use crate::osascript::{App, Host};
use crate::provider::{Page, Provider};
use crate::rights::Level;
use crate::text::{clean_str, one_line_str};

const SLACK: u64 = 60;
const ANSWER_SECONDS: u64 = 600;
const BUTTON_SECONDS: u64 = 300;
const LIST_SECONDS: u64 = 300;
const SIGNAL_SECONDS: u64 = 20;
const DEFAULT_TITLE: &str = "Avery einrichten";
const MAX_TITLE: usize = 70;
const MAX_TEXT: usize = 1800;
const MAX_SIGNAL: usize = 240;
const MAX_LABEL: usize = 32;
const MAX_ITEMS: usize = 500;
const MAX_ITEM: usize = 140;
const MAX_ANSWER: usize = 512;
const MAX_CLIENT: usize = 400;
const MAX_NOTE_LINE: usize = 400;
const MAX_NOTE_LINES: usize = 200;
const MAX_NOTE_CHARS: usize = 1500;
const CUT: &str = "[...]";
const TIMED_OUT: &str = "Keine Antwort im Dialog.";
const TOO_LONG: &str = "Die Eingabe war zu lang.";
const MALFORMED: &str = "Der Dialog hat eine unerwartete Antwort geliefert.";
const BAD_REQUEST: &str = "Der Dialog konnte nicht angezeigt werden (interner Fehler).";
const READ_PROMPT: &str = "Welche davon soll Avery lesen?";
const WRITE_PROMPT: &str = "Bei welchen darf Avery auch schreiben? Jede Änderung braucht danach deinen Klick in einem macOS-Dialog.";
const PAGES_PROMPT: &str = "Welche Seiten sollen sich im Browser öffnen?";
const PAGES_NOTICE: &str = "Registriere die App wie in der Anleitung. Danach startest du Avery erneut: die Einrichtung geht dort weiter.";
const BROWSER_NOTICE: &str =
    "Melde dich im Browser an und erlaube den Zugriff. Avery wartet bis zu 3 Minuten.";
const APPLE_TEXT: &str = "Avery startet jetzt Mail und Kalender und listet deine Konten und Kalender auf. macOS fragt dabei einmal um Erlaubnis (Automation): bitte erlauben. Avery braucht für Apple keine Anmeldung.\n\nNur PRIVATE Konten und Kalender freigeben, niemals geschaeftliche.";
const LEVEL_TEXT: &str = "Was darf Avery mit diesem Konto tun?\n\nNur lesen: Mails und Termine lesen.\nLesen und mit Freigabe schreiben: zusätzlich Entwürfe, Senden und Termine ändern; jede Änderung erst nach deinem Klick in einem macOS-Dialog, den der Agent nicht bedienen kann.";
const READ_ONLY: &str = "Nur lesen";
const READ_WRITE: &str = "Lesen und mit Freigabe schreiben";
const OPEN_GUIDE: &str = "Anleitung öffnen";
const CANCEL: &str = "Abbrechen";
const NEXT: &str = "Weiter";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    Cancelled,
    TimedOut,
    Away,
    Failed(String),
}

impl fmt::Display for Stop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Stop::Cancelled => f.write_str("Abgebrochen."),
            Stop::TimedOut => f.write_str(TIMED_OUT),
            Stop::Away => f.write_str("Die Anleitung wurde geöffnet."),
            Stop::Failed(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for Stop {}

#[derive(Default)]
struct Notes {
    lines: Vec<String>,
    cut: bool,
}

impl Notes {
    fn push(&mut self, text: &str) {
        for line in text.split('\n') {
            self.lines.push(clean_str(line, Some(MAX_NOTE_LINE)));
            if self.lines.len() > MAX_NOTE_LINES {
                self.lines.remove(0);
                self.cut = true;
            }
        }
    }

    fn take(&mut self) -> String {
        let mut cut = std::mem::take(&mut self.cut);
        let lines = std::mem::take(&mut self.lines);
        let mut kept: Vec<&str> = Vec::new();
        let mut used = 0usize;
        for line in lines.iter().rev() {
            used += line.chars().count() + 1;
            if used > MAX_NOTE_CHARS {
                cut = true;
                break;
            }
            kept.push(line);
        }
        kept.reverse();
        let body = clean_str(&kept.join("\n"), None);
        match (cut, body.is_empty()) {
            (_, true) => String::new(),
            (true, false) => format!("{CUT}\n{body}"),
            (false, false) => body,
        }
    }
}

pub(crate) struct Talk {
    title: String,
    notes: Notes,
    stop: Option<Stop>,
}

impl Talk {
    pub(crate) fn new() -> Talk {
        Talk {
            title: DEFAULT_TITLE.to_string(),
            notes: Notes::default(),
            stop: None,
        }
    }

    pub(crate) fn set_title(&mut self, title: &str) {
        self.title = one_line_str(title, MAX_TITLE);
    }

    pub(crate) fn take_stop(&mut self) -> Option<Stop> {
        self.stop.take()
    }

    pub(crate) fn take_notes(&mut self) -> String {
        self.notes.take()
    }
}

pub(crate) struct Text<'a> {
    pub(crate) text: &'a str,
    pub(crate) default: &'a str,
    pub(crate) hidden: bool,
    pub(crate) max: usize,
    pub(crate) ok: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Icon {
    Note,
    Caution,
}

impl Icon {
    fn as_str(self) -> &'static str {
        match self {
            Icon::Note => "note",
            Icon::Caution => "caution",
        }
    }
}

pub(crate) struct Buttons<'a> {
    pub(crate) text: &'a str,
    pub(crate) buttons: &'a [&'a str],
    pub(crate) default: usize,
    pub(crate) cancel: Option<usize>,
    pub(crate) icon: Icon,
}

pub(crate) struct List<'a> {
    pub(crate) text: &'a str,
    pub(crate) items: &'a [String],
    pub(crate) defaults: &'a [usize],
    pub(crate) multiple: bool,
    pub(crate) ok: &'a str,
}

#[derive(Debug, PartialEq, Eq)]
enum Parsed {
    Text(String),
    Button(usize),
    List(Vec<usize>),
    Done,
    TooLong,
    Cancelled,
    TimedOut,
}

fn index_of(value: &Json) -> Option<usize> {
    match value {
        Json::Int(number) => usize::try_from(*number).ok(),
        _ => None,
    }
}

fn parse_reply(data: &Json) -> Option<Parsed> {
    let object = data.as_object()?;
    let result = object.get("result")?.as_str()?;
    let keys: &[&str] = match result {
        "text" => &["result", "text"],
        "button" => &["result", "index"],
        "list" => &["result", "selected"],
        "cancel" | "timeout" | "too_long" | "ok" => &["result"],
        _ => return None,
    };
    if !object.keys().all(|key| keys.contains(&key)) || object.len() != keys.len() {
        return None;
    }
    match result {
        "text" => Some(Parsed::Text(object.get("text")?.as_str()?.to_string())),
        "button" => Some(Parsed::Button(index_of(object.get("index")?)?)),
        "list" => {
            let selected: Option<Vec<usize>> = object
                .get("selected")?
                .as_array()?
                .iter()
                .map(index_of)
                .collect();
            Some(Parsed::List(selected?))
        }
        "cancel" => Some(Parsed::Cancelled),
        "timeout" => Some(Parsed::TimedOut),
        "too_long" => Some(Parsed::TooLong),
        _ => Some(Parsed::Done),
    }
}

fn failure(text: &str, _op: Option<&str>, _uncertain: bool) -> ToolError {
    let low = text.to_lowercase();
    if text == "timed out" || low.contains("-1712") {
        return ToolError::msg(TIMED_OUT);
    }
    if low.contains("-1743") || low.contains("not authorized") || low.contains("nicht berechtigt") {
        return ToolError::msg(concat!(
            "macOS erlaubt die Dialoge nicht. Bitte in den Systemeinstellungen unter Datenschutz & Sicherheit die ",
            "Berechtigung erlauben, nach der macOS gefragt hat (Automation oder Benachrichtigungen), und Avery erneut starten."
        ));
    }
    if low.contains("bad_request") {
        return ToolError::msg(BAD_REQUEST);
    }
    if low.contains("dialog_failed") || low.contains("-1713") {
        return ToolError::msg(
            "Der Dialog konnte nicht angezeigt werden. Avery braucht eine angemeldete Sitzung am Mac mit entsperrtem Bildschirm.",
        );
    }
    ToolError::msg(format!(
        "Der Dialog ist fehlgeschlagen ({}).",
        one_line_str(text, 120)
    ))
}

fn seconds(count: u64) -> Json {
    Json::Int(i64::try_from(count).unwrap_or(i64::MAX))
}

fn label(text: &str) -> String {
    one_line_str(text, MAX_LABEL)
}

fn tail(text: &str, count: usize) -> String {
    let total = text.chars().count();
    if total <= count {
        return text.to_string();
    }
    let kept: String = text.chars().skip(total - count).collect();
    format!("{CUT}{kept}")
}

fn numbered(index: usize, text: &str) -> String {
    format!("{}. {}", index + 1, one_line_str(text, MAX_ITEM))
}

pub(crate) struct Dialogs<'a> {
    host: &'a dyn Host,
    talk: &'a mut Talk,
}

impl<'a> Dialogs<'a> {
    pub(crate) fn new(host: &'a dyn Host, talk: &'a mut Talk) -> Dialogs<'a> {
        Dialogs { host, talk }
    }

    fn base(&self, op: &str, text: &str) -> Object {
        let mut request = Object::new();
        request.insert("op", op);
        request.insert("title", self.talk.title.as_str());
        request.insert("text", text);
        request
    }

    fn framed(&mut self, text: &str) -> String {
        let own = clean_str(text, Some(MAX_TEXT));
        let notes = self.talk.notes.take();
        let room = MAX_TEXT.saturating_sub(own.chars().count() + 2);
        if notes.is_empty() || room < CUT.len() + 8 {
            return own;
        }
        format!("{}\n\n{own}", tail(&notes, room))
    }

    fn converse(&self, request: Object, wait: u64) -> Result<Parsed, Stop> {
        match exchange(self.host, App::Dialog, request, wait, failure) {
            Ok(data) => match parse_reply(&data) {
                Some(Parsed::Cancelled) => Err(Stop::Cancelled),
                Some(Parsed::TimedOut) => Err(Stop::TimedOut),
                Some(parsed) => Ok(parsed),
                None => Err(Stop::Failed(MALFORMED.to_string())),
            },
            Err(error) if error.message() == TIMED_OUT => Err(Stop::TimedOut),
            Err(error) => Err(Stop::Failed(error.message())),
        }
    }

    fn asked(&mut self, request: Object, wait: u64) -> Result<Parsed, Stop> {
        let answer = self.converse(request, wait);
        if let Err(stop) = &answer {
            self.talk.stop = Some(stop.clone());
        }
        answer
    }

    fn fail(&mut self, text: &str) -> Stop {
        let stop = Stop::Failed(text.to_string());
        self.talk.stop = Some(stop.clone());
        stop
    }

    pub(crate) fn ask_text(&mut self, ask: &Text) -> Result<String, Stop> {
        let max = ask.max.clamp(1, MAX_ANSWER);
        let text = self.framed(ask.text);
        let mut request = self.base("ask_text", &text);
        request.insert("default", one_line_str(ask.default, max));
        request.insert("hidden", ask.hidden);
        request.insert("max", max);
        request.insert("ok", label(ask.ok));
        request.insert("cancel", CANCEL);
        request.insert("seconds", seconds(ANSWER_SECONDS));
        match self.asked(request, ANSWER_SECONDS + SLACK)? {
            Parsed::Text(answer) => {
                let answer = clean_str(&answer, None);
                if answer.chars().count() > max {
                    return Err(self.fail(TOO_LONG));
                }
                Ok(answer)
            }
            Parsed::TooLong => Err(self.fail(TOO_LONG)),
            _ => Err(self.fail(MALFORMED)),
        }
    }

    pub(crate) fn ask_buttons(&mut self, ask: &Buttons) -> Result<usize, Stop> {
        let valid = (1..=3).contains(&ask.buttons.len())
            && ask.default < ask.buttons.len()
            && ask.cancel.is_none_or(|cancel| cancel < ask.buttons.len());
        if !valid {
            return Err(self.fail(BAD_REQUEST));
        }
        let text = self.framed(ask.text);
        let mut request = self.base("ask_buttons", &text);
        let names: Vec<String> = ask.buttons.iter().map(|name| label(name)).collect();
        request.insert("buttons", names);
        request.insert("default", ask.default);
        request.insert("cancel", ask.cancel.map_or(Json::Null, Json::from));
        request.insert("icon", ask.icon.as_str());
        request.insert("seconds", seconds(BUTTON_SECONDS));
        match self.asked(request, BUTTON_SECONDS + SLACK)? {
            Parsed::Button(index) if index < ask.buttons.len() => Ok(index),
            _ => Err(self.fail(MALFORMED)),
        }
    }

    pub(crate) fn choose_list(&mut self, ask: &List) -> Result<Vec<usize>, Stop> {
        if ask.items.is_empty() || ask.items.len() > MAX_ITEMS {
            return Err(self.fail(BAD_REQUEST));
        }
        let text = self.framed(ask.text);
        let mut request = self.base("choose_list", &text);
        let items: Vec<String> = ask
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| numbered(index, item))
            .collect();
        let mut defaults: Vec<usize> = ask
            .defaults
            .iter()
            .copied()
            .filter(|index| *index < items.len())
            .collect();
        defaults.sort_unstable();
        defaults.dedup();
        if !ask.multiple {
            defaults.truncate(1);
        }
        request.insert("items", items);
        request.insert(
            "defaults",
            Json::Arr(defaults.into_iter().map(Json::from).collect()),
        );
        request.insert("multiple", ask.multiple);
        request.insert("ok", label(ask.ok));
        request.insert("cancel", CANCEL);
        request.insert("seconds", seconds(LIST_SECONDS));
        match self.asked(request, LIST_SECONDS)? {
            Parsed::List(selected)
                if selected
                    .windows(2)
                    .all(|pair| matches!(pair, [low, high] if low < high))
                    && selected.iter().all(|index| *index < ask.items.len())
                    && (ask.multiple || selected.len() <= 1) =>
            {
                Ok(selected)
            }
            _ => Err(self.fail(MALFORMED)),
        }
    }

    fn signal(&self, request: Object) -> bool {
        matches!(self.converse(request, SIGNAL_SECONDS), Ok(Parsed::Done))
    }

    pub(crate) fn notify(&mut self, text: &str) {
        let shown = clean_str(text, Some(MAX_SIGNAL));
        let request = self.base("notify", &shown);
        self.signal(request);
    }

    pub(crate) fn open_url(&mut self, page: Page) -> bool {
        let mut request = Object::new();
        request.insert("op", "open_url");
        request.insert("url", page.url());
        self.signal(request)
    }

    pub(crate) fn say(&mut self, text: &str) {
        self.talk.notes.push(text);
    }

    pub(crate) fn ask(&mut self, prompt: &str) -> Option<String> {
        self.ask_text(&Text {
            text: prompt,
            default: "",
            hidden: false,
            max: MAX_ANSWER,
            ok: "OK",
        })
        .ok()
    }

    pub(crate) fn confirm(&mut self, question: &str) -> bool {
        matches!(
            self.ask_buttons(&Buttons {
                text: question,
                buttons: &["Nein", "Ja"],
                default: 0,
                cancel: Some(0),
                icon: Icon::Note,
            }),
            Ok(1)
        )
    }

    pub(crate) fn explain(&mut self, topic: Topic) -> bool {
        match topic {
            Topic::Registration(provider) => self.explain_registration(provider),
            Topic::AppleAccess => matches!(
                self.ask_buttons(&Buttons {
                    text: APPLE_TEXT,
                    buttons: &[CANCEL, NEXT],
                    default: 1,
                    cancel: Some(0),
                    icon: Icon::Note,
                }),
                Ok(1)
            ),
        }
    }

    fn explain_registration(&mut self, provider: Provider) -> bool {
        let Some(oauth) = provider.oauth() else {
            return true;
        };
        match self.ask_buttons(&Buttons {
            text: oauth.explanation,
            buttons: &[CANCEL, OPEN_GUIDE, NEXT],
            default: 2,
            cancel: Some(0),
            icon: Icon::Note,
        }) {
            Ok(2) => true,
            Ok(1) => {
                self.open_pages(provider);
                false
            }
            _ => false,
        }
    }

    fn open_pages(&mut self, provider: Provider) {
        let pages = provider.pages();
        let labels: Vec<String> = pages.iter().map(|page| page.label().to_string()).collect();
        let Ok(chosen) = self.choose_list(&List {
            text: PAGES_PROMPT,
            items: &labels,
            defaults: &[0, 1],
            multiple: true,
            ok: "Öffnen",
        }) else {
            return;
        };
        for page in chosen.into_iter().filter_map(|index| pages.get(index)) {
            self.open_url(*page);
        }
        self.notify(PAGES_NOTICE);
        self.talk.stop = Some(Stop::Away);
    }

    pub(crate) fn ask_client(&mut self, provider: Provider) -> Option<(String, String)> {
        let name = provider.display();
        let id = self
            .ask_text(&Text {
                text: &format!("{name}: Client-ID deiner App"),
                default: "",
                hidden: false,
                max: MAX_CLIENT,
                ok: NEXT,
            })
            .ok()?;
        if !provider.oauth().is_some_and(|oauth| oauth.secret) {
            return Some((id, String::new()));
        }
        let secret = self
            .ask_text(&Text {
                text: &format!(
                    "{name}: Client-Secret deiner App. Es wird nicht angezeigt und nur im macOS-Schlüsselbund gespeichert."
                ),
                default: "",
                hidden: true,
                max: MAX_CLIENT,
                ok: NEXT,
            })
            .ok()?;
        Some((id, secret))
    }

    pub(crate) fn ask_write(&mut self) -> Option<bool> {
        match self.ask_buttons(&Buttons {
            text: LEVEL_TEXT,
            buttons: &[CANCEL, READ_ONLY, READ_WRITE],
            default: 1,
            cancel: Some(0),
            icon: Icon::Note,
        }) {
            Ok(1) => Some(false),
            Ok(2) => Some(true),
            _ => None,
        }
    }

    pub(crate) fn browser(&mut self) {
        self.notify(BROWSER_NOTICE);
    }

    pub(crate) fn ask_levels(&mut self, choices: &[Choice]) -> Result<Vec<Option<Level>>, u8> {
        if choices.is_empty() {
            return Ok(Vec::new());
        }
        let titles: Vec<String> = choices.iter().map(|choice| choice.title.clone()).collect();
        let current: Vec<usize> = choices
            .iter()
            .enumerate()
            .filter(|(_, choice)| choice.current.is_some())
            .map(|(index, _)| index)
            .collect();
        let read = self
            .choose_list(&List {
                text: READ_PROMPT,
                items: &titles,
                defaults: &current,
                multiple: true,
                ok: NEXT,
            })
            .map_err(|_| 1u8)?;
        let mut levels: Vec<Option<Level>> = (0..choices.len())
            .map(|index| read.contains(&index).then_some(Level::Read))
            .collect();
        let writable: Vec<(usize, &Choice)> = choices
            .iter()
            .enumerate()
            .filter(|(index, choice)| read.contains(index) && choice.highest == Level::Write)
            .collect();
        if writable.is_empty() {
            return Ok(levels);
        }
        let items: Vec<String> = writable
            .iter()
            .map(|(_, choice)| choice.title.clone())
            .collect();
        let preset: Vec<usize> = writable
            .iter()
            .enumerate()
            .filter(|(_, (_, choice))| choice.current == Some(Level::Write))
            .map(|(at, _)| at)
            .collect();
        let write = self
            .choose_list(&List {
                text: WRITE_PROMPT,
                items: &items,
                defaults: &preset,
                multiple: true,
                ok: NEXT,
            })
            .map_err(|_| 1u8)?;
        for (index, _) in write.iter().filter_map(|at| writable.get(*at)) {
            if let Some(level) = levels.get_mut(*index) {
                *level = Some(Level::Write);
            }
        }
        Ok(levels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::console::PRIVATE_ONLY;
    use crate::json;
    use crate::testutil::{Reply, Script, TestResult};
    use std::time::Duration;

    fn titled(texts: &[(&str, Level, Option<Level>)]) -> Vec<Choice> {
        texts
            .iter()
            .map(|(title, highest, current)| Choice {
                title: title.to_string(),
                current: *current,
                highest: *highest,
            })
            .collect()
    }

    fn text(prompt: &str) -> Text<'_> {
        Text {
            text: prompt,
            default: "",
            hidden: false,
            max: 40,
            ok: "Weiter",
        }
    }

    fn chars(value: &str) -> usize {
        value.chars().count()
    }

    #[test]
    fn a_text_question_is_a_strict_bounded_request_and_returns_the_answer() -> TestResult {
        let script = Script::new().then("ask_text", Reply::text("abc-123"));
        let mut talk = Talk::new();
        talk.set_title("Gmail verbinden");
        let answer = Dialogs::new(&script, &mut talk).ask_text(&Text {
            text: "Client-ID",
            default: "alt",
            hidden: true,
            max: 40,
            ok: "Weiter",
        })?;
        assert_eq!(answer, "abc-123");
        let seen = script.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].wait, Duration::from_secs(ANSWER_SECONDS + SLACK));
        let request = json::parse(&seen[0].payload)?;
        let object = request.as_object().ok_or("object")?;
        let mut keys: Vec<&str> = object.keys().collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["cancel", "default", "hidden", "max", "ok", "op", "seconds", "text", "title"]
        );
        assert_eq!(seen[0].text("op"), "ask_text");
        assert_eq!(seen[0].text("title"), "Gmail verbinden");
        assert_eq!(seen[0].text("text"), "Client-ID");
        assert_eq!(seen[0].text("default"), "alt");
        assert_eq!(seen[0].text("cancel"), "Abbrechen");
        assert_eq!(seen[0].field("hidden"), Some(Json::Bool(true)));
        assert_eq!(seen[0].field("max"), Some(Json::Int(40)));
        assert_eq!(seen[0].field("seconds"), Some(Json::Int(600)));
        assert!(script.mismatches().is_empty());
        Ok(())
    }

    #[test]
    fn the_answer_is_cleaned_and_a_secret_stays_out_of_every_request() -> TestResult {
        let secret = "GOCSPX-s3cr3t_value";
        let script = Script::new().then("ask_text", Reply::text("id-1")).then(
            "ask_text",
            Reply::text(&format!("\u{200b}{secret}\u{202e}\n")),
        );
        let mut talk = Talk::new();
        let (id, found) = Dialogs::new(&script, &mut talk)
            .ask_client(Provider::Google)
            .ok_or("client")?;
        assert_eq!(id, "id-1");
        assert_eq!(found, secret);
        let requests = script.seen();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].field("hidden"), Some(Json::Bool(false)));
        assert_eq!(requests[1].field("hidden"), Some(Json::Bool(true)));
        for request in &requests {
            assert!(!request.payload.contains(secret), "{}", request.payload);
        }
        Ok(())
    }

    #[test]
    fn answers_beyond_the_bound_are_refused_and_never_returned() -> TestResult {
        for reply in [
            Reply::text(&"x".repeat(41)),
            Reply::text(&"x".repeat(200_000)),
            Reply::TooLong,
        ] {
            let script = Script::new().then("ask_text", reply);
            let mut talk = Talk::new();
            let answer = Dialogs::new(&script, &mut talk).ask_text(&text("Frage"));
            assert_eq!(answer, Err(Stop::Failed(TOO_LONG.to_string())));
            assert_eq!(talk.take_stop(), Some(Stop::Failed(TOO_LONG.to_string())));
        }
        let script = Script::new().then("ask_text", Reply::text(&"x".repeat(40)));
        let mut talk = Talk::new();
        assert_eq!(
            Dialogs::new(&script, &mut talk).ask_text(&text("Frage")),
            Ok("x".repeat(40))
        );
        Ok(())
    }

    #[test]
    fn cancel_timeout_and_a_hung_helper_stop_the_conversation_and_say_why() -> TestResult {
        for (reply, expected) in [
            (Reply::Cancel, Stop::Cancelled),
            (Reply::Timeout, Stop::TimedOut),
            (Reply::Hung, Stop::TimedOut),
        ] {
            let script = Script::new().then("ask_text", reply);
            let mut talk = Talk::new();
            assert_eq!(
                Dialogs::new(&script, &mut talk).ask_text(&text("Frage")),
                Err(expected.clone())
            );
            assert_eq!(talk.take_stop(), Some(expected));
            assert_eq!(talk.take_stop(), None);
        }
        Ok(())
    }

    #[test]
    fn wrong_shapes_wrong_json_and_failing_helpers_are_refused_without_echoing_them() -> TestResult
    {
        let leak = "LEAK-ME-NOT";
        let hostile = [
            Reply::Data(format!(
                "{{\"result\": \"text\", \"text\": 5, \"x\": \"{leak}\"}}"
            )),
            Reply::Data("{\"result\": \"text\"}".to_string()),
            Reply::Data(format!(
                "{{\"result\": \"text\", \"text\": \"a\", \"extra\": \"{leak}\"}}"
            )),
            Reply::Data(format!("{{\"result\": \"{leak}\"}}")),
            Reply::Data("{}".to_string()),
            Reply::Data("[1]".to_string()),
            Reply::Data("\"text\"".to_string()),
            Reply::Data("null".to_string()),
            Reply::Data("{\"result\": \"button\", \"index\": 0}".to_string()),
            Reply::Data("{\"result\": \"list\", \"selected\": []}".to_string()),
            Reply::Raw {
                code: Some(0),
                stdout: format!("this is not json {leak}"),
                stderr: String::new(),
            },
            Reply::Raw {
                code: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            },
            Reply::Raw {
                code: Some(0),
                stdout: format!("{{\"ok\": false, \"error\": \"bad_request: {leak}\"}}"),
                stderr: String::new(),
            },
            Reply::Raw {
                code: Some(1),
                stdout: String::new(),
                stderr: format!("execution error: {leak} (-2700)"),
            },
            Reply::Silent,
            Reply::Flood,
        ];
        for reply in hostile {
            let script = Script::new().then("ask_text", reply.clone());
            let mut talk = Talk::new();
            let Err(Stop::Failed(message)) = Dialogs::new(&script, &mut talk).ask_text(&text("F"))
            else {
                return Err(format!("{reply:?} was accepted").into());
            };
            assert!(!message.is_empty());
            assert!(!message.contains("secret"), "{message}");
            if !matches!(reply, Reply::Raw { code: Some(1), .. }) {
                assert!(!message.contains(leak), "{reply:?}: {message}");
            }
            assert!(
                matches!(talk.take_stop(), Some(Stop::Failed(_))),
                "{reply:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn macos_refusing_the_dialogs_is_explained() -> TestResult {
        for stderr in [
            "execution error: Not authorized to send Apple events to System Events. (-1743)",
            "Nicht berechtigt",
        ] {
            let script = Script::new().then(
                "ask_text",
                Reply::Raw {
                    code: Some(1),
                    stdout: String::new(),
                    stderr: stderr.to_string(),
                },
            );
            let mut talk = Talk::new();
            let Err(Stop::Failed(message)) = Dialogs::new(&script, &mut talk).ask_text(&text("F"))
            else {
                return Err("expected a failure".into());
            };
            assert!(message.contains("Automation"), "{message}");
            assert!(message.contains("Benachrichtigungen"), "{message}");
            assert!(message.contains("Datenschutz & Sicherheit"), "{message}");
            assert!(!message.contains("System Events"), "{message}");
        }
        let script = Script::new().then(
            "ask_text",
            Reply::Raw {
                code: Some(0),
                stdout: "{\"ok\": false, \"error\": \"dialog_failed: no window\"}".to_string(),
                stderr: String::new(),
            },
        );
        let mut talk = Talk::new();
        let Err(Stop::Failed(message)) = Dialogs::new(&script, &mut talk).ask_text(&text("F"))
        else {
            return Err("expected a failure".into());
        };
        assert!(message.contains("angemeldete Sitzung"), "{message}");
        Ok(())
    }

    #[test]
    fn shown_text_is_cleaned_bounded_and_carries_the_notes_before_the_question() -> TestResult {
        let script = Script::new()
            .then("ask_text", Reply::text("a"))
            .then("ask_text", Reply::text("b"))
            .then("ask_text", Reply::text("c"));
        let mut talk = Talk::new();
        talk.set_title(&format!("Ti\u{202e}tel\u{200b}{}", "t".repeat(500)));
        let mut dialogs = Dialogs::new(&script, &mut talk);
        dialogs.say("Hinweis\u{200b} eins\u{202e}");
        dialogs.say("zwei\r\ndrei");
        dialogs.ask_text(&text(&format!("Frage\u{0}{}", "q".repeat(500))))?;
        dialogs.ask_text(&text("Zweite"))?;
        dialogs.say("verloren");
        dialogs.ask_text(&text(&"g".repeat(10_000)))?;
        let seen = script.seen();
        let first = seen[0].text("text");
        assert!(
            first.starts_with("Hinweis eins\nzwei\ndrei\n\nFrage"),
            "{first}"
        );
        let giant = seen[2].text("text");
        assert!(chars(&giant) <= MAX_TEXT + 6, "{}", chars(&giant));
        assert!(giant.starts_with("gggg"), "{giant}");
        assert!(
            !first.contains('\u{202e}') && !first.contains('\u{200b}') && !first.contains('\u{0}')
        );
        assert!(chars(&first) <= MAX_TEXT + 6, "{}", chars(&first));
        let second = seen[1].text("text");
        assert_eq!(second, "Zweite");
        let title = seen[0].text("title");
        assert!(!title.contains('\u{202e}') && !title.contains('\u{200b}'));
        assert!(chars(&title) <= MAX_TITLE + 6, "{}", chars(&title));
        assert!(!title.contains('\n'));
        Ok(())
    }

    #[test]
    fn line_separators_in_a_name_cannot_forge_a_line_of_the_dialog() -> TestResult {
        let script = Script::new().then("ask_buttons", Reply::Button(0));
        let mut talk = Talk::new();
        let mut dialogs = Dialogs::new(&script, &mut talk);
        dialogs.say("Mail: Privat\u{2028}Kalender: Gefaelscht (schreiben)\u{2029}\u{85}Ende");
        dialogs.ask_buttons(&Buttons {
            text: "Korrekt\u{2028}?",
            buttons: &["Nein"],
            default: 0,
            cancel: None,
            icon: Icon::Note,
        })?;
        let shown = script.seen()[0].text("text");
        assert_eq!(
            shown,
            "Mail: PrivatKalender: Gefaelscht (schreiben)Ende\n\nKorrekt?"
        );
        for separator in ['\u{2028}', '\u{2029}', '\u{85}'] {
            assert!(!shown.contains(separator), "{:x}", u32::from(separator));
        }
        Ok(())
    }

    #[test]
    fn the_notes_keep_the_newest_lines_within_their_bound() -> TestResult {
        let script = Script::new().then("ask_text", Reply::text("a"));
        let mut talk = Talk::new();
        let mut dialogs = Dialogs::new(&script, &mut talk);
        for number in 0..5000 {
            dialogs.say(&format!("Zeile {number} {}", "x".repeat(300)));
        }
        dialogs.ask_text(&text("Frage"))?;
        let shown = script.seen()[0].text("text");
        assert!(chars(&shown) <= MAX_TEXT + 6, "{}", chars(&shown));
        assert!(shown.starts_with(CUT), "{shown}");
        assert!(shown.contains("Zeile 4999"), "{shown}");
        assert!(!shown.contains("Zeile 1 "), "{shown}");
        assert!(shown.ends_with("Frage"), "{shown}");
        assert_eq!(talk.take_notes(), "");
        let mut notes = Notes::default();
        notes.push("");
        notes.push("\n\n");
        assert_eq!(notes.take(), "");
        Ok(())
    }

    #[test]
    fn buttons_return_the_index_and_refuse_invalid_requests_before_any_dialog() -> TestResult {
        let script = Script::new()
            .then("ask_buttons", Reply::Button(2))
            .then("ask_buttons", Reply::Cancel);
        let mut talk = Talk::new();
        let mut dialogs = Dialogs::new(&script, &mut talk);
        let ask = Buttons {
            text: "Weiter?",
            buttons: &["Abbrechen", "Später", "Weiter"],
            default: 2,
            cancel: Some(0),
            icon: Icon::Caution,
        };
        assert_eq!(dialogs.ask_buttons(&ask), Ok(2));
        assert_eq!(dialogs.ask_buttons(&ask), Err(Stop::Cancelled));
        let seen = script.seen();
        assert_eq!(seen[0].wait, Duration::from_secs(BUTTON_SECONDS + SLACK));
        assert_eq!(seen[0].text("icon"), "caution");
        assert_eq!(seen[0].field("cancel"), Some(Json::Int(0)));
        assert_eq!(seen[0].field("default"), Some(Json::Int(2)));
        let names = seen[0].field("buttons").ok_or("buttons")?;
        assert_eq!(
            names,
            json::parse("[\"Abbrechen\", \"Später\", \"Weiter\"]")?
        );
        let quiet = Script::new();
        let mut talk = Talk::new();
        let mut dialogs = Dialogs::new(&quiet, &mut talk);
        for (buttons, default, cancel) in [
            (&[][..], 0, None),
            (&["a", "b", "c", "d"][..], 0, None),
            (&["a", "b"][..], 2, None),
            (&["a", "b"][..], 0, Some(2)),
        ] {
            let ask = Buttons {
                text: "x",
                buttons,
                default,
                cancel,
                icon: Icon::Note,
            };
            assert!(matches!(dialogs.ask_buttons(&ask), Err(Stop::Failed(_))));
        }
        assert!(quiet.seen().is_empty());
        let none = Script::new().then("ask_buttons", Reply::Button(0));
        let mut talk = Talk::new();
        assert_eq!(
            Dialogs::new(&none, &mut talk).ask_buttons(&Buttons {
                text: "Nur OK",
                buttons: &["OK"],
                default: 0,
                cancel: None,
                icon: Icon::Note,
            }),
            Ok(0)
        );
        assert_eq!(none.seen()[0].field("cancel"), Some(Json::Null));
        Ok(())
    }

    #[test]
    fn button_labels_are_cleaned_and_bounded_like_every_other_text() -> TestResult {
        let script = Script::new().then("ask_buttons", Reply::Button(1));
        let mut talk = Talk::new();
        let long = "L".repeat(500);
        let answer = Dialogs::new(&script, &mut talk).ask_buttons(&Buttons {
            text: "x",
            buttons: &["Ab\u{202e}bre\u{200b}chen", &long],
            default: 1,
            cancel: Some(0),
            icon: Icon::Note,
        });
        assert_eq!(answer, Ok(1));
        let names = script.seen()[0].field("buttons").ok_or("buttons")?;
        let names = names.as_array().ok_or("array")?;
        assert_eq!(names[0].as_str(), Some("Abbrechen"));
        let second = names[1].as_str().ok_or("label")?;
        assert!(chars(second) <= MAX_LABEL + 6, "{}", chars(second));
        assert!(!second.contains('\n'));
        Ok(())
    }

    #[test]
    fn a_button_that_does_not_exist_is_not_believed() -> TestResult {
        for reply in [
            Reply::Button(2),
            Reply::Button(usize::MAX / 2),
            Reply::Data("{\"result\": \"button\", \"index\": -1}".to_string()),
            Reply::Data("{\"result\": \"button\", \"index\": 1.5}".to_string()),
            Reply::Data("{\"result\": \"button\", \"index\": \"1\"}".to_string()),
            Reply::Data("{\"result\": \"button\"}".to_string()),
            Reply::text("1"),
            Reply::List(vec![0]),
        ] {
            let script = Script::new().then("ask_buttons", reply.clone());
            let mut talk = Talk::new();
            let answer = Dialogs::new(&script, &mut talk).ask_buttons(&Buttons {
                text: "x",
                buttons: &["Nein", "Ja"],
                default: 0,
                cancel: Some(0),
                icon: Icon::Note,
            });
            assert!(
                matches!(answer, Err(Stop::Failed(_))),
                "{reply:?}: {answer:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn a_list_numbers_equal_titles_and_returns_the_chosen_positions() -> TestResult {
        let script = Script::new()
            .then("choose_list", Reply::List(vec![0, 2]))
            .then("choose_list", Reply::List(Vec::new()))
            .then("choose_list", Reply::Cancel);
        let mut talk = Talk::new();
        let mut dialogs = Dialogs::new(&script, &mut talk);
        let items = vec![
            "Privat".to_string(),
            "Privat".to_string(),
            "Fa\u{200b}milie\n".to_string(),
        ];
        let ask = List {
            text: "Welche?",
            items: &items,
            defaults: &[2, 0, 2, 9],
            multiple: true,
            ok: "Weiter",
        };
        assert_eq!(dialogs.choose_list(&ask), Ok(vec![0, 2]));
        assert_eq!(dialogs.choose_list(&ask), Ok(Vec::new()));
        assert_eq!(dialogs.choose_list(&ask), Err(Stop::Cancelled));
        let seen = script.seen();
        assert_eq!(seen[0].wait, Duration::from_secs(LIST_SECONDS));
        assert_eq!(
            seen[0].field("items"),
            Some(json::parse(
                "[\"1. Privat\", \"2. Privat\", \"3. Familie\"]"
            )?)
        );
        assert_eq!(seen[0].field("defaults"), Some(json::parse("[0, 2]")?));
        assert_eq!(seen[0].field("multiple"), Some(Json::Bool(true)));
        assert_eq!(seen[0].text("cancel"), "Abbrechen");
        Ok(())
    }

    #[test]
    fn a_list_refuses_selections_that_make_no_sense_and_requests_that_are_too_big() -> TestResult {
        let items = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        for (reply, multiple) in [
            (Reply::List(vec![3]), true),
            (Reply::List(vec![1, 1]), true),
            (Reply::List(vec![2, 1]), true),
            (Reply::List(vec![0, 1]), false),
            (Reply::List(vec![0, 1, 2, 3]), true),
            (
                Reply::Data("{\"result\": \"list\", \"selected\": [-1]}".to_string()),
                true,
            ),
            (
                Reply::Data("{\"result\": \"list\", \"selected\": [\"0\"]}".to_string()),
                true,
            ),
            (
                Reply::Data("{\"result\": \"list\", \"selected\": 0}".to_string()),
                true,
            ),
            (Reply::Data("{\"result\": \"list\"}".to_string()), true),
            (Reply::text("A"), true),
        ] {
            let script = Script::new().then("choose_list", reply.clone());
            let mut talk = Talk::new();
            let answer = Dialogs::new(&script, &mut talk).choose_list(&List {
                text: "x",
                items: &items,
                defaults: &[],
                multiple,
                ok: "OK",
            });
            assert!(
                matches!(answer, Err(Stop::Failed(_))),
                "{reply:?}: {answer:?}"
            );
        }
        let quiet = Script::new();
        let mut talk = Talk::new();
        let mut dialogs = Dialogs::new(&quiet, &mut talk);
        let many: Vec<String> = (0..=MAX_ITEMS).map(|number| number.to_string()).collect();
        for list in [&[][..], &many[..]] {
            assert!(matches!(
                dialogs.choose_list(&List {
                    text: "x",
                    items: list,
                    defaults: &[],
                    multiple: true,
                    ok: "OK"
                }),
                Err(Stop::Failed(_))
            ));
        }
        assert!(quiet.seen().is_empty());
        let single = Script::new().then("choose_list", Reply::List(vec![1]));
        let mut talk = Talk::new();
        assert_eq!(
            Dialogs::new(&single, &mut talk).choose_list(&List {
                text: "x",
                items: &items,
                defaults: &[2, 0],
                multiple: false,
                ok: "OK",
            }),
            Ok(vec![1])
        );
        assert_eq!(
            single.seen()[0].field("defaults"),
            Some(json::parse("[0]")?)
        );
        let wide = vec!["w".repeat(5000)];
        let long = Script::new().then("choose_list", Reply::List(vec![0]));
        let mut talk = Talk::new();
        Dialogs::new(&long, &mut talk).choose_list(&List {
            text: "x",
            items: &wide,
            defaults: &[],
            multiple: true,
            ok: "OK",
        })?;
        let item = long.seen()[0]
            .field("items")
            .and_then(|items| items.as_array().and_then(|list| list.first().cloned()))
            .and_then(|item| item.as_str().map(str::to_string))
            .ok_or("item")?;
        assert!(chars(&item) <= MAX_ITEM + 6 + 5, "{}", chars(&item));
        Ok(())
    }

    #[test]
    fn notifications_and_pages_are_best_effort_and_never_stop_the_conversation() -> TestResult {
        let script = Script::new()
            .then("notify", Reply::Done)
            .then("notify", Reply::Silent)
            .then("open_url", Reply::Done)
            .then("open_url", Reply::Hung)
            .then("open_url", Reply::Cancel);
        let mut talk = Talk::new();
        let mut dialogs = Dialogs::new(&script, &mut talk);
        dialogs.notify("Hallo\u{202e} Welt");
        dialogs.notify(&"n".repeat(5000));
        assert!(dialogs.open_url(Page::Readme));
        assert!(!dialogs.open_url(Page::GoogleProject));
        assert!(!dialogs.open_url(Page::EntraApps));
        assert_eq!(talk.take_stop(), None);
        let seen = script.seen();
        assert_eq!(seen[0].text("text"), "Hallo Welt");
        assert!(chars(&seen[1].text("text")) <= MAX_SIGNAL + 6);
        assert_eq!(seen[0].wait, Duration::from_secs(SIGNAL_SECONDS));
        assert_eq!(seen[2].text("url"), Page::Readme.url());
        let request = json::parse(&seen[2].payload)?;
        let object = request.as_object().ok_or("object")?;
        assert_eq!(object.len(), 2);
        Ok(())
    }

    #[test]
    fn the_pages_are_https_addresses_of_three_known_hosts_and_every_page_is_offered() {
        let hosts = [
            "github.com",
            "console.cloud.google.com",
            "entra.microsoft.com",
        ];
        let mut offered: Vec<Page> = Vec::new();
        for provider in Provider::ALL {
            offered.extend_from_slice(provider.pages());
            for page in provider.pages() {
                assert!(!page.label().is_empty());
                assert!(chars(page.label()) <= MAX_ITEM);
            }
        }
        assert!(Provider::Apple.pages().is_empty());
        for page in Page::ALL {
            assert!(offered.contains(&page), "{page:?}");
            let url = page.url();
            assert!(url.starts_with("https://"), "{url}");
            assert!(url.len() <= 300, "{url}");
            assert!(
                url.is_ascii() && !url.contains(char::is_whitespace),
                "{url}"
            );
            let host = url["https://".len()..]
                .split(['/', '#', '?'])
                .next()
                .unwrap_or("");
            assert!(hosts.contains(&host), "{url}");
            assert!(!url.contains('?') && !url.contains('@'), "{url}");
        }
        assert_eq!(offered.len(), Page::ALL.len() + 1);
    }

    #[test]
    fn levels_are_two_lists_read_first_then_write_for_what_may_be_written() -> TestResult {
        let script = Script::new()
            .then("choose_list", Reply::List(vec![0, 1, 3]))
            .then("choose_list", Reply::List(vec![1]));
        let mut talk = Talk::new();
        let mut dialogs = Dialogs::new(&script, &mut talk);
        let choices = titled(&[
            ("Privat", Level::Write, Some(Level::Write)),
            ("Familie", Level::Write, None),
            (
                "Feiertage [schreibgeschuetzt]",
                Level::Read,
                Some(Level::Read),
            ),
            ("Arbeit", Level::Write, Some(Level::Read)),
        ]);
        let levels = dialogs
            .ask_levels(&choices)
            .map_err(|code| format!("{code}"))?;
        assert_eq!(
            levels,
            [
                Some(Level::Read),
                Some(Level::Write),
                None,
                Some(Level::Read)
            ]
        );
        let seen = script.seen();
        assert_eq!(seen[0].field("defaults"), Some(json::parse("[0, 2, 3]")?));
        assert_eq!(
            seen[1].field("items"),
            Some(json::parse(
                "[\"1. Privat\", \"2. Familie\", \"3. Arbeit\"]"
            )?)
        );
        assert_eq!(seen[1].field("defaults"), Some(json::parse("[0]")?));
        assert!(seen[1].text("text").contains("Klick"));
        Ok(())
    }

    #[test]
    fn levels_without_a_writable_choice_ask_once_and_an_empty_list_asks_nothing() -> TestResult {
        let script = Script::new().then("choose_list", Reply::List(vec![0]));
        let mut talk = Talk::new();
        let mut dialogs = Dialogs::new(&script, &mut talk);
        let choices = titled(&[("Nur lesen", Level::Read, None)]);
        assert_eq!(dialogs.ask_levels(&choices), Ok(vec![Some(Level::Read)]));
        assert_eq!(script.seen().len(), 1);
        assert_eq!(dialogs.ask_levels(&[]), Ok(Vec::new()));
        assert_eq!(script.seen().len(), 1);
        let nothing = Script::new().then("choose_list", Reply::List(Vec::new()));
        let mut talk = Talk::new();
        let choices = titled(&[("A", Level::Write, Some(Level::Write))]);
        assert_eq!(
            Dialogs::new(&nothing, &mut talk).ask_levels(&choices),
            Ok(vec![None])
        );
        assert_eq!(nothing.seen().len(), 1);
        Ok(())
    }

    #[test]
    fn cancelling_either_list_of_the_levels_stops_with_nothing_chosen() -> TestResult {
        for script in [
            Script::new().then("choose_list", Reply::Cancel),
            Script::new()
                .then("choose_list", Reply::List(vec![0]))
                .then("choose_list", Reply::Timeout),
            Script::new().then("choose_list", Reply::Hung),
        ] {
            let mut talk = Talk::new();
            let choices = titled(&[("A", Level::Write, None)]);
            assert_eq!(
                Dialogs::new(&script, &mut talk).ask_levels(&choices),
                Err(1)
            );
            assert!(matches!(
                talk.take_stop(),
                Some(Stop::Cancelled | Stop::TimedOut)
            ));
        }
        Ok(())
    }

    #[test]
    fn a_confirmation_needs_the_yes_button_and_defaults_to_no() -> TestResult {
        let script = Script::new()
            .then("ask_buttons", Reply::Button(1))
            .then("ask_buttons", Reply::Cancel)
            .then("ask_buttons", Reply::Timeout)
            .then("ask_buttons", Reply::Button(0));
        let mut talk = Talk::new();
        let mut dialogs = Dialogs::new(&script, &mut talk);
        dialogs.say("Neue Freigaben:");
        assert!(dialogs.confirm("Korrekt?"));
        assert!(!dialogs.confirm("Korrekt?"));
        assert_eq!(talk.take_stop(), Some(Stop::Cancelled));
        let mut dialogs = Dialogs::new(&script, &mut talk);
        assert!(!dialogs.confirm("Korrekt?"));
        assert_eq!(talk.take_stop(), Some(Stop::TimedOut));
        let mut dialogs = Dialogs::new(&script, &mut talk);
        assert!(!dialogs.confirm("Korrekt?"));
        let seen = script.seen();
        assert_eq!(seen[0].text("text"), "Neue Freigaben:\n\nKorrekt?");
        assert_eq!(seen[0].field("default"), Some(Json::Int(0)));
        assert_eq!(
            seen[0].field("buttons"),
            Some(json::parse("[\"Nein\", \"Ja\"]")?)
        );
        Ok(())
    }

    #[test]
    fn the_registration_explanation_goes_on_only_with_the_third_button() -> TestResult {
        for provider in Provider::remote() {
            let explanation = provider.oauth().ok_or("oauth")?.explanation;
            let script = Script::new().then("ask_buttons", Reply::Button(2));
            let mut talk = Talk::new();
            assert!(Dialogs::new(&script, &mut talk).explain(Topic::Registration(provider)));
            let seen = script.seen();
            assert_eq!(seen[0].text("text"), explanation);
            assert_eq!(
                seen[0].field("buttons"),
                Some(json::parse(
                    "[\"Abbrechen\", \"Anleitung öffnen\", \"Weiter\"]"
                )?)
            );
            assert_eq!(seen[0].field("default"), Some(Json::Int(2)));
            for reply in [Reply::Cancel, Reply::Timeout, Reply::Silent] {
                let script = Script::new().then("ask_buttons", reply);
                let mut talk = Talk::new();
                assert!(!Dialogs::new(&script, &mut talk).explain(Topic::Registration(provider)));
                assert!(talk.take_stop().is_some());
            }
        }
        let script = Script::new();
        let mut talk = Talk::new();
        assert!(Dialogs::new(&script, &mut talk).explain(Topic::Registration(Provider::Apple)));
        assert!(script.seen().is_empty());
        Ok(())
    }

    #[test]
    fn the_guide_opens_the_chosen_pages_tells_so_and_ends_the_run_without_an_error() -> TestResult {
        let script = Script::new()
            .then("ask_buttons", Reply::Button(1))
            .then("choose_list", Reply::List(vec![0, 4]))
            .then("open_url", Reply::Done)
            .then("open_url", Reply::Done)
            .then("notify", Reply::Done);
        let mut talk = Talk::new();
        assert!(!Dialogs::new(&script, &mut talk).explain(Topic::Registration(Provider::Google)));
        assert_eq!(talk.take_stop(), Some(Stop::Away));
        let seen = script.seen();
        assert!(script.mismatches().is_empty(), "{:?}", script.mismatches());
        assert_eq!(seen[2].text("url"), Page::Readme.url());
        assert_eq!(seen[3].text("url"), Page::GoogleConsent.url());
        assert_eq!(seen[1].field("defaults"), Some(json::parse("[0, 1]")?));
        let labels = seen[1].field("items").ok_or("items")?;
        assert_eq!(
            labels.as_array().map(Vec::len),
            Some(Provider::Google.pages().len())
        );
        assert!(seen[4].text("text").contains("startest du Avery erneut"));
        for reply in [Reply::Cancel, Reply::Timeout] {
            let script = Script::new()
                .then("ask_buttons", Reply::Button(1))
                .then("choose_list", reply);
            let mut talk = Talk::new();
            assert!(
                !Dialogs::new(&script, &mut talk).explain(Topic::Registration(Provider::Microsoft))
            );
            assert!(matches!(
                talk.take_stop(),
                Some(Stop::Cancelled | Stop::TimedOut)
            ));
            assert_eq!(script.seen().len(), 2);
        }
        Ok(())
    }

    #[test]
    fn the_apple_explanation_waits_for_the_go_ahead() -> TestResult {
        let script = Script::new()
            .then("ask_buttons", Reply::Button(1))
            .then("ask_buttons", Reply::Cancel);
        let mut talk = Talk::new();
        let mut dialogs = Dialogs::new(&script, &mut talk);
        assert!(dialogs.explain(Topic::AppleAccess));
        assert!(!dialogs.explain(Topic::AppleAccess));
        let seen = script.seen();
        assert!(seen[0].text("text").contains("Automation"));
        assert_eq!(
            seen[0].field("buttons"),
            Some(json::parse("[\"Abbrechen\", \"Weiter\"]")?)
        );
        Ok(())
    }

    #[test]
    fn the_apple_step_says_to_share_private_accounts_only_like_the_terminal_does() -> TestResult {
        let script = Script::new().then("ask_buttons", Reply::Button(1));
        let mut talk = Talk::new();
        assert!(Dialogs::new(&script, &mut talk).explain(Topic::AppleAccess));
        let text = script.seen()[0].text("text");
        assert!(text.contains(PRIVATE_ONLY), "{text}");
        assert!(
            text.contains("Nur PRIVATE Konten und Kalender freigeben, niemals geschaeftliche"),
            "{text}"
        );
        assert!(APPLE_TEXT.contains(PRIVATE_ONLY));
        assert!(chars(&text) < MAX_TEXT);
        Ok(())
    }

    #[test]
    fn the_client_is_asked_for_a_secret_only_where_the_provider_needs_one() -> TestResult {
        let script = Script::new().then("ask_text", Reply::text("0a1b-guid"));
        let mut talk = Talk::new();
        let got = Dialogs::new(&script, &mut talk).ask_client(Provider::Microsoft);
        assert_eq!(got, Some(("0a1b-guid".to_string(), String::new())));
        assert_eq!(script.seen().len(), 1);
        for replies in [
            vec![Reply::Cancel],
            vec![Reply::text("id"), Reply::Cancel],
            vec![Reply::text("id"), Reply::Timeout],
            vec![Reply::text(&"x".repeat(401))],
        ] {
            let mut script = Script::new();
            for reply in replies {
                script = script.then("ask_text", reply);
            }
            let mut talk = Talk::new();
            assert_eq!(
                Dialogs::new(&script, &mut talk).ask_client(Provider::Google),
                None
            );
            assert!(talk.take_stop().is_some());
        }
        Ok(())
    }

    #[test]
    fn the_write_level_is_chosen_with_buttons_and_cancel_means_nothing() -> TestResult {
        for (reply, expected) in [
            (Reply::Button(1), Some(false)),
            (Reply::Button(2), Some(true)),
            (Reply::Cancel, None),
            (Reply::Timeout, None),
        ] {
            let script = Script::new().then("ask_buttons", reply);
            let mut talk = Talk::new();
            assert_eq!(Dialogs::new(&script, &mut talk).ask_write(), expected);
            let seen = script.seen();
            assert_eq!(
                seen[0].field("buttons"),
                Some(json::parse(
                    "[\"Abbrechen\", \"Nur lesen\", \"Lesen und mit Freigabe schreiben\"]"
                )?)
            );
            assert_eq!(seen[0].field("default"), Some(Json::Int(1)));
        }
        Ok(())
    }

    #[test]
    fn the_browser_step_is_a_notification_without_the_address() -> TestResult {
        let script = Script::new().then("notify", Reply::Done);
        let mut talk = Talk::new();
        Dialogs::new(&script, &mut talk).browser();
        let seen = script.seen();
        assert!(seen[0].text("text").contains("Browser"));
        assert!(!seen[0].payload.contains("http"));
        Ok(())
    }

    #[test]
    fn a_plain_question_uses_the_text_dialog() -> TestResult {
        let script = Script::new()
            .then("ask_text", Reply::text("42"))
            .then("ask_text", Reply::Cancel);
        let mut talk = Talk::new();
        let mut dialogs = Dialogs::new(&script, &mut talk);
        assert_eq!(dialogs.ask("Nummer?"), Some("42".to_string()));
        assert_eq!(dialogs.ask("Nummer?"), None);
        assert_eq!(script.seen()[0].field("max"), Some(Json::Int(512)));
        Ok(())
    }

    #[test]
    fn replies_are_parsed_strictly() -> TestResult {
        for (text, expected) in [
            (
                "{\"result\": \"text\", \"text\": \"a\"}",
                Some(Parsed::Text("a".into())),
            ),
            (
                "{\"result\": \"button\", \"index\": 2}",
                Some(Parsed::Button(2)),
            ),
            (
                "{\"result\": \"list\", \"selected\": [0, 3]}",
                Some(Parsed::List(vec![0, 3])),
            ),
            (
                "{\"result\": \"list\", \"selected\": []}",
                Some(Parsed::List(Vec::new())),
            ),
            ("{\"result\": \"ok\"}", Some(Parsed::Done)),
            ("{\"result\": \"cancel\"}", Some(Parsed::Cancelled)),
            ("{\"result\": \"timeout\"}", Some(Parsed::TimedOut)),
            ("{\"result\": \"too_long\"}", Some(Parsed::TooLong)),
            ("{\"result\": \"ok\", \"x\": 1}", None),
            ("{\"result\": \"cancel\", \"text\": \"a\"}", None),
            (
                "{\"result\": \"text\", \"text\": \"a\", \"index\": 1}",
                None,
            ),
            (
                "{\"result\": \"button\", \"index\": 1, \"text\": \"a\"}",
                None,
            ),
            ("{\"result\": 1}", None),
            ("{\"decision\": \"send\"}", None),
            ("{\"result\": \"list\", \"selected\": [0, null]}", None),
            ("{\"result\": \"list\", \"selected\": [0.5]}", None),
            (
                "{\"result\": \"list\", \"selected\": [9223372036854775808]}",
                None,
            ),
        ] {
            assert_eq!(parse_reply(&json::parse(text)?), expected, "{text}");
        }
        Ok(())
    }

    #[test]
    fn the_failure_texts_name_the_way_out_and_cut_unknown_text() {
        assert_eq!(failure("timed out", None, false).message(), TIMED_OUT);
        assert_eq!(failure("x (-1712)", None, false).message(), TIMED_OUT);
        assert!(failure("(-1743)", None, true)
            .message()
            .contains("Automation"));
        assert_eq!(
            failure("bad_request: title", None, false).message(),
            BAD_REQUEST
        );
        assert!(failure("dialog_failed: x", None, false)
            .message()
            .contains("Sitzung"));
        let long = failure(&"y".repeat(1000), None, false).message();
        assert!(
            long.starts_with("Der Dialog ist fehlgeschlagen ("),
            "{long}"
        );
        assert!(chars(&long) < 200, "{}", chars(&long));
    }

    #[test]
    fn a_title_that_is_set_is_cleaned_and_used_by_every_dialog() -> TestResult {
        let script = Script::new()
            .then("ask_buttons", Reply::Button(0))
            .then("choose_list", Reply::List(Vec::new()));
        let mut talk = Talk::new();
        talk.set_title("Outlook.com verbinden");
        let mut dialogs = Dialogs::new(&script, &mut talk);
        dialogs.ask_buttons(&Buttons {
            text: "x",
            buttons: &["OK"],
            default: 0,
            cancel: None,
            icon: Icon::Note,
        })?;
        dialogs.choose_list(&List {
            text: "x",
            items: &["a".to_string()],
            defaults: &[],
            multiple: true,
            ok: "OK",
        })?;
        for seen in script.seen() {
            assert_eq!(seen.text("title"), "Outlook.com verbinden");
        }
        assert_eq!(Talk::new().title, DEFAULT_TITLE);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    const MOCK_APP: &str = r#"
var LOG = {calls: [], text: null, opts: null, items: null, url: null};
var MOCK = {};
function MOCKAPP() {
  return {
    includeStandardAdditions: false,
    activate: function () { LOG.calls.push('activate'); },
    displayDialog: function (text, opts) {
      LOG.calls.push('displayDialog'); LOG.text = text; LOG.opts = opts;
      if (MOCK.fail) { throw new Error(MOCK.fail); }
      if (MOCK.gaveUp) { return {gaveUp: true, buttonReturned: '', textReturned: ''}; }
      return {gaveUp: false, buttonReturned: MOCK.button === undefined ? opts.defaultButton : MOCK.button,
              textReturned: MOCK.typed === undefined ? '' : MOCK.typed};
    },
    chooseFromList: function (items, opts) {
      LOG.calls.push('chooseFromList'); LOG.items = items; LOG.text = opts.withPrompt; LOG.opts = opts;
      if (MOCK.fail) { throw new Error(MOCK.fail); }
      return MOCK.chosen;
    },
    displayNotification: function (text, opts) {
      LOG.calls.push('displayNotification'); LOG.text = text; LOG.opts = opts;
      if (MOCK.fail) { throw new Error(MOCK.fail); }
    },
    openLocation: function (url) {
      LOG.calls.push('openLocation'); LOG.url = url;
      if (MOCK.fail) { throw new Error(MOCK.fail); }
    }
  };
}
function run(argv) {
  var input = JSON.parse(argv[0]), out;
  MOCK = input.mock || {};
  try { out = {ok: true, data: dispatch(input.req)}; }
  catch (e) { out = {ok: false, error: String((e && e.message) || e)}; }
  out.log = LOG;
  return JSON.stringify(out);
}
"#;

    #[cfg(target_os = "macos")]
    struct Probe {
        ok: bool,
        data: Json,
        error: String,
        log: Json,
    }

    #[cfg(target_os = "macos")]
    impl Probe {
        fn calls(&self) -> Vec<String> {
            self.log
                .as_object()
                .and_then(|log| log.get("calls"))
                .and_then(Json::as_array)
                .map(|calls| {
                    calls
                        .iter()
                        .filter_map(|call| call.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        }

        fn logged(&self, key: &str) -> Json {
            self.log
                .as_object()
                .and_then(|log| log.get(key))
                .cloned()
                .unwrap_or(Json::Null)
        }

        fn option(&self, key: &str) -> Json {
            self.logged("opts")
                .as_object()
                .and_then(|opts| opts.get(key))
                .cloned()
                .unwrap_or(Json::Null)
        }

        fn result(&self) -> String {
            self.data
                .as_object()
                .and_then(|data| data.get("result"))
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_string()
        }
    }

    #[cfg(target_os = "macos")]
    fn probe(mock: &str, request: &str) -> Result<Probe, Box<dyn std::error::Error>> {
        let script = format!(
            "{}\n{MOCK_APP}",
            crate::osascript::dialog_script()
                .replace("Application.currentApplication()", "MOCKAPP()")
        );
        assert!(
            !script.contains("Application.currentApplication") && !script.contains("Application("),
            "a real application object would show a real dialog"
        );
        let payload = format!("{{\"mock\": {mock}, \"req\": {request}}}");
        let output = std::process::Command::new(crate::osascript::OSASCRIPT)
            .args(["-l", "JavaScript", "-e", &script, &payload])
            .output()?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
        }
        let answer = json::parse(String::from_utf8_lossy(&output.stdout).trim())?;
        let object = answer.as_object().ok_or("object")?;
        Ok(Probe {
            ok: object.get("ok") == Some(&Json::Bool(true)),
            data: object.get("data").cloned().unwrap_or(Json::Null),
            error: object
                .get("error")
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_string(),
            log: object.get("log").cloned().unwrap_or(Json::Null),
        })
    }

    #[cfg(target_os = "macos")]
    fn quote(text: &str) -> String {
        json::dumps(&Json::from(text), true)
    }

    #[cfg(target_os = "macos")]
    fn ask_text_request(extra: &str) -> String {
        format!(
            r#"{{"op": "ask_text", "title": "Titel", "text": "Frage", "default": "", "hidden": false, "max": 20,
                 "ok": "Weiter", "cancel": "Abbrechen", "seconds": 30{extra}}}"#
        )
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_text_script_shows_clean_text_and_maps_every_way_out() -> TestResult {
        let request = r#"{"op": "ask_text", "title": "Ti\u200btel", "text": "Frage\u202e?\nZweite Zeile", "default": "d", "hidden": true,
                          "max": 20, "ok": "Weiter", "cancel": "Abbrechen", "seconds": 30}"#;
        let shown = probe(r#"{"typed": "abc"}"#, request)?;
        assert!(shown.ok, "{}", shown.error);
        assert_eq!(shown.result(), "text");
        assert_eq!(
            shown.data,
            json::parse(r#"{"result": "text", "text": "abc"}"#)?
        );
        assert_eq!(shown.calls(), ["activate", "displayDialog"]);
        assert_eq!(shown.logged("text"), Json::from("Frage?\nZweite Zeile"));
        assert_eq!(shown.option("withTitle"), Json::from("Titel"));
        assert_eq!(shown.option("hiddenAnswer"), Json::Bool(true));
        assert_eq!(shown.option("defaultAnswer"), Json::from("d"));
        assert_eq!(shown.option("givingUpAfter"), Json::Int(30));
        assert_eq!(shown.option("defaultButton"), Json::from("Weiter"));
        assert_eq!(shown.option("cancelButton"), Json::from("Abbrechen"));
        assert_eq!(
            shown.option("buttons"),
            json::parse(r#"["Abbrechen", "Weiter"]"#)?
        );
        for (mock, expected) in [
            (r#"{"button": "Abbrechen"}"#, "cancel"),
            (r#"{"fail": "User canceled. (-128)"}"#, "cancel"),
            (r#"{"gaveUp": true}"#, "timeout"),
            (r#"{"typed": "123456789012345678901"}"#, "too_long"),
        ] {
            let answer = probe(mock, &ask_text_request(""))?;
            assert!(answer.ok, "{mock}: {}", answer.error);
            assert_eq!(answer.result(), expected, "{mock}");
        }
        let exact = probe(
            r#"{"typed": "12345678901234567890"}"#,
            &ask_text_request(""),
        )?;
        assert_eq!(exact.result(), "text");
        let broken = probe(r#"{"fail": "boom"}"#, &ask_text_request(""))?;
        assert!(!broken.ok);
        assert_eq!(broken.error, "dialog_failed: boom");
        let separators = probe(
            r#"{"typed": "abc"}"#,
            &ask_text_request("").replace(
                "\"Frage\"",
                r#""Zeile\u2028Zwei\u2029Drei\u0085Vier\nFuenf""#,
            ),
        )?;
        assert!(separators.ok, "{}", separators.error);
        assert_eq!(
            separators.logged("text"),
            Json::from("ZeileZweiDreiVier\nFuenf")
        );
        let open = probe(r#"{"typed": "x"}"#, &ask_text_request(""))?;
        assert_eq!(open.option("hiddenAnswer"), Json::Bool(false));
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_text_script_refuses_malformed_requests_before_any_window_opens() -> TestResult {
        let long = |count: usize| "x".repeat(count);
        let bad = [
            ask_text_request("").replace("\"Titel\"", "5"),
            ask_text_request("").replace("\"Titel\"", &quote(&long(81))),
            ask_text_request("").replace("\"Frage\"", &quote(&long(2001))),
            ask_text_request("").replace("\"default\": \"\"", "\"default\": 1"),
            ask_text_request("").replace("\"hidden\": false", "\"hidden\": \"true\""),
            ask_text_request("").replace("\"max\": 20", "\"max\": 0"),
            ask_text_request("").replace("\"max\": 20", "\"max\": 1001"),
            ask_text_request("").replace("\"max\": 20", "\"max\": 1.5"),
            ask_text_request("").replace("\"seconds\": 30", "\"seconds\": 0"),
            ask_text_request("").replace("\"seconds\": 30", "\"seconds\": 3601"),
            ask_text_request("").replace("\"seconds\": 30", "\"seconds\": \"30\""),
            ask_text_request("").replace("\"Abbrechen\"", "\"Weiter\""),
            ask_text_request("").replace("\"Weiter\"", "\"\""),
            ask_text_request("").replace("\"Weiter\"", &quote(&long(41))),
            ask_text_request("").replace(r#""cancel": "Abbrechen", "#, ""),
            ask_text_request(r#", "icon": "weird""#),
        ];
        for request in bad {
            let answer = probe(r#"{"typed": "x"}"#, &request)?;
            assert!(!answer.ok, "{request}");
            assert!(
                answer.error.starts_with("bad_request"),
                "{request}: {}",
                answer.error
            );
            assert!(answer.calls().is_empty(), "{request}: {:?}", answer.calls());
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn buttons_request(extra: &str) -> String {
        format!(
            r#"{{"op": "ask_buttons", "title": "Titel", "text": "Frage\u200b?", "buttons": ["Abbrechen", "Später", "Weiter"],
                 "default": 2, "cancel": 0, "icon": "caution", "seconds": 30{extra}}}"#
        )
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_button_script_maps_the_pressed_button_to_its_index() -> TestResult {
        let shown = probe(r#"{"button": "Später"}"#, &buttons_request(""))?;
        assert!(shown.ok, "{}", shown.error);
        assert_eq!(
            shown.data,
            json::parse(r#"{"result": "button", "index": 1}"#)?
        );
        assert_eq!(shown.logged("text"), Json::from("Frage?"));
        assert_eq!(shown.option("defaultButton"), Json::from("Weiter"));
        assert_eq!(shown.option("cancelButton"), Json::from("Abbrechen"));
        assert_eq!(shown.option("withIcon"), Json::from("caution"));
        assert_eq!(shown.option("givingUpAfter"), Json::Int(30));
        let default = probe("{}", &buttons_request(""))?;
        assert_eq!(
            default.data,
            json::parse(r#"{"result": "button", "index": 2}"#)?
        );
        for (mock, expected) in [
            (r#"{"fail": "User canceled. (-128)"}"#, "cancel"),
            (r#"{"gaveUp": true}"#, "timeout"),
        ] {
            let answer = probe(mock, &buttons_request(""))?;
            assert_eq!(answer.result(), expected, "{mock}");
        }
        let single = probe(
            r#"{"button": "OK"}"#,
            r#"{"op": "ask_buttons", "title": "T", "text": "x", "buttons": ["OK"], "default": 0, "cancel": null, "seconds": 5}"#,
        )?;
        assert_eq!(
            single.data,
            json::parse(r#"{"result": "button", "index": 0}"#)?
        );
        assert_eq!(single.option("withIcon"), Json::from("note"));
        assert_eq!(single.option("cancelButton"), Json::Null);
        let unknown = probe(r#"{"button": "Ganz anders"}"#, &buttons_request(""))?;
        assert!(!unknown.ok);
        assert_eq!(unknown.error, "dialog_failed: unknown button");
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_button_script_refuses_malformed_requests() -> TestResult {
        let bad = [
            buttons_request("").replace(
                r#""buttons": ["Abbrechen", "Später", "Weiter"]"#,
                r#""buttons": []"#,
            ),
            buttons_request("").replace(r#""Weiter"]"#, r#""Weiter", "Vier"]"#),
            buttons_request("").replace(r#""Später""#, r#""Weiter""#),
            buttons_request("").replace(r#""Später""#, "7"),
            buttons_request("").replace(r#""default": 2"#, r#""default": 3"#),
            buttons_request("").replace(r#""default": 2"#, r#""default": -1"#),
            buttons_request("").replace(r#""cancel": 0"#, r#""cancel": 3"#),
            buttons_request("").replace(r#""cancel": 0, "#, ""),
            buttons_request("").replace(r#""caution""#, r#""skull""#),
            buttons_request("").replace(r#""seconds": 30"#, r#""seconds": 99999"#),
            buttons_request("").replace(r#""text": "Frage\u200b?""#, r#""text": 3"#),
        ];
        for request in bad {
            let answer = probe("{}", &request)?;
            assert!(!answer.ok, "{request}");
            assert!(
                answer.error.starts_with("bad_request"),
                "{request}: {}",
                answer.error
            );
            assert!(answer.calls().is_empty(), "{request}");
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn list_request(extra: &str) -> String {
        format!(
            r#"{{"op": "choose_list", "title": "Titel", "text": "Welche?", "items": ["1. A\u200b", "2.  B ", "3. C"],
                 "defaults": [0, 2], "multiple": true, "ok": "Weiter", "cancel": "Abbrechen", "seconds": 30{extra}}}"#
        )
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_list_script_returns_sorted_positions_and_maps_every_way_out() -> TestResult {
        let shown = probe(r#"{"chosen": ["3. C", "1. A"]}"#, &list_request(""))?;
        assert!(shown.ok, "{}", shown.error);
        assert_eq!(
            shown.data,
            json::parse(r#"{"result": "list", "selected": [0, 2]}"#)?
        );
        assert_eq!(shown.calls(), ["activate", "chooseFromList"]);
        assert_eq!(
            shown.logged("items"),
            json::parse(r#"["1. A", "2. B", "3. C"]"#)?
        );
        assert_eq!(
            shown.option("defaultItems"),
            json::parse(r#"["1. A", "3. C"]"#)?
        );
        assert_eq!(shown.option("multipleSelectionsAllowed"), Json::Bool(true));
        assert_eq!(shown.option("emptySelectionAllowed"), Json::Bool(true));
        assert_eq!(shown.option("okButtonName"), Json::from("Weiter"));
        assert_eq!(shown.option("cancelButtonName"), Json::from("Abbrechen"));
        assert_eq!(shown.option("withPrompt"), Json::from("Welche?"));
        let empty = probe(r#"{"chosen": []}"#, &list_request(""))?;
        assert_eq!(
            empty.data,
            json::parse(r#"{"result": "list", "selected": []}"#)?
        );
        let cancelled = probe(r#"{"chosen": false}"#, &list_request(""))?;
        assert_eq!(cancelled.result(), "cancel");
        let thrown = probe(r#"{"fail": "User canceled. (-128)"}"#, &list_request(""))?;
        assert_eq!(thrown.result(), "cancel");
        for (mock, message) in [
            (r#"{"chosen": ["9. X"]}"#, "dialog_failed: unknown item"),
            (
                r#"{"chosen": ["1. A", "1. A"]}"#,
                "dialog_failed: unknown item",
            ),
            (
                r#"{"chosen": "1. A"}"#,
                "dialog_failed: unexpected selection",
            ),
            (
                r#"{"chosen": ["1. A", "2. B", "3. C", "1. A"]}"#,
                "dialog_failed: unexpected selection",
            ),
            (r#"{"fail": "boom"}"#, "dialog_failed: boom"),
        ] {
            let answer = probe(mock, &list_request(""))?;
            assert!(!answer.ok, "{mock}");
            assert_eq!(answer.error, message, "{mock}");
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_list_script_refuses_malformed_requests() -> TestResult {
        let many: Vec<String> = (0..501).map(|number| format!("\"{number}\"")).collect();
        let bad = [
            list_request("").replace(
                r#""items": ["1. A\u200b", "2.  B ", "3. C"]"#,
                r#""items": []"#,
            ),
            list_request("").replace(
                r#""items": ["1. A\u200b", "2.  B ", "3. C"]"#,
                &format!("\"items\": [{}]", many.join(", ")),
            ),
            list_request("").replace(r#""2.  B ""#, r#""1. A""#),
            list_request("").replace(r#""2.  B ""#, r#""  ""#),
            list_request("").replace(r#""2.  B ""#, &quote(&"w".repeat(161))),
            list_request("").replace(r#""2.  B ""#, "7"),
            list_request("").replace(r#""defaults": [0, 2]"#, r#""defaults": "0""#),
            list_request("").replace(r#""defaults": [0, 2]"#, r#""defaults": [0, 3]"#),
            list_request("").replace(r#""defaults": [0, 2]"#, r#""defaults": [0, 0]"#),
            list_request("").replace(r#""defaults": [0, 2]"#, r#""defaults": [-1]"#),
            list_request("").replace(r#""multiple": true"#, r#""multiple": 1"#),
            list_request("").replace(r#""multiple": true"#, r#""multiple": false"#),
            list_request("").replace(r#""ok": "Weiter""#, r#""ok": """#),
            list_request("").replace(r#""seconds": 30"#, r#""seconds": 0"#),
        ];
        for request in bad {
            let answer = probe(r#"{"chosen": []}"#, &request)?;
            assert!(!answer.ok, "{request}");
            assert!(
                answer.error.starts_with("bad_request"),
                "{request}: {}",
                answer.error
            );
            assert!(answer.calls().is_empty(), "{request}");
        }
        let single = probe(
            r#"{"chosen": ["2. B"]}"#,
            &list_request("")
                .replace(r#""multiple": true"#, r#""multiple": false"#)
                .replace(r#""defaults": [0, 2]"#, r#""defaults": [1]"#),
        )?;
        assert_eq!(
            single.data,
            json::parse(r#"{"result": "list", "selected": [1]}"#)?
        );
        assert_eq!(
            single.option("multipleSelectionsAllowed"),
            Json::Bool(false)
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_notification_script_cleans_the_text_and_never_takes_the_focus() -> TestResult {
        let request = r#"{"op": "notify", "title": "Avery", "text": "Hallo\u202e Welt\u200b"}"#;
        let shown = probe("{}", request)?;
        assert!(shown.ok, "{}", shown.error);
        assert_eq!(shown.data, json::parse(r#"{"result": "ok"}"#)?);
        assert_eq!(shown.calls(), ["displayNotification"]);
        assert_eq!(shown.logged("text"), Json::from("Hallo Welt"));
        assert_eq!(shown.option("withTitle"), Json::from("Avery"));
        let broken = probe(r#"{"fail": "no center"}"#, request)?;
        assert_eq!(broken.error, "dialog_failed: no center");
        let separators = probe(
            "{}",
            r#"{"op": "notify", "title": "A\u2028B", "text": "Ha\u2028l\u2029l\u0085o"}"#,
        )?;
        assert!(separators.ok, "{}", separators.error);
        assert_eq!(separators.logged("text"), Json::from("Hallo"));
        assert_eq!(separators.option("withTitle"), Json::from("AB"));
        for bad in [
            format!(
                r#"{{"op": "notify", "title": "A", "text": {}}}"#,
                quote(&"t".repeat(301))
            ),
            format!(
                r#"{{"op": "notify", "title": {}, "text": "x"}}"#,
                quote(&"t".repeat(81))
            ),
            r#"{"op": "notify", "title": "A"}"#.to_string(),
            r#"{"op": "notify", "title": "A", "text": 5}"#.to_string(),
        ] {
            let answer = probe("{}", &bad)?;
            assert!(
                !answer.ok && answer.error.starts_with("bad_request"),
                "{bad}"
            );
            assert!(answer.calls().is_empty(), "{bad}");
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_page_script_opens_the_allowlisted_pages_and_nothing_else() -> TestResult {
        for page in Page::ALL {
            let request = format!(r#"{{"op": "open_url", "url": {}}}"#, quote(page.url()));
            let shown = probe("{}", &request)?;
            assert!(shown.ok, "{}: {}", page.url(), shown.error);
            assert_eq!(shown.calls(), ["openLocation"], "{}", page.url());
            assert_eq!(shown.logged("url"), Json::from(page.url()));
        }
        let too_long = format!("https://github.com/{}", "a".repeat(300));
        for bad in [
            "http://github.com/flavioaiello/avery",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "x-apple.systempreferences:com.apple.preference.security",
            "https://",
            "https:///path",
            "https://exa mple.com/",
            "https://github.com/a b",
            "https://github.com/\n",
            "https://github.com/\u{202e}",
            "https://github.com/\"",
            "https://github.com/<x>",
            "https://user@github.com/",
            "https://github.com:8443/",
            "HTTPS://github.com/",
            " https://github.com/",
            "ftp://github.com/",
            "",
            too_long.as_str(),
        ] {
            let request = format!(r#"{{"op": "open_url", "url": {}}}"#, quote(bad));
            let answer = probe("{}", &request)?;
            assert!(!answer.ok, "{bad:?}");
            assert!(
                answer.error.starts_with("bad_request"),
                "{bad:?}: {}",
                answer.error
            );
            assert!(answer.calls().is_empty(), "{bad:?}");
        }
        for request in [r#"{"op": "open_url"}"#, r#"{"op": "open_url", "url": 5}"#] {
            let answer = probe("{}", request)?;
            assert!(!answer.ok && answer.calls().is_empty(), "{request}");
        }
        let broken = probe(
            r#"{"fail": "no browser"}"#,
            &format!(
                r#"{{"op": "open_url", "url": {}}}"#,
                quote(Page::Readme.url())
            ),
        )?;
        assert_eq!(broken.error, "dialog_failed: no browser");
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_script_knows_its_operations_and_nothing_else() -> TestResult {
        let unknown = probe("{}", r#"{"op": "run_shell", "command": "id"}"#)?;
        assert!(!unknown.ok);
        assert_eq!(unknown.error, "unknown_op");
        assert!(unknown.calls().is_empty());
        let ping = probe("{}", r#"{"op": "ping"}"#)?;
        assert_eq!(ping.data, json::parse(r#"{"jxa": true}"#)?);
        Ok(())
    }
}
