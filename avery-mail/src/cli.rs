use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::thread;
use std::time::{Duration, Instant};

use crate::account::{Account, MailAccount};
use crate::backend::{ComposeRequest, MessageQuery, Mode, ReplySpec};
use crate::config::CONFIG_VERSION;
use crate::context::Context;
use crate::error::ToolError;
use crate::json::{Json, Object};
use crate::localtime;
use crate::sendlog::{self, Outcome};
use crate::server::{self, Line};
use crate::text::{one_line_str, to_int};
use crate::texts::{
    COMMANDS, DEFAULT_SENDS_PER_HOUR, HELP, MAX_DRAFTS_PER_HOUR, MAX_RECIPIENTS, MAX_SEND_CHARS,
    MAX_SEND_LINES, NO_ACCOUNTS, VERSION, WRITE_OFF,
};
use crate::write_tools;

const MAX_ANSWER_BYTES: usize = 8192;
const SENDLOG_LINES: usize = 200;
const SENDLOG_SHOWN: usize = 20;
const SUGGESTION_CUTOFF: f64 = 0.6;
const MAX_COMMAND_CHARS: usize = 100;

struct Console<'a> {
    input: &'a mut dyn BufRead,
    out: &'a mut dyn Write,
}

impl Console<'_> {
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

    fn confirm(&mut self, prompt: &str) -> bool {
        self.ask(prompt).is_some_and(|answer| {
            matches!(
                answer.trim().to_lowercase().as_str(),
                "j" | "ja" | "y" | "yes"
            )
        })
    }
}

fn plain(account: &Account) -> MailAccount {
    MailAccount {
        id: account.id.clone(),
        name: account.name.clone(),
        kind: account.kind.clone(),
        addresses: account.addresses.clone(),
    }
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

fn print_accounts(
    console: &mut Console,
    accounts: &[MailAccount],
    enabled: &HashSet<String>,
    writable: &HashSet<String>,
) {
    for (index, account) in accounts.iter().enumerate() {
        let mark = if enabled.contains(&account.id) {
            '*'
        } else {
            ' '
        };
        let extra = if writable.contains(&account.id) {
            "  [Entwuerfe/Senden]"
        } else {
            ""
        };
        console.say(&format!(
            "{mark} {}) {}{extra}",
            index + 1,
            describe(account)
        ));
    }
}

fn ids(accounts: &[Account]) -> HashSet<String> {
    accounts.iter().map(|account| account.id.clone()).collect()
}

fn failed(console: &mut Console, error: &ToolError) -> u8 {
    console.say(&format!("Fehler: {error}"));
    1
}

fn parse_picks(answer: &str, count: usize) -> Result<Vec<usize>, &'static str> {
    let mut picks: Vec<i64> = Vec::new();
    for piece in answer
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|piece| !piece.is_empty())
    {
        picks.push(piece.parse::<i64>().map_err(|_| "Ungueltige Eingabe.")?);
    }
    picks.sort_unstable();
    picks.dedup();
    let mut chosen = Vec::new();
    for pick in picks {
        match usize::try_from(pick) {
            Ok(number) if (1..=count).contains(&number) => chosen.push(number - 1),
            _ => return Err("Nummer ausserhalb der Liste."),
        }
    }
    Ok(chosen)
}

fn choose<'a, T>(
    console: &mut Console,
    prompt: &str,
    items: &'a [T],
) -> Result<Option<Vec<&'a T>>, u8> {
    let Some(answer) = console.ask(prompt) else {
        console.say("Ungueltige Eingabe.");
        return Err(1);
    };
    let answer = answer.trim();
    if answer.is_empty() {
        console.say("Unveraendert.");
        return Ok(None);
    }
    match parse_picks(answer, items.len()) {
        Ok(indexes) => Ok(Some(
            indexes
                .into_iter()
                .filter_map(|index| items.get(index))
                .collect(),
        )),
        Err(message) => {
            console.say(message);
            Err(1)
        }
    }
}

fn load_config(ctx: &Context, console: &mut Console) -> Result<Object, u8> {
    ctx.settings
        .load_config()
        .map_err(|error| failed(console, &error))
}

fn save_config(ctx: &Context, console: &mut Console, config: &Object) -> u8 {
    match ctx.settings.save_config(config) {
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

fn cmd_accounts(ctx: &Context, console: &mut Console) -> u8 {
    let listing = (|| {
        let enabled = ids(&ctx.settings.allowed_accounts()?);
        let writable = ids(&ctx.settings.write_settings()?.accounts);
        let accounts = ctx.backend.all_accounts()?;
        Ok::<_, ToolError>((accounts, enabled, writable))
    })();
    let (accounts, enabled, writable) = match listing {
        Ok(listing) => listing,
        Err(error) => return failed(console, &error),
    };
    if accounts.is_empty() {
        console.say("Mail kennt noch keine Konten (Systemeinstellungen > Internetaccounts).");
        return 0;
    }
    print_accounts(console, &accounts, &enabled, &writable);
    console.say("\n* = zum Lesen fuer den Agenten freigegeben");
    console.say("[Entwuerfe/Senden] = darf Entwuerfe anlegen und Mails senden (jede Mail nur nach deinem Klick)");
    0
}

fn cmd_setup(ctx: &Context, console: &mut Console) -> u8 {
    let listing = (|| {
        let enabled = ids(&ctx.settings.allowed_accounts()?);
        let accounts = ctx.backend.all_accounts()?;
        Ok::<_, ToolError>((accounts, enabled))
    })();
    let (accounts, enabled) = match listing {
        Ok(listing) => listing,
        Err(error) => return failed(console, &error),
    };
    if accounts.is_empty() {
        console.say("Mail kennt noch keine Konten. Zuerst in den Systemeinstellungen unter Internetaccounts anmelden.");
        return 1;
    }
    print_accounts(console, &accounts, &enabled, &HashSet::new());
    console.say("\nNur PRIVATE Konten freigeben, niemals geschaeftliche.");
    let chosen = match choose(
        console,
        "Nummern der freizugebenden Konten (z. B. 2 3), leer = nichts aendern: ",
        &accounts,
    ) {
        Ok(Some(chosen)) => chosen,
        Ok(None) => return 0,
        Err(code) => return code,
    };
    console.say("\nFreigabe fuer:");
    for account in &chosen {
        console.say(&format!("  - {}", describe(account)));
    }
    if !console.confirm("Korrekt? [j/N] ") {
        console.say("Abgebrochen.");
        return 1;
    }
    let previous = match load_config(ctx, console) {
        Ok(previous) => previous,
        Err(code) => return code,
    };
    let chosen_ids: HashSet<&str> = chosen.iter().map(|account| account.id.as_str()).collect();
    let mut config = Object::new();
    config.insert("version", Json::Int(CONFIG_VERSION));
    config.insert(
        "allowed_accounts",
        Json::Arr(chosen.iter().map(|account| account.to_json()).collect()),
    );
    if let Some(Json::Obj(write)) = previous.get("write") {
        let kept: Vec<Json> = match write.get("accounts") {
            Some(Json::Arr(items)) => items
                .iter()
                .filter(|item| item.as_str().is_some_and(|id| chosen_ids.contains(id)))
                .cloned()
                .collect(),
            _ => Vec::new(),
        };
        if !kept.is_empty() {
            let mut section = write.clone();
            section.insert("accounts", Json::Arr(kept));
            config.insert("write", Json::Obj(section));
        }
    }
    let code = save_config(ctx, console, &config);
    if code == 0 {
        console.say(&format!("Gespeichert in {}", ctx.settings.config_path));
    }
    code
}

fn cmd_revoke(ctx: &Context, console: &mut Console) -> u8 {
    if let Err(code) = load_config(ctx, console) {
        return code;
    }
    let mut config = Object::new();
    config.insert("version", Json::Int(CONFIG_VERSION));
    config.insert("allowed_accounts", Json::Arr(Vec::new()));
    let code = save_config(ctx, console, &config);
    if code == 0 {
        console.say("Alle Freigaben entfernt (auch Entwuerfe und Senden). Der Agent sieht keine Konten mehr.");
    }
    code
}

fn cmd_write_enable(ctx: &Context, console: &mut Console) -> u8 {
    let listing = (|| {
        let accounts = ctx.settings.allowed_accounts()?;
        let current = ids(&ctx.settings.write_settings()?.accounts);
        Ok::<_, ToolError>((accounts, current))
    })();
    let (accounts, current) = match listing {
        Ok(listing) => listing,
        Err(error) => return failed(console, &error),
    };
    if accounts.is_empty() {
        console.say(NO_ACCOUNTS);
        return 1;
    }
    let shown: Vec<MailAccount> = accounts.iter().map(plain).collect();
    print_accounts(console, &shown, &ids(&accounts), &current);
    console.say(
        "\nDamit darf der Agent fuer die gewaehlten Konten Entwuerfe anlegen und Mails senden.",
    );
    console
        .say("Jede Mail wird erst gesendet, wenn du in einem macOS-Dialog auf 'Senden' klickst;");
    console.say("der Agent kann diesen Dialog nicht bedienen.");
    console.say(&format!(
        "Grenzen: Klartext, kein Bcc, keine Anhaenge, max. {MAX_RECIPIENTS} Empfaenger, max. {MAX_SEND_CHARS} Zeichen pro gesendeter Mail,"
    ));
    console.say(&format!(
        "         max. {MAX_SEND_LINES} Zeilen, max. {DEFAULT_SENDS_PER_HOUR} gesendete Mails und max. {MAX_DRAFTS_PER_HOUR} Entwuerfe pro Stunde. Jeder Versuch steht in {}",
        ctx.settings.sendlog_path()
    ));
    let chosen = match choose(
        console,
        "Nummern der Konten mit Entwurf/Senden (z. B. 1 2), leer = nichts aendern: ",
        &accounts,
    ) {
        Ok(Some(chosen)) => chosen,
        Ok(None) => return 0,
        Err(code) => return code,
    };
    console.say("\nEntwuerfe und Senden (mit Freigabe) fuer:");
    for account in &chosen {
        console.say(&format!(
            "  - {}  {}",
            account.name,
            if account.addresses.is_empty() {
                "-".to_string()
            } else {
                account.addresses.join(", ")
            }
        ));
    }
    if !console.confirm("Korrekt? [j/N] ") {
        console.say("Abgebrochen.");
        return 1;
    }
    let mut config = match load_config(ctx, console) {
        Ok(config) => config,
        Err(code) => return code,
    };
    let old = match config.get("write") {
        Some(Json::Obj(old)) => old.get("max_per_hour").cloned(),
        _ => None,
    };
    let limit = match to_int(old.as_ref(), DEFAULT_SENDS_PER_HOUR, 1, 30) {
        Ok(limit) => limit,
        Err(error) => return failed(console, &error),
    };
    let mut write = Object::new();
    write.insert(
        "accounts",
        Json::Arr(
            chosen
                .iter()
                .map(|account| Json::from(account.id.as_str()))
                .collect(),
        ),
    );
    write.insert("max_per_hour", Json::Int(limit));
    config.insert("version", Json::Int(CONFIG_VERSION));
    config.insert("write", Json::Obj(write));
    let code = save_config(ctx, console, &config);
    if code == 0 {
        console
            .say("Gespeichert. Goose danach neu starten. Pruefen mit: avery mail selftest-write");
    }
    code
}

fn cmd_write_disable(ctx: &Context, console: &mut Console) -> u8 {
    let mut config = match load_config(ctx, console) {
        Ok(config) => config,
        Err(code) => return code,
    };
    config.remove("write");
    let code = save_config(ctx, console, &config);
    if code == 0 {
        console.say("Entwuerfe und Senden sind aus. Lesen bleibt wie freigegeben.");
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

fn cmd_status(ctx: &Context, console: &mut Console) -> u8 {
    let state = (|| {
        let accounts = ctx.settings.allowed_accounts()?;
        let writable = ctx.settings.write_settings()?.accounts;
        Ok::<_, ToolError>((accounts, writable))
    })();
    let (accounts, writable) = match state {
        Ok(state) => state,
        Err(error) => return failed(console, &error),
    };
    let labels = |list: &[Account]| {
        list.iter()
            .map(|account| account.label.as_str())
            .collect::<Vec<_>>()
            .join(", ")
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
    console.say(&format!(
        "Lesen freigegeben fuer: {}",
        if accounts.is_empty() {
            "kein Konto".to_string()
        } else {
            labels(&accounts)
        }
    ));
    console.say(&format!(
        "Entwuerfe/Senden: {}",
        if writable.is_empty() {
            "aus".to_string()
        } else {
            labels(&writable)
        }
    ));
    0
}

fn timed<T>(action: impl FnOnce() -> Result<T, ToolError>) -> (Duration, Result<T, ToolError>) {
    let started = Instant::now();
    let result = action();
    (started.elapsed(), result)
}

fn cmd_selftest(ctx: &Context, console: &mut Console) -> u8 {
    let accounts = match ctx.settings.allowed_accounts() {
        Ok(accounts) => accounts,
        Err(error) => {
            console.say(&format!("FAIL  config: {error}"));
            return 1;
        }
    };
    if accounts.is_empty() {
        console.say(NO_ACCOUNTS);
        return 1;
    }
    let mut failures = 0usize;
    for (number, account) in accounts.iter().enumerate() {
        let tag = format!("Konto {}", number + 1);
        let mut step =
            |console: &mut Console, title: &str, result: (Duration, Result<String, ToolError>)| {
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
                        failures += 1;
                        console.say(&format!("FAIL  {tag}  {title}: {error}"));
                        false
                    }
                }
            };
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
        if !step(console, "Konto erreichbar", reachable) {
            continue;
        }
        let boxes = timed(|| {
            Ok(format!(
                "{} Mailboxen",
                ctx.backend.mailboxes(account)?.len()
            ))
        });
        step(console, "Mailboxen lesen", boxes);
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
        if step(console, "Posteingang listen", listing) {
            let reading = timed(|| {
                let Some(id) = &first else {
                    return Ok("uebersprungen (keine Nachricht in 30 Tagen)".to_string());
                };
                let id = id
                    .parse::<i64>()
                    .map_err(|_| ToolError::msg("Nachrichten-ID ist keine Zahl"))?;
                let message = ctx.backend.read(account, None, id, 500)?;
                Ok(format!(
                    "Text {} Zeichen, {} Anhaenge",
                    message
                        .body_chars
                        .map_or_else(|| "?".to_string(), |count| count.to_string()),
                    message.attachments.len()
                ))
            });
            step(console, "Nachricht lesen", reading);
        }
        let search = timed(|| {
            Ok(format!(
                "{} Treffer",
                ctx.backend
                    .messages(account, &probe_query(30, Some("e"), 5))?
                    .total
            ))
        });
        step(console, "Suche", search);
    }
    console.say("");
    if failures > 0 {
        console.say(&format!("{failures} Pruefung(en) fehlgeschlagen."));
        return 1;
    }
    console.say("Alle Pruefungen bestanden. Es wurde nichts gesendet, geloescht oder verschoben; Inhalte wurden nicht ausgegeben.");
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

fn cmd_selftest_write(ctx: &Context, console: &mut Console) -> u8 {
    let settings = match ctx.settings.write_settings() {
        Ok(settings) => settings,
        Err(error) => {
            console.say(&format!("FAIL  config: {error}"));
            return 1;
        }
    };
    if settings.accounts.is_empty() {
        console.say(WRITE_OFF);
        return 1;
    }
    let mut failures: Vec<String> = Vec::new();
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
            let mut found: Option<i64> = None;
            for number in 0..rounds {
                let query = MessageQuery {
                    field: "subject".to_string(),
                    ..probe_query(2, Some("Scout Selbsttest - Mail"), 3)
                };
                if let Ok(list) = ctx.backend.messages(account, &query) {
                    found = list
                        .items
                        .first()
                        .and_then(|item| item.id.parse::<i64>().ok());
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
                        id,
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
    console.say("");
    if !failures.is_empty() {
        console.say(&format!("{} Pruefung(en) fehlgeschlagen.", failures.len()));
        return 1;
    }
    console.say("Fertig. Test-Entwurf und Test-Mail kannst du loeschen.");
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
        "setup" => cmd_setup(ctx, &mut console),
        "revoke" => cmd_revoke(ctx, &mut console),
        "selftest" => cmd_selftest(ctx, &mut console),
        "write-enable" => cmd_write_enable(ctx, &mut console),
        "write-disable" => cmd_write_disable(ctx, &mut console),
        "sendlog" => cmd_sendlog(ctx, &mut console),
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
    use crate::fake::{FakeBackend, FakeOptions};
    use crate::testutil::{j, log_entry, settings_at, TempDir, TestResult};
    use std::fs;
    use std::io::Cursor;

    const ACCOUNT_1: &str = r#"{"id": "ACC-1", "name": "Privat Gmail", "type": "imap", "addresses": ["me@gmail.example"]}"#;
    const ACCOUNT_2: &str =
        r#"{"id": "ACC-2", "name": "Outlook", "type": "imap", "addresses": ["me@gmail.example"]}"#;

    fn base() -> String {
        format!(r#"{{"version": 2, "allowed_accounts": [{ACCOUNT_1}, {ACCOUNT_2}]}}"#)
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
        Ok(Context::new(settings, Box::new(FakeBackend::new(options))))
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

    fn write_accounts(config: &Json) -> Vec<String> {
        let write = config
            .as_object()
            .and_then(|o| o.get("write"))
            .and_then(Json::as_object);
        crate::account::string_list(write.and_then(|w| w.get("accounts")))
    }

    struct Untouchable;

    impl crate::backend::Backend for Untouchable {
        fn all_accounts(&self) -> crate::error::ToolResult<Vec<MailAccount>> {
            Err(ToolError::msg("BACKEND TOUCHED"))
        }
        fn account_info(&self, _: &Account) -> crate::error::ToolResult<MailAccount> {
            Err(ToolError::msg("BACKEND TOUCHED"))
        }
        fn mailboxes(&self, _: &Account) -> crate::error::ToolResult<Vec<crate::backend::Mailbox>> {
            Err(ToolError::msg("BACKEND TOUCHED"))
        }
        fn messages(
            &self,
            _: &Account,
            _: &MessageQuery,
        ) -> crate::error::ToolResult<crate::backend::MessageList> {
            Err(ToolError::msg("BACKEND TOUCHED"))
        }
        fn read(
            &self,
            _: &Account,
            _: Option<&str>,
            _: i64,
            _: usize,
        ) -> crate::error::ToolResult<crate::backend::MessageBody> {
            Err(ToolError::msg("BACKEND TOUCHED"))
        }
        fn compose(
            &self,
            _: &Account,
            _: &ComposeRequest,
        ) -> crate::error::ToolResult<crate::backend::Preview> {
            Err(ToolError::msg("BACKEND TOUCHED"))
        }
        fn approval_test(&self, _: u64) -> crate::error::ToolResult<String> {
            Err(ToolError::msg("BACKEND TOUCHED"))
        }
    }

    #[test]
    fn a_broken_config_is_reported_before_mail_is_touched() -> TestResult {
        for config in [
            "{\"version\": 3, \"allowed_accounts\": []}",
            "not json",
            "[]",
            "{\"allowed_accounts\": []}",
        ] {
            let dir = TempDir::new()?;
            let mut settings = settings_at(&dir.path("config.json"));
            fs::write(&settings.config_path, config)?;
            settings.fake = false;
            let ctx = Context::new(settings, Box::new(Untouchable));
            for args in [
                vec!["accounts"],
                vec!["setup"],
                vec!["selftest"],
                vec!["selftest-write"],
            ] {
                let (code, out) = run(&ctx, &args, "1\nj\n");
                assert_eq!(code, 1, "{config:?} {args:?}: {out}");
                assert!(
                    !out.contains("BACKEND TOUCHED"),
                    "{config:?} {args:?}: {out}"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn write_enable_saves_the_chosen_accounts_with_the_default_limit() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, Some(&base()))?;
        let (code, out) = run(&ctx, &["write-enable"], "2\nj\n");
        assert_eq!(code, 0, "{out}");
        let config = saved(&ctx)?;
        assert_eq!(write_accounts(&config), ["ACC-2"]);
        let limit = config
            .as_object()
            .and_then(|o| o.get("write"))
            .and_then(Json::as_object)
            .and_then(|w| w.get("max_per_hour"));
        assert_eq!(limit, Some(&Json::Int(5)));
        assert!(out.contains(
            "Grenzen: Klartext, kein Bcc, keine Anhaenge, max. 5 Empfaenger, max. 1500 Zeichen"
        ));
        Ok(())
    }

    #[test]
    fn write_enable_keeps_a_configured_limit() -> TestResult {
        let dir = TempDir::new()?;
        let config = format!(
            r#"{{"version": 2, "allowed_accounts": [{ACCOUNT_1}], "write": {{"accounts": [], "max_per_hour": 99}}}}"#
        );
        let ctx = real(&dir, Some(&config))?;
        let (code, _) = run(&ctx, &["write-enable"], "1\nj\n");
        assert_eq!(code, 0);
        let limit = saved(&ctx)?
            .as_object()
            .and_then(|o| o.get("write"))
            .and_then(Json::as_object)
            .and_then(|w| w.get("max_per_hour"))
            .cloned();
        assert_eq!(limit, Some(Json::Int(30)));
        Ok(())
    }

    #[test]
    fn write_disable_removes_only_the_write_section() -> TestResult {
        let dir = TempDir::new()?;
        let config = format!(
            r#"{{"version": 2, "allowed_accounts": [{ACCOUNT_1}, {ACCOUNT_2}], "write": {{"accounts": ["ACC-2"]}}}}"#
        );
        let ctx = real(&dir, Some(&config))?;
        let (code, _) = run(&ctx, &["write-disable"], "");
        assert_eq!(code, 0);
        assert_eq!(saved(&ctx)?, j(&base())?);
        Ok(())
    }

    #[test]
    fn write_enable_without_input_changes_nothing() -> TestResult {
        let dir = TempDir::new()?;
        let config = format!(
            r#"{{"version": 2, "allowed_accounts": [{ACCOUNT_1}], "write": {{"accounts": ["ACC-1"]}}}}"#
        );
        let ctx = real(&dir, Some(&config))?;
        let (code, out) = run(&ctx, &["write-enable"], "\n");
        assert_eq!(code, 0);
        assert!(out.contains("Unveraendert"));
        assert_eq!(write_accounts(&saved(&ctx)?), ["ACC-1"]);
        Ok(())
    }

    #[test]
    fn write_enable_refuses_wrong_numbers_unconfirmed_choices_and_missing_input() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, Some(&base()))?;
        for (input, expected) in [
            ("9\n", "Nummer ausserhalb der Liste."),
            ("0\n", "Nummer ausserhalb der Liste."),
            ("-1\n", "Nummer ausserhalb der Liste."),
            ("x\n", "Ungueltige Eingabe."),
            ("1\nn\n", "Abgebrochen."),
            ("1\n", "Abgebrochen."),
            ("", "Ungueltige Eingabe."),
        ] {
            let (code, out) = run(&ctx, &["write-enable"], input);
            assert_eq!(code, 1, "{input:?}");
            assert!(out.contains(expected), "{input:?}: {out}");
            assert_eq!(saved(&ctx)?, j(&base())?, "{input:?}");
        }
        Ok(())
    }

    #[test]
    fn write_enable_needs_accounts_that_are_enabled_for_reading() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, Some(r#"{"version": 2, "allowed_accounts": []}"#))?;
        let (code, out) = run(&ctx, &["write-enable"], "");
        assert_eq!(code, 1);
        assert!(out.contains("avery mail setup"));
        Ok(())
    }

    #[test]
    fn revoke_removes_reading_and_writing() -> TestResult {
        let dir = TempDir::new()?;
        let config = format!(
            r#"{{"version": 2, "allowed_accounts": [{ACCOUNT_1}], "write": {{"accounts": ["ACC-1"]}}}}"#
        );
        let ctx = real(&dir, Some(&config))?;
        let (code, out) = run(&ctx, &["revoke"], "");
        assert_eq!(code, 0);
        assert!(out.contains("Alle Freigaben entfernt"));
        assert_eq!(
            saved(&ctx)?,
            j(r#"{"version": 2, "allowed_accounts": []}"#)?
        );
        Ok(())
    }

    #[test]
    fn setup_keeps_write_rights_only_for_accounts_that_stay_enabled() -> TestResult {
        let dir = TempDir::new()?;
        let start = r#"{"version": 2, "allowed_accounts": [], "write": {"accounts": ["FAKE-GMAIL", "FAKE-OUTLOOK"], "max_per_hour": 3}}"#;
        let ctx = real(&dir, Some(start))?;
        let (code, _) = run(&ctx, &["setup"], "1\nj\n");
        assert_eq!(code, 0);
        let config = saved(&ctx)?;
        assert_eq!(write_accounts(&config), ["FAKE-GMAIL"]);
        let limit = config
            .as_object()
            .and_then(|o| o.get("write"))
            .and_then(Json::as_object)
            .and_then(|w| w.get("max_per_hour"));
        assert_eq!(limit, Some(&Json::Int(3)));
        let (code, _) = run(&ctx, &["setup"], "2\nj\n");
        assert_eq!(code, 0);
        let config = saved(&ctx)?;
        assert!(config.as_object().is_some_and(|o| o.get("write").is_none()));
        assert_eq!(
            config.as_object().and_then(|o| o.get("version")),
            Some(&Json::Int(2))
        );
        fs::write(&ctx.settings.config_path, start)?;
        let (code, _) = run(&ctx, &["setup"], "1 2\nj\n");
        assert_eq!(code, 0);
        let config = saved(&ctx)?;
        assert_eq!(write_accounts(&config), ["FAKE-GMAIL", "FAKE-OUTLOOK"]);
        let limit = config
            .as_object()
            .and_then(|o| o.get("write"))
            .and_then(Json::as_object)
            .and_then(|w| w.get("max_per_hour"));
        assert_eq!(limit, Some(&Json::Int(3)));
        Ok(())
    }

    #[test]
    fn setup_accepts_several_numbers_and_stores_their_details() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, None)?;
        let (code, out) = run(&ctx, &["setup"], "1, 2 1\nja\n");
        assert_eq!(code, 0, "{out}");
        let config = saved(&ctx)?;
        let accounts = config
            .as_object()
            .and_then(|o| o.get("allowed_accounts"))
            .and_then(Json::as_array)
            .ok_or("accounts")?;
        assert_eq!(accounts.len(), 2);
        assert_eq!(
            accounts[0],
            j(
                r#"{"id": "FAKE-GMAIL", "name": "Demo Gmail", "type": "imap", "addresses": ["demo.privat@gmail.example"]}"#
            )?
        );
        assert!(out.contains("Gespeichert in"));
        Ok(())
    }

    #[test]
    fn a_config_that_cannot_be_read_is_never_overwritten() -> TestResult {
        for text in [
            "{\"version\": 3, \"allowed_accounts\": []}",
            "{\"allowed_accounts\": []}",
            "not json at all",
            "[]",
        ] {
            let dir = TempDir::new()?;
            let ctx = real(&dir, Some(text))?;
            for (args, input) in [
                (vec!["setup"], "1\nj\n"),
                (vec!["revoke"], ""),
                (vec!["write-enable"], "1\nj\n"),
                (vec!["write-disable"], ""),
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
        let (code, out) = run(&ctx, &["write-disable"], "");
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
        let now = sendlog::now_seconds();
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
        fs::OpenOptions::new()
            .append(true)
            .open(ctx.settings.sendlog_path())?;
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
    fn status_shows_enabled_accounts_and_write_rights() -> TestResult {
        let dir = TempDir::new()?;
        let writing = format!(
            r#"{{"version": 2, "allowed_accounts": [{ACCOUNT_1}], "write": {{"accounts": ["ACC-1"]}}}}"#
        );
        let ctx = real(&dir, Some(&writing))?;
        let (code, out) = run(&ctx, &["status"], "");
        assert_eq!(code, 0);
        assert!(out.contains("Lesen freigegeben fuer: Privat Gmail"));
        assert!(out.contains("Entwuerfe/Senden: Privat Gmail"));
        assert!(out.contains(&format!("Version: {VERSION}")));
        let reading = format!(r#"{{"version": 2, "allowed_accounts": [{ACCOUNT_1}]}}"#);
        let (_, out) = run(&real(&dir, Some(&reading))?, &["status"], "");
        assert!(out.contains("Entwuerfe/Senden: aus"));
        let empty = TempDir::new()?;
        let (code, out) = run(&real(&empty, None)?, &["status"], "");
        assert_eq!(code, 0);
        assert!(out.contains("(fehlt)") && out.contains("kein Konto"));
        Ok(())
    }

    #[test]
    fn status_reports_an_unsupported_config_instead_of_guessing() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, Some(r#"{"version": 9, "allowed_accounts": []}"#))?;
        let (code, out) = run(&ctx, &["status"], "");
        assert_eq!(code, 1);
        assert!(out.contains("Fehler: config.json has an unsupported format version"));
        Ok(())
    }

    #[test]
    fn accounts_marks_enabled_and_writable_accounts() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = context(&dir, true, "FAKE-GMAIL", None, FakeOptions::default())?;
        let (code, out) = run(&ctx, &["accounts,"], "");
        assert_eq!(code, 0);
        assert!(
            out.contains("* 1) Demo Gmail  [imap]  demo.privat@gmail.example  [Entwuerfe/Senden]"),
            "{out}"
        );
        assert!(out.contains("* 2) Demo Outlook  [exchange]"));
        assert!(out.contains("  3) Demo Firma"));
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
        let (code, out) = run(
            &ctx,
            &["selftest-write"],
            "j
j
",
        );
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
        let now = sendlog::now_seconds();
        for index in 0..5 {
            let entry = log_entry(now - 1000 - index, Outcome::Sent, None);
            sendlog::append(&ctx.settings, &entry, true)?;
        }
        let (code, out) = run(
            &ctx,
            &["selftest-write"],
            "j
",
        );
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
        assert!(out.contains("Writing is not enabled."));
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
            "Befehle: accounts, setup, revoke, selftest, write-enable, write-disable, sendlog, selftest-write, status"
        ));
        assert!(out.contains("Hilfe: avery mail help"));
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
            ("write-enabel", Some("write-enable")),
            ("serv", Some("serve")),
            ("xyz", None),
            ("", None),
        ] {
            assert_eq!(closest_command(typed), expected, "{typed}");
        }
        assert_eq!(closest_command(&"a".repeat(500)), None);
    }

    #[test]
    fn picks_are_sorted_deduplicated_and_checked() {
        assert_eq!(parse_picks("3, 1 1,2", 3), Ok(vec![0, 1, 2]));
        assert_eq!(parse_picks("4", 3), Err("Nummer ausserhalb der Liste."));
        assert_eq!(parse_picks("0", 3), Err("Nummer ausserhalb der Liste."));
        assert_eq!(parse_picks("a", 3), Err("Ungueltige Eingabe."));
        assert_eq!(parse_picks("1.5", 3), Err("Ungueltige Eingabe."));
        assert_eq!(
            parse_picks("99999999999999999999", 3),
            Err("Ungueltige Eingabe.")
        );
    }

    #[test]
    fn an_endless_answer_line_is_refused() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = real(&dir, Some(&base()))?;
        let long = format!("{}\n", "1 ".repeat(20_000));
        let (code, out) = run(&ctx, &["write-enable"], &long);
        assert_eq!(code, 1);
        assert!(out.contains("Ungueltige Eingabe."));
        Ok(())
    }
}
