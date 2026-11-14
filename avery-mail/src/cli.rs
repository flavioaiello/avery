use std::io::{BufRead, Write};
use std::thread;
use std::time::{Duration, Instant};

use crate::account::MailAccount;
use crate::backend::{ComposeRequest, MessageId, MessageQuery, Mode, ReplySpec};
use crate::calendar::{
    local_today, perform, Action, Calendar, CalendarEntry, CalendarInfo, Change, ChangeResult,
    Decision, EventFields, Origin, Window,
};
use crate::changelog::{self, ChangeEntry};
use crate::civil::DateTime;
use crate::config::{UpdateError, WriteSettings};
use crate::console::{
    failed, level_word, Area, Choice, Console, GrantChange, Recap, Screen, PRIVATE_ONLY,
    RESTART_GOOSE,
};
use crate::context::Context;
use crate::error::{ToolError, ToolResult};
use crate::localtime;
use crate::recurrence;
use crate::rights::{name_or_id, Level, Rights, MAX_ENTRIES, MAX_NAME};
use crate::sendlog::{self, Outcome};
use crate::server;
use crate::text::{bracketed, one_line_str};
use crate::texts::{
    CALENDAR_WRITE_OFF, COMMANDS, HELP, MAX_DRAFTS_PER_HOUR, MAX_RECIPIENTS, MAX_SEND_CHARS,
    MAX_SEND_LINES, NO_ACCOUNTS, NO_CALENDARS, VERSION, WRITE_OFF,
};
use crate::write_tools;

mod onboard;

const SENDLOG_LINES: usize = 200;
const SENDLOG_SHOWN: usize = 20;
const CHANGES_LINES: usize = 300;
const CHANGES_SHOWN: usize = 20;
const SUGGESTION_CUTOFF: f64 = 0.6;
const MAX_COMMAND_CHARS: usize = 100;
const SETUP_HINT: &str = "Freigeben: avery mail setup";
const REVOKE_WITHOUT_APPS: &str = "Alles entziehen ohne Mail: avery mail revoke";

fn sent_by(sender: &str, address: &str) -> bool {
    bracketed(sender)
        .unwrap_or(sender)
        .trim()
        .eq_ignore_ascii_case(address)
}

fn describe(account: &MailAccount) -> String {
    let kind = if account.kind.is_empty() {
        "?".to_string()
    } else {
        one_line_str(&account.kind, 30)
    };
    let addresses = if account.addresses.is_empty() {
        "-".to_string()
    } else {
        one_line_str(&account.addresses.join(", "), 200)
    };
    format!(
        "{}  [{kind}]  {addresses}",
        one_line_str(&account.name, MAX_NAME)
    )
}

fn sanitized_calendar(info: CalendarInfo) -> CalendarInfo {
    CalendarInfo {
        name: name_or_id(one_line_str(&info.name, MAX_NAME), &info.id),
        ..info
    }
}

fn sanitized(account: MailAccount) -> MailAccount {
    MailAccount {
        name: name_or_id(one_line_str(&account.name, MAX_NAME), &account.id),
        id: account.id,
        kind: one_line_str(&account.kind, 30),
        addresses: account
            .addresses
            .iter()
            .map(|address| one_line_str(address, MAX_NAME))
            .collect(),
    }
}

fn editable(ctx: &Context, console: &mut Console) -> bool {
    if ctx.settings.fake {
        console.say("Im Demo-Modus (MAIL_MCP_FAKE=1) nicht moeglich: Einrichten und Widerrufen aendern die echte Konfiguration.");
        return false;
    }
    true
}

fn update_failed(ctx: &Context, console: &mut Console, error: &UpdateError) -> u8 {
    match error {
        UpdateError::Unreadable(error) => return failed(console, error),
        UpdateError::Busy => console.say(&format!(
            "Fehler: Ein anderer Avery-Befehl aendert gerade die Konfiguration ({}). Nichts gespeichert, gleich noch einmal versuchen.",
            ctx.settings.lock_path()
        )),
        UpdateError::Lock(kind) => console.say(&format!(
            "Fehler: {} nicht sperrbar ({kind:?}). Nichts gespeichert.",
            ctx.settings.lock_path()
        )),
        UpdateError::Invalid(what) => console.say(&format!(
            "Interner Fehler: Die neue Konfiguration waere ungueltig ({}). Nichts gespeichert.",
            one_line_str(what, 300)
        )),
        UpdateError::Write(kind) => console.say(&format!(
            "Fehler: config.json nicht schreibbar ({kind:?})"
        )),
        UpdateError::Changed => console.say(
            "Die Konfiguration wurde inzwischen geaendert (anderes Terminal?). Nichts gespeichert, den Befehl bitte erneut ausfuehren.",
        ),
    }
    1
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
    let rights = match ctx.settings.rights() {
        Ok(rights) => rights,
        Err(error) => return failed(console, &error),
    };
    if rights.mail.is_empty() {
        console.say("Noch kein Konto freigegeben.");
        console.say(SETUP_HINT);
        return 0;
    }
    for (index, (account, level)) in rights.mail.iter().enumerate() {
        console.say(&format!(
            "* {}) {}  [{}]",
            index + 1,
            describe(account),
            level_word(Some(*level))
        ));
    }
    console.say("\n* = fuer den Agenten freigegeben, Stufe in Klammern: lesen oder schreiben");
    console.say("(schreiben = Entwuerfe und Senden, jede Mail nur nach deinem Klick)");
    0
}

fn cmd_calendars(ctx: &Context, console: &mut Console) -> u8 {
    let rights = match ctx.settings.rights() {
        Ok(rights) => rights,
        Err(error) => return failed(console, &error),
    };
    if rights.calendars.is_empty() {
        console.say("Noch kein Kalender freigegeben.");
        console.say(SETUP_HINT);
        return 0;
    }
    for (index, (calendar, level)) in rights.calendars.iter().enumerate() {
        console.say(&format!(
            "* {}) {}  [{}]",
            index + 1,
            one_line_str(&calendar.name, 120),
            level_word(Some(*level))
        ));
    }
    console.say("\n* = fuer den Agenten freigegeben, Stufe in Klammern: lesen oder schreiben");
    console.say(
        "(schreiben = Termine anlegen, aendern, loeschen, jede Aenderung nur nach deinem Klick)",
    );
    0
}

struct Held {
    id: String,
    title: String,
    level: Level,
}

fn mail_held(rights: &Rights) -> Vec<Held> {
    rights
        .mail
        .iter()
        .map(|(account, level)| Held {
            id: account.id.clone(),
            title: account.name.clone(),
            level: *level,
        })
        .collect()
}

fn calendar_held(rights: &Rights) -> Vec<Held> {
    rights
        .calendars
        .iter()
        .map(|(calendar, level)| Held {
            id: calendar.id.clone(),
            title: calendar.name.clone(),
            level: *level,
        })
        .collect()
}

fn changes_between(area: Area, before: &[Held], after: &[Held]) -> Vec<GrantChange> {
    let mut changes: Vec<GrantChange> = Vec::new();
    for now in after {
        let was = before.iter().find(|known| known.id == now.id);
        if was.is_some_and(|known| known.level == now.level && known.title == now.title) {
            continue;
        }
        changes.push(GrantChange {
            area,
            title: now.title.clone(),
            was: was.map(|known| known.level),
            now: Some(now.level),
        });
    }
    for was in before {
        if !after.iter().any(|now| now.id == was.id) {
            changes.push(GrantChange {
                area,
                title: was.title.clone(),
                was: Some(was.level),
                now: None,
            });
        }
    }
    changes
}

fn grant_changes(before: &Rights, after: &Rights) -> Vec<GrantChange> {
    let mut changes = changes_between(Area::Mail, &mail_held(before), &mail_held(after));
    changes.extend(changes_between(
        Area::Calendar,
        &calendar_held(before),
        &calendar_held(after),
    ));
    changes
}

fn finish(ctx: &Context, console: &mut Console, before: &Rights, rights: &Rights) -> u8 {
    if rights == before {
        console.say("Unveraendert.");
        return 0;
    }
    let recap = Recap {
        mail: granted(&mail_grants(rights)),
        calendars: granted(&calendar_grants(rights)),
        changes: grant_changes(before, rights),
    };
    if !console.confirm_grants(&recap) {
        console.say("Abgebrochen.");
        return 1;
    }
    match ctx
        .settings
        .update_rights(|fresh| fresh.apply(before, rights))
    {
        Ok(()) => {
            console.hint(&format!("Gespeichert in {}", ctx.settings.config_path));
            console.hint(&format!(
                "{RESTART_GOOSE} Pruefen mit: avery mail selftest (lesen) und avery mail selftest-write (schreiben)"
            ));
            0
        }
        Err(error) => update_failed(ctx, console, &error),
    }
}

fn explain_levels(ctx: &Context, console: &mut Console, rights: &Rights) {
    console.hint(PRIVATE_ONLY);
    console
        .hint("Stufen: 0 = aus, 1 = lesen, 2 = schreiben (Entwuerfe, Senden und Termine aendern,");
    console.hint("jedes Mal erst nach deinem Klick in einem macOS-Dialog; der Agent kann ihn nicht bedienen).");
    console.hint(&format!(
        "Mail-Grenzen: Klartext, kein Bcc, keine Anhaenge, max. {MAX_RECIPIENTS} Empfaenger, max. {MAX_SEND_CHARS} Zeichen pro gesendeter Mail,"
    ));
    console.hint(&format!(
        "         max. {MAX_SEND_LINES} Zeilen, max. {} gesendete Mails und max. {MAX_DRAFTS_PER_HOUR} Entwuerfe pro Stunde. Jeder Versuch steht in {}",
        rights.sends_per_hour,
        ctx.settings.sendlog_path()
    ));
    console.hint(&format!(
        "Kalender-Grenzen: nur einzelne Termine ohne Teilnehmer und ohne Wiederholung, max. {} Aenderungen pro Stunde. Jeder Versuch steht in {}",
        rights.changes_per_hour,
        ctx.settings.changes_path()
    ));
}

fn unique<T>(items: Vec<T>, id: impl Fn(&T) -> &str) -> Vec<T> {
    let mut kept: Vec<T> = Vec::new();
    for item in items {
        let name = id(&item);
        if !name.is_empty() && !kept.iter().any(|known| id(known) == name) {
            kept.push(item);
        }
    }
    kept
}

fn setup(ctx: &Context, console: &mut Console) -> u8 {
    if !editable(ctx, console) {
        return 1;
    }
    let mut rights = match ctx.settings.stored_rights() {
        Ok(rights) => rights,
        Err(error) => return failed(console, &error),
    };
    let before = rights.clone();
    if !console.explain() {
        return 1;
    }
    let mail = match ctx.backend.all_accounts() {
        Ok(found) => unique(found.into_iter().map(sanitized).collect(), |account| {
            account.id.as_str()
        }),
        Err(error) => {
            console.say(&format!(
                "Fehler: Apple Mail ist nicht lesbar ({error}). {REVOKE_WITHOUT_APPS}"
            ));
            return 1;
        }
    };
    let calendars = match ctx.calendar.calendars() {
        Ok(found) => unique(
            found.into_iter().map(sanitized_calendar).collect(),
            |info| info.id.as_str(),
        ),
        Err(error) => {
            console.say(&format!(
                "Fehler: Apple Kalender ist nicht lesbar ({error}). {REVOKE_WITHOUT_APPS}"
            ));
            return 1;
        }
    };
    if mail.len() > MAX_ENTRIES || calendars.len() > MAX_ENTRIES {
        console.say(&format!(
            "Fehler: Mail oder Kalender melden mehr als {MAX_ENTRIES} Eintraege."
        ));
        return 1;
    }
    explain_levels(ctx, console, &rights);
    let choices: Vec<Choice> = mail
        .iter()
        .map(|account| Choice {
            title: describe(account),
            current: rights
                .mail
                .iter()
                .find(|(known, _)| known.id == account.id)
                .map(|(_, level)| *level),
            highest: Level::Write,
        })
        .chain(calendars.iter().map(|info| {
            let highest = if info.writable {
                Level::Write
            } else {
                Level::Read
            };
            Choice {
                title: if info.writable {
                    one_line_str(&info.name, 120)
                } else {
                    format!("{} [schreibgeschuetzt]", one_line_str(&info.name, 120))
                },
                current: rights
                    .calendars
                    .iter()
                    .find(|(known, _)| known.id == info.id)
                    .map(|(_, level)| (*level).min(highest)),
                highest,
            }
        }))
        .collect();
    let levels = match console.ask_levels(&choices) {
        Ok(levels) => levels,
        Err(code) => return code,
    };
    let mut levels = levels.into_iter();
    let gone: Vec<String> = rights
        .mail
        .iter()
        .map(|(known, _)| (known.id.as_str(), known.name.as_str()))
        .chain(
            rights
                .calendars
                .iter()
                .map(|(known, _)| (known.id.as_str(), known.name.as_str())),
        )
        .filter(|(id, _)| {
            !mail.iter().any(|account| account.id == *id)
                && !calendars.iter().any(|info| info.id == *id)
        })
        .map(|(_, name)| name.to_string())
        .collect();
    if !gone.is_empty() {
        console.hint(&format!(
            "Nicht mehr vorhanden, wird entfernt: {}",
            gone.join(", ")
        ));
    }
    rights.mail = mail
        .into_iter()
        .zip(levels.by_ref())
        .filter_map(|(account, level)| level.map(|level| (account, level)))
        .collect();
    rights.calendars = calendars
        .into_iter()
        .zip(levels)
        .filter_map(|(info, level)| {
            level.map(|level| {
                (
                    CalendarEntry {
                        id: info.id,
                        name: info.name,
                    },
                    level,
                )
            })
        })
        .collect();
    let code = finish(ctx, console, &before, &rights);
    if code == 0 && before.enabled() && !rights.enabled() {
        console.say("Mail und Kalender sind wieder aus: Avery startet sie nicht mehr.");
    }
    code
}

fn cmd_setup(ctx: &Context, console: &mut Console, extra: &[String]) -> u8 {
    if !extra.is_empty() {
        console.say("Benutzung: avery mail setup");
        return 2;
    }
    setup(ctx, console)
}

fn cmd_revoke(ctx: &Context, console: &mut Console) -> u8 {
    if !editable(ctx, console) {
        return 1;
    }
    let revoked = ctx.settings.update_rights(|rights| {
        let had = rights.enabled();
        rights.revoke();
        Ok(had)
    });
    let had = match revoked {
        Ok(had) => had,
        Err(error) => return update_failed(ctx, console, &error),
    };
    console.say("Alle Freigaben entfernt (Mail und Kalender, auch Schreiben). Der Agent sieht keine Konten und Kalender mehr.");
    if had {
        console.say("Mail und Kalender sind wieder aus: Avery startet sie nicht mehr. Wieder freigeben: avery mail setup");
    }
    0
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
    let writable = ctx
        .settings
        .rights()
        .is_ok_and(|rights| rights.calendar_level(&entry.calendar_id) == Some(Level::Write));
    if !writable {
        console.say(
            "Dieser Kalender ist nicht mehr zum Schreiben freigegeben. Zuerst: avery mail setup",
        );
        return 1;
    }
    console.say(&format!(
        "\n{} | {} | {}: {}",
        one_line_str(&entry.calendar, 60),
        entry.action.as_str(),
        title_of(entry),
        undo_text(entry)
    ));
    if !console.confirm("Rueckgaengig machen? Ein Freigabedialog erscheint.") {
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
    if !rights.enabled() {
        console.say(SETUP_HINT);
    }
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
        let known = ctx.calendar.calendars()?;
        let info = known
            .iter()
            .find(|info| info.id == calendar.id)
            .ok_or_else(|| ToolError::msg("Kalender nicht mehr vorhanden"))?;
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
        match write_tools::perform_draft(ctx, account, &draft, Origin::Human) {
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
        if console.confirm("\n3) Test-Mail an dich selbst senden? Der Freigabedialog erscheint.") {
            let send = self_message(
                Mode::Send,
                vec![own.clone()],
                "Scout Selbsttest - Mail",
                "Test des Freigabedialogs. Du kannst diese Mail loeschen.",
                None,
            );
            match write_tools::perform_send(ctx, account, &send, Origin::Human) {
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
        if console
            .confirm("\n4) Antworten testen: Entwurf und Antwort auf die Test-Mail an dich selbst?")
        {
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
                        .iter()
                        .find(|item| sent_by(&item.from, &own))
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
                    Mode::Draft => {
                        write_tools::perform_draft(ctx, account, &request, Origin::Human)
                    }
                    Mode::Send => write_tools::perform_send(ctx, account, &request, Origin::Human)
                        .map(|(preview, _)| preview),
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
    failures: &mut Vec<String>,
) {
    let origin = Origin::Human;
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
    if !console.confirm("Test-Termin anlegen? Der Freigabedialog erscheint.") {
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
    if console.confirm("Test-Termin aendern (Titel)? Der Freigabedialog erscheint.") {
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
    if console.confirm("Test-Termin loeschen? Der Freigabedialog erscheint.") {
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

fn selftest_write_all_day(
    ctx: &Context,
    console: &mut Console,
    calendar: &Calendar,
    failures: &mut Vec<String>,
) {
    let Some(first) = local_today().and_then(|today| today.add_days(3)) else {
        failures.push("clock".to_string());
        console.say("FAIL  Datum nicht lesbar");
        return;
    };
    let Some(next) = first.add_days(1) else {
        failures.push("clock".to_string());
        console.say("FAIL  Datum ausserhalb des Bereichs");
        return;
    };
    if !console.confirm(
        "Ganztaegigen Test-Termin anlegen? Das prueft, wie Calendar das Ende von Ganztagsterminen speichert.",
    ) {
        return;
    }
    let fields = EventFields {
        summary: "Scout Selbsttest - Ganztag (bitte loeschen)".to_string(),
        start: DateTime::midnight(first),
        end: DateTime::midnight(next),
        all_day: true,
        location: String::new(),
        notes: String::new(),
        url: String::new(),
    };
    let created = perform(
        ctx,
        calendar,
        &Change::Create {
            after: fields.clone(),
        },
        Origin::Human,
        None,
    );
    let Some(created) = change_line(console, failures, "Ganztagstermin angelegt", created) else {
        return;
    };
    let Some(uid) = created.uid.clone() else {
        failures.push("uid".to_string());
        console.say("FAIL  Calendar nannte keine Termin-ID");
        return;
    };
    let stored = ctx
        .calendar
        .event(calendar, &uid)
        .map(|detail| EventFields::of(&detail));
    match stored {
        Ok(stored)
            if stored.all_day && stored.start == fields.start && stored.end == fields.end =>
        {
            console.say("PASS  Ganztags-Ende wie erwartet (00:00 des Folgetags)");
        }
        Ok(stored) => {
            failures.push("all-day".to_string());
            console.say(&format!(
                "FAIL  Ganztagstermin gespeichert als {} bis {} (ganztaegig: {}), erwartet {} bis {}",
                stored.start, stored.end, stored.all_day, fields.start, fields.end
            ));
        }
        Err(error) => {
            failures.push("all-day".to_string());
            console.say(&format!("FAIL  Ganztagstermin nicht lesbar: {error}"));
        }
    }
    let current = created.after.clone().unwrap_or(fields);
    if console.confirm("Ganztaegigen Test-Termin loeschen? Der Freigabedialog erscheint.") {
        let change = Change::Delete {
            uid,
            before: current,
        };
        if change_line(
            console,
            failures,
            "Ganztagstermin geloescht",
            perform(ctx, calendar, &change, Origin::Human, None),
        )
        .is_some()
        {
            return;
        }
    }
    console.say("INFO  Der Ganztags-Test-Termin bleibt bestehen: bitte im Kalender loeschen.");
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
        selftest_write_calendar(ctx, console, calendar, &mut failures);
        selftest_write_all_day(ctx, console, calendar, &mut failures);
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
    screen: &Screen,
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
    let mut console = Console::terminal(input, out);
    match command {
        "accounts" => cmd_accounts(ctx, &mut console),
        "calendars" => cmd_calendars(ctx, &mut console),
        "setup" => cmd_setup(ctx, &mut console, args.get(1..).unwrap_or_default()),
        "onboard" => onboard::run(ctx, &mut console, screen, args.get(1..).unwrap_or_default()),
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
    use crate::testutil::{
        args, dark, j, log_entry, settings_at, TempDir, TestResult, Untouchable,
    };
    use std::fs;
    use std::io::{BufRead, Cursor, Read};

    const ACCOUNT_1: &str = r#"{"id": "ACC-1", "name": "Privat Gmail", "type": "imap", "addresses": ["me@gmail.example"], "level": "read"}"#;
    const ACCOUNT_2: &str = r#"{"id": "ACC-2", "name": "Outlook", "type": "imap", "addresses": ["me@gmail.example"], "level": "read"}"#;

    fn config_with(entries: &str) -> String {
        format!(r#"{{"version": 4, {entries}}}"#)
    }

    fn base() -> String {
        config_with(&format!(r#""mail": [{ACCOUNT_1}, {ACCOUNT_2}]"#))
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
            &dark(),
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

    #[test]
    fn a_broken_config_is_reported_before_mail_or_calendar_is_touched() -> TestResult {
        for config in [
            "{\"version\": 2, \"allowed_accounts\": []}",
            "{\"version\": 3, \"apple\": {\"mail\": [{\"id\": \"A\", \"level\": \"read\"}]}}",
            "{\"version\": 4, \"allowed_accounts\": []}",
            "{\"version\": 4, \"apple\": {}}",
            "{\"version\": 4, \"mail\": {\"accounts\": []}}",
            "not json",
            "[]",
            "{\"allowed_accounts\": []}",
        ] {
            let dir = TempDir::new()?;
            let mut settings = settings_at(&dir.path("config.json"));
            fs::write(&settings.config_path, config)?;
            settings.fake = false;
            let apple = Untouchable::default();
            let ctx = Context::new(settings, Box::new(apple.clone()), Box::new(apple.clone()));
            for args in [
                vec!["accounts"],
                vec!["calendars"],
                vec!["setup"],
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
            assert_eq!(apple.calls(), 0, "{config:?}");
        }
        Ok(())
    }

    #[test]
    fn the_changes_between_two_states_are_the_new_the_moved_the_renamed_and_the_removed_grants(
    ) -> TestResult {
        let before = Rights::parse(&args(
            r#"{"version": 4,
                "mail": [{"id": "M1", "name": "Privat", "level": "read"}],
                "calendars": [{"id": "c1", "name": "Eins", "level": "read"},
                              {"id": "c2", "name": "Zwei", "level": "write"},
                              {"id": "c3", "name": "Drei", "level": "read"},
                              {"id": "c6", "name": "Sechs", "level": "read"},
                              {"id": "A1", "name": "Privat", "level": "read"}]}"#,
        )?)?;
        let after = Rights::parse(&args(
            r#"{"version": 4,
                "mail": [{"id": "M1", "name": "Privat", "level": "write"}],
                "calendars": [{"id": "c1", "name": "Eins", "level": "read"},
                              {"id": "c2", "name": "Zwei", "level": "read"},
                              {"id": "c3", "name": "Drei neu", "level": "read"},
                              {"id": "c4", "name": "Vier", "level": "write"},
                              {"id": "A1", "name": "Privat", "level": "read"},
                              {"id": "A2", "name": "Neu", "level": "write"}]}"#,
        )?)?;
        let changes = grant_changes(&before, &after);
        let found: Vec<(Area, &str, Option<Level>, Option<Level>)> = changes
            .iter()
            .map(|change| (change.area, change.title.as_str(), change.was, change.now))
            .collect();
        assert_eq!(
            found,
            vec![
                (Area::Mail, "Privat", Some(Level::Read), Some(Level::Write)),
                (
                    Area::Calendar,
                    "Zwei",
                    Some(Level::Write),
                    Some(Level::Read)
                ),
                (
                    Area::Calendar,
                    "Drei neu",
                    Some(Level::Read),
                    Some(Level::Read)
                ),
                (Area::Calendar, "Vier", None, Some(Level::Write)),
                (Area::Calendar, "Neu", None, Some(Level::Write)),
                (Area::Calendar, "Sechs", Some(Level::Read), None),
            ]
        );
        assert!(grant_changes(&before, &before).is_empty());
        assert!(grant_changes(&after, &after).is_empty());
        let reversed = grant_changes(&after, &before);
        assert_eq!(reversed.len(), 6);
        assert!(reversed
            .iter()
            .any(|change| change.title == "Neu" && change.now.is_none()));
        Ok(())
    }

    #[test]
    fn setup_asks_a_level_per_item_and_saves_mail_and_calendars() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, None)?;
        let (code, out) = run(&ctx, &["setup"], "2\n1\n\n2\n1\n\n\nj\n");
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("Automation-Berechtigung"), "{out}");
        assert!(out.contains("Neue Freigaben:"));
        assert!(out.contains("Mail:     Demo Gmail (schreiben), Demo Outlook (lesen)"));
        assert!(out.contains("Kalender: Demo Privat (schreiben), Demo Familie (lesen)"));
        assert!(out.contains("Gespeichert in"));
        assert!(out.contains(PRIVATE_ONLY), "{out}");
        assert!(out.contains(RESTART_GOOSE), "{out}");
        let config = saved(&ctx)?;
        let object = config.as_object().ok_or("object")?;
        assert_eq!(object.get("version"), Some(&Json::Int(4)));
        let accounts = object
            .get("mail")
            .and_then(Json::as_array)
            .ok_or("accounts")?;
        assert_eq!(accounts.len(), 2);
        assert_eq!(
            accounts[0],
            j(
                r#"{"id": "FAKE-GMAIL", "name": "Demo Gmail", "type": "imap", "addresses": ["demo.privat@gmail.example"], "level": "write"}"#
            )?
        );
        let calendars = object
            .get("calendars")
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
    fn setup_keeps_the_limits() -> TestResult {
        let dir = TempDir::new()?;
        let start = r#"{"version": 4,
            "mail": [{"id": "FAKE-GMAIL", "name": "Demo Gmail", "level": "write"}],
            "calendars": [{"id": "FAKE-CAL-PRIVAT", "name": "Demo Privat", "level": "read"}],
            "sends_per_hour": 3, "changes_per_hour": 4}"#;
        let ctx = real(&dir, Some(start))?;
        let (code, out) = run(&ctx, &["setup"], "\n\n\n\n2\n\n\nj\n");
        assert_eq!(code, 0, "{out}");
        let config = saved(&ctx)?;
        let object = config.as_object().ok_or("object")?;
        assert_eq!(object.get("sends_per_hour"), Some(&Json::Int(3)));
        assert_eq!(object.get("changes_per_hour"), Some(&Json::Int(4)));
        let rights = ctx.settings.stored_rights().map_err(|e| e.message())?;
        assert_eq!(rights.mail_level("FAKE-GMAIL"), Some(Level::Write));
        assert_eq!(rights.calendar_level("FAKE-CAL-PRIVAT"), Some(Level::Read));
        assert_eq!(
            rights.calendar_level("FAKE-CAL-FAMILIE"),
            Some(Level::Write)
        );
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
        let (code, out) = run(&ctx, &["setup"], "\n\n\n\n\n2\n1\n\nj\n");
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("Demo Feiertage [schreibgeschuetzt]"));
        assert!(out.contains("Dieser Eintrag ist schreibgeschuetzt (hoechstens Stufe 1)."));
        assert!(out.contains("Kalender: Demo Feiertage (lesen)"));
        Ok(())
    }

    #[test]
    fn entries_that_vanished_are_removed_with_a_note_and_all_off_switches_everything_off(
    ) -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, Some(&base()))?;
        let (code, out) = run(&ctx, &["setup"], "\n\n\n\n\n\n\nj\n");
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("Nicht mehr vorhanden, wird entfernt: Privat Gmail, Outlook"));
        assert!(out.contains("Mail und Kalender sind wieder aus"), "{out}");
        let config = saved(&ctx)?;
        assert!(section(&config, "mail").is_none());
        assert!(section(&config, "calendars").is_none());
        assert!(!ctx
            .settings
            .stored_rights()
            .map_err(|e| e.message())?
            .enabled());
        Ok(())
    }

    #[test]
    fn setup_refuses_unconfirmed_choices_nonsense_and_missing_input() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, Some(&base()))?;
        for (input, expected) in [
            ("1\n\n\n\n\n\n\nn\n", "Abgebrochen."),
            ("1\n\n\n\n\n\n\n", "Abgebrochen."),
            ("", "Ungueltige Eingabe."),
            ("1\n", "Ungueltige Eingabe."),
            ("x\nx\nx\n", "Zu viele ungueltige Eingaben."),
            ("3\n9\n-1\n", "Zu viele ungueltige Eingaben."),
        ] {
            let (code, out) = run(&ctx, &["setup"], input);
            assert_eq!(code, 1, "{input:?}: {out}");
            assert!(out.contains(expected), "{input:?}: {out}");
            assert_eq!(saved(&ctx)?, j(&base())?, "{input:?}");
        }
        Ok(())
    }

    #[test]
    fn setup_names_the_way_out_when_mail_or_calendar_cannot_be_listed() -> TestResult {
        let listing = crate::testutil::Listing {
            accounts: vec![MailAccount {
                id: "ACC-1".to_string(),
                name: "iCloud".to_string(),
                kind: "imap".to_string(),
                addresses: vec!["me@icloud.example".to_string()],
            }],
            calendars: vec![CalendarInfo {
                id: "CAL-1".to_string(),
                name: "Privat".to_string(),
                writable: true,
            }],
        };
        for (mail_up, failed_listing) in [
            (false, "Fehler: Apple Mail ist nicht lesbar ("),
            (true, "Fehler: Apple Kalender ist nicht lesbar ("),
        ] {
            let dir = TempDir::new()?;
            let (mail, calendar): (Box<dyn Backend>, Box<dyn CalendarBackend>) = if mail_up {
                (Box::new(listing.clone()), Box::new(Untouchable::default()))
            } else {
                (Box::new(Untouchable::default()), Box::new(listing.clone()))
            };
            let settings = settings_at(&dir.path("config.json"));
            fs::write(&settings.config_path, base())?;
            let ctx = Context::new(settings, mail, calendar);
            let (code, out) = run(&ctx, &["setup"], "1\n1\nj\n");
            assert_eq!(code, 1, "{out}");
            assert!(out.contains(failed_listing), "{out}");
            assert!(
                out.contains("). Alles entziehen ohne Mail: avery mail revoke"),
                "{out}"
            );
            assert_eq!(saved(&ctx)?, j(&base())?);
            let (code, out) = run(&ctx, &["revoke"], "");
            assert_eq!(code, 0, "{out}");
            let rights = ctx.settings.stored_rights().map_err(|e| e.message())?;
            assert!(!rights.enabled());
        }
        Ok(())
    }

    #[test]
    fn setup_takes_no_argument() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, None)?;
        for word in ["apple", "notes\u{b}", "mail", "calendar"] {
            let (code, out) = run(&ctx, &["setup", word], "");
            assert_eq!(code, 2, "{word:?}");
            assert_eq!(out, "Benutzung: avery mail setup\n", "{word:?}");
        }
        assert!(!std::path::Path::new(&ctx.settings.config_path).exists());
        Ok(())
    }

    #[test]
    fn an_endless_answer_line_is_refused() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, Some(&base()))?;
        let long = format!("{}\n", "1".repeat(20_000));
        let (code, out) = run(&ctx, &["setup"], &long);
        assert_eq!(code, 1);
        assert!(out.contains("Ungueltige Eingabe."));
        Ok(())
    }

    #[test]
    fn revoke_removes_all_grants_and_keeps_the_limits() -> TestResult {
        let dir = TempDir::new()?;
        let config = r#"{"version": 4,
            "mail": [{"id": "A", "name": "A", "level": "write"}],
            "calendars": [{"id": "C", "name": "C", "level": "write"}],
            "sends_per_hour": 3}"#;
        let ctx = real(&dir, Some(config))?;
        let (code, out) = run(&ctx, &["revoke"], "");
        assert_eq!(code, 0);
        assert!(out.contains("Alle Freigaben entfernt"));
        assert!(
            out.contains("Mail und Kalender sind wieder aus: Avery startet sie nicht mehr."),
            "{out}"
        );
        assert_eq!(
            saved(&ctx)?,
            j(r#"{"version": 4, "sends_per_hour": 3, "changes_per_hour": 10}"#)?
        );
        let (code, out) = run(&ctx, &["revoke"], "");
        assert_eq!(code, 0);
        assert!(!out.contains("sind wieder aus"), "{out}");
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
            for (args, input) in [(vec!["setup"], "1\nj\n"), (vec!["revoke"], "")] {
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
        let config = r#"{"version": 4,
            "mail": [{"id": "A", "name": "Privat Gmail", "level": "write"}, {"id": "B", "name": "Outlook", "level": "read"}],
            "calendars": [{"id": "C", "name": "Familie", "level": "read"}],
            "sends_per_hour": 2, "changes_per_hour": 6}"#;
        let ctx = real(&dir, Some(config))?;
        let (code, out) = run(&ctx, &["status"], "");
        assert_eq!(code, 0);
        assert!(
            out.contains("Mail: Privat Gmail (schreiben), Outlook (lesen)"),
            "{out}"
        );
        assert!(out.contains("Kalender: Familie (lesen)"));
        assert!(!out.contains(SETUP_HINT), "{out}");
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
        assert!(out.contains(SETUP_HINT), "{out}");
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
    fn accounts_marks_the_level_of_every_configured_account() -> TestResult {
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
        assert!(!out.contains("Demo Firma"), "{out}");
        assert!(!out.contains(SETUP_HINT), "{out}");
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
    fn calendars_marks_the_level_of_every_configured_calendar() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = context(&dir, true, "FAKE-CAL-PRIVAT", None, FakeOptions::default())?;
        let (code, out) = run(&ctx, &["calendars"], "");
        assert_eq!(code, 0);
        assert!(out.contains("* 1) Demo Privat  [schreiben]"), "{out}");
        assert!(out.contains("* 2) Demo Familie  [lesen]"));
        assert!(out.contains("* 3) Demo Feiertage  [lesen]"));
        assert!(!out.contains("Demo Firma"), "{out}");
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
        let only = config_with(
            r#""calendars": [{"id": "FAKE-CAL-PRIVAT", "name": "Demo Privat", "level": "read"}]"#,
        );
        let ctx = context(&dir, false, "", Some(&only), FakeOptions::default())?;
        let (code, out) = run(&ctx, &["selftest"], "");
        assert_eq!(code, 0, "{out}");
        assert!(!out.contains("Konto 1") && out.contains("Kalender 1"));
        let gone = config_with(r#""calendars": [{"id": "GONE", "name": "Weg", "level": "read"}]"#);
        let ctx = context(&dir, false, "", Some(&gone), FakeOptions::default())?;
        let (code, out) = run(&ctx, &["selftest"], "");
        assert_eq!(code, 1);
        assert!(
            out.contains("FAIL  Kalender 1  Kalender erreichbar: Kalender nicht mehr vorhanden")
        );
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
    fn selftest_write_neither_uses_nor_obeys_the_limits_of_the_model() -> TestResult {
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
        let (code, out) = run(&ctx, &["selftest-write"], "j\nn\n");
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("an Mail uebergeben"), "{out}");
        assert!(fs::read_to_string(dir.path("outbox.jsonl"))?.contains("\"status\": \"sent\""));
        let view = sendlog::read_entries(&ctx.settings, 50)?;
        let human = view
            .entries
            .iter()
            .filter(|entry| entry.by == crate::journal::By::Human)
            .count();
        assert!(human >= 3, "{human}");
        let blocked = sendlog::check_send_limits(&view.entries, now + 10, 5);
        assert!(
            matches!(blocked, Err(ToolError::Msg(m)) if m.starts_with("Send limit reached (5"))
        );
        let fresh = TempDir::new()?;
        let options = FakeOptions {
            approve: "yes".into(),
            outbox: Some(fresh.path("outbox.jsonl")),
            ..FakeOptions::default()
        };
        let clean = context(&fresh, true, "FAKE-GMAIL", None, options)?;
        let (code, out) = run(&clean, &["selftest-write"], "j\nn\n");
        assert_eq!(code, 0, "{out}");
        let after = sendlog::read_entries(&clean.settings, 50)?;
        assert_eq!(
            sendlog::check_send_limits(&after.entries, journal::now_seconds(), 5),
            Ok(())
        );
        assert_eq!(
            sendlog::check_draft_limit(&after.entries, journal::now_seconds()),
            Ok(())
        );
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
            "Befehle: accounts, calendars, onboard, setup, revoke, selftest, sendlog, changes, undo, selftest-write, status"
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
            assert!(out.contains("avery mail setup"));
            assert!(!out.contains("link") && !out.contains("unlink"));
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
                by: crate::journal::By::Model,
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
        let (code, out) = run(&ctx, &["selftest-write"], "j\nj\nj\nj\nj\n");
        assert_eq!(code, 0, "{out}");
        assert!(!out.contains("Freigabedialog funktioniert"), "{out}");
        assert!(out.contains("PASS  Termin angelegt"));
        assert!(out.contains("PASS  Termin geaendert"));
        assert!(out.contains("PASS  Termin geloescht"));
        assert!(out.contains("PASS  Ganztagstermin angelegt"));
        assert!(out.contains("PASS  Ganztags-Ende wie erwartet (00:00 des Folgetags)"));
        assert!(out.contains("PASS  Ganztagstermin geloescht"));
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
                ("create", "pending"),
                ("create", "done"),
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
    fn selftest_write_of_several_calendars_leaves_the_budget_of_the_model_alone() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = context(
            &dir,
            true,
            "FAKE-CAL-PRIVAT,FAKE-CAL-FAMILIE",
            None,
            FakeOptions {
                approve: "yes".to_string(),
                ..FakeOptions::default()
            },
        )?;
        let (code, out) = run(&ctx, &["selftest-write"], &"j\n".repeat(10));
        assert_eq!(code, 0, "{out}");
        assert_eq!(out.matches("PASS  Termin angelegt").count(), 2, "{out}");
        let log = crate::changelog::read_entries(&ctx.settings, 50)?;
        assert_eq!(log.entries.len(), 20);
        assert!(log
            .entries
            .iter()
            .all(|entry| entry.by == crate::journal::By::Human));
        let now = journal::now_seconds();
        assert_eq!(
            crate::changelog::check_limits(&log.entries, now, 10),
            Ok(())
        );
        assert!(crate::changelog::undoable(&log.entries).is_empty());
        let declining = TempDir::new()?;
        let ctx = writer(&declining, "no")?;
        run(&ctx, &["selftest-write"], "j\n");
        let log = crate::changelog::read_entries(&ctx.settings, 50)?;
        assert_eq!(
            crate::changelog::check_limits(&log.entries, journal::now_seconds(), 10),
            Ok(())
        );
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

    #[test]
    fn revoke_works_when_the_lock_file_is_read_only() -> TestResult {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new()?;
        let ctx = real(&dir, Some(&base()))?;
        let lock = ctx.settings.lock_path();
        fs::write(&lock, "")?;
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o400))?;
        let (code, out) = run(&ctx, &["revoke"], "");
        assert_eq!(code, 0, "{out}");
        assert!(!out.contains("nicht sperrbar"), "{out}");
        let rights = ctx.settings.stored_rights().map_err(|e| e.message())?;
        assert!(rights.mail_at(Level::Read).is_empty());
        assert_eq!(fs::metadata(&lock)?.permissions().mode() & 0o777, 0o400);
        Ok(())
    }

    struct Typed<'a> {
        data: Cursor<Vec<u8>>,
        after: Vec<(usize, Box<dyn FnOnce() + 'a>)>,
        lines: usize,
    }

    impl<'a> Typed<'a> {
        fn new(text: &str) -> Typed<'a> {
            Typed {
                data: Cursor::new(text.as_bytes().to_vec()),
                after: Vec::new(),
                lines: 0,
            }
        }

        fn after_line(mut self, line: usize, hook: impl FnOnce() + 'a) -> Typed<'a> {
            self.after.push((line, Box::new(hook)));
            self
        }
    }

    impl Read for Typed<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.data.read(buffer)
        }
    }

    impl BufRead for Typed<'_> {
        fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
            self.data.fill_buf()
        }

        fn consume(&mut self, amount: usize) {
            if let Ok(available) = self.data.fill_buf() {
                let taken = available.get(..amount).unwrap_or(available);
                self.lines += taken.iter().filter(|byte| **byte == b'\n').count();
            }
            self.data.consume(amount);
            while let Some(index) = self.after.iter().position(|(line, _)| *line <= self.lines) {
                let (_, hook) = self.after.remove(index);
                hook();
            }
        }
    }

    fn run_typed(ctx: &Context, args: &[&str], input: &mut Typed) -> (u8, String) {
        let args: Vec<String> = args.iter().map(|argument| argument.to_string()).collect();
        let mut out = Vec::new();
        let code = dispatch(&args, ctx, &dark(), input, &mut out);
        (code, String::from_utf8_lossy(&out).into_owned())
    }

    fn second_terminal(dir: &TempDir) -> Context {
        Context::demo(
            settings_at(&dir.path("config.json")),
            FakeOptions::default(),
        )
    }

    #[test]
    fn setup_does_not_bring_back_grants_that_were_revoked_meanwhile() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, Some(&base()))?;
        let other = second_terminal(&dir);
        let mut input = Typed::new("\n\n\n\n\n\n\nj\n").after_line(8, || {
            let _ = run(&other, &["revoke"], "");
        });
        let (code, out) = run_typed(&ctx, &["setup"], &mut input);
        assert_eq!(code, 1, "{out}");
        assert!(
            out.contains("Die Konfiguration wurde inzwischen geaendert"),
            "{out}"
        );
        let config = saved(&ctx)?;
        assert!(section(&config, "mail").is_none());
        assert!(section(&config, "calendars").is_none());
        Ok(())
    }

    #[test]
    fn every_config_command_gives_up_cleanly_when_another_one_holds_the_lock() -> TestResult {
        for (args, input) in [(vec!["revoke"], ""), (vec!["setup"], "1\n\n\n\n\n\n\nj\n")] {
            let dir = TempDir::new()?;
            let ctx = real(&dir, Some(&base()))?;
            let held = lock_held(&ctx)?;
            let (code, out) = run(&ctx, &args, input);
            assert_eq!(code, 1, "{args:?}: {out}");
            assert!(
                out.contains("Ein anderer Avery-Befehl aendert gerade die Konfiguration")
                    && out.contains("config.lock")
                    && out.contains("Nichts gespeichert"),
                "{args:?}: {out}"
            );
            assert_eq!(fs::read_to_string(&ctx.settings.config_path)?, base());
            held.unlock()?;
            drop(held);
            if args.first() == Some(&"revoke") {
                let (code, _) = run(&ctx, &args, input);
                assert_eq!(code, 0);
            }
        }
        Ok(())
    }

    fn lock_held(ctx: &Context) -> Result<fs::File, Box<dyn std::error::Error>> {
        let held = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(ctx.settings.lock_path())?;
        held.lock()?;
        Ok(held)
    }

    fn nasty_names() -> Vec<String> {
        vec![
            "Free \u{200b} Events".to_string(),
            "Work\t\u{202e}Mail  \u{2066} x".to_string(),
            format!("{} tail words", "L".repeat(119)),
            format!("{} tail", "M".repeat(118)),
            "x ".repeat(100),
            "\u{e9}".repeat(300),
            "\u{200b}\u{200b}".to_string(),
            "\n\r\n\t ".to_string(),
            format!("{}\u{200b}{}", "a".repeat(60), "b".repeat(80)),
            format!("{} \u{200b} {}", "c".repeat(59), "d".repeat(80)),
            "Feiertage \u{e0041}\u{e0042} (Deutschland)".to_string(),
        ]
    }

    #[test]
    fn what_the_sanitiser_makes_of_names_always_passes_the_config_validator() -> TestResult {
        for (index, name) in nasty_names().into_iter().enumerate() {
            let account = sanitized(MailAccount {
                id: format!("ACC-{index}"),
                name: name.clone(),
                kind: format!("imap {name}"),
                addresses: vec![name.clone(), format!("me \u{200b} {index}@example.org")],
            });
            let info = sanitized_calendar(CalendarInfo {
                id: format!("CAL-{index}"),
                name: name.clone(),
                writable: true,
            });
            let calendar = CalendarEntry {
                id: info.id,
                name: info.name,
            };
            let mut rights = Rights::empty();
            rights.mail.push((account.clone(), Level::Read));
            rights.calendars.push((calendar, Level::Read));
            let parsed = Rights::parse(&rights.to_config())
                .map_err(|what| format!("{name:?} was saved but is refused: {what}"))?;
            assert_eq!(parsed.mail[0].0.addresses, account.addresses);
            assert_eq!(parsed.mail[0].0.kind, account.kind);
            assert_eq!(
                one_line_str(&account.name, MAX_NAME),
                account.name,
                "{name:?}"
            );
            assert_eq!(parsed, rights, "{name:?}");
        }
        Ok(())
    }

    #[test]
    fn an_internal_error_of_the_config_writer_is_reported_with_the_reason() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = second_terminal(&dir);
        let mut out = Vec::new();
        let mut input = Cursor::new(Vec::new());
        let code = update_failed(
            &ctx,
            &mut Console::terminal(&mut input, &mut out),
            &UpdateError::Invalid("mail[0].name must be one line".to_string()),
        );
        let text = String::from_utf8_lossy(&out).into_owned();
        assert_eq!(code, 1);
        assert!(
            text.contains("Interner Fehler: Die neue Konfiguration waere ungueltig (mail[0].name must be one line). Nichts gespeichert."),
            "{text}"
        );
        assert!(!std::path::Path::new(&ctx.settings.config_path).exists());
        Ok(())
    }

    #[test]
    fn nasty_names_never_lock_the_config() -> TestResult {
        let mut named: Vec<(String, String)> = nasty_names()
            .into_iter()
            .enumerate()
            .map(|(index, name)| (format!("ACC-{index}"), name))
            .collect();
        named.push((" padded id ".to_string(), String::new()));
        named.push(("W".repeat(300), "\u{200b}".to_string()));
        let listing = crate::testutil::Listing {
            accounts: named
                .iter()
                .enumerate()
                .map(|(index, (id, name))| MailAccount {
                    id: id.clone(),
                    name: name.clone(),
                    kind: "imap".to_string(),
                    addresses: vec![format!("me{index}@example.org")],
                })
                .collect(),
            calendars: named
                .iter()
                .map(|(id, name)| CalendarInfo {
                    id: id.replace("ACC-", "CAL-"),
                    name: name.clone(),
                    writable: true,
                })
                .collect(),
        };
        let dir = TempDir::new()?;
        let ctx = Context::new(
            settings_at(&dir.path("config.json")),
            Box::new(listing.clone()),
            Box::new(listing),
        );
        let count = named.len();
        let (code, out) = run(&ctx, &["setup"], &format!("{}j\n", "1\n".repeat(2 * count)));
        assert_eq!(code, 0, "{out}");
        let rights = ctx.settings.stored_rights().map_err(|e| e.message())?;
        assert_eq!(rights.mail.len(), count);
        assert_eq!(rights.calendars.len(), count);
        let stored = fs::read_to_string(&ctx.settings.config_path)?;
        for hidden in ["\\u200b", "\\u202e", "\\u2066", "\\t", "\\n"] {
            assert!(!stored.contains(hidden), "{hidden}: {stored}");
        }
        for (calendar, _) in &rights.calendars {
            assert_eq!(one_line_str(&calendar.name, MAX_NAME), calendar.name);
        }
        let (code, out) = run(&ctx, &["setup"], &"\n".repeat(2 * count));
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("Unveraendert."), "{out}");
        for command in ["status", "accounts", "calendars"] {
            let (code, out) = run(&ctx, &[command], "");
            assert_eq!(code, 0, "{command}: {out}");
        }
        let (code, out) = run(&ctx, &["revoke"], "");
        assert_eq!(code, 0, "{out}");
        assert!(ctx.settings.stored_rights().is_ok());
        Ok(())
    }

    #[test]
    fn the_config_commands_refuse_in_demo_mode_and_touch_nothing() -> TestResult {
        for (args, input) in [(vec!["setup"], "1\n1\n1\nj\n"), (vec!["revoke"], "")] {
            let dir = TempDir::new()?;
            let mut ctx = real(&dir, Some(&base()))?;
            ctx.settings.fake = true;
            let (code, out) = run(&ctx, &args, input);
            assert_eq!(code, 1, "{args:?}: {out}");
            assert!(
                out.contains("Im Demo-Modus (MAIL_MCP_FAKE=1) nicht moeglich"),
                "{args:?}: {out}"
            );
            assert!(!out.contains("[j/N]"), "{args:?}: {out}");
            assert_eq!(fs::read_to_string(&ctx.settings.config_path)?, base());
            assert!(!std::path::Path::new(&ctx.settings.lock_path()).exists());
        }
        let (code, out) = run(&real(&TempDir::new()?, None)?, &["setup", "nonsense"], "");
        assert_eq!(code, 2, "{out}");
        Ok(())
    }
}
