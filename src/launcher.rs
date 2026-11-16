mod dock;
mod doctor;
mod profile;
mod start;
mod system;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};

use crate::cli;
use crate::config::Settings;
use crate::console::Screen;
use crate::context::Context;
use crate::server;
use crate::texts::{USAGE, VERSION};

pub(crate) use system::{own_path, Plan, RealSystem, System};

const KNOWN: [&str; 8] = [
    "AVERY_MODEL",
    "AVERY_MAX_MEMORY",
    "AVERY_MAX_CONTEXT",
    "AVERY_MAX_CACHE_DISK",
    "AVERY_OFFLINE",
    "AVERY_GOOSE_APP",
    "AVERY_GOOSE_ROOT",
    "AVERY_NO_ONBOARD",
];
const ENV_FILE_LIMIT: u64 = 64 * 1024;
const DEFAULT_MODEL: &str = "unsloth/Qwen3.8-27B-GGUF:UD-Q4_K_M";
const DEFAULT_MODEL_URL: &str = "http://127.0.0.1:8000/health";
const DEFAULT_GOOSE_APP: &str = "/Applications/Goose.app";
const DEFAULT_MAX_MEMORY: &str = "28G";
const DEFAULT_MAX_CONTEXT: &str = "100K";
const DEFAULT_MAX_CACHE_DISK: &str = "4G";

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Refusal(pub(crate) String);

impl Refusal {
    fn new(text: impl Into<String>) -> Refusal {
        Refusal(text.into())
    }
}

fn say(out: &mut dyn Write, text: &str) {
    let _ = writeln!(out, "{text}");
}

fn avery_home(lookup: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let present = |key: &str| lookup(key).filter(|value| !value.is_empty());
    match present("AVERY_HOME") {
        Some(home) => Some(PathBuf::from(home)),
        None => present("HOME").map(|home| PathBuf::from(home).join(".avery")),
    }
}

fn read_limited(path: &Path) -> Option<String> {
    let mut text = String::new();
    File::open(path)
        .ok()?
        .take(ENV_FILE_LIMIT)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

fn parse_env_file(text: &str) -> BTreeMap<String, String> {
    let mut values: BTreeMap<String, String> = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if !KNOWN.contains(&key) || values.get(key).is_some_and(|kept| !kept.is_empty()) {
            continue;
        }
        values.insert(key.to_string(), value.to_string());
    }
    values
}

pub(crate) struct Variables<'a> {
    real: &'a dyn Fn(&str) -> Option<String>,
    file: BTreeMap<String, String>,
}

impl<'a> Variables<'a> {
    pub(crate) fn load(real: &'a dyn Fn(&str) -> Option<String>) -> Variables<'a> {
        let file = avery_home(real)
            .and_then(|home| read_limited(&home.join("avery.env")))
            .map(|text| parse_env_file(&text))
            .unwrap_or_default();
        Variables { real, file }
    }

    pub(crate) fn get(&self, key: &str) -> Option<String> {
        let real = (self.real)(key);
        if !KNOWN.contains(&key) {
            return real;
        }
        real.filter(|value| !value.is_empty())
            .or_else(|| self.file.get(key).cloned())
    }

    fn present(&self, key: &str) -> Option<String> {
        self.get(key).filter(|value| !value.is_empty())
    }
}

pub(crate) struct Env {
    pub(crate) home: PathBuf,
    pub(crate) user: Option<PathBuf>,
    pub(crate) goose_root: PathBuf,
    pub(crate) goose_app: PathBuf,
    pub(crate) model_url: String,
    pub(crate) model: String,
    pub(crate) max_memory: String,
    pub(crate) max_context: String,
    pub(crate) max_cache_disk: String,
    pub(crate) offline: bool,
    pub(crate) dry_run: bool,
    pub(crate) hf_home: Option<PathBuf>,
}

impl Env {
    pub(crate) fn from_variables(variables: &Variables) -> Result<Env, Refusal> {
        let home = avery_home(&|key| variables.get(key))
            .ok_or_else(|| Refusal::new("HOME ist nicht gesetzt"))?;
        let text = |key: &str, default: &str| {
            variables
                .present(key)
                .unwrap_or_else(|| default.to_string())
        };
        let user = variables.present("HOME").map(PathBuf::from);
        Ok(Env {
            goose_root: variables
                .present("AVERY_GOOSE_ROOT")
                .map_or_else(|| home.join("goose"), PathBuf::from),
            goose_app: PathBuf::from(text("AVERY_GOOSE_APP", DEFAULT_GOOSE_APP)),
            model_url: text("AVERY_MODEL_URL", DEFAULT_MODEL_URL),
            model: text("AVERY_MODEL", DEFAULT_MODEL),
            max_memory: text("AVERY_MAX_MEMORY", DEFAULT_MAX_MEMORY),
            max_context: text("AVERY_MAX_CONTEXT", DEFAULT_MAX_CONTEXT),
            max_cache_disk: text("AVERY_MAX_CACHE_DISK", DEFAULT_MAX_CACHE_DISK),
            offline: text("AVERY_OFFLINE", "1") == "1",
            dry_run: variables.get("AVERY_DRY_RUN").as_deref() == Some("1"),
            hf_home: variables
                .present("HF_HOME")
                .map(PathBuf::from)
                .or_else(|| user.as_ref().map(|user| user.join(".cache/huggingface"))),
            user,
            home,
        })
    }
}

pub(crate) struct Launcher<'a> {
    pub(crate) env: Env,
    pub(crate) system: &'a dyn System,
    pub(crate) exe: PathBuf,
}

impl Launcher<'_> {
    fn begin(&self, plan: &Plan, out: &mut dyn Write) -> u8 {
        if self.env.dry_run {
            say(out, &plan.line());
            return 0;
        }
        let _ = out.flush();
        self.system.start(plan)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Launch {
    Init(Vec<String>),
    App { dialogs: bool },
    Cli(Vec<String>),
    Model(Vec<String>),
    Pull(Vec<String>),
    Doctor,
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Launch(Launch),
    Mail(Vec<String>),
    Serve,
    Version,
    Help,
    Unknown(String),
}

fn parse(args: &[String]) -> Command {
    let word = args
        .first()
        .map_or("help", |first| first.trim())
        .trim_end_matches(['.', ',', ';', ':']);
    let rest = || args.get(1..).unwrap_or_default().to_vec();
    match word {
        "" | "help" | "-h" | "--help" => Command::Help,
        "version" | "-V" | "--version" => Command::Version,
        "init" => Command::Launch(Launch::Init(rest())),
        "app" => match rest().as_slice() {
            [] => Command::Launch(Launch::App { dialogs: false }),
            [flag] if flag == "--dialogs" => Command::Launch(Launch::App { dialogs: true }),
            other => Command::Unknown(format!("app {}", other.join(" "))),
        },
        "cli" => Command::Launch(Launch::Cli(rest())),
        "model" => Command::Launch(Launch::Model(rest())),
        "pull" => Command::Launch(Launch::Pull(rest())),
        "doctor" => Command::Launch(Launch::Doctor),
        "mail" => Command::Mail(rest()),
        "serve" => Command::Serve,
        other => Command::Unknown(other.to_string()),
    }
}

pub(crate) struct Call<'a> {
    pub(crate) args: &'a [String],
    pub(crate) lookup: &'a dyn Fn(&str) -> Option<String>,
    pub(crate) system: &'a dyn System,
    pub(crate) screen: &'a Screen<'a>,
    pub(crate) exe: PathBuf,
}

pub(crate) fn dispatch(
    call: &Call,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    match parse(call.args) {
        Command::Version => {
            say(out, &format!("avery {VERSION}"));
            0
        }
        Command::Help => {
            say(out, USAGE);
            0
        }
        Command::Unknown(word) => {
            say(err, &format!("avery: unbekannter Befehl: {word}"));
            say(err, "");
            say(err, USAGE);
            2
        }
        Command::Serve => {
            let ctx = Context::from_env();
            u8::from(server::serve(&ctx, input, out).is_err())
        }
        Command::Mail(rest) => {
            let variables = Variables::load(call.lookup);
            let ctx = Context::from_settings(Settings::from_lookup(&|key| variables.get(key)));
            cli::dispatch(&rest, &ctx, call.screen, input, out)
        }
        Command::Launch(launch) => run(call, launch, out, err),
    }
}

fn run(call: &Call, launch: Launch, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let variables = Variables::load(call.lookup);
    let dialogs = matches!(launch, Launch::App { dialogs: true });
    let outcome = Env::from_variables(&variables).and_then(|env| {
        let launcher = Launcher {
            env,
            system: call.system,
            exe: call.exe.clone(),
        };
        let context = || Context::from_settings(Settings::from_lookup(&|key| variables.get(key)));
        match launch {
            Launch::Init(args) => profile::init(&launcher, &args, out),
            Launch::Model(args) => start::model(&launcher, &args, true, out),
            Launch::Pull(args) => start::pull(&launcher, &args, out),
            Launch::Cli(args) => start::goose_cli(&launcher, &args, out),
            Launch::App { dialogs } => {
                let ctx = context();
                let mut ui = start::AppUi {
                    ctx: &ctx,
                    screen: call.screen,
                    dialogs,
                };
                start::app(&launcher, ctx.settings.no_onboard, &mut ui, out, err)
            }
            Launch::Doctor => {
                let ctx = context();
                let mut status = |out: &mut dyn Write| {
                    let args = ["status".to_string()];
                    cli::dispatch(&args, &ctx, call.screen, &mut io::empty(), out)
                };
                doctor::run(&launcher, &mut status, out)
            }
        }
    });
    match outcome {
        Ok(code) => code,
        Err(refusal) => {
            say(err, &format!("avery: {}", refusal.0));
            if dialogs {
                start::tell(call.screen, &refusal.0);
            }
            1
        }
    }
}
