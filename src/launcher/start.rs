use std::io::Write;

use super::{say, Launcher, Plan, Refusal};

const SPLASH_MISSING: &str = "Splash fehlt. Installieren mit: brew install incoai/tap/splash";
const GOOSE_MISSING: &str =
    "Goose Desktop fehlt. Installieren mit: brew install --cask block-goose";
const ONBOARD_HINT: &str =
    "Hinweis: Die Einrichtung in Dialogen ist nicht abgeschlossen. Wiederholen: avery mail onboard";
const MODEL_HINT: &str =
    "Hinweis: Das Modell laeuft nicht. Mit 'brew services start avery' starten.";
const OTHER_PROFILE: &str = "Goose laeuft bereits mit einem anderen Profil (z. B. dem Standardprofil). Bitte beenden (Cmd+Q) und danach erneut starten.";

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

pub(crate) fn app(
    l: &Launcher,
    no_onboard: bool,
    onboard: &mut dyn FnMut(&mut dyn Write) -> u8,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<u8, Refusal> {
    if !no_onboard {
        first_setup(l, onboard, out, err);
    }
    open_goose(l, out, err)
}

fn first_setup(
    l: &Launcher,
    onboard: &mut dyn FnMut(&mut dyn Write) -> u8,
    out: &mut dyn Write,
    err: &mut dyn Write,
) {
    if l.env.dry_run {
        say(
            out,
            &format!("{} mail onboard --if-needed", l.exe.display()),
        );
    } else if onboard(out) != 0 {
        say(err, ONBOARD_HINT);
    }
}

fn open_goose(l: &Launcher, out: &mut dyn Write, err: &mut dyn Write) -> Result<u8, Refusal> {
    let app = l.env.goose_app.to_string_lossy().into_owned();
    let root = l.env.goose_root.to_string_lossy().into_owned();
    if !l.env.dry_run {
        if !l.env.goose_app.is_dir() {
            return Err(Refusal::new(GOOSE_MISSING));
        }
        if !l.env.goose_root.join("config/config.yaml").is_file() {
            return Err(Refusal::new("Profil fehlt. Zuerst: avery init"));
        }
        if !l.system.healthy(&l.env.model_url) {
            say(err, MODEL_HINT);
        }
        if l.system.running(&format!("{app}/Contents/")) {
            if !l.system.running(&format!("GOOSE_PATH_ROOT\":\"{root}\"")) {
                return Err(Refusal::new(OTHER_PROFILE));
            }
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
