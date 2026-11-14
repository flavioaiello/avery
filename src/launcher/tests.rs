use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::system::Ran;
use super::*;
use crate::osascript::{App, Finished, Host};
use crate::testutil::TempDir;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const EXE: &str = "/opt/test/bin/avery";
const SPLASH: &str = "/opt/homebrew/bin/splash";
const MODEL_ARGS: &str = "serve --model unsloth/Qwen3.8-27B-GGUF:UD-Q4_K_M --served-model-name splash-local --max-memory 28G --max-context 100K --max-cache-disk 4G";

#[derive(Default)]
struct Fake {
    found: BTreeMap<String, PathBuf>,
    ran: BTreeMap<String, (Option<i32>, String)>,
    healthy: bool,
    running: Vec<String>,
    started: RefCell<Vec<Plan>>,
}

impl Fake {
    fn with_found(mut self, program: &str, path: &str) -> Fake {
        self.found.insert(program.to_string(), PathBuf::from(path));
        self
    }

    fn with_ran(mut self, program: &str, code: Option<i32>, stdout: &str) -> Fake {
        self.ran
            .insert(program.to_string(), (code, stdout.to_string()));
        self
    }

    fn with_running(mut self, needle: String) -> Fake {
        self.running.push(needle);
        self
    }

    fn started(&self) -> Vec<Plan> {
        self.started.borrow().clone()
    }
}

impl System for Fake {
    fn find(&self, program: &str) -> Option<PathBuf> {
        self.found.get(program).cloned()
    }

    fn run(&self, program: &str, _args: &[&str]) -> Option<Ran> {
        self.ran.get(program).map(|(code, stdout)| Ran {
            code: *code,
            stdout: stdout.clone(),
        })
    }

    fn healthy(&self, _url: &str) -> bool {
        self.healthy
    }

    fn running(&self, needle: &str) -> bool {
        self.running.iter().any(|entry| entry == needle)
    }

    fn start(&self, plan: &Plan) -> u8 {
        self.started.borrow_mut().push(plan.clone());
        0
    }
}

struct NoHost;

impl Host for NoHost {
    fn ensure_running(&self, _app: App) {}

    fn run(&self, _app: App, _payload: &str, _timeout: Duration) -> Finished {
        Finished::NotStarted
    }
}

struct Rig {
    dir: TempDir,
}

impl Rig {
    fn new() -> Result<Rig, io::Error> {
        Ok(Rig {
            dir: TempDir::new()?,
        })
    }

    fn root(&self) -> PathBuf {
        PathBuf::from(self.dir.path(""))
    }

    fn home(&self) -> String {
        self.root().to_string_lossy().into_owned()
    }

    fn avery_home(&self) -> PathBuf {
        self.root().join(".avery")
    }

    fn goose_root(&self) -> PathBuf {
        self.avery_home().join("goose")
    }

    fn write(&self, relative: &str, text: &str) -> Result<PathBuf, io::Error> {
        let path = self.root().join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, text)?;
        Ok(path)
    }
}

fn lookup_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: BTreeMap<String, String> = pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect();
    move |key: &str| map.get(key).cloned()
}

fn invoke(args: &[&str], pairs: &[(&str, &str)], fake: &Fake) -> (u8, String, String) {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
    let lookup = lookup_of(pairs);
    let host = NoHost;
    let gui = || false;
    let screen = Screen {
        host: &host,
        gui: &gui,
    };
    let call = Call {
        args: &args,
        lookup: &lookup,
        system: fake,
        screen: &screen,
        exe: PathBuf::from(EXE),
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = dispatch(&call, &mut io::empty(), &mut out, &mut err);
    (
        code,
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    )
}

fn env_of(pairs: &[(&str, &str)]) -> Result<Env, String> {
    let lookup = lookup_of(pairs);
    Env::from_variables(&Variables::load(&lookup)).map_err(|refusal| refusal.0)
}

fn launcher<'a>(fake: &'a Fake, pairs: &[(&str, &str)]) -> Result<Launcher<'a>, String> {
    Ok(Launcher {
        env: env_of(pairs)?,
        system: fake,
        exe: PathBuf::from(EXE),
    })
}

fn lines(text: &str) -> Vec<&str> {
    text.lines().collect()
}

#[test]
fn commands_are_parsed_with_their_trailing_punctuation_tolerated() {
    let words = |args: &[&str]| -> Vec<String> { args.iter().map(|a| (*a).to_string()).collect() };
    assert_eq!(parse(&[]), Command::Help);
    assert_eq!(parse(&words(&[""])), Command::Help);
    for word in ["help", "-h", "--help", "help."] {
        assert_eq!(parse(&words(&[word])), Command::Help, "{word}");
    }
    for word in ["version", "-V", "--version", "version."] {
        assert_eq!(parse(&words(&[word])), Command::Version, "{word}");
    }
    assert_eq!(parse(&words(&["serve"])), Command::Serve);
    assert_eq!(parse(&words(&["app", "x"])), Command::Launch(Launch::App));
    assert_eq!(parse(&words(&["doctor,"])), Command::Launch(Launch::Doctor));
    assert_eq!(
        parse(&words(&["init", "--force"])),
        Command::Launch(Launch::Init(words(&["--force"])))
    );
    assert_eq!(
        parse(&words(&["cli", "session", "-r"])),
        Command::Launch(Launch::Cli(words(&["session", "-r"])))
    );
    assert_eq!(
        parse(&words(&["mail", "status"])),
        Command::Mail(words(&["status"]))
    );
    assert_eq!(parse(&words(&["mail"])), Command::Mail(Vec::new()));
    assert_eq!(parse(&words(&["up"])), Command::Unknown("up".to_string()));
}

#[test]
fn version_help_and_unknown_commands_answer_without_touching_the_system() {
    let fake = Fake::default();
    let (code, out, _) = invoke(&["version"], &[], &fake);
    assert_eq!((code, out), (0, format!("avery {VERSION}\n")));
    let (code, out, _) = invoke(&[], &[], &fake);
    assert_eq!(code, 0);
    assert!(out.starts_with(&format!("avery {VERSION} - ")));
    for word in [
        "avery init",
        "avery mail",
        "avery app",
        "avery cli",
        "avery model",
        "avery pull",
        "avery serve",
        "avery doctor",
        "avery version",
        "onboard, setup",
        "AVERY_NO_ONBOARD=1",
        "brew services start avery",
    ] {
        assert!(out.contains(word), "{word}");
    }
    for gone in ["avery up", "avery down", "install-app", "Avery.app", "link"] {
        assert!(!out.contains(gone), "{gone}");
    }
    let (code, out, err) = invoke(&["foo"], &[], &fake);
    assert_eq!(code, 2);
    assert!(out.is_empty());
    assert!(err.starts_with("avery: unbekannter Befehl: foo\n\navery "));
    assert!(fake.started().is_empty());
}

#[test]
fn the_environment_wins_over_avery_env_and_only_known_keys_are_read() -> TestResult {
    let rig = Rig::new()?;
    let pwned = rig.root().join("PWNED");
    rig.write(
        ".avery/avery.env",
        &format!(
            "# Kommentar\nAVERY_MAX_MEMORY=20G\nAVERY_MAX_CONTEXT=8K\nAVERY_BOGUS=1\nAVERY_MODEL=$(touch {})\nAVERY_MAX_MEMORY=99G\nno equals sign\r\n",
            pwned.display()
        ),
    )?;
    let env = env_of(&[("HOME", &rig.home()), ("AVERY_MAX_CONTEXT", "16K")])?;
    assert_eq!(env.max_memory, "20G");
    assert_eq!(env.max_context, "16K");
    assert_eq!(env.model, format!("$(touch {})", pwned.display()));
    assert!(!pwned.exists());
    let variables_lookup = lookup_of(&[("HOME", &rig.home())]);
    let variables = Variables::load(&variables_lookup);
    assert_eq!(variables.get("AVERY_BOGUS"), None);
    Ok(())
}

#[test]
fn an_empty_environment_value_falls_back_to_the_file_and_the_defaults_apply() -> TestResult {
    let rig = Rig::new()?;
    rig.write(".avery/avery.env", "AVERY_NO_ONBOARD=1\nAVERY_OFFLINE=0\n")?;
    let lookup = lookup_of(&[("HOME", &rig.home()), ("AVERY_NO_ONBOARD", "")]);
    assert_eq!(
        Variables::load(&lookup).get("AVERY_NO_ONBOARD").as_deref(),
        Some("1")
    );
    let env = env_of(&[("HOME", &rig.home())])?;
    assert!(!env.offline && !env.dry_run);
    let bare = Rig::new()?;
    let env = env_of(&[("HOME", &bare.home())])?;
    assert_eq!(env.model, "unsloth/Qwen3.8-27B-GGUF:UD-Q4_K_M");
    assert_eq!(env.goose_app, Path::new("/Applications/Goose.app"));
    assert_eq!(env.goose_root, bare.goose_root());
    assert_eq!(env.model_url, "http://127.0.0.1:8000/health");
    assert!(env.offline);
    assert_eq!(env.hf_home, Some(bare.root().join(".cache/huggingface")));
    assert_eq!(env_of(&[]).err().as_deref(), Some("HOME ist nicht gesetzt"));
    Ok(())
}

#[test]
fn init_writes_the_profile_with_this_program_and_keeps_it_private() -> TestResult {
    let rig = Rig::new()?;
    let missing_goose = rig.root().join("nirgends/Goose.app");
    let fake = Fake::default();
    let pairs = [
        ("HOME", rig.home()),
        (
            "AVERY_GOOSE_APP",
            missing_goose.to_string_lossy().into_owned(),
        ),
    ];
    let pairs: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let (code, out, err) = invoke(&["init"], &pairs, &fake);
    assert_eq!((code, err.as_str()), (0, ""), "{out}");
    assert_eq!(out.matches("  geschrieben: ").count(), 5);
    let goose = rig.goose_root();
    assert!(out.contains(&format!("Avery-Profil einrichten: {}", goose.display())));
    let config = fs::read_to_string(goose.join("config/config.yaml"))?;
    assert!(config.contains(&format!(
        "GOOSE_MOIM_MESSAGE_FILE: \"{}/top-of-mind.md\"",
        goose.display()
    )));
    assert!(config.contains(&format!("cmd: \"{EXE}\"")));
    assert!(config.contains("- serve") && config.contains("envs: {}"));
    assert!(!config.contains("@GOOSE_ROOT@") && !config.contains("@AVERY@"));
    let permission = fs::read_to_string(goose.join("config/permission.yaml"))?;
    assert!(permission.contains(
        "ask_before:\n  - mail__send_message\n  - mail__move_messages\n  - mail__create_event\n  - mail__update_event\n  - mail__delete_event"
    ));
    assert!(permission.contains("- mail__create_draft"));
    assert!(permission.contains("- mail__list_events"));
    assert!(permission.contains("never_allow: []"));
    for relative in [
        "config/custom_providers/custom_splash.json",
        "top-of-mind.md",
    ] {
        assert!(goose.join(relative).is_file(), "{relative}");
    }
    assert!(rig.avery_home().join("avery.env").is_file());
    for dir in [rig.avery_home(), rig.avery_home().join("mail")] {
        assert_eq!(fs::metadata(&dir)?.permissions().mode() & 0o777, 0o700);
    }
    assert!(out.contains(
        "Hinweis: Goose Desktop fehlt. Installieren mit: brew install --cask block-goose"
    ));
    assert!(out.contains("Hinweis: Splash fehlt. Installieren mit: brew install incoai/tap/splash"));
    assert!(out.contains("brew services start avery"));
    assert!(out.contains("avery app") && !out.contains("avery up"));
    Ok(())
}

#[test]
fn init_hints_disappear_when_goose_and_splash_exist() -> TestResult {
    let rig = Rig::new()?;
    let app = rig.root().join("Goose.app");
    fs::create_dir_all(&app)?;
    let fake = Fake::default().with_found("splash", SPLASH);
    let app_text = app.to_string_lossy().into_owned();
    let (code, out, _) = invoke(
        &["init"],
        &[("HOME", &rig.home()), ("AVERY_GOOSE_APP", &app_text)],
        &fake,
    );
    assert_eq!(code, 0);
    assert!(!out.contains("Hinweis: Goose") && !out.contains("Hinweis: Splash"));
    Ok(())
}

#[test]
fn a_second_init_keeps_everything_and_force_keeps_backups() -> TestResult {
    let rig = Rig::new()?;
    let fake = Fake::default();
    let pairs = [("HOME", rig.home())];
    let pairs: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (*k, v.as_str())).collect();
    invoke(&["init"], &pairs, &fake);
    let notes = rig.goose_root().join("top-of-mind.md");
    let mut text = fs::read_to_string(&notes)?;
    text.push_str("\nEIGENE ZEILE\n");
    fs::write(&notes, &text)?;
    let (code, out, _) = invoke(&["init"], &pairs, &fake);
    assert_eq!(code, 0);
    assert_eq!(out.matches("nicht ueberschrieben").count(), 5);
    assert_eq!(fs::read_to_string(&notes)?, text);
    let (code, out, _) = invoke(&["init", "--force"], &pairs, &fake);
    assert_eq!(code, 0);
    assert_eq!(out.matches("  Sicherung: ").count(), 5);
    assert_eq!(out.matches("  geschrieben: ").count(), 5);
    let backups: Vec<_> = fs::read_dir(rig.goose_root())?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("top-of-mind.md.bak-")
        })
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read_to_string(backups[0].path())?, text);
    assert!(!fs::read_to_string(&notes)?.contains("EIGENE ZEILE"));
    Ok(())
}

#[test]
fn init_rejects_unknown_options_and_answers_help() -> TestResult {
    let rig = Rig::new()?;
    let fake = Fake::default();
    let (code, out, err) = invoke(&["init", "--bogus"], &[("HOME", &rig.home())], &fake);
    assert_eq!(code, 1);
    assert_eq!(err, "avery: Unbekannte Option: --bogus\n");
    assert!(out.is_empty() && !rig.avery_home().exists());
    let (code, out, _) = invoke(&["init", "--help"], &[("HOME", &rig.home())], &fake);
    assert_eq!(code, 0);
    assert!(out.starts_with("Aufruf: avery init [--force]"));
    assert!(!rig.avery_home().exists());
    Ok(())
}

#[test]
fn init_keeps_odd_paths_intact_and_the_yaml_closed() -> TestResult {
    let rig = Rig::new()?;
    let fake = Fake::default();
    for name in ["a&b|c d", "q\"uote\\slash"] {
        let home = rig.root().join(name);
        let home_text = home.to_string_lossy().into_owned();
        let (code, _, err) = invoke(
            &["init"],
            &[("HOME", &rig.home()), ("AVERY_HOME", &home_text)],
            &fake,
        );
        assert_eq!((code, err.as_str()), (0, ""), "{name}");
        let config = fs::read_to_string(home.join("goose/config/config.yaml"))?;
        let escaped = format!("{home_text}/goose/top-of-mind.md")
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        assert!(
            config.contains(&format!("GOOSE_MOIM_MESSAGE_FILE: \"{escaped}\"")),
            "{name}"
        );
    }
    Ok(())
}

#[test]
fn init_needs_an_absolute_program_path() -> TestResult {
    let rig = Rig::new()?;
    let fake = Fake::default();
    let mut launcher = launcher(&fake, &[("HOME", &rig.home())])?;
    launcher.exe = PathBuf::from("avery");
    let mut out = Vec::new();
    assert_eq!(
        profile::init(&launcher, &[], &mut out),
        Err(Refusal::new("Der Pfad von avery ist nicht ermittelbar."))
    );
    assert!(!rig.avery_home().exists());
    Ok(())
}

#[test]
fn model_starts_splash_with_the_defaults_offline() -> TestResult {
    let rig = Rig::new()?;
    let fake = Fake::default().with_found("splash", SPLASH);
    let (code, out, _) = invoke(
        &["model"],
        &[("HOME", &rig.home()), ("AVERY_DRY_RUN", "1")],
        &fake,
    );
    assert_eq!(code, 0);
    assert_eq!(out.trim(), format!("{SPLASH} {MODEL_ARGS} --offline"));
    assert!(fake.started().is_empty());
    let (code, out, _) = invoke(&["model", "--port", "9"], &[("HOME", &rig.home())], &fake);
    assert_eq!((code, out.as_str()), (0, ""));
    let started = fake.started();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].program, SPLASH);
    assert_eq!(
        started[0].args.join(" "),
        format!("{MODEL_ARGS} --offline --port 9")
    );
    Ok(())
}

#[test]
fn model_goes_online_when_asked_and_pull_always_does() -> TestResult {
    let rig = Rig::new()?;
    let fake = Fake::default().with_found("splash", SPLASH);
    let dry = [("HOME", rig.home()), ("AVERY_DRY_RUN", "1".to_string())];
    let dry: Vec<(&str, &str)> = dry.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let mut online = dry.clone();
    online.push(("AVERY_OFFLINE", "0"));
    let (_, out, _) = invoke(&["model"], &online, &fake);
    assert_eq!(out.trim(), format!("{SPLASH} {MODEL_ARGS}"));
    let mut offline = dry.clone();
    offline.push(("AVERY_OFFLINE", "1"));
    let (code, out, _) = invoke(&["pull"], &offline, &fake);
    assert_eq!(code, 0);
    assert_eq!(
        lines(&out),
        [
            "Das Modell wird einmalig geladen (ca. 17 GB). Sobald 'Ready' erscheint, mit Ctrl+C beenden.",
            &format!("{SPLASH} {MODEL_ARGS}"),
        ]
    );
    Ok(())
}

#[test]
fn model_needs_splash_except_in_a_dry_run() -> TestResult {
    let rig = Rig::new()?;
    let fake = Fake::default();
    let (code, out, err) = invoke(&["model"], &[("HOME", &rig.home())], &fake);
    assert_eq!(code, 1);
    assert!(out.is_empty());
    assert_eq!(
        err,
        "avery: Splash fehlt. Installieren mit: brew install incoai/tap/splash\n"
    );
    let (code, out, _) = invoke(
        &["model"],
        &[("HOME", &rig.home()), ("AVERY_DRY_RUN", "1")],
        &fake,
    );
    assert_eq!(code, 0);
    assert!(out.starts_with("splash serve "));
    assert!(fake.started().is_empty());
    Ok(())
}

#[test]
fn the_goose_cli_runs_with_the_avery_profile() -> TestResult {
    let rig = Rig::new()?;
    let app = "/Opt/Mein Goose.app";
    let bin = "/Opt/Mein Goose.app/Contents/Resources/bin/goose";
    let fake = Fake::default().with_found(bin, bin);
    let pairs = [("HOME", rig.home()), ("AVERY_GOOSE_APP", app.to_string())];
    let pairs: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let (code, out, err) = invoke(&["cli", "session"], &pairs, &fake);
    assert_eq!((code, out.as_str(), err.as_str()), (0, "", ""));
    let started = fake.started();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].program, bin);
    assert_eq!(started[0].args, ["session"]);
    let root = rig.goose_root().to_string_lossy().into_owned();
    assert_eq!(
        started[0].env,
        [
            ("GOOSE_PATH_ROOT".to_string(), root.clone()),
            (
                "GOOSE_MOIM_MESSAGE_FILE".to_string(),
                format!("{root}/top-of-mind.md")
            ),
        ]
    );
    let none = Fake::default();
    let (code, _, err) = invoke(&["cli"], &pairs, &none);
    assert_eq!(code, 1);
    assert_eq!(err, format!("avery: Goose CLI nicht gefunden: {bin}\n"));
    let mut dry = pairs.clone();
    dry.push(("AVERY_DRY_RUN", "1"));
    let (_, out, _) = invoke(&["cli", "session"], &dry, &none);
    assert_eq!(out.trim(), format!("{bin} session"));
    Ok(())
}

fn app_pairs(rig: &Rig, extra: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut pairs = vec![("HOME".to_string(), rig.home())];
    pairs.extend(
        extra
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string())),
    );
    pairs
}

fn borrowed(pairs: &[(String, String)]) -> Vec<(&str, &str)> {
    pairs
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect()
}

#[test]
fn a_dry_run_of_app_shows_the_setup_and_then_goose_with_the_profile() -> TestResult {
    let rig = Rig::new()?;
    let fake = Fake::default();
    let goose = rig.goose_root().to_string_lossy().into_owned();
    let open = format!(
        "open -n -a /Applications/Goose.app --env GOOSE_PATH_ROOT={goose} --env GOOSE_MOIM_MESSAGE_FILE={goose}/top-of-mind.md"
    );
    let pairs = app_pairs(&rig, &[("AVERY_DRY_RUN", "1")]);
    let (code, out, err) = invoke(&["app"], &borrowed(&pairs), &fake);
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(
        lines(&out),
        [format!("{EXE} mail onboard --if-needed"), open.clone()]
    );
    let pairs = app_pairs(&rig, &[("AVERY_DRY_RUN", "1"), ("AVERY_NO_ONBOARD", "1")]);
    let (_, out, _) = invoke(&["app"], &borrowed(&pairs), &fake);
    assert_eq!(lines(&out), [open.as_str()]);
    rig.write(".avery/avery.env", "AVERY_NO_ONBOARD=1\n")?;
    let pairs = app_pairs(&rig, &[("AVERY_DRY_RUN", "1")]);
    let (_, out, _) = invoke(&["app"], &borrowed(&pairs), &fake);
    assert_eq!(lines(&out), [open.as_str()]);
    let pairs = app_pairs(&rig, &[("AVERY_DRY_RUN", "1"), ("AVERY_NO_ONBOARD", "0")]);
    let (_, out, _) = invoke(&["app"], &borrowed(&pairs), &fake);
    assert_eq!(
        lines(&out),
        [format!("{EXE} mail onboard --if-needed"), open]
    );
    let pairs = app_pairs(
        &rig,
        &[
            ("AVERY_DRY_RUN", "1"),
            ("AVERY_NO_ONBOARD", "1"),
            ("AVERY_GOOSE_APP", "/Opt/Mein Goose.app"),
        ],
    );
    let (_, out, _) = invoke(&["app"], &borrowed(&pairs), &fake);
    assert!(out.contains("-a /Opt/Mein Goose.app --env"));
    assert!(fake.started().is_empty());
    Ok(())
}

struct Ready {
    rig: Rig,
    app: String,
    root: String,
}

fn ready() -> Result<Ready, io::Error> {
    let rig = Rig::new()?;
    let app = rig.root().join("Goose.app");
    fs::create_dir_all(app.join("Contents"))?;
    rig.write(".avery/goose/config/config.yaml", "x: 1\n")?;
    let root = rig.goose_root().to_string_lossy().into_owned();
    Ok(Ready {
        app: app.to_string_lossy().into_owned(),
        rig,
        root,
    })
}

fn start_app(
    ready: &Ready,
    fake: &Fake,
    no_onboard: bool,
    onboard_code: u8,
) -> Result<(Result<u8, Refusal>, String, String, usize), String> {
    let pairs = [
        ("HOME", ready.rig.home()),
        ("AVERY_GOOSE_APP", ready.app.clone()),
    ];
    let pairs: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let launcher = launcher(fake, &pairs)?;
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut calls = 0usize;
    let mut onboard = |out: &mut dyn io::Write| {
        calls += 1;
        let _ = writeln!(out, "dialogs");
        onboard_code
    };
    let result = start::app(&launcher, no_onboard, &mut onboard, &mut out, &mut err);
    Ok((
        result,
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
        calls,
    ))
}

fn open_plan(ready: &Ready) -> Plan {
    Plan::new(
        "open",
        vec![
            "-n".to_string(),
            "-a".to_string(),
            ready.app.clone(),
            "--env".to_string(),
            format!("GOOSE_PATH_ROOT={}", ready.root),
            "--env".to_string(),
            format!("GOOSE_MOIM_MESSAGE_FILE={}/top-of-mind.md", ready.root),
        ],
    )
}

#[test]
fn app_opens_goose_with_the_profile_when_goose_is_not_running() -> TestResult {
    let ready = ready()?;
    let fake = Fake {
        healthy: true,
        ..Fake::default()
    };
    let (result, out, err, calls) = start_app(&ready, &fake, false, 0)?;
    assert_eq!(result, Ok(0));
    assert_eq!((out.as_str(), err.as_str(), calls), ("dialogs\n", "", 1));
    assert_eq!(fake.started(), [open_plan(&ready)]);
    Ok(())
}

#[test]
fn a_failed_setup_never_blocks_the_start_and_no_onboard_skips_it() -> TestResult {
    let ready = ready()?;
    let fake = Fake {
        healthy: true,
        ..Fake::default()
    };
    let (result, _, err, calls) = start_app(&ready, &fake, false, 1)?;
    assert_eq!(result, Ok(0));
    assert_eq!(calls, 1);
    assert_eq!(
        err,
        "Hinweis: Die Einrichtung in Dialogen ist nicht abgeschlossen. Wiederholen: avery mail onboard\n"
    );
    assert_eq!(fake.started(), [open_plan(&ready)]);
    let (result, out, err, calls) = start_app(&ready, &fake, true, 1)?;
    assert_eq!(result, Ok(0));
    assert_eq!((out.as_str(), err.as_str(), calls), ("", "", 0));
    assert_eq!(fake.started().len(), 2);
    Ok(())
}

#[test]
fn app_warns_when_the_model_is_down_but_still_starts() -> TestResult {
    let ready = ready()?;
    let fake = Fake::default();
    let (result, _, err, _) = start_app(&ready, &fake, true, 0)?;
    assert_eq!(result, Ok(0));
    assert_eq!(
        err,
        "Hinweis: Das Modell laeuft nicht. Mit 'brew services start avery' starten.\n"
    );
    assert_eq!(fake.started(), [open_plan(&ready)]);
    Ok(())
}

#[test]
fn app_only_brings_goose_forward_when_it_runs_with_the_profile() -> TestResult {
    let ready = ready()?;
    let fake = Fake {
        healthy: true,
        ..Fake::default()
    }
    .with_running(format!("{}/Contents/", ready.app))
    .with_running(format!("GOOSE_PATH_ROOT\":\"{}\"", ready.root));
    let (result, out, _, _) = start_app(&ready, &fake, true, 0)?;
    assert_eq!(result, Ok(0));
    assert_eq!(out, "Goose laeuft bereits mit dem Avery-Profil.\n");
    assert_eq!(
        fake.started(),
        [Plan::new("open", vec!["-a".to_string(), ready.app.clone()])]
    );
    Ok(())
}

#[test]
fn app_refuses_to_start_goose_over_another_profile() -> TestResult {
    let ready = ready()?;
    let fake = Fake {
        healthy: true,
        ..Fake::default()
    }
    .with_running(format!("{}/Contents/", ready.app));
    let (result, _, _, _) = start_app(&ready, &fake, true, 0)?;
    assert_eq!(
        result.err().map(|refusal| refusal.0),
        Some("Goose laeuft bereits mit einem anderen Profil (z. B. dem Standardprofil). Bitte beenden (Cmd+Q) und danach erneut starten.".to_string())
    );
    assert!(fake.started().is_empty());
    Ok(())
}

#[test]
fn app_needs_goose_and_the_profile() -> TestResult {
    let rig = Rig::new()?;
    let fake = Fake::default();
    let missing = rig.root().join("nirgends/Goose.app");
    let missing_text = missing.to_string_lossy().into_owned();
    let pairs = app_pairs(
        &rig,
        &[
            ("AVERY_NO_ONBOARD", "1"),
            ("AVERY_GOOSE_APP", &missing_text),
        ],
    );
    let (code, _, err) = invoke(&["app"], &borrowed(&pairs), &fake);
    assert_eq!(code, 1);
    assert_eq!(
        err,
        "avery: Goose Desktop fehlt. Installieren mit: brew install --cask block-goose\n"
    );
    let app = rig.root().join("Goose.app");
    fs::create_dir_all(&app)?;
    let app_text = app.to_string_lossy().into_owned();
    let pairs = app_pairs(
        &rig,
        &[("AVERY_NO_ONBOARD", "1"), ("AVERY_GOOSE_APP", &app_text)],
    );
    let (code, _, err) = invoke(&["app"], &borrowed(&pairs), &fake);
    assert_eq!(code, 1);
    assert_eq!(err, "avery: Profil fehlt. Zuerst: avery init\n");
    assert!(fake.started().is_empty());
    Ok(())
}

struct Green {
    rig: Rig,
    pairs: Vec<(String, String)>,
    fake: Fake,
}

fn green() -> Result<Green, Box<dyn std::error::Error>> {
    let rig = Rig::new()?;
    let app = rig.root().join("Goose.app");
    fs::create_dir_all(app.join("Contents"))?;
    let hub = rig
        .root()
        .join(".cache/huggingface/hub/models--unsloth--Qwen3.8-27B-GGUF/snapshots");
    fs::create_dir_all(hub)?;
    rig.write(".avery/goose/data/models/whisper-small-q8_0.gguf", "")?;
    let app_text = app.to_string_lossy().into_owned();
    let pairs = app_pairs(&rig, &[("AVERY_GOOSE_APP", &app_text)]);
    let fake = Fake {
        healthy: true,
        ..Fake::default()
    }
    .with_found("splash", SPLASH)
    .with_found("ruby", "/usr/bin/ruby")
    .with_found(EXE, EXE)
    .with_ran("sw_vers", Some(0), "26.0.1\n")
    .with_ran("defaults", Some(0), "1.59.0\n")
    .with_ran("ruby", Some(0), "");
    let (code, _, err) = invoke(&["init"], &borrowed(&pairs), &fake);
    assert_eq!((code, err.as_str()), (0, ""));
    Ok(Green { rig, pairs, fake })
}

fn doctor(green: &Green) -> (u8, String) {
    let (code, out, err) = invoke(&["doctor"], &borrowed(&green.pairs), &green.fake);
    assert_eq!(err, "");
    (code, out)
}

#[test]
fn doctor_is_content_when_everything_is_in_place() -> TestResult {
    let green = green()?;
    let (code, out) = doctor(&green);
    let app = green.rig.root().join("Goose.app");
    let expected = [
        format!("Avery {VERSION}"),
        "  ok      macOS 26.0.1".to_string(),
        format!("  ok      Goose Desktop 1.59.0 ({})", app.display()),
        format!("  ok      Splash ({SPLASH})"),
        "  ok      Modell im Cache".to_string(),
        "  ok      Modellserver laeuft".to_string(),
        format!(
            "  ok      Goose-Profil ({})",
            green.rig.goose_root().display()
        ),
        "  ok      YAML gueltig: config.yaml".to_string(),
        "  ok      YAML gueltig: permission.yaml".to_string(),
        "  ok      Senden, Verschieben und Kalenderaenderungen in Goose auf 'nachfragen' gestellt"
            .to_string(),
        format!("  ok      Mail-Server im Goose-Profil ({EXE})"),
        "  ok      Diktat: lokales Whisper-Modell small".to_string(),
    ];
    assert_eq!(&lines(&out)[..expected.len()], expected, "{out}");
    assert!(out.contains("  Mail: keine"), "{out}");
    assert!(out.trim_end().ends_with("\nAlles bereit."), "{out}");
    assert_eq!(code, 0);
    Ok(())
}

#[test]
fn doctor_names_what_is_missing_and_how_to_fix_it() -> TestResult {
    let rig = Rig::new()?;
    let nothing = Fake::default();
    let missing_app = rig.root().join("nirgends/Goose.app");
    let missing_text = missing_app.to_string_lossy().into_owned();
    let pairs = app_pairs(&rig, &[("AVERY_GOOSE_APP", &missing_text)]);
    let (code, out, _) = invoke(&["doctor"], &borrowed(&pairs), &nothing);
    assert_eq!(code, 1);
    for line in [
        "  ok      macOS ?",
        "  FEHLT   Goose Desktop -> brew install --cask block-goose",
        "  FEHLT   Splash -> brew install incoai/tap/splash",
        "  FEHLT   Modell nicht im Cache -> avery pull",
        "  FEHLT   Modellserver laeuft nicht -> brew services start avery",
        "  FEHLT   Goose-Profil -> avery init",
        "  Mail: keine",
    ] {
        assert!(out.lines().any(|l| l == line), "{line}\n{out}");
    }
    assert!(out.trim_end().ends_with("\n5 Punkt(e) offen."), "{out}");
    Ok(())
}

#[test]
fn doctor_checks_the_model_cache_for_the_configured_model() -> TestResult {
    let green = green()?;
    let other = format!("{}:Q4", "foo/bar");
    let mut pairs = green.pairs.clone();
    pairs.push(("AVERY_MODEL".to_string(), other.clone()));
    let (_, out, _) = invoke(&["doctor"], &borrowed(&pairs), &green.fake);
    assert!(out.contains("FEHLT   Modell nicht im Cache -> avery pull"));
    let hub = green
        .rig
        .root()
        .join(".cache/huggingface/hub/models--foo--bar/snapshots");
    fs::create_dir_all(hub)?;
    let (_, out, _) = invoke(&["doctor"], &borrowed(&pairs), &green.fake);
    assert!(out.contains("  ok      Modell im Cache"));
    Ok(())
}

#[test]
fn doctor_reports_unparsable_yaml_and_skips_the_check_without_ruby() -> TestResult {
    let mut green = green()?;
    green.fake = green.fake.with_ran("ruby", Some(1), "");
    let (code, out) = doctor(&green);
    let config = green.rig.goose_root().join("config/config.yaml");
    assert_eq!(code, 1);
    assert!(out.contains(&format!(
        "  FEHLT   YAML UNGUELTIG: {} (Goose kann die Konfiguration nicht laden)",
        config.display()
    )));
    green.fake.found.remove("ruby");
    let (code, out) = doctor(&green);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("YAML"));
    Ok(())
}

#[test]
fn doctor_insists_on_questions_before_sending_and_a_present_mail_server() -> TestResult {
    let mut green = green()?;
    green
        .rig
        .write(".avery/goose/config/permission.yaml", "ask_before: []\n")?;
    let (code, out) = doctor(&green);
    assert_eq!(code, 1);
    assert!(out.contains("  FEHLT   permission.yaml: mail__send_message, mail__move_messages oder mail__delete_event fehlt unter ask_before (avery init --force)"));
    green.fake.found.remove(EXE);
    let (_, out) = doctor(&green);
    assert!(out.contains(&format!(
        "  FEHLT   Goose-Profil verweist auf ein fehlendes Programm: {EXE} -> avery init --force"
    )));
    green
        .rig
        .write(".avery/goose/config/config.yaml", "extensions: {}\n")?;
    let (_, out) = doctor(&green);
    assert!(out.contains("  FEHLT   Goose-Profil ohne Mail-Server -> avery init --force"));
    Ok(())
}

#[test]
fn doctor_checks_the_local_dictation_and_its_whisper_model() -> TestResult {
    let green = green()?;
    let config = ".avery/goose/config/config.yaml";
    let text = fs::read_to_string(green.rig.goose_root().join("config/config.yaml"))?;
    assert!(text.contains(
        "voice_dictation_provider: local\nLOCAL_WHISPER_MODEL: small\nLOCAL_WHISPER_LANGUAGE: de\nGOOSE_LIVE_VOICE_ENABLED: false\n"
    ));
    fs::remove_file(
        green
            .rig
            .goose_root()
            .join("data/models/whisper-small-q8_0.gguf"),
    )?;
    let (code, out) = doctor(&green);
    assert_eq!(code, 1);
    assert!(out.contains("  FEHLT   Diktat: Whisper-Modell small fehlt -> einmalig in Goose laden (Settings > Chat > Voice Dictation Provider)"), "{out}");
    green.rig.write(
        config,
        &text.replace("LOCAL_WHISPER_MODEL: small", "LOCAL_WHISPER_MODEL: huge"),
    )?;
    let (_, out) = doctor(&green);
    assert!(out.contains("  FEHLT   Diktat: unbekanntes Whisper-Modell im Goose-Profil (huge) -> avery init --force"), "{out}");
    let without: Vec<&str> = text
        .lines()
        .filter(|line| !line.starts_with("voice_dictation_provider"))
        .collect();
    green.rig.write(config, &without.join("\n"))?;
    let (_, out) = doctor(&green);
    assert!(
        out.contains("  FEHLT   Diktat fehlt im Goose-Profil -> avery init --force"),
        "{out}"
    );
    green.rig.write(
        config,
        &text.replace(
            "voice_dictation_provider: local",
            "voice_dictation_provider: ''",
        ),
    )?;
    let (code, out) = doctor(&green);
    assert!(
        out.contains("  ok      Diktat: nicht lokal oder aus, so im Goose-Profil gewaehlt ()"),
        "{out}"
    );
    assert_eq!(code, 0, "{out}");
    Ok(())
}

#[test]
fn doctor_reports_an_unreadable_mail_configuration() -> TestResult {
    let green = green()?;
    green
        .rig
        .write(".avery/mail/config.json", "{\"version\": 2}\n")?;
    let (code, out) = doctor(&green);
    assert_eq!(code, 1);
    assert!(out.contains("  Fehler: "), "{out}");
    assert!(out.contains(
        "  FEHLT   Mail-Konfiguration unlesbar oder in unbekanntem Format (avery mail status)"
    ));
    Ok(())
}
