use std::io::{self, Write};

use super::{profile, say, Launcher, Plan, Refusal};
use crate::cli;
use crate::console::{Buttons, Dialogs, Icon, Screen, Talk};
use crate::context::Context;

const SPLASH_MISSING: &str = "Splash fehlt. Installieren mit: brew install incoai/tap/splash";
const GOOSE_MISSING: &str =
    "Goose Desktop fehlt. Installieren mit: brew install --cask block-goose";
const ONBOARD_HINT: &str =
    "Hinweis: Die Einrichtung in Dialogen ist nicht abgeschlossen. Wiederholen: avery mail onboard";
const MODEL_HINT: &str =
    "Hinweis: Das Modell laeuft nicht. Mit 'brew services start avery' starten.";
const OTHER_PROFILE: &str = "Goose laeuft bereits mit einem anderen Profil (z. B. dem Standardprofil). Bitte beenden (Cmd+Q) und danach erneut starten.";
const DIALOG_TITLE: &str = "Avery";
const ACKNOWLEDGE: &str = "OK";
const REFRESH_QUESTION: &str = "Das Avery-Profil für Goose ist nicht mehr aktuell (neue Version, zum Beispiel neue Werkzeuge und Berechtigungen). Jetzt aktualisieren? Die alten Dateien bleiben als Sicherung (.bak) erhalten.";
const REFRESH_YES: &str = "Aktualisieren";
const REFRESH_LATER: &str = "Später";
const REFRESH_HINT: &str = "Hinweis: Das Profil ist nicht aktuell. Aktualisieren (die alten Dateien bleiben als Sicherung): avery init --force";
const REFRESH_FAILED: &str = "Das Profil konnte nicht aktualisiert werden:";

pub(crate) fn model(
    l: &Launcher,
    extra: &[String],
    offline: bool,
    out: &mut dyn Write,
) -> Result<u8, Refusal> {
    let splash = match l.system.find("splash") {
        Some(path) => path.to_string_lossy().into_owned(),
        None if l.env.dry_run => "splash".to_string(),
        None => return Err(Refusal::new(SPLASH_MISSING)),
    };
    let mut args: Vec<String> = [
        "serve",
        "--model",
        l.env.model.as_str(),
        "--served-model-name",
        "splash-local",
        "--max-memory",
        l.env.max_memory.as_str(),
        "--max-context",
        l.env.max_context.as_str(),
        "--max-cache-disk",
        l.env.max_cache_disk.as_str(),
    ]
    .iter()
    .map(|word| (*word).to_string())
    .collect();
    if offline && l.env.offline {
        args.push("--offline".to_string());
    }
    args.extend(extra.iter().cloned());
    Ok(l.begin(&Plan::new(splash, args), out))
}

pub(crate) fn pull(l: &Launcher, extra: &[String], out: &mut dyn Write) -> Result<u8, Refusal> {
    say(
        out,
        "Das Modell wird einmalig geladen (ca. 17 GB). Sobald 'Ready' erscheint, mit Ctrl+C beenden.",
    );
    model(l, extra, false, out)
}

fn profile_plan(l: &Launcher, plan: Plan) -> Plan {
    let root = l.env.goose_root.to_string_lossy().into_owned();
    plan.with_env("GOOSE_PATH_ROOT", root.clone())
        .with_env("GOOSE_MOIM_MESSAGE_FILE", format!("{root}/top-of-mind.md"))
}

pub(crate) fn goose_cli(
    l: &Launcher,
    extra: &[String],
    out: &mut dyn Write,
) -> Result<u8, Refusal> {
    let bin = l
        .env
        .goose_app
        .join("Contents/Resources/bin/goose")
        .to_string_lossy()
        .into_owned();
    if !l.env.dry_run && l.system.find(&bin).is_none() {
        return Err(Refusal::new(format!("Goose CLI nicht gefunden: {bin}")));
    }
    Ok(l.begin(&profile_plan(l, Plan::new(bin, extra.to_vec())), out))
}

pub(crate) trait Ui {
    fn setup(&mut self, out: &mut dyn Write) -> u8;
    fn alert(&mut self, text: &str);
    fn agree(&mut self, question: &str, yes: &str, no: &str) -> Option<bool>;
}

pub(crate) struct AppUi<'a> {
    pub(crate) ctx: &'a Context,
    pub(crate) screen: &'a Screen<'a>,
    pub(crate) dialogs: bool,
}

pub(crate) fn tell(screen: &Screen, text: &str) {
    if (screen.gui)() {
        let mut talk = Talk::new();
        talk.set_title(DIALOG_TITLE);
        let ask = Buttons {
            text,
            buttons: &[ACKNOWLEDGE],
            default: 0,
            cancel: None,
            icon: Icon::Caution,
        };
        let _ = Dialogs::new(screen.host, &mut talk).ask_buttons(&ask);
    }
}

impl Ui for AppUi<'_> {
    fn setup(&mut self, out: &mut dyn Write) -> u8 {
        let args = ["onboard".to_string(), "--if-needed".to_string()];
        cli::dispatch(&args, self.ctx, self.screen, &mut io::empty(), out)
    }

    fn alert(&mut self, text: &str) {
        if self.dialogs {
            tell(self.screen, text);
        }
    }

    fn agree(&mut self, question: &str, yes: &str, no: &str) -> Option<bool> {
        if !self.dialogs || !(self.screen.gui)() {
            return None;
        }
        let mut talk = Talk::new();
        talk.set_title(DIALOG_TITLE);
        let ask = Buttons {
            text: question,
            buttons: &[yes, no],
            default: 0,
            cancel: Some(1),
            icon: Icon::Note,
        };
        Some(matches!(
            Dialogs::new(self.screen.host, &mut talk).ask_buttons(&ask),
            Ok(0)
        ))
    }
}

pub(crate) fn app(
    l: &Launcher,
    no_onboard: bool,
    ui: &mut dyn Ui,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<u8, Refusal> {
    if !no_onboard {
        first_setup(l, ui, out, err);
    }
    open_goose(l, ui, out, err)
}

fn first_setup(l: &Launcher, ui: &mut dyn Ui, out: &mut dyn Write, err: &mut dyn Write) {
    if l.env.dry_run {
        say(
            out,
            &format!("{} mail onboard --if-needed", l.exe.display()),
        );
    } else if ui.setup(out) != 0 {
        say(err, ONBOARD_HINT);
    }
}

fn offer_refresh(l: &Launcher, ui: &mut dyn Ui, out: &mut dyn Write, err: &mut dyn Write) {
    if profile::current(l) {
        return;
    }
    match ui.agree(REFRESH_QUESTION, REFRESH_YES, REFRESH_LATER) {
        None => say(err, REFRESH_HINT),
        Some(false) => {}
        Some(true) => {
            if let Err(refusal) = profile::refresh(l, out) {
                say(err, &format!("avery: {}", refusal.0));
                ui.alert(&format!("{REFRESH_FAILED} {}", refusal.0));
            }
        }
    }
}

fn open_goose(
    l: &Launcher,
    ui: &mut dyn Ui,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<u8, Refusal> {
    let app = l.env.goose_app.to_string_lossy().into_owned();
    let root = l.env.goose_root.to_string_lossy().into_owned();
    if !l.env.dry_run {
        if !l.env.goose_app.is_dir() {
            return Err(Refusal::new(GOOSE_MISSING));
        }
        if !l.env.goose_root.join("config/config.yaml").is_file() {
            return Err(Refusal::new("Profil fehlt. Zuerst: avery init"));
        }
        let running = l.system.running(&format!("{app}/Contents/"));
        if running && !l.system.running(&format!("GOOSE_PATH_ROOT\":\"{root}\"")) {
            return Err(Refusal::new(OTHER_PROFILE));
        }
        if !running {
            offer_refresh(l, ui, out, err);
        }
        if !l.system.healthy(&l.env.model_url) {
            say(err, MODEL_HINT);
            ui.alert(MODEL_HINT);
        }
        if running {
            say(out, "Goose laeuft bereits mit dem Avery-Profil.");
            return Ok(l.begin(&Plan::new("open", vec!["-a".to_string(), app]), out));
        }
    }
    let plan = Plan::new(
        "open",
        vec![
            "-n".to_string(),
            "-a".to_string(),
            app,
            "--env".to_string(),
            format!("GOOSE_PATH_ROOT={root}"),
            "--env".to_string(),
            format!("GOOSE_MOIM_MESSAGE_FILE={root}/top-of-mind.md"),
        ],
    );
    Ok(l.begin(&plan, out))
}
