use std::io::Write;
use std::path::Path;

use super::{read_limited, say, Launcher, Refusal};
use crate::texts::VERSION;

struct Report<'a> {
    out: &'a mut dyn Write,
    missing: usize,
}

impl Report<'_> {
    fn ok(&mut self, text: &str) {
        say(self.out, &format!("  ok      {text}"));
    }

    fn miss(&mut self, text: &str) {
        self.missing += 1;
        say(self.out, &format!("  FEHLT   {text}"));
    }

    fn plain(&mut self, text: &str) {
        say(self.out, text);
    }
}

fn model_cached(l: &Launcher) -> bool {
    let repo = l.env.model.split(':').next().unwrap_or_default();
    l.env.hf_home.as_ref().is_some_and(|hf_home| {
        hf_home
            .join("hub")
            .join(format!("models--{}", repo.replace('/', "--")))
            .join("snapshots")
            .is_dir()
    })
}

fn valid_yaml(l: &Launcher, file: &Path) -> Option<bool> {
    l.system.find("ruby")?;
    let file = file.to_string_lossy();
    let ran = l
        .system
        .run("ruby", &["-ryaml", "-e", "YAML.load_file(ARGV[0])", &file])?;
    match ran.code {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

fn trimmed(text: Option<String>) -> Option<String> {
    text.map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn query(l: &Launcher, program: &str, args: &[&str]) -> Option<String> {
    trimmed(
        l.system
            .run(program, args)
            .filter(|ran| ran.code == Some(0))
            .map(|ran| ran.stdout),
    )
}

fn profile_program(config: &str) -> Option<&str> {
    config
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("cmd: \""))
        .and_then(|rest| rest.strip_suffix('"'))
}

const WHISPER_FILES: [(&str, &str); 4] = [
    ("tiny", "model-tiny-q80.gguf"),
    ("base", "whisper-base-q8_0.gguf"),
    ("small", "whisper-small-q8_0.gguf"),
    ("medium", "whisper-medium-q8_0.gguf"),
];

fn top_level<'a>(config: &'a str, key: &str) -> Option<&'a str> {
    config.lines().find_map(|line| {
        line.strip_prefix(key)
            .and_then(|rest| rest.strip_prefix(':'))
            .map(|value| value.trim().trim_matches(['\'', '"']))
    })
}

fn voice(l: &Launcher, config: &str, report: &mut Report) {
    match top_level(config, "voice_dictation_provider") {
        None => report.miss("Diktat fehlt im Goose-Profil -> avery init --force"),
        Some("local") => {
            let model = top_level(config, "LOCAL_WHISPER_MODEL").unwrap_or_default();
            match WHISPER_FILES.iter().find(|(id, _)| *id == model) {
                None => report.miss(&format!(
                    "Diktat: unbekanntes Whisper-Modell im Goose-Profil ({model}) -> avery init --force"
                )),
                Some((_, file)) if l.env.goose_root.join("data/models").join(file).is_file() => {
                    report.ok(&format!("Diktat: lokales Whisper-Modell {model}"));
                }
                Some(_) => report.miss(&format!(
                    "Diktat: Whisper-Modell {model} fehlt -> einmalig in Goose laden (Settings > Chat > Voice Dictation Provider)"
                )),
            }
        }
        Some(other) => report.ok(&format!(
            "Diktat: nicht lokal oder aus, so im Goose-Profil gewaehlt ({other})"
        )),
    }
}

fn profile(l: &Launcher, report: &mut Report) {
    let root = &l.env.goose_root;
    let config = root.join("config/config.yaml");
    if !config.is_file() {
        report.miss("Goose-Profil -> avery init");
        return;
    }
    report.ok(&format!("Goose-Profil ({})", root.display()));
    for name in ["config.yaml", "permission.yaml"] {
        let file = root.join("config").join(name);
        if !file.is_file() {
            continue;
        }
        match valid_yaml(l, &file) {
            Some(true) => report.ok(&format!("YAML gueltig: {name}")),
            Some(false) => report.miss(&format!(
                "YAML UNGUELTIG: {} (Goose kann die Konfiguration nicht laden)",
                file.display()
            )),
            None => {}
        }
    }
    let permission = read_limited(&root.join("config/permission.yaml")).unwrap_or_default();
    if [
        "mail__send_message",
        "mail__move_messages",
        "mail__delete_event",
    ]
    .iter()
    .all(|tool| permission.contains(tool))
    {
        report.ok("Senden, Verschieben und Kalenderaenderungen in Goose auf 'nachfragen' gestellt");
    } else {
        report.miss("permission.yaml: mail__send_message, mail__move_messages oder mail__delete_event fehlt unter ask_before (avery init --force)");
    }
    let text = read_limited(&config).unwrap_or_default();
    match profile_program(&text) {
        Some(program) if l.system.find(program).is_some() => {
            report.ok(&format!("Mail-Server im Goose-Profil ({program})"));
        }
        Some(program) => report.miss(&format!(
            "Goose-Profil verweist auf ein fehlendes Programm: {program} -> avery init --force"
        )),
        None => report.miss("Goose-Profil ohne Mail-Server -> avery init --force"),
    }
    voice(l, &text, report);
}

pub(crate) fn run(
    l: &Launcher,
    status: &mut dyn FnMut(&mut dyn Write) -> u8,
    out: &mut dyn Write,
) -> Result<u8, Refusal> {
    let mut report = Report { out, missing: 0 };
    report.plain(&format!("Avery {VERSION}"));
    if cfg!(target_os = "macos") {
        let version = query(l, "sw_vers", &["-productVersion"]).unwrap_or_else(|| "?".to_string());
        report.ok(&format!("macOS {version}"));
    } else {
        report.miss("kein macOS");
    }
    let app = &l.env.goose_app;
    if app.is_dir() {
        let info = app.join("Contents/Info");
        let version = query(
            l,
            "defaults",
            &[
                "read",
                &info.to_string_lossy(),
                "CFBundleShortVersionString",
            ],
        )
        .unwrap_or_else(|| "?".to_string());
        report.ok(&format!("Goose Desktop {version} ({})", app.display()));
    } else {
        report.miss("Goose Desktop -> brew install --cask block-goose");
    }
    match l.system.find("splash") {
        Some(path) => report.ok(&format!("Splash ({})", path.display())),
        None => report.miss("Splash -> brew install incoai/tap/splash"),
    }
    if model_cached(l) {
        report.ok("Modell im Cache");
    } else {
        report.miss("Modell nicht im Cache -> avery pull");
    }
    if l.system.healthy(&l.env.model_url) {
        report.ok("Modellserver laeuft");
    } else {
        report.miss("Modellserver laeuft nicht -> brew services start avery");
    }
    profile(l, &mut report);
    let mut buffer = Vec::new();
    let code = status(&mut buffer);
    for line in String::from_utf8_lossy(&buffer).lines() {
        report.plain(&format!("  {line}"));
    }
    if code != 0 {
        report.miss("Mail-Konfiguration unlesbar oder in unbekanntem Format (avery mail status)");
    }
    report.plain("");
    if report.missing == 0 {
        report.plain("Alles bereit.");
        Ok(0)
    } else {
        let missing = report.missing;
        report.plain(&format!("{missing} Punkt(e) offen."));
        Ok(1)
    }
}
