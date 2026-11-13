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
use crate::console::{ask_levels, failed, level_word, Choice, Console};
use crate::context::Context;
use crate::error::{ToolError, ToolResult};
use crate::localtime;
use crate::provider::{calendar_origin, Client, Provider};
use crate::recurrence;
use crate::rights::{
    name_or_id, Apple, CalendarGrant, Level, Refusal, RemoteAccount, Rights, MAX_ACCOUNTS,
    MAX_APPLE, MAX_NAME,
};
use crate::sendlog::{self, Outcome};
use crate::server;
use crate::text::{bracketed, clean_str, one_line_str};
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

fn link_hint() -> String {
    format!(
        "{} verbinden: avery mail link {}",
        Provider::remote_names(" oder "),
        Provider::remote_ids(" | ")
    )
}

fn apple_command() -> String {
    format!("avery mail setup {}", Provider::Apple.as_str())
}

fn apple_hint() -> String {
    format!(
        "{} einbeziehen (optional): {}",
        Provider::Apple.display(),
        apple_command()
    )
}

fn unlink_forms() -> String {
    Provider::remote()
        .map(|provider| format!("{}:<adresse>", provider.as_str()))
        .collect::<Vec<_>>()
        .join(" | ")
}

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
        console.say("Im Demo-Modus (MAIL_MCP_FAKE=1) nicht moeglich: Verbinden, Trennen, Einrichten und Widerrufen aendern die echte Konfiguration.");
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
        UpdateError::Refused(Refusal::Changed) => console.say(
            "Die Konfiguration wurde inzwischen geaendert (anderes Terminal?). Nichts gespeichert, den Befehl bitte erneut ausfuehren.",
        ),
        UpdateError::Refused(Refusal::Full) => console.say(&format!(
            "Es sind schon {MAX_ACCOUNTS} Konten verbunden. Zuerst eines trennen: avery mail unlink <adresse>"
        )),
        UpdateError::Refused(Refusal::Taken { address, provider }) if *provider == Provider::Apple => {
            console.say(&format!(
                "Die Adresse {address} ist schon ueber {} eingebunden. Eine Adresse kann nur einmal verwendet werden. Zuerst dort auf Stufe 0 stellen ({}), dann erneut verbinden.",
                provider.display(),
                apple_command()
            ))
        }
        UpdateError::Refused(Refusal::Taken { address, provider }) => console.say(&format!(
            "Die Adresse {address} ist schon bei {} verbunden. Eine Adresse kann nur einmal verbunden werden. Zuerst trennen: avery mail unlink {}:{address}",
            provider.display(),
            provider.as_str()
        )),
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
        .mail_entries()
        .into_iter()
        .map(|(account, level)| (account.name, level))
        .collect()
}

fn calendar_grants(rights: &Rights) -> Vec<(String, Level)> {
    rights
        .calendar_entries()
        .into_iter()
        .map(|(calendar, level)| (calendar.name, level))
        .collect()
}

fn link_note(ctx: &Context, account: &RemoteAccount) -> &'static str {
    if ctx
        .linker
        .capability(account.provider(), account.address())
        .is_some()
    {
        ""
    } else {
        "  [Anmeldung fehlt, neu verbinden mit: avery mail link]"
    }
}

fn cmd_accounts(ctx: &Context, console: &mut Console) -> u8 {
    let rights = match ctx.settings.rights() {
        Ok(rights) => rights,
        Err(error) => return failed(console, &error),
    };
    if rights.accounts.is_empty() && rights.apple.mail.is_empty() {
        console.say("Noch kein Konto verbunden.");
        console.say(&link_hint());
        if !rights.apple.enabled() {
            console.say(&apple_hint());
        }
        return 0;
    }
    let mut number = 0usize;
    for account in &rights.accounts {
        number += 1;
        let mark = if account.mail.is_some() { '*' } else { ' ' };
        console.say(&format!(
            "{mark} {number}) {}  [{}]{}",
            describe(&account.mail_account()),
            level_word(account.mail),
            link_note(ctx, account)
        ));
    }
    for (account, level) in &rights.apple.mail {
        number += 1;
        console.say(&format!(
            "* {number}) {}  [{}]",
            describe(account),
            level_word(Some(*level))
        ));
    }
    console.say("\n* = fuer den Agenten freigegeben, Stufe in Klammern: lesen oder schreiben");
    console.say("(schreiben = Entwuerfe und Senden, jede Mail nur nach deinem Klick)");
    if !rights.apple.enabled() {
        console.say(&apple_hint());
    }
    0
}

fn calendar_name(name: &str, address: &str) -> String {
    let name = one_line_str(name, 80);
    if name.to_lowercase().contains(address) {
        name
    } else {
        one_line_str(&format!("{name} ({address})"), 120)
    }
}

struct Listed {
    info: CalendarInfo,
    write_capable: bool,
}

fn list_account_calendars(ctx: &Context, account: &RemoteAccount) -> ToolResult<Vec<Listed>> {
    let found = ctx
        .calendar
        .calendars(account.provider(), account.address())?;
    let write_capable = ctx
        .linker
        .capability(account.provider(), account.address())
        .is_some_and(|capability| capability.write_calendar);
    Ok(found
        .into_iter()
        .map(|info| Listed {
            info: CalendarInfo {
                name: calendar_name(&info.name, account.address()),
                ..info
            },
            write_capable,
        })
        .collect())
}

fn calendar_line(
    console: &mut Console,
    number: usize,
    name: &str,
    writable: bool,
    level: Option<Level>,
) {
    let mark = if level.is_some() { '*' } else { ' ' };
    console.say(&format!(
        "{mark} {number}) {}{}  [{}]",
        one_line_str(name, 120),
        if writable { "" } else { " [schreibgeschuetzt]" },
        level_word(level)
    ));
}

fn cmd_calendars(ctx: &Context, console: &mut Console) -> u8 {
    let rights = match ctx.settings.rights() {
        Ok(rights) => rights,
        Err(error) => return failed(console, &error),
    };
    if rights.accounts.is_empty() && rights.apple.calendars.is_empty() {
        console.say("Noch kein Kalender verfuegbar.");
        console.say(&link_hint());
        if !rights.apple.enabled() {
            console.say(&apple_hint());
        }
        return 0;
    }
    let mut number = 0usize;
    let mut unreachable = 0usize;
    for account in &rights.accounts {
        match list_account_calendars(ctx, account) {
            Ok(listed) => {
                for entry in &listed {
                    number += 1;
                    let level = account
                        .calendars
                        .iter()
                        .find(|grant| grant.id == entry.info.id)
                        .map(|grant| grant.level);
                    calendar_line(
                        console,
                        number,
                        &entry.info.name,
                        entry.info.writable,
                        level,
                    );
                }
            }
            Err(error) => {
                unreachable += 1;
                console.say(&format!(
                    "Hinweis: Kalender von {} nicht lesbar ({error})",
                    account.address()
                ));
            }
        }
    }
    for (calendar, level) in &rights.apple.calendars {
        number += 1;
        calendar_line(console, number, &calendar.name, true, Some(*level));
    }
    if number == 0 {
        console.say("Keine Kalender gefunden.");
        return u8::from(unreachable > 0);
    }
    console.say("\n* = fuer den Agenten freigegeben, Stufe in Klammern: lesen oder schreiben");
    console.say(
        "(schreiben = Termine anlegen, aendern, loeschen, jede Aenderung nur nach deinem Klick)",
    );
    if !rights.apple.enabled() {
        console.say(&apple_hint());
    }
    0
}

fn finish(ctx: &Context, console: &mut Console, before: &Rights, rights: &Rights) -> u8 {
    if rights == before {
        console.say("Unveraendert.");
        return 0;
    }
    console.say("\nNeue Freigaben:");
    console.say(&format!("  Mail:     {}", granted(&mail_grants(rights))));
    console.say(&format!(
        "  Kalender: {}",
        granted(&calendar_grants(rights))
    ));
    if !console.confirm("Korrekt? [j/N] ") {
        console.say("Abgebrochen.");
        return 1;
    }
    match ctx
        .settings
        .update_rights(|fresh| fresh.apply(before, rights))
    {
        Ok(()) => {
            console.say(&format!("Gespeichert in {}", ctx.settings.config_path));
            console.say("Goose danach neu starten. Pruefen mit: avery mail selftest (lesen) und avery mail selftest-write (schreiben)");
            0
        }
        Err(error) => update_failed(ctx, console, &error),
    }
}

fn explain_levels(ctx: &Context, console: &mut Console, rights: &Rights) {
    console.say("Nur PRIVATE Konten und Kalender freigeben, niemals geschaeftliche.");
    console
        .say("Stufen: 0 = aus, 1 = lesen, 2 = schreiben (Entwuerfe, Senden und Termine aendern,");
    console.say("jedes Mal erst nach deinem Klick in einem macOS-Dialog; der Agent kann ihn nicht bedienen).");
    console.say(&format!(
        "Mail-Grenzen: Klartext, kein Bcc, keine Anhaenge, max. {MAX_RECIPIENTS} Empfaenger, max. {MAX_SEND_CHARS} Zeichen pro gesendeter Mail,"
    ));
    console.say(&format!(
        "         max. {MAX_SEND_LINES} Zeilen, max. {} gesendete Mails und max. {MAX_DRAFTS_PER_HOUR} Entwuerfe pro Stunde. Jeder Versuch steht in {}",
        rights.sends_per_hour,
        ctx.settings.sendlog_path()
    ));
    console.say(&format!(
        "Kalender-Grenzen: nur einzelne Termine ohne Teilnehmer und ohne Wiederholung, max. {} Aenderungen pro Stunde. Jeder Versuch steht in {}",
        rights.changes_per_hour,
        ctx.settings.changes_path()
    ));
}

fn setup_account(
    ctx: &Context,
    console: &mut Console,
    account: &mut RemoteAccount,
    listing: ToolResult<Vec<Listed>>,
) -> Result<(), u8> {
    console.say(&format!(
        "\n{}: {}",
        account.provider().display(),
        account.address()
    ));
    let capability = ctx.linker.capability(account.provider(), account.address());
    let mail_highest = if capability.is_some_and(|found| found.write_mail) {
        Level::Write
    } else {
        Level::Read
    };
    let mut choices = vec![Choice {
        title: format!("Mail{}", link_note(ctx, account)),
        current: account.mail.map(|level| level.min(mail_highest)),
        highest: mail_highest,
    }];
    let listed = match listing {
        Ok(listed) => Some(listed),
        Err(error) => {
            console.say(&format!(
                "Hinweis: Kalender von {} nicht lesbar ({error}); deren Freigaben bleiben unveraendert.",
                account.address()
            ));
            None
        }
    };
    if let Some(listed) = &listed {
        choices.extend(listed.iter().map(|entry| {
            let writable = entry.info.writable && entry.write_capable;
            let highest = if writable { Level::Write } else { Level::Read };
            Choice {
                title: if writable {
                    one_line_str(&entry.info.name, 120)
                } else {
                    format!(
                        "{} [schreibgeschuetzt]",
                        one_line_str(&entry.info.name, 120)
                    )
                },
                current: account
                    .calendars
                    .iter()
                    .find(|grant| grant.id == entry.info.id)
                    .map(|grant| grant.level.min(highest)),
                highest,
            }
        }));
    }
    let mut levels = ask_levels(console, &choices)?.into_iter();
    account.mail = levels.next().flatten();
    if let Some(listed) = listed {
        let gone: Vec<String> = account
            .calendars
            .iter()
            .filter(|grant| !listed.iter().any(|entry| entry.info.id == grant.id))
            .map(|grant| grant.name.clone())
            .collect();
        if !gone.is_empty() {
            console.say(&format!(
                "Nicht mehr vorhanden, wird entfernt: {}",
                gone.join(", ")
            ));
        }
        account.calendars = listed
            .into_iter()
            .zip(levels)
            .filter_map(|(entry, level)| {
                level.map(|level| CalendarGrant {
                    id: entry.info.id,
                    name: entry.info.name,
                    level,
                })
            })
            .collect();
    }
    Ok(())
}

fn setup_apple_entries(console: &mut Console, apple: &mut Apple) -> Result<(), u8> {
    console.say(&format!(
        "\n{} (einbezogen; neu auflisten: {})",
        Provider::Apple.display(),
        apple_command()
    ));
    let choices: Vec<Choice> = apple
        .mail
        .iter()
        .map(|(account, level)| Choice {
            title: describe(account),
            current: Some(*level),
            highest: Level::Write,
        })
        .chain(apple.calendars.iter().map(|(calendar, level)| Choice {
            title: one_line_str(&calendar.name, 120),
            current: Some(*level),
            highest: Level::Write,
        }))
        .collect();
    let mut levels = ask_levels(console, &choices)?.into_iter();
    apple.mail = apple
        .mail
        .drain(..)
        .zip(levels.by_ref())
        .filter_map(|((account, _), level)| level.map(|level| (account, level)))
        .collect();
    apple.calendars = apple
        .calendars
        .drain(..)
        .zip(levels)
        .filter_map(|((calendar, _), level)| level.map(|level| (calendar, level)))
        .collect();
    Ok(())
}

fn setup_accounts(ctx: &Context, console: &mut Console) -> u8 {
    if !editable(ctx, console) {
        return 1;
    }
    let mut rights = match ctx.settings.stored_rights() {
        Ok(rights) => rights,
        Err(error) => return failed(console, &error),
    };
    let before = rights.clone();
    if rights.accounts.is_empty() && !rights.apple.enabled() {
        console.say("Noch kein Konto verbunden, es gibt nichts einzurichten.");
        console.say(&link_hint());
        console.say(&apple_hint());
        return 1;
    }
    explain_levels(ctx, console, &rights);
    if !rights.accounts.is_empty() {
        console.say("Stufe 0 entzieht nur die Freigabe, das Konto bleibt verbunden (trennen: avery mail unlink <adresse>).");
    }
    let listings: Vec<ToolResult<Vec<Listed>>> = rights
        .accounts
        .iter()
        .map(|account| list_account_calendars(ctx, account))
        .collect();
    for (account, listing) in rights.accounts.iter_mut().zip(listings) {
        if let Err(code) = setup_account(ctx, console, account, listing) {
            return code;
        }
    }
    if rights.apple.enabled() {
        if let Err(code) = setup_apple_entries(console, &mut rights.apple) {
            return code;
        }
    }
    let code = finish(ctx, console, &before, &rights);
    if code == 0 && !rights.apple.enabled() {
        console.say(&apple_hint());
    }
    code
}

fn unique<T>(items: Vec<T>, id: impl Fn(&T) -> &str) -> Vec<T> {
    let mut kept: Vec<T> = Vec::new();
    for item in items {
        let name = id(&item);
        if !name.is_empty()
            && Provider::is_apple_id(name)
            && !kept.iter().any(|known| id(known) == name)
        {
            kept.push(item);
        }
    }
    kept
}

fn setup_apple(ctx: &Context, console: &mut Console) -> u8 {
    if !editable(ctx, console) {
        return 1;
    }
    let mut rights = match ctx.settings.stored_rights() {
        Ok(rights) => rights,
        Err(error) => return failed(console, &error),
    };
    let before = rights.clone();
    console.say(&format!(
        "{} einbeziehen (optional). Dazu startet Avery Mail und Kalender;",
        Provider::Apple.display()
    ));
    console.say("macOS fragt beim ersten Mal nach der Automation-Berechtigung. Avery braucht dafuer keine Anmeldung.");
    let mail = match ctx.backend.all_accounts() {
        Ok(found) => unique(found.into_iter().map(sanitized).collect(), |account| {
            account.id.as_str()
        }),
        Err(error) => {
            console.say(&format!("Fehler: Apple Mail ist nicht lesbar ({error})"));
            return 1;
        }
    };
    let calendars = match ctx.calendar.calendars(Provider::Apple, "") {
        Ok(found) => unique(
            found.into_iter().map(sanitized_calendar).collect(),
            |info| info.id.as_str(),
        ),
        Err(error) => {
            console.say(&format!(
                "Fehler: Apple Kalender ist nicht lesbar ({error})"
            ));
            return 1;
        }
    };
    if mail.len() > MAX_APPLE || calendars.len() > MAX_APPLE {
        console.say(&format!(
            "Fehler: Mail oder Kalender melden mehr als {MAX_APPLE} Eintraege."
        ));
        return 1;
    }
    let (mail, shared): (Vec<MailAccount>, Vec<MailAccount>) = mail
        .into_iter()
        .partition(|account| rights.shared_address(&account.addresses).is_none());
    explain_levels(ctx, console, &rights);
    for account in &shared {
        if let Some(address) = rights.shared_address(&account.addresses) {
            console.say(&format!(
                "{} ({address}): schon direkt verbunden, bleibt hier aus (zum Wechseln zuerst: avery mail unlink {address}).",
                one_line_str(&account.name, MAX_NAME)
            ));
        }
    }
    let choices: Vec<Choice> = mail
        .iter()
        .map(|account| Choice {
            title: describe(account),
            current: rights
                .apple
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
                    .apple
                    .calendars
                    .iter()
                    .find(|(known, _)| known.id == info.id)
                    .map(|(_, level)| (*level).min(highest)),
                highest,
            }
        }))
        .collect();
    let levels = match ask_levels(console, &choices) {
        Ok(levels) => levels,
        Err(code) => return code,
    };
    let mut levels = levels.into_iter();
    let gone: Vec<String> = rights
        .apple
        .mail
        .iter()
        .map(|(known, _)| (known.id.as_str(), known.name.as_str()))
        .chain(
            rights
                .apple
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
        console.say(&format!(
            "Nicht mehr vorhanden, wird entfernt: {}",
            gone.join(", ")
        ));
    }
    let apple = Apple {
        mail: mail
            .into_iter()
            .zip(levels.by_ref())
            .filter_map(|(account, level)| level.map(|level| (account, level)))
            .collect(),
        calendars: calendars
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
            .collect(),
    };
    rights.apple = apple;
    let code = finish(ctx, console, &before, &rights);
    if code == 0 && before.apple.enabled() && !rights.apple.enabled() {
        console.say(&format!(
            "{} sind wieder aus: Avery startet sie nicht mehr.",
            Provider::Apple.display()
        ));
    }
    code
}

fn cmd_setup(ctx: &Context, console: &mut Console, word: Option<&str>) -> u8 {
    match word {
        None => setup_accounts(ctx, console),
        Some(word) if word == Provider::Apple.as_str() => setup_apple(ctx, console),
        Some(other) => {
            console.say(&format!(
                "Unbekannter Bereich: {} (nur: {})",
                clean_str(other, Some(30)),
                Provider::Apple.as_str()
            ));
            2
        }
    }
}

fn cmd_revoke(ctx: &Context, console: &mut Console) -> u8 {
    if !editable(ctx, console) {
        return 1;
    }
    let revoked = ctx.settings.update_rights(|rights| {
        let had_apple = rights.apple.enabled();
        rights.revoke();
        Ok((had_apple, !rights.accounts.is_empty()))
    });
    let (had_apple, linked) = match revoked {
        Ok(done) => done,
        Err(error) => return update_failed(ctx, console, &error),
    };
    console.say("Alle Freigaben entfernt (Mail und Kalender, auch Schreiben). Der Agent sieht keine Konten und Kalender mehr.");
    if linked {
        console.say(&format!(
            "Direkt verbundene Konten ({}) bleiben verbunden; ihre Anmeldung liegt im Schluesselbund.",
            Provider::remote_names("/")
        ));
        console.say("Verbindung ganz loesen: avery mail unlink <adresse>");
    }
    if had_apple {
        console.say(&format!(
            "{} sind wieder aus. Wieder einbeziehen: {}",
            Provider::Apple.display(),
            apple_command()
        ));
    }
    0
}

fn prompt_level(console: &mut Console) -> Option<bool> {
    let answer = console.ask("Stufe: 1 = lesen, 2 = schreiben (Entwuerfe, Senden, Termine aendern; jedes Mal mit Klick) [1]: ")?;
    match answer.trim() {
        "" | "1" => Some(false),
        "2" => Some(true),
        _ => None,
    }
}

fn abandon(ctx: &Context, console: &mut Console, provider: Provider, address: &str) {
    match ctx.linker.unlink(provider, address) {
        Ok(()) => {
            console.say("Die eben im Schluesselbund gespeicherte Anmeldung wurde wieder entfernt.")
        }
        Err(error) => {
            console.say(&format!(
                "Die eben im Schluesselbund gespeicherte Anmeldung blieb: {error}"
            ));
            console.say(&format!(
                "Entfernen mit: avery mail unlink {}:{address}",
                provider.as_str()
            ));
        }
    }
}

fn cmd_link(ctx: &Context, console: &mut Console, provider: Option<&str>) -> u8 {
    if !editable(ctx, console) {
        return 1;
    }
    let Some(provider) = provider.and_then(Provider::parse_remote) else {
        console.say(&format!(
            "Benutzung: avery mail link {}",
            Provider::remote_ids(" | ")
        ));
        return 2;
    };
    let Some(oauth) = provider.oauth() else {
        console.say("Dieser Anbieter braucht keine Anmeldung.");
        return 2;
    };
    let rights = match ctx.settings.stored_rights() {
        Ok(rights) => rights,
        Err(error) => return failed(console, &error),
    };
    if rights.accounts.len() >= MAX_ACCOUNTS {
        console.say(&format!(
            "Hinweis: Es sind schon {MAX_ACCOUNTS} Konten verbunden. Erneut verbinden geht nur fuer ein schon verbundenes Konto, ein neues wird abgelehnt."
        ));
    }
    for line in oauth.registration {
        console.say(line);
    }
    console.say(&format!(
        "Details: README, Abschnitt '{}'.",
        Provider::remote_names(" and ")
    ));
    console.say("Avery speichert die Anmeldung im macOS-Schluesselbund (Dienst avery-mail), nie in einer Datei.");
    console.say(
        "Der Agent bekommt nur das, was du danach mit 'avery mail setup' oder hier freigibst.",
    );
    let Some(id) = console.ask("Client-ID: ") else {
        console.say("Ungueltige Eingabe.");
        return 1;
    };
    let secret = if oauth.secret {
        let Some(secret) = console.ask("Client-Secret: ") else {
            console.say("Ungueltige Eingabe.");
            return 1;
        };
        secret
    } else {
        String::new()
    };
    let client = match Client::new(provider, &id, &secret) {
        Ok(client) => client,
        Err(error) => {
            console.say(&format!("Fehler: {error}"));
            return 1;
        }
    };
    let Some(write) = prompt_level(console) else {
        console.say("Ungueltige Eingabe.");
        return 1;
    };
    console.say("\nDer Browser oeffnet sich. Melde dich an und erlaube den Zugriff. Falls nicht, oeffne diese Adresse:");
    let mut announce = |url: &str| console.say(&format!("  {url}"));
    let linked = match ctx.linker.link(provider, &client, write, &mut announce) {
        Ok(linked) => linked,
        Err(error) => {
            console.say(&format!("Fehler: {error}"));
            return 1;
        }
    };
    let Some(mut fresh) = RemoteAccount::new(provider, &linked.address) else {
        console.say("Fehler: Die Adresse des Kontos ist nicht verwendbar.");
        abandon(ctx, console, provider, &linked.address);
        return 1;
    };
    fresh.mail = Some(if write && linked.write_mail {
        Level::Write
    } else {
        Level::Read
    });
    let saved = ctx.settings.update_rights(|rights| {
        if let Some(known) = rights.account_mut(provider, &linked.address) {
            known.lower(linked.write_mail, linked.write_calendar);
            return Ok((true, known.mail));
        }
        let level = fresh.mail;
        rights.add_account(fresh)?;
        Ok((false, level))
    });
    let (relinked, level) = match saved {
        Ok(done) => done,
        Err(error) => {
            let code = update_failed(ctx, console, &error);
            abandon(ctx, console, provider, &linked.address);
            return code;
        }
    };
    console.say(&format!(
        "\n{}: {} ({}). Mail: {}.",
        if relinked {
            "Erneut verbunden"
        } else {
            "Verbunden"
        },
        linked.address,
        provider.kind(),
        level_word(level)
    ));
    if relinked {
        console.say("Freigaben bleiben wie sie waren, hoechstens abgesenkt auf das, was die neue Anmeldung erlaubt.");
    }
    console.say("Kalender freigeben: avery mail setup. Pruefen: avery mail selftest");
    console.say("Goose danach neu starten.");
    0
}

fn single_account(
    console: &mut Console,
    rights: &Rights,
    wanted: &str,
) -> Result<(Provider, String), u8> {
    let hit = rights
        .accounts
        .iter()
        .find(|account| !wanted.is_empty() && account.address() == wanted);
    if let Some(account) = hit {
        return Ok((account.provider(), account.address().to_string()));
    }
    if rights.accounts.is_empty() {
        console.say("Es ist kein Konto direkt verbunden.");
    } else {
        console.say("Benutzung: avery mail unlink <adresse>. Verbunden:");
        for account in &rights.accounts {
            console.say(&format!(
                "  {} ({})",
                account.address(),
                account.provider().kind()
            ));
        }
    }
    console.say(&format!(
        "Eine Anmeldung ohne Eintrag in der Konfiguration (etwa nach einem abgebrochenen Verbinden) entfernt: avery mail unlink {}",
        unlink_forms()
    ));
    Err(1)
}

fn cmd_unlink(ctx: &Context, console: &mut Console, wanted: Option<&str>) -> u8 {
    if !editable(ctx, console) {
        return 1;
    }
    let wanted = wanted
        .map(|text| text.trim().to_lowercase())
        .unwrap_or_default();
    let named = wanted.split_once(':').and_then(|(name, address)| {
        let provider = Provider::parse_remote(name)?;
        RemoteAccount::new(provider, address).map(|_| (provider, address.to_string()))
    });
    let snapshot = ctx.settings.stored_rights();
    let (provider, address) = match (named, &snapshot) {
        (Some(target), _) => target,
        (None, Err(error)) => return failed(console, error),
        (None, Ok(rights)) => match single_account(console, rights, &wanted) {
            Ok(target) => target,
            Err(code) => return code,
        },
    };
    let configured = snapshot
        .as_ref()
        .is_ok_and(|rights| rights.account(provider, &address).is_some());
    let held = ctx.linker.stored(provider, &address);
    if !configured && !held {
        if let Err(error) = &snapshot {
            return failed(console, error);
        }
        console.say(&format!(
            "{}:{address} ist nicht verbunden: weder ein Eintrag in der Konfiguration noch eine gespeicherte Anmeldung. Nichts geaendert.",
            provider.as_str()
        ));
        return 1;
    }
    let question = if configured {
        format!(
            "{address} trennen? Freigaben und die gespeicherte Anmeldung werden geloescht. [j/N] "
        )
    } else {
        format!(
            "{}:{address} steht nicht in der Konfiguration, aber seine Anmeldung liegt noch im Schluesselbund. Anmeldung loeschen? [j/N] ",
            provider.as_str()
        )
    };
    if !console.confirm(&question) {
        console.say("Abgebrochen.");
        return 1;
    }
    if let Err(error) = ctx.linker.unlink(provider, &address) {
        console.say(&format!(
            "Fehler: {error} Nichts geaendert, die Konfiguration blieb wie sie war."
        ));
        console.say("Im Programm 'Schluesselbundverwaltung' den Eintrag 'avery-mail' loeschen.");
        return 1;
    }
    let removed = ctx
        .settings
        .update_rights(|rights| Ok(rights.remove_account(provider, &address)));
    let removed = match removed {
        Ok(removed) => removed,
        Err(error) => {
            console.say(&format!(
                "{address}: die Anmeldung im Schluesselbund ist geloescht, die Konfiguration aber nicht geaendert."
            ));
            let code = update_failed(ctx, console, &error);
            console.say(&format!(
                "Nach der Behebung den Befehl wiederholen: avery mail unlink {}:{address}",
                provider.as_str()
            ));
            return code;
        }
    };
    match (removed, held) {
        (true, true) => console.say(&format!("{address} getrennt.")),
        (true, false) => console.say(&format!(
            "{address} getrennt (die Anmeldung im Schluesselbund fehlte schon)."
        )),
        (false, true) => console.say(&format!(
            "{}:{address} stand nicht in der Konfiguration; die gespeicherte Anmeldung im Schluesselbund ist geloescht.",
            provider.as_str()
        )),
        (false, false) => console.say(&format!("{address} war schon getrennt.")),
    }
    if let Some(url) = provider.consent_url() {
        console.say(&format!("Zugriff der App auch im Konto widerrufen: {url}"));
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
        console.say(&format!(
            "Dieser Kalender ist nicht mehr zum Schreiben freigegeben. Zuerst: avery mail setup (Apple Kalender: {})",
            apple_command()
        ));
        return 1;
    }
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
    if !rights.accounts.is_empty() {
        let linked: Vec<String> = rights
            .accounts
            .iter()
            .map(|account| format!("{} ({})", account.address(), account.provider().kind()))
            .collect();
        console.say(&format!("Direkt verbunden: {}", linked.join(", ")));
    }
    if rights.apple.enabled() {
        console.say(&format!(
            "{}: an ({} Konten, {} Kalender)",
            Provider::Apple.display(),
            rights.apple.mail.len(),
            rights.apple.calendars.len()
        ));
    } else {
        console.say(&format!(
            "{}: aus. {}",
            Provider::Apple.display(),
            apple_hint()
        ));
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
        let (provider, address, own) = calendar_origin(&calendar.id)
            .ok_or_else(|| ToolError::msg("Kalender-ID ist ungueltig"))?;
        let known = ctx.calendar.calendars(provider, address)?;
        let info = known
            .iter()
            .find(|info| info.id == own)
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
    if !Provider::of_id(&calendar.id).expands_recurrence_locally() {
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
        "Ganztaegigen Test-Termin anlegen? Das prueft, wie Calendar das Ende von Ganztagsterminen speichert. [j/N] ",
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
    if console.confirm("Ganztaegigen Test-Termin loeschen? Der Freigabedialog erscheint. [j/N] ") {
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
        "link" => cmd_link(ctx, &mut console, args.get(1).map(String::as_str)),
        "unlink" => cmd_unlink(ctx, &mut console, args.get(1).map(String::as_str)),
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
    use crate::calendar::{CalendarBackend, CalendarInfo};
    use crate::error::ToolResult;
    use crate::fake::FakeOptions;
    use crate::journal;
    use crate::json::Json;
    use crate::provider::Linker;
    use crate::testutil::{gm, j, log_entry, settings_at, TempDir, TestResult, Untouchable};
    use std::fs;
    use std::io::{BufRead, Cursor, Read};

    const ACCOUNT_1: &str = r#"{"id": "ACC-1", "name": "Privat Gmail", "type": "imap", "addresses": ["me@gmail.example"], "level": "read"}"#;
    const ACCOUNT_2: &str = r#"{"id": "ACC-2", "name": "Outlook", "type": "imap", "addresses": ["me@gmail.example"], "level": "read"}"#;

    fn apple_only(entries: &str) -> String {
        format!(r#"{{"version": 3, "apple": {{{entries}}}}}"#)
    }

    fn base() -> String {
        apple_only(&format!(r#""mail": [{ACCOUNT_1}, {ACCOUNT_2}]"#))
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

    #[test]
    fn a_broken_config_is_reported_before_mail_or_calendar_is_touched() -> TestResult {
        for config in [
            "{\"version\": 2, \"allowed_accounts\": []}",
            "{\"version\": 3, \"allowed_accounts\": []}",
            "{\"version\": 3, \"apple\": 5}",
            "{\"version\": 3, \"mail\": {\"accounts\": []}}",
            "not json",
            "[]",
            "{\"allowed_accounts\": []}",
        ] {
            let dir = TempDir::new()?;
            let mut settings = settings_at(&dir.path("config.json"));
            fs::write(&settings.config_path, config)?;
            settings.fake = false;
            let apple = Untouchable::default();
            let ctx = Context::new(
                settings,
                Box::new(apple.clone()),
                Box::new(apple.clone()),
                Linker::inert(),
            );
            for args in [
                vec!["accounts"],
                vec!["calendars"],
                vec!["setup"],
                vec!["setup", "apple"],
                vec!["revoke"],
                vec!["status"],
                vec!["selftest"],
                vec!["selftest-write"],
                vec!["unlink", "alice@gmail.example"],
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
    fn setup_apple_asks_a_level_per_item_and_saves_the_apple_section() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, None)?;
        let (code, out) = run(&ctx, &["setup", "apple"], "2\n1\n\n2\n1\n\n\nj\n");
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("Automation-Berechtigung"), "{out}");
        assert!(out.contains("Neue Freigaben:"));
        assert!(out.contains("Mail:     Demo Gmail (schreiben), Demo Outlook (lesen)"));
        assert!(out.contains("Kalender: Demo Privat (schreiben), Demo Familie (lesen)"));
        assert!(out.contains("Gespeichert in"));
        let config = saved(&ctx)?;
        assert!(section(&config, "accounts").is_none());
        let apple = section(&config, "apple")
            .and_then(Json::as_object)
            .ok_or("apple")?;
        let accounts = apple
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
        let calendars = apple
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
    fn plain_setup_asks_for_the_apple_entries_that_exist_without_listing_the_apps() -> TestResult {
        let dir = TempDir::new()?;
        let config = apple_only(&format!(
            r#""mail": [{ACCOUNT_1}, {ACCOUNT_2}], "calendars": [{{"id": "CAL-1", "name": "Privat", "level": "write"}}]"#
        ));
        let mut settings = settings_at(&dir.path("config.json"));
        fs::write(&settings.config_path, &config)?;
        settings.fake = false;
        let apple = Untouchable::default();
        let ctx = Context::new(
            settings,
            Box::new(apple.clone()),
            Box::new(apple.clone()),
            Linker::inert(),
        );
        let (code, out) = run(&ctx, &["setup"], "2\n0\n1\nj\n");
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("avery mail setup apple"), "{out}");
        assert!(out.contains("Mail:     Privat Gmail (schreiben)"), "{out}");
        assert!(out.contains("Kalender: Privat (lesen)"), "{out}");
        let rights = ctx.settings.stored_rights().map_err(|e| e.message())?;
        assert_eq!(rights.mail_level("ACC-1"), Some(Level::Write));
        assert_eq!(rights.mail_level("ACC-2"), None);
        assert_eq!(rights.calendar_level("CAL-1"), Some(Level::Read));
        assert_eq!(apple.calls(), 0);
        let (code, out) = run(&ctx, &["setup"], "0\n0\nj\n");
        assert_eq!(code, 0, "{out}");
        assert!(saved(&ctx)?
            .as_object()
            .ok_or("object")?
            .get("apple")
            .is_none());
        let (code, out) = run(&ctx, &["setup"], "");
        assert_eq!(code, 1, "{out}");
        assert!(out.contains("Noch kein Konto verbunden"), "{out}");
        assert!(out.contains("avery mail setup apple"), "{out}");
        assert_eq!(apple.calls(), 0);
        Ok(())
    }

    #[test]
    fn setup_apple_keeps_the_accounts_and_the_limits() -> TestResult {
        let dir = TempDir::new()?;
        let start = r#"{"version": 3,
            "accounts": [{"provider": "google", "address": "alice@gmail.example", "mail": "write"}],
            "apple": {"mail": [{"id": "FAKE-GMAIL", "name": "Demo Gmail", "level": "write"}],
                      "calendars": [{"id": "FAKE-CAL-PRIVAT", "name": "Demo Privat", "level": "read"}]},
            "sends_per_hour": 3, "changes_per_hour": 4}"#;
        let ctx = real(&dir, Some(start))?;
        let (code, out) = run(&ctx, &["setup", "apple"], "\n\n\n\n2\n\n\nj\n");
        assert_eq!(code, 0, "{out}");
        let config = saved(&ctx)?;
        let object = config.as_object().ok_or("object")?;
        assert_eq!(object.get("sends_per_hour"), Some(&Json::Int(3)));
        assert_eq!(object.get("changes_per_hour"), Some(&Json::Int(4)));
        assert_eq!(
            object.get("accounts"),
            Some(&j(
                r#"[{"provider": "google", "address": "alice@gmail.example", "mail": "write"}]"#
            )?)
        );
        let rights = ctx.settings.stored_rights().map_err(|e| e.message())?;
        assert_eq!(
            rights.mail_level("google:alice@gmail.example"),
            Some(Level::Write)
        );
        assert_eq!(rights.mail_level("FAKE-GMAIL"), Some(Level::Write));
        assert_eq!(rights.calendar_level("FAKE-CAL-PRIVAT"), Some(Level::Read));
        assert_eq!(
            rights.calendar_level("FAKE-CAL-FAMILIE"),
            Some(Level::Write)
        );
        Ok(())
    }

    #[test]
    fn setup_apple_without_changes_says_so_and_writes_nothing() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, None)?;
        let (code, out) = run(&ctx, &["setup", "apple"], "\n\n\n\n\n\n\n");
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("Unveraendert."));
        assert!(!std::path::Path::new(&ctx.settings.config_path).exists());
        Ok(())
    }

    #[test]
    fn a_read_only_apple_calendar_cannot_get_the_write_level() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, None)?;
        let (code, out) = run(&ctx, &["setup", "apple"], "\n\n\n\n\n2\n1\n\nj\n");
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("Demo Feiertage [schreibgeschuetzt]"));
        assert!(out.contains("Dieser Eintrag ist schreibgeschuetzt (hoechstens Stufe 1)."));
        assert!(out.contains("Kalender: Demo Feiertage (lesen)"));
        Ok(())
    }

    #[test]
    fn apple_entries_that_vanished_are_removed_with_a_note_and_all_off_switches_apple_off(
    ) -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, Some(&base()))?;
        let (code, out) = run(&ctx, &["setup", "apple"], "\n\n\n\n\n\n\nj\n");
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("Nicht mehr vorhanden, wird entfernt: Privat Gmail, Outlook"));
        assert!(
            out.contains("Apple Mail und Kalender sind wieder aus"),
            "{out}"
        );
        let config = saved(&ctx)?;
        assert!(section(&config, "apple").is_none());
        assert!(!ctx
            .settings
            .stored_rights()
            .map_err(|e| e.message())?
            .apple
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
            let (code, out) = run(&ctx, &["setup", "apple"], input);
            assert_eq!(code, 1, "{input:?}: {out}");
            assert!(out.contains(expected), "{input:?}: {out}");
            assert_eq!(saved(&ctx)?, j(&base())?, "{input:?}");
        }
        Ok(())
    }

    #[test]
    fn an_unknown_setup_area_is_refused() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, None)?;
        for area in ["notes\u{b}", "mail", "calendar"] {
            let (code, out) = run(&ctx, &["setup", area], "");
            assert_eq!(code, 2);
            assert!(out.contains("Unbekannter Bereich:"), "{out}");
            assert!(out.contains("(nur: apple)"), "{out}");
        }
        let (_, out) = run(&ctx, &["setup", "notes\u{b}"], "");
        assert!(
            out.contains("Unbekannter Bereich: notes (nur: apple)"),
            "{out}"
        );
        Ok(())
    }

    #[test]
    fn an_endless_answer_line_is_refused() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, Some(&base()))?;
        let long = format!("{}\n", "1".repeat(20_000));
        let (code, out) = run(&ctx, &["setup", "apple"], &long);
        assert_eq!(code, 1);
        assert!(out.contains("Ungueltige Eingabe."));
        Ok(())
    }

    #[test]
    fn revoke_removes_all_grants_and_the_apple_section_and_keeps_accounts_and_limits() -> TestResult
    {
        let dir = TempDir::new()?;
        let config = r#"{"version": 3,
            "accounts": [{"provider": "google", "address": "alice@gmail.example", "mail": "write",
                          "calendars": [{"id": "c", "name": "C", "level": "write"}]}],
            "apple": {"mail": [{"id": "A", "name": "A", "level": "write"}],
                      "calendars": [{"id": "C", "name": "C", "level": "write"}]},
            "sends_per_hour": 3}"#;
        let ctx = real(&dir, Some(config))?;
        let (code, out) = run(&ctx, &["revoke"], "");
        assert_eq!(code, 0);
        assert!(out.contains("Alle Freigaben entfernt"));
        assert!(out.contains("avery mail unlink <adresse>"), "{out}");
        assert!(
            out.contains("Apple Mail und Kalender sind wieder aus"),
            "{out}"
        );
        assert_eq!(
            saved(&ctx)?,
            j(
                r#"{"version": 3, "accounts": [{"provider": "google", "address": "alice@gmail.example"}], "sends_per_hour": 3, "changes_per_hour": 10}"#
            )?
        );
        let (code, out) = run(&ctx, &["revoke"], "");
        assert_eq!(code, 0);
        assert!(
            !out.contains("Apple Mail und Kalender sind wieder aus"),
            "{out}"
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
                (vec!["setup", "apple"], "1\nj\n"),
                (vec!["revoke"], ""),
                (vec!["unlink", "alice@gmail.example"], "j\n"),
                (vec!["link", "google"], "id\nsecret\n1\n"),
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
    fn status_shows_the_levels_per_area_the_accounts_apple_and_the_limits() -> TestResult {
        let dir = TempDir::new()?;
        let config = r#"{"version": 3,
            "accounts": [{"provider": "google", "address": "alice@gmail.example", "mail": "write"}],
            "apple": {"mail": [{"id": "A", "name": "Privat Gmail", "level": "write"}, {"id": "B", "name": "Outlook", "level": "read"}],
                      "calendars": [{"id": "C", "name": "Familie", "level": "read"}]},
            "sends_per_hour": 2, "changes_per_hour": 6}"#;
        let ctx = real(&dir, Some(config))?;
        let (code, out) = run(&ctx, &["status"], "");
        assert_eq!(code, 0);
        assert!(
            out.contains(
                "Mail: alice@gmail.example (schreiben), Privat Gmail (schreiben), Outlook (lesen)"
            ),
            "{out}"
        );
        assert!(out.contains("Kalender: Familie (lesen)"));
        assert!(
            out.contains("Direkt verbunden: alice@gmail.example (gmail)"),
            "{out}"
        );
        assert!(
            out.contains("Apple Mail und Kalender: an (2 Konten, 1 Kalender)"),
            "{out}"
        );
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
        assert!(
            out.contains("Apple Mail und Kalender: aus. Apple Mail und Kalender einbeziehen (optional): avery mail setup apple"),
            "{out}"
        );
        assert!(!out.contains("Direkt verbunden"), "{out}");
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
        assert!(!out.contains("avery mail setup apple"), "{out}");
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
        let only = apple_only(
            r#""calendars": [{"id": "FAKE-CAL-PRIVAT", "name": "Demo Privat", "level": "read"}]"#,
        );
        let ctx = context(&dir, false, "", Some(&only), FakeOptions::default())?;
        let (code, out) = run(&ctx, &["selftest"], "");
        assert_eq!(code, 0, "{out}");
        assert!(!out.contains("Konto 1") && out.contains("Kalender 1"));
        let gone = apple_only(r#""calendars": [{"id": "GONE", "name": "Weg", "level": "read"}]"#);
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
            "Befehle: accounts, calendars, setup, link, unlink, revoke, selftest, sendlog, changes, undo, selftest-write, status"
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
            assert!(out.contains("avery mail setup apple"));
            assert!(out.contains("avery mail link google|microsoft"));
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
    struct Listings {
        apple: Untouchable,
        remote: Vec<(&'static str, ToolResult<Vec<CalendarInfo>>)>,
    }

    impl CalendarBackend for Listings {
        fn calendars(&self, provider: Provider, address: &str) -> ToolResult<Vec<CalendarInfo>> {
            if provider == Provider::Apple {
                return self.apple.calendars(provider, address);
            }
            self.remote
                .iter()
                .find(|(known, _)| *known == address)
                .map_or_else(|| Ok(Vec::new()), |(_, result)| result.clone())
        }
        fn events(
            &self,
            calendar: &crate::calendar::Calendar,
            window: crate::calendar::Window,
        ) -> ToolResult<Vec<crate::calendar::RawEvent>> {
            if Provider::of_id(&calendar.id) == Provider::Apple {
                return self.apple.events(calendar, window);
            }
            Ok(Vec::new())
        }
        fn event(
            &self,
            _: &crate::calendar::Calendar,
            _: &str,
        ) -> ToolResult<crate::calendar::EventDetail> {
            Err(ToolError::msg("not used"))
        }
        fn change(
            &self,
            _: &crate::calendar::Calendar,
            _: &crate::calendar::Change,
            _: &crate::calendar::Approval,
        ) -> ToolResult<crate::calendar::ChangeResult> {
            Err(ToolError::msg("not used"))
        }
    }

    fn info(id: &str, name: &str, writable: bool) -> CalendarInfo {
        CalendarInfo {
            id: id.to_string(),
            name: name.to_string(),
            writable,
        }
    }

    type Remote = Vec<(&'static str, ToolResult<Vec<CalendarInfo>>)>;

    struct Cloudy {
        ctx: Context,
        vault: std::sync::Arc<crate::vault::memory::Memory>,
        apple: Untouchable,
    }

    #[derive(Clone)]
    struct Probe {
        config_lock: String,
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        violations: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Probe {
        fn new(config_lock: String) -> Probe {
            Probe {
                config_lock,
                calls: Default::default(),
                violations: Default::default(),
            }
        }

        fn check(&self) {
            use std::sync::atomic::Ordering;
            self.calls.fetch_add(1, Ordering::SeqCst);
            if crate::lock::acquire(&self.config_lock, Duration::ZERO).is_err() {
                self.violations.fetch_add(1, Ordering::SeqCst);
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }

        fn violations(&self) -> usize {
            self.violations.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[derive(Default)]
    struct Faults {
        on_store: Option<Box<dyn Fn()>>,
        fail_delete: bool,
        probe: Option<Probe>,
    }

    impl Faults {
        fn on_store(hook: impl Fn() + 'static) -> Faults {
            Faults {
                on_store: Some(Box::new(hook)),
                ..Faults::default()
            }
        }

        fn undeletable() -> Faults {
            Faults {
                fail_delete: true,
                ..Faults::default()
            }
        }

        fn probed(probe: &Probe) -> Faults {
            Faults {
                probe: Some(probe.clone()),
                ..Faults::default()
            }
        }

        fn probe(&self) {
            if let Some(probe) = &self.probe {
                probe.check();
            }
        }
    }

    struct Meddling {
        inner: crate::vault::memory::Shared,
        faults: Faults,
    }

    impl crate::vault::Vault for Meddling {
        fn load(&self, name: &str) -> Result<Option<String>, crate::vault::VaultError> {
            self.faults.probe();
            self.inner.load(name)
        }

        fn store(&self, name: &str, secret: &str) -> Result<(), crate::vault::VaultError> {
            self.faults.probe();
            if let Some(hook) = &self.faults.on_store {
                hook();
            }
            self.inner.store(name, secret)
        }

        fn replace(
            &self,
            name: &str,
            expected: &str,
            secret: &str,
        ) -> Result<bool, crate::vault::VaultError> {
            self.faults.probe();
            self.inner.replace(name, expected, secret)
        }

        fn delete(&self, name: &str) -> Result<(), crate::vault::VaultError> {
            self.faults.probe();
            if self.faults.fail_delete {
                return Err(crate::vault::VaultError::Unavailable("locked".to_string()));
            }
            self.inner.delete(name)
        }
    }

    fn cloudy(
        dir: &TempDir,
        config: &str,
        records: &[(&str, &str)],
        remote: Remote,
        visit: crate::provider::fakes::Visit,
        transport: crate::http::scripted::Scripted,
    ) -> Result<Cloudy, Box<dyn std::error::Error>> {
        meddled(
            dir,
            config,
            records,
            remote,
            visit,
            transport,
            Faults::default(),
        )
    }

    fn meddled(
        dir: &TempDir,
        config: &str,
        records: &[(&str, &str)],
        remote: Remote,
        visit: crate::provider::fakes::Visit,
        transport: crate::http::scripted::Scripted,
        faults: Faults,
    ) -> Result<Cloudy, Box<dyn std::error::Error>> {
        let settings = settings_at(&dir.path("config.json"));
        fs::write(&settings.config_path, config)?;
        let mut memory = crate::vault::memory::Memory::new();
        for (name, scope) in records {
            memory = memory.with(name, &crate::provider::fakes::record(scope).to_json());
        }
        let vault = std::sync::Arc::new(memory);
        let linker = Linker::new(
            Box::new(transport),
            Box::new(Meddling {
                inner: crate::vault::memory::Shared(std::sync::Arc::clone(&vault)),
                faults,
            }),
            Box::new(crate::provider::fakes::Visitor {
                visit,
                seen: Default::default(),
            }),
        )
        .within(Duration::from_millis(700));
        let apple = Untouchable::default();
        Ok(Cloudy {
            ctx: Context::new(
                settings,
                Box::new(apple.clone()),
                Box::new(Listings {
                    apple: apple.clone(),
                    remote,
                }),
                linker,
            ),
            vault,
            apple,
        })
    }

    fn quiet() -> Remote {
        Vec::new()
    }

    const READ_SCOPE: &str = "https://www.googleapis.com/auth/gmail.readonly https://www.googleapis.com/auth/calendar.readonly";
    const WRITE_SCOPE: &str = "https://www.googleapis.com/auth/gmail.readonly https://www.googleapis.com/auth/calendar.readonly https://www.googleapis.com/auth/gmail.compose https://www.googleapis.com/auth/calendar.events";

    const LINKED: &str = r#"{"version": 3, "accounts": [
        {"provider": "google", "address": "alice@gmail.example", "mail": "write",
         "calendars": [{"id": "alice@gmail.example", "name": "alice@gmail.example", "level": "write"}]},
        {"provider": "microsoft", "address": "bob@outlook.example",
         "calendars": [{"id": "AQMk", "name": "Calendar (bob@outlook.example)", "level": "read"}]}]}"#;

    const LINKED_GMAIL: &str = r#"{"version": 3, "accounts": [
        {"provider": "google", "address": "alice@{gmail}", "mail": "write",
         "calendars": [{"id": "alice@{gmail}", "name": "alice@{gmail}", "level": "write"}]},
        {"provider": "microsoft", "address": "bob@outlook.example",
         "calendars": [{"id": "AQMk", "name": "Calendar (bob@outlook.example)", "level": "read"}]}]}"#;

    fn google_tokens(write: bool) -> String {
        let extra = if write {
            " https://www.googleapis.com/auth/gmail.compose https://www.googleapis.com/auth/calendar.events"
        } else {
            ""
        };
        crate::provider::fakes::GOOGLE_TOKENS.replace(
            "calendar.readonly\"",
            &format!("calendar.readonly{extra}\""),
        )
    }

    fn stored(cloud: &Cloudy) -> Result<Rights, String> {
        cloud
            .ctx
            .settings
            .stored_rights()
            .map_err(|error| error.message())
    }

    #[test]
    fn linking_a_google_account_adds_an_entry_with_a_read_grant() -> TestResult {
        let dir = TempDir::new()?;
        let cloud = cloudy(
            &dir,
            r#"{"version": 3}"#,
            &[],
            quiet(),
            crate::provider::fakes::Visit::Good("4/0code"),
            crate::provider::fakes::google_transport(&google_tokens(false)),
        )?;
        let (code, out) = run(
            &cloud.ctx,
            &["link", "google"],
            "123-abc.apps.example\nGOCSPX-secret_1\n\n",
        );
        assert_eq!(code, 0, "{out}");
        assert!(
            out.contains(&gm("Verbunden: alice@{gmail} (gmail). Mail: lesen.")),
            "{out}"
        );
        assert!(out.contains("https://accounts.google.com/o/oauth2/v2/auth?"));
        assert!(out.contains("Nur persoenliche Gmail-Konten"), "{out}");
        let config = saved(&cloud.ctx)?;
        assert_eq!(
            section(&config, "accounts"),
            Some(&j(&gm(
                r#"[{"provider": "google", "address": "alice@{gmail}", "mail": "read"}]"#
            ))?)
        );
        assert!(section(&config, "apple").is_none());
        assert!(cloud.vault.get(&gm("google:alice@{gmail}")).is_some());
        assert!(!fs::read_to_string(&cloud.ctx.settings.config_path)?.contains("GOCSPX"));
        assert!(!out.contains("ya29"));
        assert_eq!(cloud.apple.calls(), 0);
        Ok(())
    }

    #[test]
    fn linking_with_the_write_level_grants_write_only_when_the_scopes_arrived() -> TestResult {
        let dir = TempDir::new()?;
        let cloud = cloudy(
            &dir,
            r#"{"version": 3}"#,
            &[],
            quiet(),
            crate::provider::fakes::Visit::Good("c"),
            crate::provider::fakes::google_transport(&google_tokens(true)),
        )?;
        let (code, out) = run(&cloud.ctx, &["link", "google"], "id1\nsecret1\n2\n");
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("Mail: schreiben."), "{out}");
        assert_eq!(
            stored(&cloud)?.mail_level(&gm("google:alice@{gmail}")),
            Some(Level::Write)
        );
        let second = TempDir::new()?;
        let again = cloudy(
            &second,
            r#"{"version": 3}"#,
            &[],
            quiet(),
            crate::provider::fakes::Visit::Good("c"),
            crate::provider::fakes::google_transport(&google_tokens(false)),
        )?;
        let (code, out) = run(&again.ctx, &["link", "google"], "id1\nsecret1\n2\n");
        assert_eq!(code, 1, "{out}");
        assert!(out.contains("did not grant everything"), "{out}");
        assert_eq!(
            fs::read_to_string(&again.ctx.settings.config_path)?,
            r#"{"version": 3}"#
        );
        assert!(again.vault.get(&gm("google:alice@{gmail}")).is_none());
        Ok(())
    }

    #[test]
    fn linking_again_may_only_lower_the_rights_of_that_account() -> TestResult {
        let dir = TempDir::new()?;
        let cloud = cloudy(
            &dir,
            &gm(LINKED_GMAIL),
            &[(gm("google:alice@{gmail}").as_str(), WRITE_SCOPE)],
            quiet(),
            crate::provider::fakes::Visit::Good("c"),
            crate::provider::fakes::google_transport(&google_tokens(false)),
        )?;
        let (code, out) = run(&cloud.ctx, &["link", "google"], "id1\nsecret1\n1\n");
        assert_eq!(code, 0, "{out}");
        assert!(
            out.contains(&gm("Erneut verbunden: alice@{gmail} (gmail)")),
            "{out}"
        );
        let rights = stored(&cloud)?;
        assert_eq!(rights.accounts.len(), 2);
        assert_eq!(
            rights.mail_level(&gm("google:alice@{gmail}")),
            Some(Level::Read)
        );
        assert_eq!(
            rights.calendar_level(&gm("google:alice@{gmail}:alice@{gmail}")),
            Some(Level::Read)
        );
        assert_eq!(
            rights.calendar_level("microsoft:bob@outlook.example:AQMk"),
            Some(Level::Read)
        );
        let kept = crate::provider::fakes::stored_scope(&cloud.vault, &gm("google:alice@{gmail}"));
        assert!(!kept.contains("gmail.compose"), "{kept}");

        let second = TempDir::new()?;
        let off = gm(
            r#"{"version": 3, "accounts": [{"provider": "google", "address": "alice@{gmail}", "calendars": [{"id": "alice@{gmail}", "level": "read"}]}]}"#,
        );
        let again = cloudy(
            &second,
            &off,
            &[],
            quiet(),
            crate::provider::fakes::Visit::Good("c"),
            crate::provider::fakes::google_transport(&google_tokens(true)),
        )?;
        let (code, out) = run(&again.ctx, &["link", "google"], "id1\nsecret1\n2\n");
        assert_eq!(code, 0, "{out}");
        let rights = stored(&again)?;
        assert_eq!(rights.mail_level(&gm("google:alice@{gmail}")), None);
        assert_eq!(
            rights.calendar_level(&gm("google:alice@{gmail}:alice@{gmail}")),
            Some(Level::Read)
        );
        assert!(again.vault.get(&gm("google:alice@{gmail}")).is_some());
        Ok(())
    }

    #[test]
    fn link_refuses_bad_input_demo_mode_unfinished_sign_ins_and_a_full_list() -> TestResult {
        let dir = TempDir::new()?;
        let cloud = cloudy(
            &dir,
            r#"{"version": 3}"#,
            &[],
            quiet(),
            crate::provider::fakes::Visit::Nothing,
            crate::provider::fakes::google_transport(&google_tokens(false)),
        )?;
        assert_eq!(run(&cloud.ctx, &["link"], "").0, 2);
        assert_eq!(run(&cloud.ctx, &["link", "apple"], "").0, 2);
        for (input, wanted) in [
            ("bad id\nsecret\n1\n", "client id looks wrong"),
            ("id\n\n1\n", "client secret looks wrong"),
            ("id\nsecret\n3\n", "Ungueltige Eingabe"),
            ("", "Ungueltige Eingabe"),
            ("id\nsecret\n1\n", "No sign-in arrived in time"),
        ] {
            let (code, out) = run(&cloud.ctx, &["link", "google"], input);
            assert_eq!(code, 1, "{input:?}: {out}");
            assert!(out.contains(wanted), "{input:?}: {out}");
        }
        assert_eq!(
            fs::read_to_string(&cloud.ctx.settings.config_path)?,
            r#"{"version": 3}"#
        );
        let mut demo = cloudy(
            &TempDir::new()?,
            r#"{"version": 3}"#,
            &[],
            quiet(),
            crate::provider::fakes::Visit::Nothing,
            crate::http::scripted::Scripted::new(),
        )?;
        demo.ctx.settings.fake = true;
        let (code, out) = run(&demo.ctx, &["link", "google"], "");
        assert_eq!(code, 1);
        assert!(out.contains("Demo-Modus"), "{out}");
        let full = sixteen(None);
        let crowded_dir = TempDir::new()?;
        let crowded = cloudy(
            &crowded_dir,
            &full,
            &[],
            quiet(),
            crate::provider::fakes::Visit::Nothing,
            crate::http::scripted::Scripted::new(),
        )?;
        let (code, out) = run(&crowded.ctx, &["link", "google"], "id\nsecret\n1\n");
        assert_eq!(code, 1);
        assert!(
            out.contains("Hinweis: Es sind schon 16 Konten verbunden."),
            "{out}"
        );
        assert!(out.contains("Client-ID"), "{out}");
        assert!(out.contains("No sign-in arrived in time"), "{out}");
        assert_eq!(fs::read_to_string(&crowded.ctx.settings.config_path)?, full);
        Ok(())
    }

    #[test]
    fn a_google_account_that_is_not_a_gmail_address_is_refused_and_nothing_is_saved() -> TestResult
    {
        for email in [
            "alice@firma.example",
            "x@{gmail}.evil.example",
            "x@evil{gmail}",
        ]
        .map(gm)
        {
            let dir = TempDir::new()?;
            let transport = crate::http::scripted::Scripted::new()
                .json(
                    crate::http::Method::Post,
                    "oauth2.googleapis.com/token",
                    200,
                    &google_tokens(false),
                )
                .json(
                    crate::http::Method::Get,
                    "openidconnect.googleapis.com",
                    200,
                    &format!(r#"{{"sub": "1", "email": "{email}", "email_verified": true}}"#),
                );
            let cloud = cloudy(
                &dir,
                r#"{"version": 3}"#,
                &[],
                quiet(),
                crate::provider::fakes::Visit::Good("c"),
                transport,
            )?;
            let (code, out) = run(&cloud.ctx, &["link", "google"], "id1\nsecret1\n1\n");
            assert_eq!(code, 1, "{email}: {out}");
            assert!(
                out.contains(
                    "Avery verbindet nur persoenliche Gmail-Konten (Adressen auf gmail.com oder googlemail.com), keine Google-Workspace-Konten und keine eigenen Domains."
                ),
                "{out}"
            );
            assert_eq!(
                fs::read_to_string(&cloud.ctx.settings.config_path)?,
                r#"{"version": 3}"#
            );
            assert_eq!(cloud.vault.count(), 0, "{email}");
        }
        Ok(())
    }

    #[test]
    fn unlinking_removes_the_entry_and_the_sign_in_of_one_account_only() -> TestResult {
        let dir = TempDir::new()?;
        let cloud = cloudy(
            &dir,
            LINKED,
            &[
                ("google:alice@gmail.example", READ_SCOPE),
                ("microsoft:bob@outlook.example", "Mail.Read"),
            ],
            quiet(),
            crate::provider::fakes::Visit::Nothing,
            crate::http::scripted::Scripted::new(),
        )?;
        let (code, out) = run(&cloud.ctx, &["unlink", "bob@outlook.example"], "n\n");
        assert_eq!(code, 1, "{out}");
        assert_eq!(stored(&cloud)?.accounts.len(), 2);
        assert!(cloud.vault.get("microsoft:bob@outlook.example").is_some());
        let (code, out) = run(&cloud.ctx, &["unlink", "Bob@Outlook.example"], "j\n");
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("bob@outlook.example getrennt."), "{out}");
        let rights = stored(&cloud)?;
        assert_eq!(rights.accounts.len(), 1);
        assert_eq!(rights.accounts[0].address(), "alice@gmail.example");
        assert_eq!(rights.calendars_at(Level::Read).len(), 1);
        assert_eq!(rights.mail_at(Level::Read).len(), 1);
        assert!(cloud.vault.get("microsoft:bob@outlook.example").is_none());
        assert!(cloud.vault.get("google:alice@gmail.example").is_some());
        for (arguments, wanted) in [
            (vec!["unlink"], "Benutzung: avery mail unlink"),
            (vec!["unlink", "nobody@x.example"], "Verbunden:"),
            (vec!["unlink", "apple:alice@gmail.example"], "Verbunden:"),
            (
                vec!["unlink", "microsoft:alice@gmail.example"],
                "ist nicht verbunden",
            ),
        ] {
            let (code, out) = run(&cloud.ctx, &arguments, "j\n");
            assert_eq!(code, 1, "{out}");
            assert!(out.contains(wanted), "{out}");
        }
        let plain = real(&TempDir::new()?, None)?;
        let (code, out) = run(&plain, &["unlink", "x@y.example"], "");
        assert_eq!(
            (code, out.contains("kein Konto direkt verbunden")),
            (1, true)
        );
        assert_eq!(cloud.apple.calls(), 0);
        Ok(())
    }

    #[test]
    fn an_unlink_names_only_the_consent_page_of_its_own_provider() -> TestResult {
        for (address, shown, hidden) in [
            (
                "bob@outlook.example",
                "https://account.live.com/consent/Manage",
                "myaccount.google.com",
            ),
            (
                "alice@gmail.example",
                "https://myaccount.google.com/permissions",
                "account.live.com",
            ),
        ] {
            let dir = TempDir::new()?;
            let cloud = cloudy(
                &dir,
                LINKED,
                &bob_and_alice(),
                quiet(),
                crate::provider::fakes::Visit::Nothing,
                crate::http::scripted::Scripted::new(),
            )?;
            let (code, out) = run(&cloud.ctx, &["unlink", address], "j\n");
            assert_eq!(code, 0, "{out}");
            assert!(
                out.contains(&format!(
                    "Zugriff der App auch im Konto widerrufen: {shown}"
                )),
                "{out}"
            );
            assert!(!out.contains(hidden), "{out}");
        }
        Ok(())
    }

    #[test]
    fn a_config_with_one_address_under_two_providers_is_refused_and_never_rewritten() -> TestResult
    {
        let dir = TempDir::new()?;
        let twice = r#"{"version": 3, "accounts": [{"provider": "google", "address": "x@example.org"}, {"provider": "microsoft", "address": "x@example.org"}]}"#;
        let cloud = cloudy(
            &dir,
            twice,
            &[
                ("google:x@example.org", READ_SCOPE),
                ("microsoft:x@example.org", "Mail.Read"),
            ],
            quiet(),
            crate::provider::fakes::Visit::Nothing,
            crate::http::scripted::Scripted::new(),
        )?;
        for args in [
            vec!["accounts"],
            vec!["status"],
            vec!["revoke"],
            vec!["setup"],
            vec!["unlink", "x@example.org"],
        ] {
            let (code, out) = run(&cloud.ctx, &args, "j\n");
            assert_eq!(code, 1, "{args:?}: {out}");
            assert!(
                out.contains("accounts[1].address is already linked under google: an address can be linked only once"),
                "{args:?}: {out}"
            );
            assert_eq!(fs::read_to_string(&cloud.ctx.settings.config_path)?, twice);
        }
        let (code, out) = run(&cloud.ctx, &["unlink", "Microsoft:x@example.org"], "j\n");
        assert_eq!(code, 1, "{out}");
        assert!(
            out.contains("die Anmeldung im Schluesselbund ist geloescht, die Konfiguration aber nicht geaendert"),
            "{out}"
        );
        assert!(cloud.vault.get("microsoft:x@example.org").is_none());
        assert!(cloud.vault.get("google:x@example.org").is_some());
        assert_eq!(fs::read_to_string(&cloud.ctx.settings.config_path)?, twice);
        Ok(())
    }

    #[test]
    fn an_address_that_is_linked_at_another_provider_is_refused_and_the_new_sign_in_removed(
    ) -> TestResult {
        let dir = TempDir::new()?;
        let config = gm(
            r#"{"version": 3, "accounts": [{"provider": "microsoft", "address": "alice@{gmail}", "mail": "read"}]}"#,
        );
        let cloud = cloudy(
            &dir,
            &config,
            &[(gm("microsoft:alice@{gmail}").as_str(), "Mail.Read")],
            quiet(),
            crate::provider::fakes::Visit::Good("c"),
            crate::provider::fakes::google_transport(&google_tokens(false)),
        )?;
        let (code, out) = run(&cloud.ctx, &["link", "google"], "id1\nsecret1\n1\n");
        assert_eq!(code, 1, "{out}");
        assert!(
            out.contains(&gm("Die Adresse alice@{gmail} ist schon bei Outlook.com verbunden. Eine Adresse kann nur einmal verbunden werden. Zuerst trennen: avery mail unlink microsoft:alice@{gmail}")),
            "{out}"
        );
        assert!(out.contains("wieder entfernt"), "{out}");
        assert!(cloud.vault.get(&gm("google:alice@{gmail}")).is_none());
        assert!(cloud.vault.get(&gm("microsoft:alice@{gmail}")).is_some());
        assert_eq!(fs::read_to_string(&cloud.ctx.settings.config_path)?, config);
        Ok(())
    }

    fn sixteen(including: Option<&str>) -> String {
        let mut accounts: Vec<String> = (0..15)
            .map(|n| format!(r#"{{"provider": "google", "address": "a{n}@gmail.example"}}"#))
            .collect();
        accounts.push(match including {
            Some(address) => format!(
                r#"{{"provider": "google", "address": "{address}", "mail": "write", "calendars": [{{"id": "primary", "level": "write"}}]}}"#
            ),
            None => r#"{"provider": "google", "address": "a15@gmail.example"}"#.to_string(),
        });
        format!(r#"{{"version": 3, "accounts": [{}]}}"#, accounts.join(","))
    }

    #[test]
    fn linking_again_works_at_the_account_limit_and_still_only_lowers_the_rights() -> TestResult {
        let dir = TempDir::new()?;
        let cloud = cloudy(
            &dir,
            &sixteen(Some(&gm("alice@{gmail}"))),
            &[(gm("google:alice@{gmail}").as_str(), WRITE_SCOPE)],
            quiet(),
            crate::provider::fakes::Visit::Good("c"),
            crate::provider::fakes::google_transport(&google_tokens(false)),
        )?;
        let (code, out) = run(&cloud.ctx, &["link", "google"], "id1\nsecret1\n1\n");
        assert_eq!(code, 0, "{out}");
        assert!(
            out.contains("Hinweis: Es sind schon 16 Konten verbunden."),
            "{out}"
        );
        assert!(out.contains("Client-ID:"), "{out}");
        assert!(
            out.contains(&gm("Erneut verbunden: alice@{gmail} (gmail). Mail: lesen.")),
            "{out}"
        );
        let rights = stored(&cloud)?;
        assert_eq!(rights.accounts.len(), 16);
        assert_eq!(
            rights.mail_level(&gm("google:alice@{gmail}")),
            Some(Level::Read)
        );
        assert_eq!(
            rights.calendar_level(&gm("google:alice@{gmail}:primary")),
            Some(Level::Read)
        );
        assert!(cloud.vault.get(&gm("google:alice@{gmail}")).is_some());
        Ok(())
    }

    #[test]
    fn a_new_account_at_the_limit_is_refused_after_the_sign_in_and_its_sign_in_removed(
    ) -> TestResult {
        let dir = TempDir::new()?;
        let full = sixteen(None);
        let cloud = cloudy(
            &dir,
            &full,
            &[],
            quiet(),
            crate::provider::fakes::Visit::Good("c"),
            crate::provider::fakes::google_transport(&google_tokens(false)),
        )?;
        let (code, out) = run(&cloud.ctx, &["link", "google"], "id1\nsecret1\n1\n");
        assert_eq!(code, 1, "{out}");
        assert!(
            out.contains("Hinweis: Es sind schon 16 Konten verbunden."),
            "{out}"
        );
        assert!(
            out.contains("Es sind schon 16 Konten verbunden. Zuerst eines trennen: avery mail unlink <adresse>"),
            "{out}"
        );
        assert!(out.contains("wieder entfernt"), "{out}");
        assert!(!out.contains("Verbunden:"), "{out}");
        assert!(cloud.vault.get(&gm("google:alice@{gmail}")).is_none());
        assert_eq!(fs::read_to_string(&cloud.ctx.settings.config_path)?, full);
        Ok(())
    }

    #[test]
    fn accounts_and_status_show_linked_accounts_and_missing_sign_ins() -> TestResult {
        let dir = TempDir::new()?;
        let cloud = cloudy(
            &dir,
            LINKED,
            &[("google:alice@gmail.example", READ_SCOPE)],
            quiet(),
            crate::provider::fakes::Visit::Nothing,
            crate::http::scripted::Scripted::new(),
        )?;
        let (code, out) = run(&cloud.ctx, &["accounts"], "");
        assert_eq!(code, 0, "{out}");
        assert!(
            out.contains("* 1) alice@gmail.example  [gmail]  alice@gmail.example  [schreiben]\n"),
            "{out}"
        );
        assert!(
            out.contains("  2) bob@outlook.example  [outlook]  bob@outlook.example  [aus]  [Anmeldung fehlt, neu verbinden mit: avery mail link]"),
            "{out}"
        );
        assert!(out.contains("avery mail setup apple"), "{out}");
        let (code, out) = run(&cloud.ctx, &["status"], "");
        assert_eq!(code, 0, "{out}");
        assert!(
            out.contains(
                "Direkt verbunden: alice@gmail.example (gmail), bob@outlook.example (outlook)"
            ),
            "{out}"
        );
        assert!(out.contains("Apple Mail und Kalender: aus."), "{out}");
        let empty = real(&TempDir::new()?, None)?;
        let (code, out) = run(&empty, &["accounts"], "");
        assert_eq!(code, 0);
        assert!(out.contains("Noch kein Konto verbunden."), "{out}");
        assert!(out.contains("avery mail link google | microsoft"), "{out}");
        assert!(out.contains("avery mail setup apple"), "{out}");
        assert_eq!(cloud.apple.calls(), 0);
        Ok(())
    }

    #[test]
    fn calendars_of_linked_accounts_are_listed_named_and_failures_are_tolerated() -> TestResult {
        let dir = TempDir::new()?;
        let remote: Remote = vec![
            (
                "alice@gmail.example",
                Ok(vec![
                    info("alice@gmail.example", "alice@gmail.example", true),
                    info("holidays@group", "Feiertage", false),
                ]),
            ),
            ("bob@outlook.example", Err(ToolError::msg("offline"))),
        ];
        let cloud = cloudy(
            &dir,
            LINKED,
            &[("google:alice@gmail.example", READ_SCOPE)],
            remote,
            crate::provider::fakes::Visit::Nothing,
            crate::http::scripted::Scripted::new(),
        )?;
        let (code, out) = run(&cloud.ctx, &["calendars"], "");
        assert_eq!(code, 0, "{out}");
        assert!(
            out.contains("* 1) alice@gmail.example  [schreiben]"),
            "{out}"
        );
        assert!(
            out.contains("  2) Feiertage (alice@gmail.example) [schreibgeschuetzt]  [aus]"),
            "{out}"
        );
        assert!(
            out.contains("Hinweis: Kalender von bob@outlook.example nicht lesbar (offline)"),
            "{out}"
        );
        assert_eq!(cloud.apple.calls(), 0);
        let down_dir = TempDir::new()?;
        let all_down = cloudy(
            &down_dir,
            LINKED,
            &[],
            vec![
                ("alice@gmail.example", Err(ToolError::msg("offline"))),
                ("bob@outlook.example", Err(ToolError::msg("offline"))),
            ],
            crate::provider::fakes::Visit::Nothing,
            crate::http::scripted::Scripted::new(),
        )?;
        let (code, out) = run(&all_down.ctx, &["calendars"], "");
        assert_eq!(code, 1, "{out}");
        assert!(out.contains("Keine Kalender gefunden."), "{out}");
        let (code, out) = run(&real(&TempDir::new()?, None)?, &["calendars"], "");
        assert_eq!(code, 0);
        assert!(out.contains("Noch kein Kalender verfuegbar."), "{out}");
        Ok(())
    }

    #[test]
    fn setup_asks_per_account_for_mail_and_calendars_within_what_the_sign_in_allows() -> TestResult
    {
        let dir = TempDir::new()?;
        let remote: Remote = vec![
            (
                "alice@gmail.example",
                Ok(vec![
                    info("alice@gmail.example", "alice@gmail.example", true),
                    info("family@group", "Familie", true),
                ]),
            ),
            ("bob@outlook.example", Err(ToolError::msg("offline"))),
        ];
        let cloud = cloudy(
            &dir,
            LINKED,
            &[("google:alice@gmail.example", READ_SCOPE)],
            remote,
            crate::provider::fakes::Visit::Nothing,
            crate::http::scripted::Scripted::new(),
        )?;
        let (code, out) = run(&cloud.ctx, &["setup"], "\n\n2\n1\n\nj\n");
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("Stufe 0 entzieht nur die Freigabe"), "{out}");
        assert!(
            out.contains("Dieser Eintrag ist schreibgeschuetzt"),
            "{out}"
        );
        assert!(
            out.contains("Hinweis: Kalender von bob@outlook.example nicht lesbar (offline); deren Freigaben bleiben unveraendert."),
            "{out}"
        );
        assert!(out.contains("avery mail setup apple"), "{out}");
        let rights = stored(&cloud)?;
        assert_eq!(rights.accounts.len(), 2);
        assert_eq!(
            rights.mail_level("google:alice@gmail.example"),
            Some(Level::Read)
        );
        assert_eq!(rights.mail_level("microsoft:bob@outlook.example"), None);
        assert_eq!(
            rights.calendar_level("google:alice@gmail.example:alice@gmail.example"),
            Some(Level::Read)
        );
        assert_eq!(
            rights.calendar_level("google:alice@gmail.example:family@group"),
            Some(Level::Read)
        );
        assert_eq!(
            rights.calendar_level("microsoft:bob@outlook.example:AQMk"),
            Some(Level::Read)
        );
        assert_eq!(cloud.apple.calls(), 0);
        Ok(())
    }

    #[test]
    fn setup_removes_calendars_that_vanished_and_can_switch_the_mail_of_an_account_off(
    ) -> TestResult {
        let dir = TempDir::new()?;
        let config = r#"{"version": 3, "accounts": [{"provider": "google", "address": "alice@gmail.example", "mail": "read",
            "calendars": [{"id": "alice@gmail.example", "level": "read"}, {"id": "gone@group", "name": "Weg", "level": "read"}]}]}"#;
        let remote: Remote = vec![(
            "alice@gmail.example",
            Ok(vec![info(
                "alice@gmail.example",
                "alice@gmail.example",
                true,
            )]),
        )];
        let cloud = cloudy(
            &dir,
            config,
            &[("google:alice@gmail.example", WRITE_SCOPE)],
            remote,
            crate::provider::fakes::Visit::Nothing,
            crate::http::scripted::Scripted::new(),
        )?;
        let (code, out) = run(&cloud.ctx, &["setup"], "0\n2\nj\n");
        assert_eq!(code, 0, "{out}");
        assert!(
            out.contains("Nicht mehr vorhanden, wird entfernt: Weg"),
            "{out}"
        );
        let rights = stored(&cloud)?;
        assert_eq!(rights.mail_level("google:alice@gmail.example"), None);
        assert_eq!(
            rights.calendar_level("google:alice@gmail.example:alice@gmail.example"),
            Some(Level::Write)
        );
        assert_eq!(rights.calendars_at(Level::Read).len(), 1);
        assert_eq!(rights.accounts.len(), 1);
        Ok(())
    }

    #[test]
    fn setup_without_any_account_points_to_link_and_apple() -> TestResult {
        let dir = TempDir::new()?;
        let cloud = cloudy(
            &dir,
            r#"{"version": 3}"#,
            &[],
            quiet(),
            crate::provider::fakes::Visit::Nothing,
            crate::http::scripted::Scripted::new(),
        )?;
        let (code, out) = run(&cloud.ctx, &["setup"], "1\n1\nj\n");
        assert_eq!(code, 1, "{out}");
        assert!(out.contains("avery mail link google | microsoft"), "{out}");
        assert!(out.contains("avery mail setup apple"), "{out}");
        assert!(!out.contains("Stufe 0=aus"), "{out}");
        assert_eq!(
            fs::read_to_string(&cloud.ctx.settings.config_path)?,
            r#"{"version": 3}"#
        );
        assert_eq!(cloud.apple.calls(), 0);
        Ok(())
    }

    #[test]
    fn revoke_removes_the_grants_but_keeps_the_accounts_and_their_sign_ins() -> TestResult {
        let dir = TempDir::new()?;
        let cloud = cloudy(
            &dir,
            LINKED,
            &[("google:alice@gmail.example", READ_SCOPE)],
            quiet(),
            crate::provider::fakes::Visit::Nothing,
            crate::http::scripted::Scripted::new(),
        )?;
        let (code, out) = run(&cloud.ctx, &["revoke"], "");
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("avery mail unlink <adresse>"), "{out}");
        let rights = stored(&cloud)?;
        assert!(rights.mail_at(Level::Read).is_empty());
        assert!(rights.calendars_at(Level::Read).is_empty());
        assert_eq!(rights.accounts.len(), 2);
        assert!(cloud.vault.get("google:alice@gmail.example").is_some());
        Ok(())
    }

    #[test]
    fn revoke_works_when_the_lock_file_is_read_only() -> TestResult {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new()?;
        let cloud = cloudy(
            &dir,
            LINKED,
            &[("google:alice@gmail.example", READ_SCOPE)],
            quiet(),
            crate::provider::fakes::Visit::Nothing,
            crate::http::scripted::Scripted::new(),
        )?;
        let lock = cloud.ctx.settings.lock_path();
        fs::write(&lock, "")?;
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o400))?;
        let (code, out) = run(&cloud.ctx, &["revoke"], "");
        assert_eq!(code, 0, "{out}");
        assert!(!out.contains("nicht sperrbar"), "{out}");
        let rights = stored(&cloud)?;
        assert!(rights.mail_at(Level::Read).is_empty());
        assert_eq!(rights.accounts.len(), 2);
        assert_eq!(fs::metadata(&lock)?.permissions().mode() & 0o777, 0o400);
        Ok(())
    }

    #[test]
    fn a_cloud_only_configuration_never_touches_apple_but_setup_apple_does() -> TestResult {
        let dir = TempDir::new()?;
        let remote: Remote = vec![
            (
                "alice@gmail.example",
                Ok(vec![info(
                    "alice@gmail.example",
                    "alice@gmail.example",
                    true,
                )]),
            ),
            (
                "bob@outlook.example",
                Ok(vec![info("AQMk", "Calendar", true)]),
            ),
        ];
        let cloud = cloudy(
            &dir,
            LINKED,
            &[
                ("google:alice@gmail.example", WRITE_SCOPE),
                ("microsoft:bob@outlook.example", "Mail.Read"),
            ],
            remote,
            crate::provider::fakes::Visit::Nothing,
            crate::http::scripted::Scripted::new(),
        )?;
        for (args, input) in [
            (vec!["accounts"], ""),
            (vec!["calendars"], ""),
            (vec!["status"], ""),
            (vec!["setup"], "\n\n\n\n\n"),
            (vec!["setup"], "0\n0\n0\n0\nn\n"),
            (vec!["sendlog"], ""),
            (vec!["changes"], ""),
            (vec!["undo"], ""),
            (vec!["unlink", "bob@outlook.example"], "n\n"),
            (vec!["revoke"], ""),
            (vec!["accounts"], ""),
            (vec!["calendars"], ""),
            (vec!["status"], ""),
        ] {
            let (_, out) = run(&cloud.ctx, &args, input);
            assert!(!out.contains("BACKEND TOUCHED"), "{args:?}: {out}");
            assert_eq!(cloud.apple.calls(), 0, "{args:?}: {out}");
        }
        let (code, out) = run(&cloud.ctx, &["setup", "apple"], "");
        assert_eq!(code, 1, "{out}");
        assert!(out.contains("Fehler: Apple Mail ist nicht lesbar"), "{out}");
        assert_eq!(cloud.apple.calls(), 1);
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
        let code = dispatch(&args, ctx, input, &mut out);
        (code, String::from_utf8_lossy(&out).into_owned())
    }

    fn second_terminal(dir: &TempDir) -> Context {
        Context::demo(
            settings_at(&dir.path("config.json")),
            FakeOptions::default(),
        )
    }

    fn setup_answers() -> (Remote, &'static str) {
        let remote: Remote = vec![
            (
                "alice@gmail.example",
                Ok(vec![info(
                    "alice@gmail.example",
                    "alice@gmail.example",
                    true,
                )]),
            ),
            ("bob@outlook.example", Err(ToolError::msg("offline"))),
        ];
        (remote, "1\n\n\nj\n")
    }

    #[test]
    fn a_revoke_from_another_terminal_while_setup_waits_is_not_undone() -> TestResult {
        let dir = TempDir::new()?;
        let (remote, answers) = setup_answers();
        let cloud = cloudy(
            &dir,
            LINKED,
            &[("google:alice@gmail.example", WRITE_SCOPE)],
            remote,
            crate::provider::fakes::Visit::Nothing,
            crate::http::scripted::Scripted::new(),
        )?;
        let other = second_terminal(&dir);
        let mut input = Typed::new(answers).after_line(4, || {
            let _ = run(&other, &["revoke"], "");
        });
        let (code, out) = run_typed(&cloud.ctx, &["setup"], &mut input);
        assert_eq!(code, 1, "{out}");
        assert!(out.contains("Neue Freigaben:"), "{out}");
        assert!(
            out.contains("Die Konfiguration wurde inzwischen geaendert"),
            "{out}"
        );
        assert!(!out.contains("Gespeichert"), "{out}");
        let rights = stored(&cloud)?;
        assert!(rights.mail_at(Level::Read).is_empty());
        assert!(rights.calendars_at(Level::Read).is_empty());
        assert_eq!(rights.accounts.len(), 2);
        Ok(())
    }

    #[test]
    fn setup_saves_only_what_it_asked_about_and_keeps_other_changes_made_meanwhile() -> TestResult {
        let dir = TempDir::new()?;
        let (remote, answers) = setup_answers();
        let cloud = cloudy(
            &dir,
            LINKED,
            &[("google:alice@gmail.example", WRITE_SCOPE)],
            remote,
            crate::provider::fakes::Visit::Nothing,
            crate::http::scripted::Scripted::new(),
        )?;
        let other = second_terminal(&dir);
        let mut input = Typed::new(answers).after_line(4, || {
            let _ = other.settings.update_rights(|rights| {
                rights.sends_per_hour = 7;
                if let Some(bob) = rights.account_mut(Provider::Microsoft, "bob@outlook.example") {
                    bob.mail = Some(Level::Read);
                }
                Ok(())
            });
        });
        let (code, out) = run_typed(&cloud.ctx, &["setup"], &mut input);
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("Gespeichert in"), "{out}");
        let rights = stored(&cloud)?;
        assert_eq!(rights.sends_per_hour, 7);
        assert_eq!(
            rights.mail_level("google:alice@gmail.example"),
            Some(Level::Read)
        );
        assert_eq!(
            rights.mail_level("microsoft:bob@outlook.example"),
            Some(Level::Read)
        );
        assert_eq!(
            rights.calendar_level("google:alice@gmail.example:alice@gmail.example"),
            Some(Level::Write)
        );
        Ok(())
    }

    #[test]
    fn setup_apple_does_not_bring_back_an_apple_section_that_was_revoked_meanwhile() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, Some(&base()))?;
        let other = second_terminal(&dir);
        let mut input = Typed::new("\n\n\n\n\n\n\nj\n").after_line(8, || {
            let _ = run(&other, &["revoke"], "");
        });
        let (code, out) = run_typed(&ctx, &["setup", "apple"], &mut input);
        assert_eq!(code, 1, "{out}");
        assert!(
            out.contains("Die Konfiguration wurde inzwischen geaendert"),
            "{out}"
        );
        let config = saved(&ctx)?;
        assert!(section(&config, "apple").is_none());
        Ok(())
    }

    #[test]
    fn a_link_keeps_what_another_terminal_changed_during_the_browser_flow() -> TestResult {
        let dir = TempDir::new()?;
        let other = second_terminal(&dir);
        let cloud = meddled(
            &dir,
            &gm(LINKED_GMAIL),
            &[(gm("google:alice@{gmail}").as_str(), WRITE_SCOPE)],
            quiet(),
            crate::provider::fakes::Visit::Good("c"),
            crate::provider::fakes::google_transport(&google_tokens(true)),
            Faults::on_store(move || {
                let _ = run(&other, &["revoke"], "");
            }),
        )?;
        let (code, out) = run(&cloud.ctx, &["link", "google"], "id1\nsecret1\n2\n");
        assert_eq!(code, 0, "{out}");
        assert!(
            out.contains(&gm("Erneut verbunden: alice@{gmail} (gmail). Mail: aus.")),
            "{out}"
        );
        let rights = stored(&cloud)?;
        assert_eq!(rights.accounts.len(), 2);
        assert!(rights.mail_at(Level::Read).is_empty());
        assert!(rights.calendars_at(Level::Read).is_empty());
        Ok(())
    }

    #[test]
    fn a_link_does_not_overwrite_accounts_added_meanwhile_and_respects_the_limit() -> TestResult {
        let dir = TempDir::new()?;
        let path = dir.path("config.json");
        let accounts = |count: usize| {
            let entries: Vec<String> = (0..count)
                .map(|n| format!(r#"{{"provider": "google", "address": "a{n}@gmail.example"}}"#))
                .collect();
            format!(r#"{{"version": 3, "accounts": [{}]}}"#, entries.join(","))
        };
        let crowded = accounts(15);
        let cloud = meddled(
            &dir,
            r#"{"version": 3}"#,
            &[],
            quiet(),
            crate::provider::fakes::Visit::Good("c"),
            crate::provider::fakes::google_transport(&google_tokens(false)),
            Faults::on_store(move || {
                let _ = fs::write(&path, &crowded);
            }),
        )?;
        let (code, out) = run(&cloud.ctx, &["link", "google"], "id1\nsecret1\n1\n");
        assert_eq!(code, 0, "{out}");
        assert_eq!(stored(&cloud)?.accounts.len(), 16);
        let again = TempDir::new()?;
        let path = again.path("config.json");
        let full = accounts(16);
        let cloud = meddled(
            &again,
            r#"{"version": 3}"#,
            &[],
            quiet(),
            crate::provider::fakes::Visit::Good("c"),
            crate::provider::fakes::google_transport(&google_tokens(false)),
            Faults::on_store(move || {
                let _ = fs::write(&path, &full);
            }),
        )?;
        let (code, out) = run(&cloud.ctx, &["link", "google"], "id1\nsecret1\n1\n");
        assert_eq!(code, 1, "{out}");
        assert!(out.contains("Es sind schon 16 Konten verbunden"), "{out}");
        assert_eq!(stored(&cloud)?.accounts.len(), 16);
        assert!(stored(&cloud)?
            .account(Provider::Google, &gm("alice@{gmail}"))
            .is_none());
        assert!(cloud.vault.get(&gm("google:alice@{gmail}")).is_none());
        Ok(())
    }

    #[test]
    fn an_unlink_removes_only_its_account_and_keeps_changes_made_while_it_asked() -> TestResult {
        let dir = TempDir::new()?;
        let cloud = cloudy(
            &dir,
            LINKED,
            &[
                ("google:alice@gmail.example", READ_SCOPE),
                ("microsoft:bob@outlook.example", "Mail.Read"),
            ],
            quiet(),
            crate::provider::fakes::Visit::Nothing,
            crate::http::scripted::Scripted::new(),
        )?;
        let other = second_terminal(&dir);
        let mut input = Typed::new("j\n").after_line(1, || {
            let _ = other.settings.update_rights(|rights| {
                rights.sends_per_hour = 7;
                if let Some(alice) = rights.account_mut(Provider::Google, "alice@gmail.example") {
                    alice.mail = Some(Level::Read);
                }
                Ok(())
            });
        });
        let (code, out) = run_typed(&cloud.ctx, &["unlink", "bob@outlook.example"], &mut input);
        assert_eq!(code, 0, "{out}");
        let rights = stored(&cloud)?;
        assert_eq!(rights.accounts.len(), 1);
        assert_eq!(rights.sends_per_hour, 7);
        assert_eq!(
            rights.mail_level("google:alice@gmail.example"),
            Some(Level::Read)
        );
        Ok(())
    }

    #[test]
    fn every_config_command_gives_up_cleanly_when_another_one_holds_the_lock() -> TestResult {
        for (args, input) in [
            (vec!["revoke"], ""),
            (vec!["unlink", "bob@outlook.example"], "j\n"),
            (vec!["link", "google"], "id1\nsecret1\n1\n"),
            (vec!["setup"], "1\n\n\nj\n"),
        ] {
            let dir = TempDir::new()?;
            let (remote, _) = setup_answers();
            let cloud = meddled(
                &dir,
                LINKED,
                &[
                    ("google:alice@gmail.example", WRITE_SCOPE),
                    ("microsoft:bob@outlook.example", "Mail.Read"),
                ],
                remote,
                crate::provider::fakes::Visit::Good("c"),
                crate::provider::fakes::google_transport(&google_tokens(false)),
                Faults::default(),
            )?;
            let held = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .open(cloud.ctx.settings.lock_path())?;
            held.lock()?;
            let (code, out) = run(&cloud.ctx, &args, input);
            assert_eq!(code, 1, "{args:?}: {out}");
            assert!(
                out.contains("Ein anderer Avery-Befehl aendert gerade die Konfiguration")
                    && out.contains("config.lock")
                    && out.contains("Nichts gespeichert"),
                "{args:?}: {out}"
            );
            assert_eq!(fs::read_to_string(&cloud.ctx.settings.config_path)?, LINKED);
            held.unlock()?;
            drop(held);
            if args.first() == Some(&"revoke") {
                let (code, _) = run(&cloud.ctx, &args, input);
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

    fn bob_and_alice() -> [(&'static str, &'static str); 2] {
        [
            ("google:alice@gmail.example", READ_SCOPE),
            ("microsoft:bob@outlook.example", "Mail.Read"),
        ]
    }

    #[test]
    fn an_unlink_deletes_the_sign_in_first_so_a_failed_config_update_leaves_no_orphan() -> TestResult
    {
        let dir = TempDir::new()?;
        let cloud = cloudy(
            &dir,
            LINKED,
            &bob_and_alice(),
            quiet(),
            crate::provider::fakes::Visit::Nothing,
            crate::http::scripted::Scripted::new(),
        )?;
        let held = lock_held(&cloud.ctx)?;
        let (code, out) = run(&cloud.ctx, &["unlink", "bob@outlook.example"], "j\n");
        assert_eq!(code, 1, "{out}");
        assert!(
            out.contains("die Anmeldung im Schluesselbund ist geloescht, die Konfiguration aber nicht geaendert"),
            "{out}"
        );
        assert!(out.contains("Ein anderer Avery-Befehl"), "{out}");
        assert!(
            out.contains("avery mail unlink microsoft:bob@outlook.example"),
            "{out}"
        );
        assert!(cloud.vault.get("microsoft:bob@outlook.example").is_none());
        assert!(cloud.vault.get("google:alice@gmail.example").is_some());
        assert_eq!(fs::read_to_string(&cloud.ctx.settings.config_path)?, LINKED);
        held.unlock()?;
        let (code, out) = run(&cloud.ctx, &["unlink", "bob@outlook.example"], "j\n");
        assert_eq!(code, 0, "{out}");
        assert!(
            out.contains(
                "bob@outlook.example getrennt (die Anmeldung im Schluesselbund fehlte schon)."
            ),
            "{out}"
        );
        assert_eq!(stored(&cloud)?.accounts.len(), 1);
        Ok(())
    }

    #[test]
    fn a_sign_in_that_cannot_be_deleted_stops_the_unlink_before_the_config_changes() -> TestResult {
        let dir = TempDir::new()?;
        let cloud = meddled(
            &dir,
            LINKED,
            &bob_and_alice(),
            quiet(),
            crate::provider::fakes::Visit::Nothing,
            crate::http::scripted::Scripted::new(),
            Faults::undeletable(),
        )?;
        let (code, out) = run(&cloud.ctx, &["unlink", "bob@outlook.example"], "j\n");
        assert_eq!(code, 1, "{out}");
        assert!(out.contains("could not be removed"), "{out}");
        assert!(out.contains("Schluesselbundverwaltung"), "{out}");
        assert!(out.contains("Nichts geaendert"), "{out}");
        assert!(!out.contains("getrennt."), "{out}");
        assert_eq!(fs::read_to_string(&cloud.ctx.settings.config_path)?, LINKED);
        assert!(cloud.vault.get("microsoft:bob@outlook.example").is_some());
        Ok(())
    }

    #[test]
    fn a_sign_in_without_a_config_entry_is_removed_when_the_provider_is_named() -> TestResult {
        let dir = TempDir::new()?;
        let cloud = cloudy(
            &dir,
            r#"{"version": 3}"#,
            &[("google:orphan@gmail.example", READ_SCOPE)],
            quiet(),
            crate::provider::fakes::Visit::Nothing,
            crate::http::scripted::Scripted::new(),
        )?;
        let (code, out) = run(&cloud.ctx, &["unlink", "orphan@gmail.example"], "j\n");
        assert_eq!(code, 1, "{out}");
        assert!(out.contains("kein Konto direkt verbunden"), "{out}");
        assert!(
            out.contains("avery mail unlink google:<adresse> | microsoft:<adresse>"),
            "{out}"
        );
        assert!(cloud.vault.get("google:orphan@gmail.example").is_some());
        let (code, out) = run(
            &cloud.ctx,
            &["unlink", "microsoft:orphan@gmail.example"],
            "j\n",
        );
        assert_eq!(code, 1, "{out}");
        assert!(out.contains("ist nicht verbunden"), "{out}");
        assert!(!out.contains("[j/N]"), "{out}");
        let (code, out) = run(
            &cloud.ctx,
            &["unlink", "Google:Orphan@Gmail.example"],
            "n\n",
        );
        assert_eq!(code, 1, "{out}");
        assert!(
            out.contains("google:orphan@gmail.example steht nicht in der Konfiguration, aber seine Anmeldung liegt noch im Schluesselbund"),
            "{out}"
        );
        assert!(cloud.vault.get("google:orphan@gmail.example").is_some());
        let (code, out) = run(
            &cloud.ctx,
            &["unlink", "Google:Orphan@Gmail.example"],
            "j\n",
        );
        assert_eq!(code, 0, "{out}");
        assert!(
            out.contains("google:orphan@gmail.example stand nicht in der Konfiguration; die gespeicherte Anmeldung im Schluesselbund ist geloescht."),
            "{out}"
        );
        assert!(cloud.vault.get("google:orphan@gmail.example").is_none());
        assert_eq!(
            fs::read_to_string(&cloud.ctx.settings.config_path)?,
            r#"{"version": 3}"#
        );
        assert_eq!(cloud.apple.calls(), 0);
        Ok(())
    }

    #[test]
    fn an_unlink_with_the_provider_works_without_a_readable_config_and_never_rewrites_it(
    ) -> TestResult {
        for text in ["not json", r#"{"version": 2, "allowed_accounts": []}"#] {
            let dir = TempDir::new()?;
            let cloud = cloudy(
                &dir,
                text,
                &[("google:alice@gmail.example", READ_SCOPE)],
                quiet(),
                crate::provider::fakes::Visit::Nothing,
                crate::http::scripted::Scripted::new(),
            )?;
            let (code, out) = run(&cloud.ctx, &["unlink", "alice@gmail.example"], "j\n");
            assert_eq!(code, 1, "{out}");
            assert!(out.contains("move the file away"), "{out}");
            assert!(cloud.vault.get("google:alice@gmail.example").is_some());
            let (code, out) = run(&cloud.ctx, &["unlink", "google:alice@gmail.example"], "j\n");
            assert_eq!(code, 1, "{out}");
            assert!(
                out.contains("die Anmeldung im Schluesselbund ist geloescht, die Konfiguration aber nicht geaendert"),
                "{out}"
            );
            assert!(out.contains("move the file away"), "{out}");
            assert!(cloud.vault.get("google:alice@gmail.example").is_none());
            assert_eq!(fs::read_to_string(&cloud.ctx.settings.config_path)?, text);
            let (code, out) = run(&cloud.ctx, &["unlink", "google:alice@gmail.example"], "j\n");
            assert_eq!(code, 1, "{out}");
            assert_eq!(fs::read_to_string(&cloud.ctx.settings.config_path)?, text);
        }
        Ok(())
    }

    #[test]
    fn a_link_removes_the_fresh_sign_in_again_when_the_config_cannot_be_updated() -> TestResult {
        let dir = TempDir::new()?;
        let cloud = cloudy(
            &dir,
            r#"{"version": 3}"#,
            &[],
            quiet(),
            crate::provider::fakes::Visit::Good("c"),
            crate::provider::fakes::google_transport(&google_tokens(false)),
        )?;
        let held = lock_held(&cloud.ctx)?;
        let (code, out) = run(&cloud.ctx, &["link", "google"], "id1\nsecret1\n1\n");
        assert_eq!(code, 1, "{out}");
        assert!(out.contains("Ein anderer Avery-Befehl"), "{out}");
        assert!(
            out.contains(
                "Die eben im Schluesselbund gespeicherte Anmeldung wurde wieder entfernt."
            ),
            "{out}"
        );
        assert!(!out.contains("Verbunden:"), "{out}");
        assert!(cloud.vault.get(&gm("google:alice@{gmail}")).is_none());
        assert_eq!(
            fs::read_to_string(&cloud.ctx.settings.config_path)?,
            r#"{"version": 3}"#
        );
        held.unlock()?;
        let (code, out) = run(&cloud.ctx, &["link", "google"], "id1\nsecret1\n1\n");
        assert_eq!(code, 0, "{out}");
        assert!(cloud.vault.get(&gm("google:alice@{gmail}")).is_some());
        Ok(())
    }

    #[test]
    fn a_link_that_finds_the_list_full_removes_its_sign_in_and_says_so_when_that_fails_too(
    ) -> TestResult {
        let accounts: Vec<String> = (0..16)
            .map(|n| format!(r#"{{"provider": "google", "address": "a{n}@gmail.example"}}"#))
            .collect();
        let full = format!(r#"{{"version": 3, "accounts": [{}]}}"#, accounts.join(","));
        for undeletable in [false, true] {
            let dir = TempDir::new()?;
            let path = dir.path("config.json");
            let text = full.clone();
            let hook = move || {
                let _ = fs::write(&path, &text);
            };
            let faults = if undeletable {
                Faults {
                    fail_delete: true,
                    ..Faults::on_store(hook)
                }
            } else {
                Faults::on_store(hook)
            };
            let cloud = meddled(
                &dir,
                r#"{"version": 3}"#,
                &[],
                quiet(),
                crate::provider::fakes::Visit::Good("c"),
                crate::provider::fakes::google_transport(&google_tokens(false)),
                faults,
            )?;
            let (code, out) = run(&cloud.ctx, &["link", "google"], "id1\nsecret1\n1\n");
            assert_eq!(code, 1, "{undeletable}: {out}");
            assert!(out.contains("Es sind schon 16 Konten verbunden"), "{out}");
            assert_eq!(fs::read_to_string(&cloud.ctx.settings.config_path)?, full);
            if undeletable {
                assert!(
                    out.contains("Die eben im Schluesselbund gespeicherte Anmeldung blieb")
                        && out
                            .contains(&gm("Entfernen mit: avery mail unlink google:alice@{gmail}")),
                    "{out}"
                );
                assert!(cloud.vault.get(&gm("google:alice@{gmail}")).is_some());
            } else {
                assert!(out.contains("wieder entfernt"), "{out}");
                assert!(cloud.vault.get(&gm("google:alice@{gmail}")).is_none());
            }
        }
        Ok(())
    }

    #[test]
    fn a_link_again_that_cannot_save_removes_the_new_sign_in_and_the_account_asks_for_a_new_link(
    ) -> TestResult {
        let dir = TempDir::new()?;
        let cloud = cloudy(
            &dir,
            &gm(LINKED_GMAIL),
            &[(gm("google:alice@{gmail}").as_str(), WRITE_SCOPE)],
            quiet(),
            crate::provider::fakes::Visit::Good("c"),
            crate::provider::fakes::google_transport(&google_tokens(false)),
        )?;
        let held = lock_held(&cloud.ctx)?;
        let (code, out) = run(&cloud.ctx, &["link", "google"], "id1\nsecret1\n1\n");
        assert_eq!(code, 1, "{out}");
        assert!(cloud.vault.get(&gm("google:alice@{gmail}")).is_none());
        held.unlock()?;
        assert_eq!(
            fs::read_to_string(&cloud.ctx.settings.config_path)?,
            gm(LINKED_GMAIL)
        );
        let (code, out) = run(&cloud.ctx, &["accounts"], "");
        assert_eq!(code, 0, "{out}");
        assert!(
            out.contains(&gm("alice@{gmail}  [gmail]  alice@{gmail}  [schreiben]  [Anmeldung fehlt, neu verbinden mit: avery mail link]")),
            "{out}"
        );
        Ok(())
    }

    #[test]
    fn no_vault_call_ever_happens_while_the_config_lock_is_held() -> TestResult {
        let dir = TempDir::new()?;
        let probe = Probe::new(settings_at(&dir.path("config.json")).lock_path());
        let remote: Remote = vec![(
            "alice@gmail.example",
            Ok(vec![info(
                "alice@gmail.example",
                "alice@gmail.example",
                true,
            )]),
        )];
        let cloud = meddled(
            &dir,
            &gm(LINKED_GMAIL),
            &[
                (gm("google:alice@{gmail}").as_str(), WRITE_SCOPE),
                ("microsoft:bob@outlook.example", "Mail.Read"),
            ],
            remote,
            crate::provider::fakes::Visit::Good("c"),
            crate::provider::fakes::google_transport(&google_tokens(true)),
            Faults::probed(&probe),
        )?;
        for (args, input) in [
            (vec!["link", "google"], "id1\nsecret1\n2\n"),
            (vec!["accounts"], ""),
            (vec!["status"], ""),
            (vec!["calendars"], ""),
            (vec!["selftest"], ""),
            (vec!["setup"], "\n\n\n\n\n\nj\n"),
            (vec!["revoke"], ""),
            (vec!["unlink", "bob@outlook.example"], "j\n"),
            (vec!["unlink", "google:orphan@gmail.example"], "j\n"),
        ] {
            let (_, out) = run(&cloud.ctx, &args, input);
            assert!(!out.contains("Ein anderer Avery-Befehl"), "{args:?}: {out}");
        }
        assert!(probe.calls() >= 8, "{}", probe.calls());
        assert_eq!(probe.violations(), 0);
        let refused = TempDir::new()?;
        let second = Probe::new(settings_at(&refused.path("config.json")).lock_path());
        let path = refused.path("config.json");
        let accounts: Vec<String> = (0..16)
            .map(|n| format!(r#"{{"provider": "google", "address": "a{n}@gmail.example"}}"#))
            .collect();
        let full = format!(r#"{{"version": 3, "accounts": [{}]}}"#, accounts.join(","));
        let faults = Faults {
            probe: Some(second.clone()),
            ..Faults::on_store(move || {
                let _ = fs::write(&path, &full);
            })
        };
        let rolled_back = meddled(
            &refused,
            r#"{"version": 3}"#,
            &[],
            quiet(),
            crate::provider::fakes::Visit::Good("c"),
            crate::provider::fakes::google_transport(&google_tokens(false)),
            faults,
        )?;
        let (code, out) = run(&rolled_back.ctx, &["link", "google"], "id1\nsecret1\n1\n");
        assert_eq!(code, 1, "{out}");
        assert!(out.contains("wieder entfernt"), "{out}");
        assert!(second.calls() >= 2, "{}", second.calls());
        assert_eq!(second.violations(), 0);
        Ok(())
    }

    fn apple_copy(id: &str, name: &str, address: &str) -> MailAccount {
        MailAccount {
            id: id.to_string(),
            name: name.to_string(),
            kind: "imap".to_string(),
            addresses: vec![address.to_string()],
        }
    }

    fn listing_context(
        dir: &TempDir,
        config: &str,
        accounts: Vec<MailAccount>,
    ) -> Result<Context, Box<dyn std::error::Error>> {
        let settings = settings_at(&dir.path("config.json"));
        fs::write(&settings.config_path, config)?;
        let listing = crate::testutil::Listing {
            accounts,
            calendars: Vec::new(),
        };
        Ok(Context::new(
            settings,
            Box::new(listing.clone()),
            Box::new(listing),
            Linker::inert(),
        ))
    }

    #[test]
    fn setup_apple_keeps_a_directly_linked_address_at_zero_and_still_sets_up_the_rest() -> TestResult
    {
        let dir = TempDir::new()?;
        let alice = gm("alice@{gmail}");
        let config = gm(
            r#"{"version": 3, "accounts": [{"provider": "google", "address": "alice@{gmail}", "mail": "write"}]}"#,
        );
        let ctx = listing_context(
            &dir,
            &config,
            vec![
                apple_copy("ACC-ALICE", &alice, &alice),
                apple_copy("ACC-OTHER", "Privat iCloud", "me@icloud.example"),
            ],
        )?;
        let (code, out) = run(&ctx, &["setup", "apple"], "1\nj\n");
        assert_eq!(code, 0, "{out}");
        assert!(
            out.contains(&gm(
                "alice@{gmail} (alice@{gmail}): schon direkt verbunden, bleibt hier aus (zum Wechseln zuerst: avery mail unlink alice@{gmail})."
            )),
            "{out}"
        );
        assert_eq!(out.matches("Stufe 0=aus").count(), 1, "{out}");
        assert!(out.contains("Privat iCloud"), "{out}");
        let rights = ctx.settings.stored_rights().map_err(|e| e.message())?;
        let ids: Vec<&str> = rights
            .apple
            .mail
            .iter()
            .map(|(account, _)| account.id.as_str())
            .collect();
        assert_eq!(ids, ["ACC-OTHER"]);
        assert_eq!(rights.accounts.len(), 1);
        assert_eq!(
            rights.mail_level(&gm("google:alice@{gmail}")),
            Some(Level::Write)
        );
        let (account, _) = ctx
            .settings
            .resolve_writable(Some(&Json::Str(alice.clone())))
            .map_err(|e| e.message())?;
        assert_eq!(account.id, gm("google:alice@{gmail}"));
        Ok(())
    }

    #[test]
    fn setup_apple_with_nothing_but_directly_linked_accounts_changes_nothing_and_does_not_fail(
    ) -> TestResult {
        let dir = TempDir::new()?;
        let alice = gm("alice@{gmail}");
        let config = gm(
            r#"{"version": 3, "accounts": [{"provider": "google", "address": "alice@{gmail}", "mail": "read"}]}"#,
        );
        let ctx = listing_context(
            &dir,
            &config,
            vec![apple_copy(
                "ACC-ALICE",
                &alice.to_uppercase(),
                &alice.to_uppercase(),
            )],
        )?;
        let (code, out) = run(&ctx, &["setup", "apple"], "");
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("schon direkt verbunden"), "{out}");
        assert!(out.contains("Unveraendert."), "{out}");
        assert!(!out.contains("Stufe 0=aus"), "{out}");
        assert_eq!(fs::read_to_string(&ctx.settings.config_path)?, config);
        Ok(())
    }

    #[test]
    fn linking_an_address_that_apple_already_serves_is_refused_with_the_way_out() -> TestResult {
        let dir = TempDir::new()?;
        let config = gm(
            r#"{"version": 3, "apple": {"mail": [{"id": "ACC-ALICE", "name": "alice@{gmail}", "type": "imap", "addresses": ["alice@{gmail}"], "level": "write"}]}}"#,
        );
        let cloud = cloudy(
            &dir,
            &config,
            &[],
            quiet(),
            crate::provider::fakes::Visit::Good("c"),
            crate::provider::fakes::google_transport(&google_tokens(false)),
        )?;
        let (code, out) = run(&cloud.ctx, &["link", "google"], "id1\nsecret1\n1\n");
        assert_eq!(code, 1, "{out}");
        assert!(
            out.contains(&gm(
                "Die Adresse alice@{gmail} ist schon ueber Apple Mail und Kalender eingebunden."
            )),
            "{out}"
        );
        assert!(
            out.contains(
                "Zuerst dort auf Stufe 0 stellen (avery mail setup apple), dann erneut verbinden."
            ),
            "{out}"
        );
        assert!(out.contains("wieder entfernt"), "{out}");
        assert!(cloud.vault.get(&gm("google:alice@{gmail}")).is_none());
        assert_eq!(fs::read_to_string(&cloud.ctx.settings.config_path)?, config);
        let (code, out) = run(&cloud.ctx, &["accounts"], "");
        assert_eq!(code, 0, "{out}");
        assert_eq!(out.matches("[imap]").count(), 1, "{out}");
        assert!(!out.contains("[gmail]"), "{out}");
        Ok(())
    }

    #[test]
    fn the_reproduced_double_enablement_is_refused_by_the_config_and_never_reaches_the_tools(
    ) -> TestResult {
        let dir = TempDir::new()?;
        let config = gm(
            r#"{"version": 3, "accounts": [{"provider": "google", "address": "alice@{gmail}", "mail": "write"}],
               "apple": {"mail": [{"id": "ACC-ALICE", "name": "alice@{gmail}", "type": "imap", "addresses": ["alice@{gmail}"], "level": "write"}]}}"#,
        );
        let ctx = real(&dir, Some(&config))?;
        for args in [
            vec!["accounts"],
            vec!["status"],
            vec!["setup"],
            vec!["revoke"],
        ] {
            let (code, out) = run(&ctx, &args, "j\n");
            assert_eq!(code, 1, "{args:?}: {out}");
            assert!(
                out.contains(
                    "apple.mail[0].addresses[0] is already linked directly as accounts[0]"
                ),
                "{args:?}: {out}"
            );
            assert_eq!(fs::read_to_string(&ctx.settings.config_path)?, config);
        }
        let alice = Json::Str(gm("alice@{gmail}"));
        let Err(error) = ctx.settings.resolve_writable(Some(&alice)) else {
            return Err("a double enablement must never resolve".into());
        };
        assert!(
            error
                .message()
                .contains("is already linked directly as accounts[0]"),
            "{error}"
        );
        assert!(ctx.settings.allowed_accounts().is_err());
        Ok(())
    }

    #[test]
    fn a_direct_link_made_while_setup_apple_waits_is_not_doubled_by_the_apple_entry() -> TestResult
    {
        let dir = TempDir::new()?;
        let alice = gm("alice@{gmail}");
        let ctx = listing_context(
            &dir,
            r#"{"version": 3}"#,
            vec![apple_copy("ACC-ALICE", &alice, &alice)],
        )?;
        let other = second_terminal(&dir);
        let address = alice.clone();
        let mut input = Typed::new("1\nj\n").after_line(2, move || {
            let _ = other.settings.update_rights(|rights| {
                rights.add_account(
                    RemoteAccount::new(Provider::Google, &address).ok_or(Refusal::Changed)?,
                )
            });
        });
        let (code, out) = run_typed(&ctx, &["setup", "apple"], &mut input);
        assert_eq!(code, 1, "{out}");
        assert!(
            out.contains("Die Konfiguration wurde inzwischen geaendert"),
            "{out}"
        );
        let rights = ctx.settings.stored_rights().map_err(|e| e.message())?;
        assert_eq!(rights.accounts.len(), 1);
        assert!(!rights.apple.enabled());
        Ok(())
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
    fn what_the_sanitiser_makes_of_apple_names_always_passes_the_config_validator() -> TestResult {
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
            let remote = calendar_name(&name, &gm("alice@{gmail}"));
            let mut rights = Rights::empty();
            rights.apple.mail.push((account.clone(), Level::Read));
            rights.apple.calendars.push((calendar, Level::Read));
            let mut linked =
                RemoteAccount::new(Provider::Google, &gm("alice@{gmail}")).ok_or("account")?;
            linked.calendars.push(CalendarGrant {
                id: "primary".to_string(),
                name: remote.clone(),
                level: Level::Read,
            });
            rights.accounts.push(linked);
            let parsed = Rights::parse(&rights.to_config())
                .map_err(|what| format!("{name:?} was saved but is refused: {what}"))?;
            assert_eq!(parsed.apple.mail[0].0.addresses, account.addresses);
            assert_eq!(parsed.apple.mail[0].0.kind, account.kind);
            assert_eq!(
                one_line_str(&account.name, MAX_NAME),
                account.name,
                "{name:?}"
            );
            assert_eq!(one_line_str(&remote, MAX_NAME), remote, "{name:?}");
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
            &mut Console {
                input: &mut input,
                out: &mut out,
            },
            &UpdateError::Invalid("apple.mail[0].name must be one line".to_string()),
        );
        let text = String::from_utf8_lossy(&out).into_owned();
        assert_eq!(code, 1);
        assert!(
            text.contains("Interner Fehler: Die neue Konfiguration waere ungueltig (apple.mail[0].name must be one line). Nichts gespeichert."),
            "{text}"
        );
        assert!(!std::path::Path::new(&ctx.settings.config_path).exists());
        Ok(())
    }

    #[test]
    fn nasty_apple_names_never_lock_the_config() -> TestResult {
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
            Linker::inert(),
        );
        let count = named.len();
        let (code, out) = run(
            &ctx,
            &["setup", "apple"],
            &format!("{}j\n", "1\n".repeat(2 * count)),
        );
        assert_eq!(code, 0, "{out}");
        let rights = ctx.settings.stored_rights().map_err(|e| e.message())?;
        assert_eq!(rights.apple.mail.len(), count);
        assert_eq!(rights.apple.calendars.len(), count);
        let stored = fs::read_to_string(&ctx.settings.config_path)?;
        for hidden in ["\\u200b", "\\u202e", "\\u2066", "\\t", "\\n"] {
            assert!(!stored.contains(hidden), "{hidden}: {stored}");
        }
        for (calendar, _) in &rights.apple.calendars {
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
    fn the_five_config_commands_refuse_in_demo_mode_and_touch_nothing() -> TestResult {
        for (args, input) in [
            (vec!["link", "google"], "id\nsecret\n1\n"),
            (vec!["unlink", "bob@outlook.example"], "j\n"),
            (vec!["unlink", "google:alice@gmail.example"], "j\n"),
            (vec!["setup"], "1\n1\n1\nj\n"),
            (vec!["setup", "apple"], "1\n1\n1\nj\n"),
            (vec!["revoke"], ""),
        ] {
            let dir = TempDir::new()?;
            let mut cloud = cloudy(
                &dir,
                LINKED,
                &bob_and_alice(),
                quiet(),
                crate::provider::fakes::Visit::Good("c"),
                crate::provider::fakes::google_transport(&google_tokens(false)),
            )?;
            cloud.ctx.settings.fake = true;
            let (code, out) = run(&cloud.ctx, &args, input);
            assert_eq!(code, 1, "{args:?}: {out}");
            assert!(
                out.contains("Im Demo-Modus (MAIL_MCP_FAKE=1) nicht moeglich"),
                "{args:?}: {out}"
            );
            assert!(
                !out.contains("[j/N]") && !out.contains("Client-ID"),
                "{args:?}: {out}"
            );
            assert_eq!(fs::read_to_string(&cloud.ctx.settings.config_path)?, LINKED);
            assert!(!std::path::Path::new(&cloud.ctx.settings.lock_path()).exists());
            assert_eq!(cloud.vault.count(), 2, "{args:?}");
            assert_eq!(cloud.apple.calls(), 0, "{args:?}");
        }
        let (code, out) = run(&real(&TempDir::new()?, None)?, &["setup", "nonsense"], "");
        assert_eq!(code, 2, "{out}");
        Ok(())
    }
}
