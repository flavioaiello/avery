use std::io::{BufRead, Write};
use std::thread;
use std::time::{Duration, Instant};

use crate::account::MailAccount;
use crate::backend::{ComposeRequest, MessageId, MessageQuery, Mode, ReplySpec};
use crate::calendar::{
    local_today, perform, Action, Calendar, CalendarEntry, Change, ChangeResult, Decision,
    EventFields, Origin, Window,
};
use crate::changelog::{self, ChangeEntry};
use crate::civil::DateTime;
use crate::config::WriteSettings;
use crate::console::{ask_levels, failed, level_word, Choice, Console};
use crate::context::Context;
use crate::error::{ToolError, ToolResult};
use crate::localtime;
use crate::recurrence;
use crate::rights::{Level, Rights};
use crate::sendlog::{self, Outcome};
use crate::server;
use crate::source::Source;
use crate::text::{clean_str, one_line_str};
use crate::texts::{
    CALENDAR_WRITE_OFF, COMMANDS, HELP, MAX_DRAFTS_PER_HOUR, MAX_RECIPIENTS, MAX_SEND_CHARS,
    MAX_SEND_LINES, NO_ACCOUNTS, NO_CALENDARS, VERSION, WRITE_OFF,
};
use crate::write_tools;

const SENDLOG_LINES: usize = 200;
const SENDLOG_SHOWN: usize = 20;
const CHANGES_LINES: usize = 300;
const CHANGES_SHOWN: usize = 20;
const SUGGESTION_CUTOFF: f64 = 0.6;
const MAX_COMMAND_CHARS: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Area {
    All,
    Mail,
    Calendar,
}

fn describe(account: &MailAccount) -> String {
    let kind = if account.kind.is_empty() {
        "?"
    } else {
        account.kind.as_str()
    };
    let addresses = if account.addresses.is_empty() {
        "-".to_string()
    } else {
        account.addresses.join(", ")
    };
    format!("{}  [{kind}]  {addresses}", account.name)
}

fn save_rights(ctx: &Context, console: &mut Console, rights: &Rights) -> u8 {
    match ctx.settings.save_rights(rights) {
        Ok(()) => 0,
        Err(error) => {
            console.say(&format!(
                "Fehler: config.json nicht schreibbar ({:?})",
                error.kind()
            ));
            1
        }
    }
}

fn granted(entries: &[(String, Level)]) -> String {
    if entries.is_empty() {
        return "keine".to_string();
    }
    entries
        .iter()
        .map(|(name, level)| format!("{} ({})", one_line_str(name, 60), level_word(Some(*level))))
        .collect::<Vec<_>>()
        .join(", ")
}

fn mail_grants(rights: &Rights) -> Vec<(String, Level)> {
    rights
        .mail
        .iter()
        .map(|(account, level)| (account.name.clone(), *level))
        .collect()
}

fn calendar_grants(rights: &Rights) -> Vec<(String, Level)> {
    rights
        .calendars
        .iter()
        .map(|(calendar, level)| (calendar.name.clone(), *level))
        .collect()
}

fn cmd_accounts(ctx: &Context, console: &mut Console) -> u8 {
    let listing = (|| {
        let rights = ctx.settings.rights()?;
        let accounts = ctx.backend.all_accounts()?;
        Ok::<_, ToolError>((rights, accounts))
    })();
    let (rights, accounts) = match listing {
        Ok(listing) => listing,
        Err(error) => return failed(console, &error),
    };
    if accounts.is_empty() {
        console.say("Mail kennt noch keine Konten (Systemeinstellungen > Internetaccounts).");
        return 0;
    }
    for (index, account) in accounts.iter().enumerate() {
        let level = rights.mail_level(&account.id);
        let mark = if level.is_some() { '*' } else { ' ' };
        console.say(&format!(
            "{mark} {}) {}  [{}]",
            index + 1,
            describe(account),
            level_word(level)
        ));
    }
    console.say("\n* = fuer den Agenten freigegeben, Stufe in Klammern: lesen oder schreiben");
    console.say("(schreiben = Entwuerfe und Senden, jede Mail nur nach deinem Klick)");
    0
}

fn cmd_calendars(ctx: &Context, console: &mut Console) -> u8 {
    let listing = (|| {
        let rights = ctx.settings.rights()?;
        let calendars = ctx.calendar.calendars(Source::Apple, "")?;
        Ok::<_, ToolError>((rights, calendars))
    })();
    let (rights, calendars) = match listing {
        Ok(listing) => listing,
        Err(error) => return failed(console, &error),
    };
    if calendars.is_empty() {
        console.say("Kalender kennt noch keine Kalender.");
        return 0;
    }
    for (index, calendar) in calendars.iter().enumerate() {
        let level = rights.calendar_level(&calendar.id);
        let mark = if level.is_some() { '*' } else { ' ' };
        console.say(&format!(
            "{mark} {}) {}{}  [{}]",
            index + 1,
            one_line_str(&calendar.name, 80),
            if calendar.writable {
                ""
            } else {
                " [schreibgeschuetzt]"
            },
            level_word(level)
        ));
    }
    console.say("\n* = fuer den Agenten freigegeben, Stufe in Klammern: lesen oder schreiben");
    console.say(
        "(schreiben = Termine anlegen, aendern, loeschen, jede Aenderung nur nach deinem Klick)",
    );
    0
}

fn setup_mail(
    ctx: &Context,
    console: &mut Console,
    rights: &mut Rights,
    required: bool,
) -> Result<(), u8> {
    let accounts = ctx
        .backend
        .all_accounts()
        .map_err(|error| failed(console, &error))?;
    if accounts.is_empty() {
        console.say("Mail kennt noch keine Konten. Zuerst in den Systemeinstellungen unter Internetaccounts anmelden.");
        return if required { Err(1) } else { Ok(()) };
    }
    console.say("Mail-Konten. Nur PRIVATE Konten freigeben, niemals geschaeftliche.");
    console.say("Stufen: 0 = aus, 1 = lesen, 2 = schreiben (Entwuerfe anlegen und Mails senden,");
    console.say("jede Mail erst nach deinem Klick in einem macOS-Dialog; der Agent kann ihn nicht bedienen).");
    console.say(&format!(
        "Grenzen: Klartext, kein Bcc, keine Anhaenge, max. {MAX_RECIPIENTS} Empfaenger, max. {MAX_SEND_CHARS} Zeichen pro gesendeter Mail,"
    ));
    console.say(&format!(
        "         max. {MAX_SEND_LINES} Zeilen, max. {} gesendete Mails und max. {MAX_DRAFTS_PER_HOUR} Entwuerfe pro Stunde. Jeder Versuch steht in {}",
        rights.sends_per_hour,
        ctx.settings.sendlog_path()
    ));
    let choices: Vec<Choice> = accounts
        .iter()
        .map(|account| Choice {
            title: describe(account),
            current: rights.mail_level(&account.id),
            highest: Level::Write,
        })
        .collect();
    let levels = ask_levels(console, &choices)?;
    let gone: Vec<String> = rights
        .mail
        .iter()
        .filter(|(known, _)| !accounts.iter().any(|account| account.id == known.id))
        .map(|(known, _)| known.name.clone())
        .collect();
    if !gone.is_empty() {
        console.say(&format!(
            "Nicht mehr in Mail vorhanden, wird entfernt: {}",
            gone.join(", ")
        ));
    }
    rights.mail = accounts
        .into_iter()
        .zip(levels)
        .filter_map(|(account, level)| level.map(|level| (account, level)))
        .collect();
    Ok(())
}

fn setup_calendars(
    ctx: &Context,
    console: &mut Console,
    rights: &mut Rights,
    required: bool,
) -> Result<(), u8> {
    let calendars = ctx
        .calendar
        .calendars(Source::Apple, "")
        .map_err(|error| failed(console, &error))?;
    if calendars.is_empty() {
        console.say("Kalender kennt noch keine Kalender.");
        return if required { Err(1) } else { Ok(()) };
    }
    console.say("\nKalender. Nur PRIVATE Kalender freigeben, niemals geschaeftliche.");
    console
        .say("Stufen: 0 = aus, 1 = lesen, 2 = schreiben (Termine anlegen, aendern und loeschen,");
    console.say("jede Aenderung erst nach deinem Klick in einem macOS-Dialog; der Agent kann ihn nicht bedienen).");
    console.say(&format!(
        "Grenzen: nur einzelne Termine ohne Teilnehmer und ohne Wiederholung, max. {} Aenderungen pro Stunde. Jeder Versuch steht in {}",
        rights.changes_per_hour,
        ctx.settings.changes_path()
    ));
    let choices: Vec<Choice> = calendars
        .iter()
        .map(|calendar| Choice {
            title: if calendar.writable {
                one_line_str(&calendar.name, 80)
            } else {
                format!("{} [schreibgeschuetzt]", one_line_str(&calendar.name, 80))
            },
            current: rights.calendar_level(&calendar.id),
            highest: if calendar.writable {
                Level::Write
            } else {
                Level::Read
            },
        })
        .collect();
    let levels = ask_levels(console, &choices)?;
    let gone: Vec<String> = rights
        .calendars
        .iter()
        .filter(|(known, _)| !calendars.iter().any(|calendar| calendar.id == known.id))
        .map(|(known, _)| known.name.clone())
        .collect();
    if !gone.is_empty() {
        console.say(&format!(
            "Nicht mehr in Kalender vorhanden, wird entfernt: {}",
            gone.join(", ")
        ));
    }
    rights.calendars = calendars
        .into_iter()
        .zip(levels)
        .filter_map(|(calendar, level)| {
            level.map(|level| {
                (
                    CalendarEntry {
                        id: calendar.id,
                        name: calendar.name,
                    },
                    level,
                )
            })
        })
        .collect();
    Ok(())
}

fn cmd_setup(ctx: &Context, console: &mut Console, word: Option<&str>) -> u8 {
    let area = match word {
        None => Area::All,
        Some("mail") => Area::Mail,
        Some("calendar") => Area::Calendar,
        Some(other) => {
            console.say(&format!(
                "Unbekannter Bereich: {} (mail oder calendar)",
                clean_str(other, Some(30))
            ));
            return 2;
        }
    };
    let mut rights = match ctx.settings.stored_rights() {
        Ok(rights) => rights,
        Err(error) => return failed(console, &error),
    };
    let before = rights.clone();
    if matches!(area, Area::All | Area::Mail) {
        if let Err(code) = setup_mail(ctx, console, &mut rights, area == Area::Mail) {
            return code;
        }
    }
    if matches!(area, Area::All | Area::Calendar) {
        if let Err(code) = setup_calendars(ctx, console, &mut rights, area == Area::Calendar) {
            return code;
        }
    }
    if rights == before {
        console.say("Unveraendert.");
        return 0;
    }
    console.say("\nNeue Freigaben:");
    console.say(&format!("  Mail:     {}", granted(&mail_grants(&rights))));
    console.say(&format!(
        "  Kalender: {}",
        granted(&calendar_grants(&rights))
    ));
    if !console.confirm("Korrekt? [j/N] ") {
        console.say("Abgebrochen.");
        return 1;
    }
    let code = save_rights(ctx, console, &rights);
    if code == 0 {
        console.say(&format!("Gespeichert in {}", ctx.settings.config_path));
        console.say("Goose danach neu starten. Pruefen mit: avery mail selftest (lesen) und avery mail selftest-write (schreiben)");
    }
    code
}

fn cmd_revoke(ctx: &Context, console: &mut Console) -> u8 {
    let mut rights = match ctx.settings.stored_rights() {
        Ok(rights) => rights,
        Err(error) => return failed(console, &error),
    };
    rights.mail.clear();
    rights.calendars.clear();
    let code = save_rights(ctx, console, &rights);
    if code == 0 {
        console.say("Alle Freigaben entfernt (Mail und Kalender, auch Schreiben). Der Agent sieht keine Konten und Kalender mehr.");
    }
    code
}

fn cmd_sendlog(ctx: &Context, console: &mut Console) -> u8 {
    let view = match sendlog::read_entries(&ctx.settings, SENDLOG_LINES) {
        Ok(view) => view,
        Err(error) => return failed(console, &error),
    };
    let attempts = sendlog::attempts(&view.entries);
    if attempts.is_empty() && view.unreadable == 0 {
        console.say("Noch keine Eintraege.");
        return 0;
    }
    let first = attempts.len().saturating_sub(SENDLOG_SHOWN);
    for entry in &attempts[first..] {
        let outcome = match entry.outcome {
            Outcome::Pending => "unklar",
            other => other.as_str(),
        };
        let to = if entry.to.is_empty() {
            "-".to_string()
        } else {
            one_line_str(&entry.to.join(", "), 200)
        };
        console.say(&format!(
            "{}  {:<5} {:<9} {} -> {} | {}",
            localtime::minute(entry.ts),
            entry.mode.as_str(),
            outcome,
            one_line_str(&entry.account, 80),
            to,
            one_line_str(&entry.subject, 80)
        ));
    }
    if view.unreadable > 0 {
        console.say(&format!(
            "\n{} Zeile(n) in einem unbekannten Format uebersprungen.",
            view.unreadable
        ));
    }
    console.say(&format!("\nDatei: {}", ctx.settings.sendlog_path()));
    0
}

fn title_of(entry: &ChangeEntry) -> String {
    let fields = entry.after.as_ref().or(entry.before.as_ref());
    one_line_str(fields.map_or("-", |fields| fields.summary.as_str()), 80)
}

fn cmd_changes(ctx: &Context, console: &mut Console) -> u8 {
    let view = match changelog::read_entries(&ctx.settings, CHANGES_LINES) {
        Ok(view) => view,
        Err(error) => return failed(console, &error),
    };
    let attempts = changelog::attempts(&view.entries);
    if attempts.is_empty() && view.unreadable == 0 {
        console.say("Noch keine Eintraege.");
        return 0;
    }
    let open = changelog::undoable(&view.entries);
    let first = attempts.len().saturating_sub(CHANGES_SHOWN);
    for entry in &attempts[first..] {
        let outcome = match entry.outcome {
            changelog::Outcome::Pending => "unklar",
            other => other.as_str(),
        };
        let action = if entry.undoes.is_some() {
            format!("undo-{}", entry.action.as_str())
        } else {
            entry.action.as_str().to_string()
        };
        let note = if open.iter().any(|candidate| candidate.id == entry.id) {
            "  [rueckgaengig moeglich]"
        } else {
            ""
        };
        console.say(&format!(
            "{}  {:<12} {:<9} {} | {} | {}{note}",
            localtime::minute(entry.ts),
            action,
            outcome,
            one_line_str(&entry.calendar, 60),
            title_of(entry),
            one_line_str(&entry.id, 12)
        ));
    }
    if view.unreadable > 0 {
        console.say(&format!(
            "\n{} Zeile(n) in einem unbekannten Format uebersprungen.",
            view.unreadable
        ));
    }
    console.say(&format!("\nDatei: {}", ctx.settings.changes_path()));
    0
}

fn inverse(entry: &ChangeEntry) -> Option<Change> {
    let uid = entry.uid.clone();
    match entry.action {
        Action::Create => Some(Change::Delete {
            uid: uid?,
            before: entry.after.clone()?,
        }),
        Action::Update => Some(Change::Update {
            uid: uid?,
            before: entry.after.clone()?,
            after: entry.before.clone()?,
        }),
        Action::Delete => Some(Change::Create {
            after: entry.before.clone()?,
        }),
    }
}

fn undo_text(entry: &ChangeEntry) -> &'static str {
    match entry.action {
        Action::Create => "der angelegte Termin wird geloescht",
        Action::Update => "der Termin bekommt seine alten Werte zurueck",
        Action::Delete => {
            "der Termin wird neu angelegt (Titel, Zeit, Ort, Notizen; Teilnehmer und Erinnerungen gehen nicht zurueck)"
        }
    }
}

fn cmd_undo(ctx: &Context, console: &mut Console, wanted: Option<&str>) -> u8 {
    let view = match changelog::read_entries(&ctx.settings, CHANGES_LINES) {
        Ok(view) => view,
        Err(error) => return failed(console, &error),
    };
    let open = changelog::undoable(&view.entries);
    let first = open.len().saturating_sub(CHANGES_SHOWN);
    let candidates: Vec<&ChangeEntry> = open[first..].iter().rev().copied().collect();
    if candidates.is_empty() {
        console.say("Nichts zum Rueckgaengigmachen.");
        return 0;
    }
    let entry = match wanted {
        Some(prefix) => {
            let hits: Vec<&&ChangeEntry> = candidates
                .iter()
                .filter(|candidate| !prefix.is_empty() && candidate.id.starts_with(prefix))
                .collect();
            match hits.as_slice() {
                [only] => **only,
                _ => {
                    console.say(
                        "Keine eindeutige Aenderung mit dieser Kennung. Siehe: avery mail changes",
                    );
                    return 1;
                }
            }
        }
        None => {
            for (index, candidate) in candidates.iter().enumerate() {
                console.say(&format!(
                    "{}) {}  {:<7} {} | {}",
                    index + 1,
                    localtime::minute(candidate.ts),
                    candidate.action.as_str(),
                    one_line_str(&candidate.calendar, 60),
                    title_of(candidate)
                ));
            }
            let Some(answer) = console.ask(
                "Nummer der Aenderung, die rueckgaengig gemacht werden soll (leer = Abbruch): ",
            ) else {
                console.say("Ungueltige Eingabe.");
                return 1;
            };
            let answer = answer.trim();
            if answer.is_empty() {
                console.say("Unveraendert.");
                return 0;
            }
            match answer
                .parse::<usize>()
                .ok()
                .and_then(|number| number.checked_sub(1))
                .and_then(|index| candidates.get(index))
            {
                Some(found) => *found,
                None => {
                    console.say("Ungueltige Eingabe.");
                    return 1;
                }
            }
        }
    };
    let Some(change) = inverse(entry) else {
        console.say("Der Eintrag enthaelt nicht genug Angaben fuer das Rueckgaengigmachen.");
        return 1;
    };
    console.say(&format!(
        "\n{} | {} | {}: {}",
        one_line_str(&entry.calendar, 60),
        entry.action.as_str(),
        title_of(entry),
        undo_text(entry)
    ));
    if !console.confirm("Rueckgaengig machen? Ein Freigabedialog erscheint. [j/N] ") {
        console.say("Abgebrochen.");
        return 1;
    }
    let calendar = Calendar {
        id: entry.calendar_id.clone(),
        name: entry.calendar.clone(),
        label: entry.calendar.clone(),
    };
    match perform(ctx, &calendar, &change, Origin::Human, Some(&entry.id)) {
        Err(error) => failed(console, &error),
        Ok(performed) => match performed.decision {
            Decision::Applied => {
                console.say("Rueckgaengig gemacht.");
                0
            }
            Decision::Declined => {
                console.say("Im Dialog abgelehnt, nichts geaendert.");
                1
            }
            Decision::TimedOut => {
                console.say("Keine Antwort im Dialog, nichts geaendert.");
                1
            }
        },
    }
}

fn cmd_status(ctx: &Context, console: &mut Console) -> u8 {
    let rights = match ctx.settings.rights() {
        Ok(rights) => rights,
        Err(error) => return failed(console, &error),
    };
    console.say(&format!("Version: {VERSION}"));
    console.say(&format!(
        "Konfiguration: {} ({})",
        ctx.settings.config_path,
        if ctx.settings.config_exists() {
            "vorhanden"
        } else {
            "fehlt"
        }
    ));
    console.say(&format!("Mail: {}", granted(&mail_grants(&rights))));
    console.say(&format!("Kalender: {}", granted(&calendar_grants(&rights))));
    console.say(&format!(
        "Grenzen pro Stunde: {} gesendete Mails, {MAX_DRAFTS_PER_HOUR} Entwuerfe, {} Kalenderaenderungen",
        rights.sends_per_hour, rights.changes_per_hour
    ));
    0
}

fn timed<T>(action: impl FnOnce() -> Result<T, ToolError>) -> (Duration, Result<T, ToolError>) {
    let started = Instant::now();
    let result = action();
    (started.elapsed(), result)
}

fn step(
    console: &mut Console,
    failures: &mut usize,
    tag: &str,
    title: &str,
    result: (Duration, Result<String, ToolError>),
) -> bool {
    let (elapsed, result) = result;
    match result {
        Ok(detail) => {
            let detail = if detail.is_empty() {
                String::new()
            } else {
                format!("  {detail}")
            };
            console.say(&format!(
                "PASS  {tag}  {title} ({:.1} s){detail}",
                elapsed.as_secs_f64()
            ));
            true
        }
        Err(error) => {
            *failures += 1;
            console.say(&format!("FAIL  {tag}  {title}: {error}"));
            false
        }
    }
}

fn selftest_account(
    ctx: &Context,
    console: &mut Console,
    failures: &mut usize,
    tag: &str,
    account: &crate::account::Account,
) {
    let reachable = timed(|| {
        let info = ctx.backend.account_info(account)?;
        Ok(format!(
            "Typ={}",
            if info.kind.is_empty() {
                "?"
            } else {
                &info.kind
            }
        ))
    });
    if !step(console, failures, tag, "Konto erreichbar", reachable) {
        return;
    }
    let boxes = timed(|| {
        Ok(format!(
            "{} Mailboxen",
            ctx.backend.mailboxes(account)?.len()
        ))
    });
    step(console, failures, tag, "Mailboxen lesen", boxes);
    let mut first: Option<String> = None;
    let listing = timed(|| {
        let result = ctx.backend.messages(account, &probe_query(30, None, 5))?;
        first = result.items.first().map(|item| item.id.clone());
        Ok(format!(
            "Posteingang: {} Treffer in 30 Tagen, {} geladen",
            result.total,
            result.items.len()
        ))
    });
    if step(console, failures, tag, "Posteingang listen", listing) {
        let reading = timed(|| {
            let Some(id) = &first else {
                return Ok("uebersprungen (keine Nachricht in 30 Tagen)".to_string());
            };
            let id = MessageId::parse(id)
                .ok_or_else(|| ToolError::msg("Nachrichten-ID ist ungueltig"))?;
            let message = ctx.backend.read(account, None, &id, 500)?;
            Ok(format!(
                "Text {} Zeichen, {} Anhaenge",
                message
                    .body_chars
                    .map_or_else(|| "?".to_string(), |count| count.to_string()),
                message.attachments.len()
            ))
        });
        step(console, failures, tag, "Nachricht lesen", reading);
    }
    let search = timed(|| {
        Ok(format!(
            "{} Treffer",
            ctx.backend
                .messages(account, &probe_query(30, Some("e"), 5))?
                .total
        ))
    });
    step(console, failures, tag, "Suche", search);
}

fn selftest_calendar(
    ctx: &Context,
    console: &mut Console,
    failures: &mut usize,
    tag: &str,
    calendar: &Calendar,
) {
    let reachable = timed(|| {
        let known = ctx.calendar.calendars(Source::Apple, "")?;
        let info = known
            .iter()
            .find(|info| info.id == calendar.id)
            .ok_or_else(|| ToolError::msg("Kalender nicht mehr in Calendar vorhanden"))?;
        Ok(if info.writable {
            "beschreibbar".to_string()
        } else {
            "schreibgeschuetzt".to_string()
        })
    });
    if !step(console, failures, tag, "Kalender erreichbar", reachable) {
        return;
    }
    let mut rules: Vec<String> = Vec::new();
    let reading = timed(|| {
        let today = local_today().ok_or_else(|| ToolError::msg("Datum nicht lesbar"))?;
        let to = today
            .add_days(14)
            .ok_or_else(|| ToolError::msg("Datum ausserhalb des Bereichs"))?;
        let window = Window {
            from: DateTime::midnight(today),
            to: DateTime::midnight(to),
        };
        let events = ctx.calendar.events(calendar, window)?;
        rules = events
            .iter()
            .filter(|event| !event.recurrence.is_empty())
            .map(|event| event.recurrence.clone())
            .collect();
        Ok(format!(
            "{} Eintraege in 14 Tagen, davon {} Serien",
            events.len(),
            rules.len()
        ))
    });
    if !step(console, failures, tag, "Termine lesen", reading) {
        return;
    }
    let unreadable: Vec<&String> = rules
        .iter()
        .filter(|rule| recurrence::parse(rule).is_err())
        .collect();
    let detail = format!(
        "{} von {} Serien lassen sich entfalten",
        rules.len() - unreadable.len(),
        rules.len()
    );
    console.say(&format!("INFO  {tag}  Wiederholungen: {detail}"));
    for rule in unreadable.iter().take(3) {
        console.say(&format!(
            "INFO  {tag}  nicht entfaltbare Regel: {}",
            one_line_str(rule, 120)
        ));
    }
}

fn cmd_selftest(ctx: &Context, console: &mut Console) -> u8 {
    let listing = (|| {
        let accounts = ctx.settings.allowed_accounts()?;
        let calendars = ctx.settings.allowed_calendars()?;
        Ok::<_, ToolError>((accounts, calendars))
    })();
    let (accounts, calendars) = match listing {
        Ok(listing) => listing,
        Err(error) => {
            console.say(&format!("FAIL  config: {error}"));
            return 1;
        }
    };
    if accounts.is_empty() && calendars.is_empty() {
        console.say(NO_ACCOUNTS);
        console.say(NO_CALENDARS);
        return 1;
    }
    let mut failures = 0usize;
    for (number, account) in accounts.iter().enumerate() {
        selftest_account(
            ctx,
            console,
            &mut failures,
            &format!("Konto {}", number + 1),
            account,
        );
    }
    for (number, calendar) in calendars.iter().enumerate() {
        selftest_calendar(
            ctx,
            console,
            &mut failures,
            &format!("Kalender {}", number + 1),
            calendar,
        );
    }
    console.say("");
    if failures > 0 {
        console.say(&format!("{failures} Pruefung(en) fehlgeschlagen."));
        return 1;
    }
    console.say("Alle Pruefungen bestanden. Es wurde nichts gesendet, geloescht oder veraendert; Inhalte wurden nicht ausgegeben.");
    0
}

fn probe_query(days: i64, text: Option<&str>, limit: i64) -> MessageQuery {
    MessageQuery {
        mailbox: None,
        since_days: days,
        unread_only: false,
        query: text.map(str::to_string),
        field: "both".to_string(),
        limit,
    }
}

fn self_message(
    mode: Mode,
    to: Vec<String>,
    subject: &str,
    body: &str,
    reply: Option<ReplySpec>,
) -> ComposeRequest {
    ComposeRequest {
        mode,
        to,
        cc: Vec::new(),
        subject: subject.to_string(),
        body: body.to_string(),
        reply,
    }
}

fn selftest_write_mail(
    ctx: &Context,
    console: &mut Console,
    settings: &WriteSettings,
    failures: &mut Vec<String>,
) {
    console.say(
        "1) Freigabedialog. Gleich erscheint ein Testdialog, klicke 'Weiter' oder 'Abbrechen'.",
    );
    console.say("   Es wird nichts gesendet.");
    match ctx.backend.approval_test(60) {
        Err(error) => {
            failures.push("dialog".into());
            console.say(&format!("FAIL  Freigabedialog: {error}"));
        }
        Ok(decision) if decision == "send" || decision == "declined" => {
            console.say(&format!(
                "PASS  Freigabedialog funktioniert (Antwort: {})",
                if decision == "send" {
                    "Weiter"
                } else {
                    "Abbrechen"
                }
            ));
        }
        Ok(_) => {
            failures.push("dialog".into());
            console.say("FAIL  Freigabedialog: keine Antwort innerhalb von 60 s");
        }
    }
    for account in &settings.accounts {
        let addresses = match ctx.backend.account_info(account) {
            Ok(info) => info.addresses,
            Err(error) => {
                failures.push("account".into());
                console.say(&format!("FAIL  {}: {error}", account.label));
                continue;
            }
        };
        let Some(own) = addresses.first().cloned() else {
            failures.push("account".into());
            console.say(&format!(
                "FAIL  {}: Mail kennt keine Absenderadresse fuer dieses Konto",
                account.label
            ));
            continue;
        };
        console.say(&format!(
            "\n2) Entwurf an dich selbst ({}). Ein Mail-Fenster oeffnet sich, nichts wird gesendet.",
            account.label
        ));
        let draft = self_message(
            Mode::Draft,
            vec![own.clone()],
            "Scout Selbsttest - Entwurf (bitte loeschen)",
            "Testentwurf. Er wurde nicht gesendet.",
            None,
        );
        match write_tools::perform_draft(ctx, account, &draft) {
            Ok(preview) => console.say(&format!(
                "PASS  Entwurf angelegt, Absender: {}{}",
                one_line_str(&preview.from, 100),
                if preview.saved.unwrap_or(true) {
                    ""
                } else {
                    " (Fenster offen, noch nicht in Entwuerfen)"
                }
            )),
            Err(error) => {
                failures.push("draft".into());
                console.say(&format!("FAIL  Entwurf: {error}"));
            }
        }
        if console
            .confirm("\n3) Test-Mail an dich selbst senden? Der Freigabedialog erscheint. [j/N] ")
        {
            let send = self_message(
                Mode::Send,
                vec![own.clone()],
                "Scout Selbsttest - Mail",
                "Test des Freigabedialogs. Du kannst diese Mail loeschen.",
                None,
            );
            match write_tools::perform_send(ctx, account, &send, settings.max_per_hour) {
                Err(error) => {
                    failures.push("send".into());
                    console.say(&format!("FAIL  Senden: {error}"));
                }
                Ok((preview, outcome)) => {
                    if outcome == Outcome::Sent {
                        console.say(&format!(
                            "PASS  Mail an {own} an Mail uebergeben (Posteingang und Gesendet pruefen)"
                        ));
                    } else {
                        console.say(&format!("INFO  Nicht gesendet ({})", preview.status));
                    }
                }
            }
        }
        if console.confirm(
            "\n4) Antworten testen: Entwurf und Antwort auf die Test-Mail an dich selbst? [j/N] ",
        ) {
            console.say(
                "   Suche die Test-Mail 'Scout Selbsttest - Mail' im Posteingang (bis zu 60 s) ...",
            );
            let rounds = if ctx.settings.fake { 1 } else { 12 };
            let mut found: Option<MessageId> = None;
            for number in 0..rounds {
                let query = MessageQuery {
                    field: "subject".to_string(),
                    ..probe_query(2, Some("Scout Selbsttest - Mail"), 3)
                };
                if let Ok(list) = ctx.backend.messages(account, &query) {
                    found = list
                        .items
                        .first()
                        .and_then(|item| MessageId::parse(&item.id));
                }
                if found.is_some() {
                    break;
                }
                if number < rounds - 1 {
                    thread::sleep(Duration::from_secs(5));
                }
            }
            let Some(id) = found else {
                console.say("INFO  Test-Mail nicht gefunden, Schritt uebersprungen (zuerst Schritt 3 mit 'j' ausfuehren)");
                continue;
            };
            for (mode, body) in [
                (Mode::Draft, "Antwortentwurf. Er wurde nicht gesendet."),
                (
                    Mode::Send,
                    "Antwort-Test des Freigabedialogs. Du kannst diese Mail loeschen.",
                ),
            ] {
                let request = self_message(
                    mode,
                    Vec::new(),
                    "",
                    body,
                    Some(ReplySpec {
                        mailbox: None,
                        id: id.clone(),
                        all: false,
                    }),
                );
                if mode == Mode::Send {
                    console.say("   Jetzt erscheint der Freigabedialog fuer die Antwort (geht an dich selbst).");
                }
                let result = match mode {
                    Mode::Draft => write_tools::perform_draft(ctx, account, &request),
                    Mode::Send => {
                        write_tools::perform_send(ctx, account, &request, settings.max_per_hour)
                            .map(|(preview, _)| preview)
                    }
                };
                match result {
                    Err(error) => {
                        failures.push(format!("reply-{}", mode.as_str()));
                        console.say(&format!("FAIL  Antwort ({}): {error}", mode.as_str()));
                    }
                    Ok(preview) => {
                        console.say(&format!(
                            "PASS  Antwort ({}): Status {}, An: {}",
                            if mode == Mode::Draft {
                                "Entwurf"
                            } else {
                                "senden"
                            },
                            preview.status,
                            if preview.to.is_empty() {
                                "-".to_string()
                            } else {
                                preview.to.join(", ")
                            }
                        ));
                    }
                }
            }
        }
    }
    console.say("\nTest-Entwurf und Test-Mail kannst du loeschen.");
}

fn change_line(
    console: &mut Console,
    failures: &mut Vec<String>,
    title: &str,
    outcome: ToolResult<ChangeResult>,
) -> Option<ChangeResult> {
    match outcome {
        Err(error) => {
            failures.push(title.to_string());
            console.say(&format!("FAIL  {title}: {error}"));
            None
        }
        Ok(performed) => match performed.decision {
            Decision::Applied => {
                console.say(&format!("PASS  {title}"));
                Some(performed)
            }
            Decision::Declined => {
                console.say(&format!(
                    "INFO  {title}: im Dialog abgelehnt, nichts geaendert"
                ));
                None
            }
            Decision::TimedOut => {
                console.say(&format!(
                    "INFO  {title}: keine Antwort im Dialog, nichts geaendert"
                ));
                None
            }
        },
    }
}

fn selftest_write_calendar(
    ctx: &Context,
    console: &mut Console,
    calendar: &Calendar,
    max_per_hour: i64,
    failures: &mut Vec<String>,
) {
    let origin = Origin::Model { max_per_hour };
    let Some(start) = local_today()
        .and_then(|today| today.add_days(1))
        .and_then(|day| DateTime::new(day, 180))
    else {
        failures.push("clock".to_string());
        console.say("FAIL  Datum nicht lesbar");
        return;
    };
    let Some(end) = start.add_minutes(15) else {
        failures.push("clock".to_string());
        console.say("FAIL  Datum ausserhalb des Bereichs");
        return;
    };
    console.say(&format!(
        "\nKalender-Test fuer {}. Es wird ein Test-Termin angelegt, geaendert und wieder geloescht, jeweils mit Freigabedialog.",
        one_line_str(&calendar.label, 80)
    ));
    if !console.confirm("Test-Termin anlegen? Der Freigabedialog erscheint. [j/N] ") {
        return;
    }
    let created_fields = EventFields {
        summary: "Scout Selbsttest - Termin (bitte loeschen)".to_string(),
        start,
        end,
        all_day: false,
        location: String::new(),
        notes: "Test des Freigabedialogs.".to_string(),
        url: String::new(),
    };
    let created = perform(
        ctx,
        calendar,
        &Change::Create {
            after: created_fields.clone(),
        },
        origin,
        None,
    );
    let Some(created) = change_line(console, failures, "Termin angelegt", created) else {
        return;
    };
    let Some(uid) = created.uid.clone() else {
        failures.push("uid".to_string());
        console.say("FAIL  Calendar nannte keine Termin-ID");
        return;
    };
    let mut current = created.after.clone().unwrap_or(created_fields);
    if console.confirm("Test-Termin aendern (Titel)? Der Freigabedialog erscheint. [j/N] ") {
        let after = EventFields {
            summary: "Scout Selbsttest - Termin geaendert (bitte loeschen)".to_string(),
            ..current.clone()
        };
        let change = Change::Update {
            uid: uid.clone(),
            before: current.clone(),
            after: after.clone(),
        };
        if let Some(changed) = change_line(
            console,
            failures,
            "Termin geaendert",
            perform(ctx, calendar, &change, origin, None),
        ) {
            current = changed.after.clone().unwrap_or(after);
        }
    }
    if console.confirm("Test-Termin loeschen? Der Freigabedialog erscheint. [j/N] ") {
        let change = Change::Delete {
            uid,
            before: current,
        };
        if change_line(
            console,
            failures,
            "Termin geloescht",
            perform(ctx, calendar, &change, origin, None),
        )
        .is_some()
        {
            return;
        }
    }
    console.say("INFO  Der Test-Termin bleibt in Calendar bestehen: bitte dort loeschen.");
}

fn cmd_selftest_write(ctx: &Context, console: &mut Console) -> u8 {
    let settings = (|| {
        let mail = ctx.settings.write_settings()?;
        let calendars = ctx.settings.calendar_write_settings()?;
        Ok::<_, ToolError>((mail, calendars))
    })();
    let (mail, calendars) = match settings {
        Ok(settings) => settings,
        Err(error) => {
            console.say(&format!("FAIL  config: {error}"));
            return 1;
        }
    };
    if mail.accounts.is_empty() && calendars.calendars.is_empty() {
        console.say(WRITE_OFF);
        console.say(CALENDAR_WRITE_OFF);
        return 1;
    }
    let mut failures: Vec<String> = Vec::new();
    if !mail.accounts.is_empty() {
        selftest_write_mail(ctx, console, &mail, &mut failures);
    }
    for calendar in &calendars.calendars {
        selftest_write_calendar(
            ctx,
            console,
            calendar,
            calendars.max_per_hour,
            &mut failures,
        );
    }
    console.say("");
    if !failures.is_empty() {
        console.say(&format!("{} Pruefung(en) fehlgeschlagen.", failures.len()));
        return 1;
    }
    console.say("Fertig.");
    0
}

fn matching(a: &[char], b: &[char]) -> usize {
    let mut best = (0usize, 0usize, 0usize);
    for (i, left) in a.iter().enumerate() {
        for (j, right) in b.iter().enumerate() {
            if left != right {
                continue;
            }
            let mut size = 0;
            while i + size < a.len() && j + size < b.len() && a.get(i + size) == b.get(j + size) {
                size += 1;
            }
            if size > best.2 {
                best = (i, j, size);
            }
        }
    }
    let (i, j, size) = best;
    if size == 0 {
        return 0;
    }
    size + matching(&a[..i], &b[..j]) + matching(&a[i + size..], &b[j + size..])
}

fn similarity(candidate: &str, word: &str) -> f64 {
    let a: Vec<char> = candidate.chars().collect();
    let b: Vec<char> = word.chars().collect();
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let common = i32::try_from(matching(&a, &b)).unwrap_or(0);
    let total = i32::try_from(a.len() + b.len()).unwrap_or(i32::MAX);
    2.0 * f64::from(common) / f64::from(total)
}

pub(crate) fn closest_command(word: &str) -> Option<&'static str> {
    if word.chars().count() > MAX_COMMAND_CHARS {
        return None;
    }
    let mut candidates: Vec<&'static str> = COMMANDS.to_vec();
    candidates.push("serve");
    candidates
        .into_iter()
        .map(|name| (similarity(name, word), name))
        .filter(|(score, _)| *score >= SUGGESTION_CUTOFF)
        .max_by(|left, right| left.0.total_cmp(&right.0).then(left.1.cmp(right.1)))
        .map(|(_, name)| name)
}

pub(crate) fn dispatch(
    args: &[String],
    ctx: &Context,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> u8 {
    let command = args
        .first()
        .map_or("serve", |first| first.trim())
        .trim_end_matches(['.', ',', ';', ':']);
    if matches!(command, "-V" | "--version" | "version") {
        let _ = writeln!(out, "{VERSION}");
        return 0;
    }
    if matches!(command, "-h" | "--help" | "help") {
        let _ = writeln!(out, "{HELP}");
        return 0;
    }
    if command == "serve" {
        return u8::from(server::serve(ctx, input, out).is_err());
    }
    let mut console = Console { input, out };
    match command {
        "accounts" => cmd_accounts(ctx, &mut console),
        "calendars" => cmd_calendars(ctx, &mut console),
        "setup" => cmd_setup(ctx, &mut console, args.get(1).map(String::as_str)),
        "revoke" => cmd_revoke(ctx, &mut console),
        "selftest" => cmd_selftest(ctx, &mut console),
        "sendlog" => cmd_sendlog(ctx, &mut console),
        "changes" => cmd_changes(ctx, &mut console),
        "undo" => cmd_undo(ctx, &mut console, args.get(1).map(String::as_str)),
        "selftest-write" => cmd_selftest_write(ctx, &mut console),
        "status" => cmd_status(ctx, &mut console),
        unknown => {
            console.say(&format!("Unbekannter Befehl: {unknown}"));
            if let Some(close) = closest_command(unknown) {
                console.say(&format!("Meintest du: avery mail {close}"));
            }
            console.say(&format!("Befehle: {}", COMMANDS.join(", ")));
            console.say("Hilfe: avery mail help");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Backend;
    use crate::calendar::{CalendarBackend, CalendarInfo};
    use crate::error::ToolResult;
    use crate::fake::FakeOptions;
    use crate::journal;
    use crate::json::Json;
    use crate::testutil::{j, log_entry, settings_at, TempDir, TestResult};
    use std::fs;
    use std::io::Cursor;

    const ACCOUNT_1: &str = r#"{"id": "ACC-1", "name": "Privat Gmail", "type": "imap", "addresses": ["me@gmail.example"], "level": "read"}"#;
    const ACCOUNT_2: &str = r#"{"id": "ACC-2", "name": "Outlook", "type": "imap", "addresses": ["me@gmail.example"], "level": "read"}"#;

    fn base() -> String {
        format!(r#"{{"version": 3, "mail": {{"accounts": [{ACCOUNT_1}, {ACCOUNT_2}]}}}}"#)
    }

    fn context(
        dir: &TempDir,
        fake: bool,
        write: &str,
        config: Option<&str>,
        options: FakeOptions,
    ) -> Result<Context, std::io::Error> {
        let mut settings = settings_at(&dir.path("config.json"));
        settings.fake = fake;
        settings.fake_write = write.to_string();
        if let Some(text) = config {
            fs::write(&settings.config_path, text)?;
        }
        Ok(Context::demo(settings, options))
    }

    fn real(dir: &TempDir, config: Option<&str>) -> Result<Context, std::io::Error> {
        context(dir, false, "", config, FakeOptions::default())
    }

    fn run(ctx: &Context, args: &[&str], input: &str) -> (u8, String) {
        let args: Vec<String> = args.iter().map(|argument| argument.to_string()).collect();
        let mut out = Vec::new();
        let code = dispatch(
            &args,
            ctx,
            &mut Cursor::new(input.as_bytes().to_vec()),
            &mut out,
        );
        (code, String::from_utf8_lossy(&out).into_owned())
    }

    fn saved(ctx: &Context) -> Result<Json, Box<dyn std::error::Error>> {
        Ok(j(&fs::read_to_string(&ctx.settings.config_path)?)?)
    }

    fn section<'a>(config: &'a Json, name: &str) -> Option<&'a Json> {
        config.as_object().and_then(|object| object.get(name))
    }

    struct Untouchable;

    impl Backend for Untouchable {
        fn all_accounts(&self) -> ToolResult<Vec<MailAccount>> {
            Err(ToolError::msg("BACKEND TOUCHED"))
        }
        fn account_info(&self, _: &crate::account::Account) -> ToolResult<MailAccount> {
            Err(ToolError::msg("BACKEND TOUCHED"))
        }
        fn mailboxes(
            &self,
            _: &crate::account::Account,
        ) -> ToolResult<Vec<crate::backend::Mailbox>> {
            Err(ToolError::msg("BACKEND TOUCHED"))
        }
        fn messages(
            &self,
            _: &crate::account::Account,
            _: &MessageQuery,
        ) -> ToolResult<crate::backend::MessageList> {
            Err(ToolError::msg("BACKEND TOUCHED"))
        }
        fn read(
            &self,
            _: &crate::account::Account,
            _: Option<&str>,
            _: &MessageId,
            _: usize,
        ) -> ToolResult<crate::backend::MessageBody> {
            Err(ToolError::msg("BACKEND TOUCHED"))
        }
        fn compose(
            &self,
            _: &crate::account::Account,
            _: &ComposeRequest,
        ) -> ToolResult<crate::backend::Preview> {
            Err(ToolError::msg("BACKEND TOUCHED"))
        }
        fn approval_test(&self, _: u64) -> ToolResult<String> {
            Err(ToolError::msg("BACKEND TOUCHED"))
        }
    }

    struct UntouchableCalendar;

    impl CalendarBackend for UntouchableCalendar {
        fn calendars(&self, _: Source, _: &str) -> ToolResult<Vec<CalendarInfo>> {
            Err(ToolError::msg("BACKEND TOUCHED"))
        }
        fn events(
            &self,
            _: &crate::calendar::Calendar,
            _: crate::calendar::Window,
        ) -> ToolResult<Vec<crate::calendar::RawEvent>> {
            Err(ToolError::msg("BACKEND TOUCHED"))
        }
        fn event(
            &self,
            _: &crate::calendar::Calendar,
            _: &str,
        ) -> ToolResult<crate::calendar::EventDetail> {
            Err(ToolError::msg("BACKEND TOUCHED"))
        }
        fn change(
            &self,
            _: &crate::calendar::Calendar,
            _: &crate::calendar::Change,
            _: &crate::calendar::Approval,
        ) -> ToolResult<crate::calendar::ChangeResult> {
            Err(ToolError::msg("BACKEND TOUCHED"))
        }
    }

    #[test]
    fn a_broken_config_is_reported_before_mail_or_calendar_is_touched() -> TestResult {
        for config in [
            "{\"version\": 2, \"allowed_accounts\": []}",
            "{\"version\": 3, \"allowed_accounts\": []}",
            "{\"version\": 3, \"mail\": 5}",
            "not json",
            "[]",
            "{\"allowed_accounts\": []}",
        ] {
            let dir = TempDir::new()?;
            let mut settings = settings_at(&dir.path("config.json"));
            fs::write(&settings.config_path, config)?;
            settings.fake = false;
            let ctx = Context::new(
                settings,
                Box::new(Untouchable),
                Box::new(UntouchableCalendar),
            );
            for args in [
                vec!["accounts"],
                vec!["calendars"],
                vec!["setup"],
                vec!["setup", "mail"],
                vec!["setup", "calendar"],
                vec!["revoke"],
                vec!["status"],
                vec!["selftest"],
                vec!["selftest-write"],
            ] {
                let (code, out) = run(&ctx, &args, "1\nj\n");
                assert_eq!(code, 1, "{config:?} {args:?}: {out}");
                assert!(
                    !out.contains("BACKEND TOUCHED"),
                    "{config:?} {args:?}: {out}"
                );
                assert_eq!(fs::read_to_string(&ctx.settings.config_path)?, config);
            }
        }
        Ok(())
    }

    #[test]
    fn setup_asks_a_level_per_account_and_saves_levels_and_details() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, None)?;
        let (code, out) = run(&ctx, &["setup", "mail"], "2\n1\n\nj\n");
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("Neue Freigaben:"));
        assert!(out.contains("Mail:     Demo Gmail (schreiben), Demo Outlook (lesen)"));
        assert!(out.contains("Kalender: keine"));
        assert!(out.contains("Gespeichert in"));
        let config = saved(&ctx)?;
        let accounts = section(&config, "mail")
            .and_then(Json::as_object)
            .and_then(|mail| mail.get("accounts"))
            .and_then(Json::as_array)
            .ok_or("accounts")?;
        assert_eq!(accounts.len(), 2);
        assert_eq!(
            accounts[0],
            j(
                r#"{"id": "FAKE-GMAIL", "name": "Demo Gmail", "type": "imap", "addresses": ["demo.privat@gmail.example"], "level": "write"}"#
            )?
        );
        assert!(section(&config, "calendar").is_none());
        Ok(())
    }

    #[test]
    fn setup_for_all_areas_asks_mail_first_then_calendars() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, None)?;
        let (code, out) = run(&ctx, &["setup"], "1\n\n\n2\n1\n\n\nj\n");
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("Mail:     Demo Gmail (lesen)"));
        assert!(out.contains("Kalender: Demo Privat (schreiben), Demo Familie (lesen)"));
        let config = saved(&ctx)?;
        let calendars = section(&config, "calendar")
            .and_then(Json::as_object)
            .and_then(|calendar| calendar.get("calendars"))
            .and_then(Json::as_array)
            .ok_or("calendars")?;
        assert_eq!(calendars.len(), 2);
        assert_eq!(
            calendars[0],
            j(r#"{"id": "FAKE-CAL-PRIVAT", "name": "Demo Privat", "level": "write"}"#)?
        );
        Ok(())
    }

    #[test]
    fn setup_for_one_area_keeps_the_other_area_and_the_limits() -> TestResult {
        let dir = TempDir::new()?;
        let start = r#"{"version": 3,
            "mail": {"accounts": [{"id": "FAKE-GMAIL", "name": "Demo Gmail", "level": "write"}], "sends_per_hour": 3},
            "calendar": {"calendars": [{"id": "FAKE-CAL-PRIVAT", "name": "Demo Privat", "level": "read"}], "changes_per_hour": 4}}"#;
        let ctx = real(&dir, Some(start))?;
        let (code, out) = run(&ctx, &["setup", "calendar"], "\n2\n\n\nj\n");
        assert_eq!(code, 0, "{out}");
        let config = saved(&ctx)?;
        let mail = section(&config, "mail")
            .and_then(Json::as_object)
            .ok_or("mail")?;
        assert_eq!(mail.get("sends_per_hour"), Some(&Json::Int(3)));
        assert_eq!(
            mail.get("accounts").and_then(Json::as_array).map(Vec::len),
            Some(1)
        );
        let calendar = section(&config, "calendar")
            .and_then(Json::as_object)
            .ok_or("calendar")?;
        assert_eq!(calendar.get("changes_per_hour"), Some(&Json::Int(4)));
        let ids: Vec<String> = calendar
            .get("calendars")
            .and_then(Json::as_array)
            .ok_or("list")?
            .iter()
            .filter_map(Json::as_object)
            .map(|entry| {
                format!(
                    "{}:{}",
                    crate::text::text_of(entry.get("id")),
                    crate::text::text_of(entry.get("level"))
                )
            })
            .collect();
        assert_eq!(ids, ["FAKE-CAL-PRIVAT:read", "FAKE-CAL-FAMILIE:write"]);
        Ok(())
    }

    #[test]
    fn setup_without_changes_says_so_and_writes_nothing() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, None)?;
        let (code, out) = run(&ctx, &["setup"], "\n\n\n\n\n\n\n");
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("Unveraendert."));
        assert!(!std::path::Path::new(&ctx.settings.config_path).exists());
        Ok(())
    }

    #[test]
    fn a_read_only_calendar_cannot_get_the_write_level() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, None)?;
        let (code, out) = run(&ctx, &["setup", "calendar"], "\n\n2\n1\n\nj\n");
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("Demo Feiertage [schreibgeschuetzt]"));
        assert!(out.contains("Dieser Eintrag ist schreibgeschuetzt (hoechstens Stufe 1)."));
        assert!(out.contains("Kalender: Demo Feiertage (lesen)"));
        Ok(())
    }

    #[test]
    fn entries_that_vanished_from_mail_are_removed_with_a_note() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, Some(&base()))?;
        let (code, out) = run(&ctx, &["setup", "mail"], "\n\n\nj\n");
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("Nicht mehr in Mail vorhanden, wird entfernt: Privat Gmail, Outlook"));
        let config = saved(&ctx)?;
        assert!(section(&config, "mail").is_none());
        Ok(())
    }

    #[test]
    fn setup_refuses_unconfirmed_choices_nonsense_and_missing_input() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, Some(&base()))?;
        for (input, expected) in [
            ("1\n\n\nn\n", "Abgebrochen."),
            ("1\n\n\n", "Abgebrochen."),
            ("", "Ungueltige Eingabe."),
            ("1\n", "Ungueltige Eingabe."),
            ("x\nx\nx\n", "Zu viele ungueltige Eingaben."),
            ("3\n9\n-1\n", "Zu viele ungueltige Eingaben."),
        ] {
            let (code, out) = run(&ctx, &["setup", "mail"], input);
            assert_eq!(code, 1, "{input:?}: {out}");
            assert!(out.contains(expected), "{input:?}: {out}");
            assert_eq!(saved(&ctx)?, j(&base())?, "{input:?}");
        }
        Ok(())
    }

    #[test]
    fn an_unknown_area_is_refused() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, None)?;
        let (code, out) = run(&ctx, &["setup", "notes\u{b}"], "");
        assert_eq!(code, 2);
        assert!(out.contains("Unbekannter Bereich: notes (mail oder calendar)"));
        Ok(())
    }

    #[test]
    fn an_endless_answer_line_is_refused() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, Some(&base()))?;
        let long = format!("{}\n", "1".repeat(20_000));
        let (code, out) = run(&ctx, &["setup", "mail"], &long);
        assert_eq!(code, 1);
        assert!(out.contains("Ungueltige Eingabe."));
        Ok(())
    }

    #[test]
    fn revoke_removes_all_grants_and_keeps_the_limits() -> TestResult {
        let dir = TempDir::new()?;
        let config = r#"{"version": 3,
            "mail": {"accounts": [{"id": "A", "name": "A", "level": "write"}], "sends_per_hour": 3},
            "calendar": {"calendars": [{"id": "C", "name": "C", "level": "write"}]}}"#;
        let ctx = real(&dir, Some(config))?;
        let (code, out) = run(&ctx, &["revoke"], "");
        assert_eq!(code, 0);
        assert!(out.contains("Alle Freigaben entfernt"));
        assert_eq!(
            saved(&ctx)?,
            j(r#"{"version": 3, "mail": {"accounts": [], "sends_per_hour": 3}}"#)?
        );
        Ok(())
    }

    #[test]
    fn a_config_that_cannot_be_read_is_never_overwritten() -> TestResult {
        for text in [
            "{\"version\": 2, \"allowed_accounts\": []}",
            "{\"allowed_accounts\": []}",
            "not json at all",
            "[]",
        ] {
            let dir = TempDir::new()?;
            let ctx = real(&dir, Some(text))?;
            for (args, input) in [
                (vec!["setup"], "1\nj\n"),
                (vec!["setup", "mail"], "1\nj\n"),
                (vec!["revoke"], ""),
            ] {
                let (code, out) = run(&ctx, &args, input);
                assert_eq!(code, 1, "{text:?} {args:?}: {out}");
                assert!(
                    out.contains("move the file away") || out.contains("Fehler"),
                    "{out}"
                );
                assert_eq!(fs::read_to_string(&ctx.settings.config_path)?, text);
            }
        }
        Ok(())
    }

    #[test]
    fn a_failed_save_is_reported_and_leaves_the_file_alone() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, Some(&base()))?;
        fs::create_dir(format!("{}.tmp", ctx.settings.config_path))?;
        let (code, out) = run(&ctx, &["revoke"], "");
        assert_eq!(code, 1);
        assert!(out.contains("nicht schreibbar"));
        assert_eq!(saved(&ctx)?, j(&base())?);
        Ok(())
    }

    #[test]
    fn sendlog_without_entries_says_so() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, Some(&base()))?;
        let (code, out) = run(&ctx, &["sendlog"], "");
        assert_eq!(code, 0);
        assert!(out.contains("Noch keine Eintraege"));
        Ok(())
    }

    #[test]
    fn sendlog_shows_one_line_per_attempt_and_marks_unclear_ones() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, Some(&base()))?;
        let now = journal::now_seconds();
        let mut first = log_entry(now - 100, Outcome::Pending, Some("a"));
        let mut second = log_entry(now - 90, Outcome::Sent, Some("a"));
        let mut third = log_entry(now - 50, Outcome::Pending, Some("b"));
        for (entry, to, subject) in [
            (&mut first, "x@example.org", "S1"),
            (&mut second, "x@example.org", "S1"),
            (&mut third, "y@example.org", "S2"),
        ] {
            entry.to = vec![to.to_string()];
            entry.subject = subject.to_string();
            sendlog::append(&ctx.settings, entry, true)?;
        }
        let (code, out) = run(&ctx, &["sendlog"], "");
        assert_eq!(code, 0);
        let rows: Vec<&str> = out
            .lines()
            .filter(|line| line.contains("example.org"))
            .collect();
        assert_eq!(rows.len(), 2, "{out}");
        assert!(rows[0].contains("sent") && rows[0].contains("S1"));
        assert!(rows[1].contains("unklar") && rows[1].contains("S2"));
        assert!(out.contains("Datei: "));
        Ok(())
    }

    #[test]
    fn sendlog_counts_lines_it_does_not_understand() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, Some(&base()))?;
        fs::write(ctx.settings.sendlog_path(), "garbage\n{\"v\": 7}\n")?;
        let (code, out) = run(&ctx, &["sendlog"], "");
        assert_eq!(code, 0);
        assert!(out.contains("2 Zeile(n) in einem unbekannten Format uebersprungen."));
        Ok(())
    }

    #[test]
    fn status_shows_the_levels_per_area_and_the_limits() -> TestResult {
        let dir = TempDir::new()?;
        let config = r#"{"version": 3,
            "mail": {"accounts": [{"id": "A", "name": "Privat Gmail", "level": "write"}, {"id": "B", "name": "Outlook", "level": "read"}], "sends_per_hour": 2},
            "calendar": {"calendars": [{"id": "C", "name": "Familie", "level": "read"}], "changes_per_hour": 6}}"#;
        let ctx = real(&dir, Some(config))?;
        let (code, out) = run(&ctx, &["status"], "");
        assert_eq!(code, 0);
        assert!(out.contains("Mail: Privat Gmail (schreiben), Outlook (lesen)"));
        assert!(out.contains("Kalender: Familie (lesen)"));
        assert!(out.contains(
            "Grenzen pro Stunde: 2 gesendete Mails, 30 Entwuerfe, 6 Kalenderaenderungen"
        ));
        assert!(out.contains(&format!("Version: {VERSION}")));
        let empty = TempDir::new()?;
        let (code, out) = run(&real(&empty, None)?, &["status"], "");
        assert_eq!(code, 0);
        assert!(
            out.contains("(fehlt)")
                && out.contains("Mail: keine")
                && out.contains("Kalender: keine")
        );
        Ok(())
    }

    #[test]
    fn status_reports_an_unsupported_config_instead_of_guessing() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, Some(r#"{"version": 2, "allowed_accounts": []}"#))?;
        let (code, out) = run(&ctx, &["status"], "");
        assert_eq!(code, 1);
        assert!(out.contains("Fehler: config.json has an unsupported format version"));
        Ok(())
    }

    #[test]
    fn accounts_marks_the_level_of_every_account() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = context(&dir, true, "FAKE-GMAIL", None, FakeOptions::default())?;
        let (code, out) = run(&ctx, &["accounts,"], "");
        assert_eq!(code, 0);
        assert!(
            out.contains("* 1) Demo Gmail  [imap]  demo.privat@gmail.example  [schreiben]"),
            "{out}"
        );
        assert!(out.contains("* 2) Demo Outlook  [exchange]"));
        assert!(out.contains("[lesen]"));
        assert!(out.contains("  3) Demo Firma  [exchange]  demo@firma.example  [aus]"));
        assert!(!out.contains("* 3)"));
        Ok(())
    }

    #[test]
    fn selftest_passes_on_demo_data_and_prints_no_mail_text() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = context(&dir, true, "", None, FakeOptions::default())?;
        let (code, out) = run(&ctx, &["selftest."], "");
        assert_eq!(code, 0, "{out}");
        assert!(!out.contains("Unbekannter Befehl"));
        assert!(out.contains("PASS  Konto 1  Konto erreichbar"));
        assert!(out.contains("PASS  Konto 2  Suche"));
        assert!(!out.contains("FAIL"));
        assert!(!out.contains("Paket"));
        assert!(out.contains("Alle Pruefungen bestanden."));
        Ok(())
    }

    #[test]
    fn calendars_marks_the_level_and_the_read_only_ones() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = context(&dir, true, "FAKE-CAL-PRIVAT", None, FakeOptions::default())?;
        let (code, out) = run(&ctx, &["calendars"], "");
        assert_eq!(code, 0);
        assert!(out.contains("* 1) Demo Privat  [schreiben]"), "{out}");
        assert!(out.contains("* 2) Demo Familie  [lesen]"));
        assert!(out.contains("* 3) Demo Feiertage [schreibgeschuetzt]  [lesen]"));
        assert!(out.contains("  4) Demo Firma  [aus]"));
        Ok(())
    }

    #[test]
    fn selftest_checks_calendars_and_prints_no_event_text() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = context(&dir, true, "", None, FakeOptions::default())?;
        let (code, out) = run(&ctx, &["selftest"], "");
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("PASS  Kalender 1  Kalender erreichbar"));
        assert!(out.contains("PASS  Kalender 1  Termine lesen"));
        assert!(out.contains("Kalender 3  Kalender erreichbar (0.0 s)  schreibgeschuetzt"));
        assert!(
            out.contains("INFO  Kalender 2  Wiederholungen: 1 von 1 Serien lassen sich entfalten")
        );
        assert!(!out.contains("Zahnarzt") && !out.contains("Familienessen"));
        let only = r#"{"version": 3, "calendar": {"calendars": [{"id": "FAKE-CAL-PRIVAT", "name": "Demo Privat", "level": "read"}]}}"#;
        let ctx = context(&dir, false, "", Some(only), FakeOptions::default())?;
        let (code, out) = run(&ctx, &["selftest"], "");
        assert_eq!(code, 0, "{out}");
        assert!(!out.contains("Konto 1") && out.contains("Kalender 1"));
        let gone = r#"{"version": 3, "calendar": {"calendars": [{"id": "GONE", "name": "Weg", "level": "read"}]}}"#;
        let ctx = context(&dir, false, "", Some(gone), FakeOptions::default())?;
        let (code, out) = run(&ctx, &["selftest"], "");
        assert_eq!(code, 1);
        assert!(out.contains(
            "FAIL  Kalender 1  Kalender erreichbar: Kalender nicht mehr in Calendar vorhanden"
        ));
        Ok(())
    }

    #[test]
    fn selftest_needs_accounts_and_a_readable_config() -> TestResult {
        let dir = TempDir::new()?;
        let (code, out) = run(&real(&dir, None)?, &["selftest"], "");
        assert_eq!(code, 1);
        assert!(out.contains("No mail account is enabled yet."));
        let broken = TempDir::new()?;
        let (code, out) = run(&real(&broken, Some("nope"))?, &["selftest"], "");
        assert_eq!(code, 1);
        assert!(out.contains("FAIL  config:"));
        Ok(())
    }

    #[test]
    fn selftest_write_walks_through_dialog_draft_send_and_reply_in_demo_mode() -> TestResult {
        let dir = TempDir::new()?;
        let options = FakeOptions {
            approve: "yes".into(),
            outbox: Some(dir.path("outbox.jsonl")),
            ..FakeOptions::default()
        };
        let ctx = context(&dir, true, "FAKE-GMAIL", None, options)?;
        let (code, out) = run(&ctx, &["selftest-write"], "j\nj\n");
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("PASS  Freigabedialog funktioniert (Antwort: Weiter)"));
        assert!(out.contains("PASS  Entwurf angelegt, Absender: Demo <demo.privat@gmail.example>"));
        assert!(out.contains("PASS  Mail an demo.privat@gmail.example an Mail uebergeben"));
        assert!(out.contains("INFO  Test-Mail nicht gefunden"));
        assert!(out.contains("Fertig."));
        let outbox = fs::read_to_string(dir.path("outbox.jsonl"))?;
        assert!(outbox.contains("Scout Selbsttest - Entwurf"));
        assert!(outbox.contains("\"status\": \"sent\""));
        let view = sendlog::read_entries(&ctx.settings, 10)?;
        let outcomes: Vec<Outcome> = view.entries.iter().map(|entry| entry.outcome).collect();
        assert_eq!(
            outcomes,
            [Outcome::Drafted, Outcome::Pending, Outcome::Sent]
        );
        Ok(())
    }

    #[test]
    fn selftest_write_cannot_send_when_the_log_cannot_be_written() -> TestResult {
        let dir = TempDir::new()?;
        let options = FakeOptions {
            approve: "yes".into(),
            outbox: Some(dir.path("outbox.jsonl")),
            ..FakeOptions::default()
        };
        let ctx = context(&dir, true, "FAKE-GMAIL", None, options)?;
        fs::create_dir_all(ctx.settings.sendlog_path())?;
        let (code, out) = run(&ctx, &["selftest-write"], "j\nj\n");
        assert_eq!(code, 1, "{out}");
        assert!(out.contains("FAIL  Entwurf: The send log is not writable"));
        assert!(out.contains("FAIL  Senden: The send log is not writable"));
        assert_eq!(
            fs::read_to_string(dir.path("outbox.jsonl")).unwrap_or_default(),
            ""
        );
        Ok(())
    }

    #[test]
    fn selftest_write_respects_the_send_limits() -> TestResult {
        let dir = TempDir::new()?;
        let options = FakeOptions {
            approve: "yes".into(),
            outbox: Some(dir.path("outbox.jsonl")),
            ..FakeOptions::default()
        };
        let ctx = context(&dir, true, "FAKE-GMAIL", None, options)?;
        let now = journal::now_seconds();
        for index in 0..5 {
            let entry = log_entry(now - 1000 - index, Outcome::Sent, None);
            sendlog::append(&ctx.settings, &entry, true)?;
        }
        let (code, out) = run(&ctx, &["selftest-write"], "j\n");
        assert_eq!(code, 1, "{out}");
        assert!(out.contains("FAIL  Senden: Send limit reached (5 per hour)"));
        assert!(!fs::read_to_string(dir.path("outbox.jsonl"))
            .unwrap_or_default()
            .contains("\"status\": \"sent\""));
        Ok(())
    }

    #[test]
    fn selftest_write_skips_the_mail_steps_when_they_are_not_confirmed() -> TestResult {
        let dir = TempDir::new()?;
        let options = FakeOptions {
            approve: "yes".into(),
            approve_test: "declined".into(),
            outbox: Some(dir.path("outbox.jsonl")),
            ..FakeOptions::default()
        };
        let ctx = context(&dir, true, "FAKE-GMAIL", None, options)?;
        let (code, out) = run(&ctx, &["selftest-write"], "");
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("(Antwort: Abbrechen)"));
        assert!(!out.contains("an Mail uebergeben"));
        let view = sendlog::read_entries(&ctx.settings, 10)?;
        let outcomes: Vec<Outcome> = view.entries.iter().map(|entry| entry.outcome).collect();
        assert_eq!(outcomes, [Outcome::Drafted]);
        Ok(())
    }

    #[test]
    fn selftest_write_reports_failures() -> TestResult {
        let dir = TempDir::new()?;
        let options = FakeOptions {
            approve_test: "nothing".into(),
            uncertain: true,
            ..FakeOptions::default()
        };
        let ctx = context(&dir, true, "FAKE-GMAIL", None, options)?;
        let (code, out) = run(&ctx, &["selftest-write"], "j\n");
        assert_eq!(code, 1, "{out}");
        assert!(out.contains("FAIL  Freigabedialog: keine Antwort innerhalb von 60 s"));
        assert!(out.contains("FAIL  Entwurf:"));
        assert!(out.contains("FAIL  Senden:"));
        assert!(out.contains("3 Pruefung(en) fehlgeschlagen."));
        let off = context(&dir, true, "", None, FakeOptions::default())?;
        let (code, out) = run(&off, &["selftest-write"], "");
        assert_eq!(code, 1);
        assert!(out.contains("Writing mail is not enabled."));
        Ok(())
    }

    #[test]
    fn typos_get_a_suggestion_and_unknown_commands_a_short_message() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, None)?;
        let (code, out) = run(&ctx, &["selftst"], "");
        assert_eq!(code, 2);
        assert!(out.contains("Meintest du: avery mail selftest"));
        assert!(!out.contains("Usage"));
        let (code, out) = run(&ctx, &["foo"], "");
        assert_eq!(code, 2);
        assert!(out.contains("Unbekannter Befehl: foo"));
        assert!(!out.contains("Usage") && !out.contains("Meintest"));
        assert!(out.contains(
            "Befehle: accounts, calendars, setup, revoke, selftest, sendlog, changes, undo, selftest-write, status"
        ));
        assert!(out.contains("Hilfe: avery mail help"));
        let (code, out) = run(&ctx, &["write-enable"], "");
        assert_eq!(code, 2);
        assert!(out.contains("Unbekannter Befehl: write-enable"));
        Ok(())
    }

    #[test]
    fn help_and_version_are_printed() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, None)?;
        for flag in ["help", "-h", "--help", "help."] {
            let (code, out) = run(&ctx, &[flag], "");
            assert_eq!(code, 0);
            assert!(out.contains("Usage") && out.contains("selftest-write"));
            assert!(!out.contains("write-enable"));
        }
        for flag in ["version", "-V", "--version"] {
            assert_eq!(run(&ctx, &[flag], ""), (0, format!("{VERSION}\n")));
        }
        Ok(())
    }

    #[test]
    fn the_default_command_is_serve() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = context(&dir, true, "", None, FakeOptions::default())?;
        let (code, out) = run(
            &ctx,
            &[],
            "{\"jsonrpc\": \"2.0\", \"id\": 1, \"method\": \"ping\"}\n",
        );
        assert_eq!(code, 0);
        assert_eq!(
            j(out.trim())?,
            j(r#"{"jsonrpc": "2.0", "id": 1, "result": {}}"#)?
        );
        let (_, out) = run(
            &ctx,
            &["serve"],
            "{\"jsonrpc\": \"2.0\", \"id\": 2, \"method\": \"ping\"}\n",
        );
        assert!(out.contains("\"id\": 2"));
        Ok(())
    }

    #[test]
    fn suggestions_follow_the_closest_command() {
        for (typed, expected) in [
            ("selftst", Some("selftest")),
            ("sendlg", Some("sendlog")),
            ("statu", Some("status")),
            ("acount", Some("accounts")),
            ("calender", Some("calendars")),
            ("serv", Some("serve")),
            ("xyz", None),
            ("", None),
        ] {
            assert_eq!(closest_command(typed), expected, "{typed}");
        }
        assert_eq!(closest_command(&"a".repeat(500)), None);
    }

    fn writer(dir: &TempDir, approve: &str) -> Result<Context, std::io::Error> {
        context(
            dir,
            true,
            "FAKE-CAL-PRIVAT",
            None,
            FakeOptions {
                approve: approve.to_string(),
                ..FakeOptions::default()
            },
        )
    }

    fn found(ctx: &Context, uid: &str) -> ToolResult<crate::calendar::EventDetail> {
        let calendar = ctx
            .settings
            .allowed_calendars()?
            .into_iter()
            .find(|calendar| calendar.id == "FAKE-CAL-PRIVAT")
            .ok_or_else(|| ToolError::msg("calendar"))?;
        ctx.calendar.event(&calendar, uid)
    }

    fn tool_args(text: &str) -> Result<crate::json::Object, String> {
        crate::testutil::args(text)
    }

    #[test]
    fn changes_without_entries_says_so_and_unreadable_lines_are_counted() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = writer(&dir, "yes")?;
        let (code, out) = run(&ctx, &["changes"], "");
        assert_eq!(code, 0);
        assert!(out.contains("Noch keine Eintraege"));
        fs::write(ctx.settings.changes_path(), "garbage\n")?;
        let (code, out) = run(&ctx, &["changes"], "");
        assert_eq!(code, 0);
        assert!(out.contains("1 Zeile(n) in einem unbekannten Format uebersprungen."));
        Ok(())
    }

    #[test]
    fn changes_shows_one_line_per_attempt_and_marks_what_can_be_undone() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = writer(&dir, "yes")?;
        crate::calendar::create_event(
            &ctx,
            &tool_args(r#"{"summary": "Tennis", "start": "2030-05-06 18:30"}"#)?,
        )?;
        crate::calendar::delete_event(&ctx, &tool_args(r#"{"id": "demo-dentist"}"#)?)?;
        let (code, out) = run(&ctx, &["changes"], "");
        assert_eq!(code, 0, "{out}");
        let rows: Vec<&str> = out.lines().filter(|l| l.contains("Demo Privat")).collect();
        assert_eq!(rows.len(), 2, "{out}");
        assert!(
            rows[0].contains("create") && rows[0].contains("done") && rows[0].contains("Tennis")
        );
        assert!(rows[0].contains("[rueckgaengig moeglich]"));
        assert!(rows[1].contains("delete") && rows[1].contains("Zahnarzt"));
        assert!(out.contains("Datei: "));
        Ok(())
    }

    #[test]
    fn undo_reverts_a_create_an_update_and_a_delete_one_after_the_other() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = writer(&dir, "yes")?;
        let original = found(&ctx, "demo-dentist")?;
        crate::calendar::delete_event(&ctx, &tool_args(r#"{"id": "demo-dentist"}"#)?)?;
        crate::calendar::create_event(
            &ctx,
            &tool_args(r#"{"summary": "Tennis", "start": "2030-05-06 18:30"}"#)?,
        )?;
        crate::calendar::update_event(
            &ctx,
            &tool_args(r#"{"id": "fake-new-1", "summary": "Tennis abgesagt"}"#)?,
        )?;
        let (code, out) = run(&ctx, &["undo"], "1\nj\n");
        assert_eq!(code, 0, "{out}");
        assert!(
            out.contains("der Termin bekommt seine alten Werte zurueck"),
            "{out}"
        );
        assert!(out.contains("Rueckgaengig gemacht."));
        assert_eq!(found(&ctx, "fake-new-1")?.event.summary, "Tennis");
        let (code, out) = run(&ctx, &["undo"], "1\nj\n");
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("der angelegte Termin wird geloescht"));
        assert!(found(&ctx, "fake-new-1").is_err());
        let (code, out) = run(&ctx, &["undo"], "1\nj\n");
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("der Termin wird neu angelegt"));
        let recreated = found(&ctx, "fake-new-2")?;
        assert_eq!(recreated.event.summary, original.event.summary);
        assert_eq!(recreated.event.start, original.event.start);
        assert_eq!(recreated.event.location, original.event.location);
        let (code, out) = run(&ctx, &["undo"], "");
        assert_eq!(code, 0);
        assert!(out.contains("Nichts zum Rueckgaengigmachen."));
        let (_, out) = run(&ctx, &["changes"], "");
        assert!(
            out.contains("undo-update")
                && out.contains("undo-create")
                && out.contains("undo-delete")
        );
        Ok(())
    }

    #[test]
    fn undo_can_pick_a_change_by_the_start_of_its_id() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = writer(&dir, "yes")?;
        crate::calendar::create_event(
            &ctx,
            &tool_args(r#"{"summary": "Tennis", "start": "2030-05-06 18:30"}"#)?,
        )?;
        let id = crate::changelog::read_entries(&ctx.settings, 10)?
            .entries
            .first()
            .map(|entry| entry.id.clone())
            .ok_or("entry")?;
        for wanted in ["", "zzzz"] {
            let (code, out) = run(&ctx, &["undo", wanted], "j\n");
            assert_eq!(code, 1, "{out}");
            assert!(out.contains("Keine eindeutige Aenderung"), "{out}");
        }
        assert!(found(&ctx, "fake-new-1").is_ok());
        let (code, out) = run(&ctx, &["undo", &id[..6]], "j\n");
        assert_eq!(code, 0, "{out}");
        assert!(found(&ctx, "fake-new-1").is_err());
        let (code, out) = run(&ctx, &["undo", &id[..6]], "j\n");
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("Nichts zum Rueckgaengigmachen."));
        Ok(())
    }

    #[test]
    fn undo_refuses_what_changed_since_and_needs_the_dialog() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = writer(&dir, "yes")?;
        crate::calendar::create_event(
            &ctx,
            &tool_args(r#"{"summary": "Tennis", "start": "2030-05-06 18:30"}"#)?,
        )?;
        let calendar = ctx
            .settings
            .allowed_calendars()?
            .into_iter()
            .find(|calendar| calendar.id == "FAKE-CAL-PRIVAT")
            .ok_or("calendar")?;
        let current =
            crate::calendar::EventFields::of(&ctx.calendar.event(&calendar, "fake-new-1")?);
        let moved = crate::calendar::EventFields {
            summary: "Selbst umbenannt".into(),
            ..current.clone()
        };
        ctx.calendar.change(
            &calendar,
            &crate::calendar::Change::Update {
                uid: "fake-new-1".into(),
                before: current,
                after: moved,
            },
            &crate::calendar::Approval {
                seconds: 5,
                undo: false,
            },
        )?;
        let (code, out) = run(&ctx, &["undo"], "1\nj\n");
        assert_eq!(code, 1, "{out}");
        assert!(out.contains("changed in Calendar"), "{out}");
        assert_eq!(found(&ctx, "fake-new-1")?.event.summary, "Selbst umbenannt");
        let other = TempDir::new()?;
        let declining = context(
            &other,
            true,
            "FAKE-CAL-PRIVAT",
            None,
            FakeOptions::default(),
        )?;
        let yes = writer(&other, "yes")?;
        crate::calendar::delete_event(&yes, &tool_args(r#"{"id": "demo-dentist"}"#)?)?;
        let (code, out) = run(&declining, &["undo"], "1\nj\n");
        assert_eq!(code, 1, "{out}");
        assert!(
            out.contains("Im Dialog abgelehnt, nichts geaendert."),
            "{out}"
        );
        let (code, out) = run(&declining, &["undo"], "1\nn\n");
        assert_eq!(code, 1);
        assert!(out.contains("Abgebrochen."));
        let (code, out) = run(&declining, &["undo"], "9\n");
        assert_eq!(code, 1);
        assert!(out.contains("Ungueltige Eingabe."));
        let (code, out) = run(&declining, &["undo"], "\n");
        assert_eq!(code, 0);
        assert!(out.contains("Unveraendert."));
        Ok(())
    }

    #[test]
    fn undo_ignores_the_limits_of_the_model_but_not_an_unwritable_log() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = writer(&dir, "yes")?;
        crate::changelog::ensure_writable(&ctx.settings)?;
        let now = journal::now_seconds();
        for index in 0..9 {
            let entry = crate::changelog::ChangeEntry {
                ts: now - 1000 - index,
                id: format!("old{index}"),
                action: crate::calendar::Action::Create,
                outcome: crate::changelog::Outcome::Done,
                calendar_id: "FAKE-CAL-PRIVAT".into(),
                calendar: "Demo Privat".into(),
                uid: None,
                before: None,
                after: None,
                undoes: None,
            };
            crate::changelog::append(&ctx.settings, &entry, true)?;
        }
        crate::calendar::delete_event(&ctx, &tool_args(r#"{"id": "demo-dentist"}"#)?)?;
        let blocked = crate::calendar::create_event(
            &ctx,
            &tool_args(r#"{"summary": "Tennis", "start": "2030-05-06 18:30"}"#)?,
        );
        assert!(
            matches!(&blocked, Err(ToolError::Msg(m)) if m.starts_with("Change limit reached")),
            "{blocked:?}"
        );
        let (code, out) = run(&ctx, &["undo"], "1\nj\n");
        assert_eq!(code, 0, "{out}");
        assert!(found(&ctx, "fake-new-1").is_ok());
        let still_blocked =
            crate::calendar::delete_event(&ctx, &tool_args(r#"{"id": "fake-new-1"}"#)?);
        assert!(
            matches!(&still_blocked, Err(ToolError::Msg(m)) if m.starts_with("Change limit reached"))
        );
        fs::remove_file(ctx.settings.changes_path())?;
        fs::create_dir_all(ctx.settings.changes_path())?;
        let (code, out) = run(&ctx, &["undo"], "1\nj\n");
        assert_eq!(code, 1, "{out}");
        Ok(())
    }

    #[test]
    fn selftest_write_walks_through_create_change_and_delete_of_a_test_event() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = writer(&dir, "yes")?;
        let (code, out) = run(&ctx, &["selftest-write"], "j\nj\nj\n");
        assert_eq!(code, 0, "{out}");
        assert!(!out.contains("Freigabedialog funktioniert"), "{out}");
        assert!(out.contains("PASS  Termin angelegt"));
        assert!(out.contains("PASS  Termin geaendert"));
        assert!(out.contains("PASS  Termin geloescht"));
        assert!(out.contains("Fertig."));
        assert!(found(&ctx, "fake-new-1").is_err());
        let log = crate::changelog::read_entries(&ctx.settings, 20)?;
        let kinds: Vec<(&str, &str)> = log
            .entries
            .iter()
            .map(|entry| (entry.action.as_str(), entry.outcome.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [
                ("create", "pending"),
                ("create", "done"),
                ("update", "pending"),
                ("update", "done"),
                ("delete", "pending"),
                ("delete", "done"),
            ]
        );
        let (code, out) = run(&ctx, &["selftest-write"], "n\n");
        assert_eq!(code, 0, "{out}");
        assert!(!out.contains("PASS"));
        let other = TempDir::new()?;
        let declining = writer(&other, "no")?;
        let (code, out) = run(&declining, &["selftest-write"], "j\n");
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("INFO  Termin angelegt: im Dialog abgelehnt, nichts geaendert"));
        Ok(())
    }

    #[test]
    fn selftest_write_keeps_the_test_event_when_the_delete_is_not_confirmed() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = writer(&dir, "yes")?;
        let (code, out) = run(&ctx, &["selftest-write"], "j\nn\nn\n");
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("INFO  Der Test-Termin bleibt in Calendar bestehen"));
        assert!(found(&ctx, "fake-new-1").is_ok());
        let other = TempDir::new()?;
        let broken = writer(&other, "yes")?;
        fs::create_dir_all(broken.settings.changes_path())?;
        let (code, out) = run(&broken, &["selftest-write"], "j\n");
        assert_eq!(code, 1, "{out}");
        assert!(out.contains("FAIL  Termin angelegt: The change log is not writable"));
        Ok(())
    }

    #[test]
    fn selftest_write_without_any_write_level_names_both_areas() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = context(&dir, true, "", None, FakeOptions::default())?;
        let (code, out) = run(&ctx, &["selftest-write"], "");
        assert_eq!(code, 1);
        assert!(
            out.contains("Writing mail is not enabled.")
                && out.contains("Changing calendars is not enabled.")
        );
        Ok(())
    }
}
