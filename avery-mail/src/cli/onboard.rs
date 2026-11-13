use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;

use super::{
    cmd_selftest, editable, failed, finish, granted, known_calendars, link_account,
    list_account_calendars, setup_apple, setup_calendars, LinkDone,
};
use crate::config::{dirname, make_dirs, Settings};
use crate::console::{Buttons, Console, Dialogs, Icon, List, Screen, Stop, Talk, RESTART_GOOSE};
use crate::context::Context;
use crate::osascript::Host;
use crate::provider::Provider;
use crate::rights::Rights;
use crate::text::one_line_str;

const WELCOME_TITLE: &str = "Willkommen bei Avery";
const DONE_TITLE: &str = "Avery ist eingerichtet";
const WELCOME_TEXT: &str = "Avery braucht Zugriff auf dein privates Mail- und Kalenderkonto. Die Einrichtung dauert ein paar Minuten und gilt dauerhaft. Deine Anmeldung bleibt im Schlüsselbund dieses Macs. Jeden Schritt kannst du abbrechen: dann wird nichts gespeichert.";
const ACCOUNTS_TEXT: &str =
    "Welche Konten soll Avery nutzen? Apple Mail und Kalender ist optional und standardmäßig aus.";
const SELFTEST_TEXT: &str =
    "Soll Avery jetzt den Lesezugriff prüfen? Dabei wird nichts gesendet oder geändert.";
const SELFTEST_NOTICE: &str = "Der Lesetest läuft ...";
const FINISHED_NOTICE: &str =
    "Einrichtung abgeschlossen. Goose danach neu starten. Weitere Einstellungen: avery mail setup";
const CANCELLED_NOTICE: &str = "Abgebrochen. Es wurde nichts gespeichert.";
const FAILED_TITLE: &str = "Avery: Einrichtung fehlgeschlagen";
const FAILED_HINT: &str = "Erneut im Terminal: avery mail onboard";
const MAX_FAILED: usize = 190;
const UNREADABLE_NOTICE: &str = "Die Einrichtung startet nicht: die Konfiguration ist nicht lesbar. Den Grund zeigt im Terminal: avery mail status";
const NOTHING_NOTICE: &str =
    "Nichts ausgewählt. Die Einrichtung startet wieder mit: avery mail onboard";
const DISMISSED_NOTICE: &str =
    "Avery fragt nicht mehr. Die Einrichtung startet jederzeit mit: avery mail onboard";
const LATER: &str = "Später";
const NEVER: &str = "Nicht mehr fragen";
const NEXT: &str = "Weiter";
const OK: &str = "OK";
const CANCEL: &str = "Abbrechen";
const USAGE: &str = "Benutzung: avery mail onboard [--if-needed]";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Always,
    IfNeeded,
}

fn mode_of(args: &[String]) -> Option<Mode> {
    match args {
        [] => Some(Mode::Always),
        [flag] if flag == "--if-needed" => Some(Mode::IfNeeded),
        _ => None,
    }
}

fn dismissed(settings: &Settings) -> bool {
    fs::symlink_metadata(settings.onboard_marker_path()).is_ok_and(|meta| meta.is_file())
}

fn dismiss(settings: &Settings) -> io::Result<()> {
    let path = settings.onboard_marker_path();
    make_dirs(&dirname(&path), 0o700)?;
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && dismissed(settings) => Ok(()),
        Err(error) => Err(error),
    }
}

enum Need {
    No,
    Yes,
    Unreadable(crate::error::ToolError),
}

fn needed(ctx: &Context, screen: &Screen) -> Need {
    if ctx.settings.fake || ctx.settings.no_onboard || dismissed(&ctx.settings) {
        return Need::No;
    }
    match ctx.settings.stored_rights() {
        Err(error) => return Need::Unreadable(error),
        Ok(rights) if !rights.accounts.is_empty() || rights.apple.enabled() => return Need::No,
        Ok(_) => {}
    }
    if (screen.gui)() {
        Need::Yes
    } else {
        Need::No
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Ending {
    Finished,
    Later,
    Dismissed,
    NothingChosen,
    Empty,
    Cancelled,
    TimedOut,
    Away,
    Failed(String),
}

fn ending_of(stop: Stop) -> Ending {
    match stop {
        Stop::Cancelled => Ending::Cancelled,
        Stop::TimedOut => Ending::TimedOut,
        Stop::Away => Ending::Away,
        Stop::Failed(message) => Ending::Failed(message),
    }
}

enum Step {
    Welcome,
    Accounts,
    Link(Provider),
    Calendars(Provider, String),
    Apple,
    Summary,
}

enum Next {
    Go(Step),
    End(Ending),
}

#[derive(Default)]
struct Plan {
    remote: VecDeque<Provider>,
    apple: bool,
}

struct Flow<'a> {
    ctx: &'a Context,
    host: &'a dyn Host,
    talk: Talk,
    plan: Plan,
}

fn overview(rights: &Rights) -> String {
    let mut lines: Vec<String> = rights
        .accounts
        .iter()
        .map(|account| {
            let calendars: Vec<(String, crate::rights::Level)> = account
                .calendars
                .iter()
                .map(|grant| (grant.name.clone(), grant.level))
                .collect();
            format!(
                "{}: {}\n    Mail: {}\n    Kalender: {}",
                account.provider().display(),
                account.address(),
                super::level_word(account.mail),
                granted(&calendars)
            )
        })
        .collect();
    if rights.apple.enabled() {
        lines.push(format!(
            "{}: {} Konten, {} Kalender",
            Provider::Apple.display(),
            rights.apple.mail.len(),
            rights.apple.calendars.len()
        ));
    }
    lines.join("\n")
}

fn holds_something(rights: &Rights) -> bool {
    !rights.accounts.is_empty() || rights.apple.enabled()
}

impl<'a> Flow<'a> {
    fn new(ctx: &'a Context, host: &'a dyn Host) -> Flow<'a> {
        Flow {
            ctx,
            host,
            talk: Talk::new(),
            plan: Plan::default(),
        }
    }

    fn dialogs(&mut self) -> Dialogs<'_> {
        Dialogs::new(self.host, &mut self.talk)
    }

    fn note(&mut self, text: &str) {
        self.dialogs().say(text);
    }

    fn run(&mut self) -> Ending {
        let mut step = Step::Welcome;
        loop {
            let next = match step {
                Step::Welcome => self.welcome(),
                Step::Accounts => self.accounts(),
                Step::Link(provider) => self.link(provider),
                Step::Calendars(provider, address) => self.calendars(provider, &address),
                Step::Apple => self.apple(),
                Step::Summary => self.summary(),
            };
            match next {
                Next::Go(following) => step = following,
                Next::End(ending) => return ending,
            }
        }
    }

    fn after_providers(&mut self) -> Step {
        if let Some(provider) = self.plan.remote.pop_front() {
            Step::Link(provider)
        } else if std::mem::take(&mut self.plan.apple) {
            Step::Apple
        } else {
            Step::Summary
        }
    }

    fn welcome(&mut self) -> Next {
        self.talk.set_title(WELCOME_TITLE);
        let answer = self.dialogs().ask_buttons(&Buttons {
            text: WELCOME_TEXT,
            buttons: &[LATER, NEVER, NEXT],
            default: 2,
            cancel: Some(0),
            icon: Icon::Note,
        });
        match answer {
            Ok(2) => Next::Go(Step::Accounts),
            Ok(1) => match dismiss(&self.ctx.settings) {
                Ok(()) => Next::End(Ending::Dismissed),
                Err(error) => Next::End(Ending::Failed(format!(
                    "Die Einstellung konnte nicht gespeichert werden ({:?}).",
                    error.kind()
                ))),
            },
            Ok(_) | Err(Stop::Cancelled | Stop::TimedOut) => Next::End(Ending::Later),
            Err(stop) => Next::End(ending_of(stop)),
        }
    }

    fn accounts(&mut self) -> Next {
        let remote: Vec<Provider> = Provider::remote().collect();
        let mut labels: Vec<String> = remote
            .iter()
            .map(|provider| provider.display().to_string())
            .collect();
        labels.push(format!("{} (optional)", Provider::Apple.display()));
        let defaults: Vec<usize> = (0..remote.len()).collect();
        let chosen = self.dialogs().choose_list(&List {
            text: ACCOUNTS_TEXT,
            items: &labels,
            defaults: &defaults,
            multiple: true,
            ok: NEXT,
        });
        let selected = match chosen {
            Ok(selected) => selected,
            Err(stop) => return Next::End(ending_of(stop)),
        };
        self.plan.remote = remote
            .iter()
            .enumerate()
            .filter(|(index, _)| selected.contains(index))
            .map(|(_, provider)| *provider)
            .collect();
        self.plan.apple = selected.contains(&remote.len());
        if self.plan.remote.is_empty() && !self.plan.apple {
            return Next::End(Ending::NothingChosen);
        }
        Next::Go(self.after_providers())
    }

    fn failure(&mut self, title: &str) -> Next {
        let stop = self.talk.take_stop();
        let notes = self.talk.take_notes();
        let message = match stop {
            Some(Stop::Cancelled) => return Next::End(Ending::Cancelled),
            Some(Stop::TimedOut) => return Next::End(Ending::TimedOut),
            Some(Stop::Away) => return Next::End(Ending::Away),
            Some(Stop::Failed(message)) if notes.is_empty() => message,
            Some(Stop::Failed(message)) => format!("{notes}\n{message}"),
            None if notes.is_empty() => "Unbekannter Fehler.".to_string(),
            None => notes,
        };
        self.talk.set_title(title);
        let answer = self.dialogs().ask_buttons(&Buttons {
            text: &message,
            buttons: &[CANCEL, NEXT],
            default: 1,
            cancel: Some(0),
            icon: Icon::Caution,
        });
        match answer {
            Ok(1) => Next::Go(self.after_providers()),
            Ok(_) | Err(Stop::Cancelled) => Next::End(Ending::Cancelled),
            Err(Stop::TimedOut) => Next::End(Ending::TimedOut),
            Err(_) => Next::End(Ending::Failed(message)),
        }
    }

    fn link(&mut self, provider: Provider) -> Next {
        let title = format!("{} verbinden", provider.display());
        self.talk.set_title(&title);
        let outcome = {
            let mut console = Console::Dialogs(Dialogs::new(self.host, &mut self.talk));
            link_account(self.ctx, &mut console, provider)
        };
        match outcome {
            Ok(done) => self.linked(provider, done),
            Err(_) => self.failure(&format!("{title}: fehlgeschlagen")),
        }
    }

    fn linked(&mut self, provider: Provider, done: LinkDone) -> Next {
        self.talk.take_notes();
        let relinked = if done.relinked {
            "\nDas Konto war schon verbunden: die Freigaben bleiben, höchstens abgesenkt auf das, was die neue Anmeldung erlaubt."
        } else {
            ""
        };
        let text = format!(
            "{} ist verbunden: {}\nMail: {}.{relinked}",
            provider.display(),
            done.address,
            super::level_word(done.level)
        );
        let answer = self.dialogs().ask_buttons(&Buttons {
            text: &text,
            buttons: &[NEXT],
            default: 0,
            cancel: None,
            icon: Icon::Note,
        });
        match answer {
            Ok(_) => Next::Go(Step::Calendars(provider, done.address)),
            Err(stop) => Next::End(ending_of(stop)),
        }
    }

    fn calendars(&mut self, provider: Provider, address: &str) -> Next {
        let title = format!("{}: Kalender", provider.display());
        self.talk.set_title(&title);
        let mut rights = match self.ctx.settings.stored_rights() {
            Ok(rights) => rights,
            Err(error) => {
                self.note(&format!("Fehler: {error}"));
                return self.failure(&title);
            }
        };
        let before = rights.clone();
        let Some(account) = rights.account_mut(provider, address) else {
            self.note("Das Konto steht nicht mehr in der Konfiguration.");
            return self.failure(&title);
        };
        let listing = list_account_calendars(self.ctx, account);
        let code = {
            let mut console = Console::Dialogs(Dialogs::new(self.host, &mut self.talk));
            match known_calendars(&mut console, account, listing) {
                None => None,
                Some(listed) if listed.is_empty() => {
                    console.say("Dieses Konto hat keine Kalender.");
                    None
                }
                Some(listed) => match setup_calendars(&mut console, account, listed) {
                    Ok(()) => Some(finish(
                        self.ctx,
                        &mut console,
                        &before,
                        &rights,
                        "avery mail setup",
                    )),
                    Err(code) => Some(code),
                },
            }
        };
        match code {
            Some(0) => {
                self.talk.take_notes();
                Next::Go(self.after_providers())
            }
            Some(_) => self.failure(&title),
            None => self.notice(&title),
        }
    }

    fn notice(&mut self, title: &str) -> Next {
        let notes = self.talk.take_notes();
        self.talk.set_title(title);
        let answer = self.dialogs().ask_buttons(&Buttons {
            text: &notes,
            buttons: &[NEXT],
            default: 0,
            cancel: None,
            icon: Icon::Note,
        });
        match answer {
            Ok(_) => Next::Go(self.after_providers()),
            Err(stop) => Next::End(ending_of(stop)),
        }
    }

    fn apple(&mut self) -> Next {
        let title = Provider::Apple.display();
        self.talk.set_title(title);
        let code = {
            let mut console = Console::Dialogs(Dialogs::new(self.host, &mut self.talk));
            setup_apple(self.ctx, &mut console)
        };
        if code != 0 {
            return self.failure(title);
        }
        self.talk.take_notes();
        Next::Go(Step::Summary)
    }

    fn summary(&mut self) -> Next {
        self.talk.set_title(DONE_TITLE);
        let rights = match self.ctx.settings.stored_rights() {
            Ok(rights) => rights,
            Err(error) => return Next::End(Ending::Failed(error.message())),
        };
        if !holds_something(&rights) {
            return Next::End(Ending::Empty);
        }
        let text = format!(
            "Das ist eingerichtet:\n{}\n\n{SELFTEST_TEXT}",
            overview(&rights)
        );
        let answer = self.dialogs().ask_buttons(&Buttons {
            text: &text,
            buttons: &[LATER, "Lesetest jetzt"],
            default: 1,
            cancel: Some(0),
            icon: Icon::Note,
        });
        match answer {
            Ok(1) => self.selftest(),
            Ok(_) | Err(Stop::Cancelled | Stop::TimedOut) => Next::End(Ending::Finished),
            Err(stop) => Next::End(ending_of(stop)),
        }
    }

    fn selftest(&mut self) -> Next {
        self.dialogs().notify(SELFTEST_NOTICE);
        let code = {
            let mut console = Console::Dialogs(Dialogs::new(self.host, &mut self.talk));
            cmd_selftest(self.ctx, &mut console)
        };
        let notes = self.talk.take_notes();
        let head = if code == 0 {
            "Lesetest bestanden."
        } else {
            "Lesetest fehlgeschlagen."
        };
        let text = format!("{head}\n\n{notes}");
        let answer = self.dialogs().ask_buttons(&Buttons {
            text: &text,
            buttons: &[OK],
            default: 0,
            cancel: None,
            icon: if code == 0 { Icon::Note } else { Icon::Caution },
        });
        match answer {
            Ok(_) | Err(Stop::Cancelled | Stop::TimedOut) => Next::End(Ending::Finished),
            Err(stop) => Next::End(ending_of(stop)),
        }
    }

    fn kept(&mut self, lead: &str) {
        let kept = self
            .ctx
            .settings
            .stored_rights()
            .ok()
            .filter(holds_something);
        match kept {
            None => self.dialogs().notify(CANCELLED_NOTICE),
            Some(rights) => {
                self.talk.set_title(DONE_TITLE);
                let text = format!(
                    "{lead} Das ist schon gespeichert und bleibt:\n{}\n\n{RESTART_GOOSE} Weiter einrichten: Avery erneut starten oder im Terminal: avery mail setup",
                    overview(&rights)
                );
                let _ = self.dialogs().ask_buttons(&Buttons {
                    text: &text,
                    buttons: &[OK],
                    default: 0,
                    cancel: None,
                    icon: Icon::Note,
                });
            }
        }
    }

    fn alert(&mut self, message: &str) {
        self.talk.set_title(FAILED_TITLE);
        let text = format!("{} {FAILED_HINT}", one_line_str(message, MAX_FAILED));
        self.dialogs().notify(&text);
    }

    fn conclude(&mut self, ending: Ending, console: &mut Console) -> u8 {
        match ending {
            Ending::Finished => {
                self.dialogs().notify(FINISHED_NOTICE);
                console.say("Einrichtung abgeschlossen.");
                0
            }
            Ending::Later => {
                console.say("Die Einrichtung wurde auf spaeter verschoben.");
                0
            }
            Ending::Dismissed => {
                self.dialogs().notify(DISMISSED_NOTICE);
                console.say(&format!(
                    "Avery fragt nicht mehr nach der Einrichtung. Wieder einschalten: {} loeschen.",
                    one_line_str(&self.ctx.settings.onboard_marker_path(), 200)
                ));
                0
            }
            Ending::NothingChosen => {
                self.dialogs().notify(NOTHING_NOTICE);
                console.say("Nichts ausgewaehlt, nichts geaendert.");
                0
            }
            Ending::Empty => {
                self.dialogs().notify(NOTHING_NOTICE);
                console.say("Es ist noch kein Konto verbunden.");
                0
            }
            Ending::Cancelled => {
                self.kept("Abgebrochen.");
                console.say("Abgebrochen.");
                0
            }
            Ending::TimedOut => {
                self.kept("Keine Antwort im Dialog, die Einrichtung wurde beendet.");
                console.say("Keine Antwort im Dialog, abgebrochen.");
                0
            }
            Ending::Away => {
                console.say("Die Anleitung wurde geoeffnet. Danach Avery erneut starten.");
                0
            }
            Ending::Failed(message) => {
                console.say(&format!("Fehler: {message}"));
                self.alert(&message);
                1
            }
        }
    }
}

fn announce(screen: &Screen, text: &str) {
    if (screen.gui)() {
        let mut talk = Talk::new();
        talk.set_title(FAILED_TITLE);
        Dialogs::new(screen.host, &mut talk).notify(text);
    }
}

pub(super) fn run(ctx: &Context, console: &mut Console, screen: &Screen, args: &[String]) -> u8 {
    let Some(mode) = mode_of(args) else {
        console.say(USAGE);
        return 2;
    };
    match mode {
        Mode::Always => {
            if !editable(ctx, console) {
                return 1;
            }
        }
        Mode::IfNeeded => match needed(ctx, screen) {
            Need::No => return 0,
            Need::Unreadable(error) => {
                let code = failed(console, &error);
                announce(screen, UNREADABLE_NOTICE);
                return code;
            }
            Need::Yes => {}
        },
    }
    let mut flow = Flow::new(ctx, screen.host);
    let ending = flow.run();
    flow.conclude(ending, console)
}

#[cfg(test)]
mod tests;
