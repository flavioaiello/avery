use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{say, Launcher, Refusal};
use crate::civil::civil_from_days;

const CONFIG: &str = include_str!("../../profile/config.yaml.tmpl");
const PERMISSION: &str = include_str!("../../profile/permission.yaml");
const PROVIDER: &str = include_str!("../../profile/custom_splash.json");
const TOP_OF_MIND: &str = include_str!("../../profile/top-of-mind.md");
const ENV_TEMPLATE: &str = include_str!("../../profile/avery.env");
const MAX_BACKUPS: u32 = 99;
const PRIVATE: u32 = 0o700;
const NEXT: [&str; 6] = [
    "Weiter mit:",
    "  brew services start avery   (Modell als Dienst starten, auch bei jedem Login)",
    "  avery app                   (Goose mit dem Avery-Profil oeffnen; beim ersten Start fuehren Dialoge durch die Einrichtung von Mail und Kalender; wiederholen: avery mail onboard)",
    "  oder im Terminal:",
    "  avery mail setup            (je Konto und Kalender die Stufe waehlen: aus/lesen/schreiben; macOS fragt einmal nach der Automation-Berechtigung)",
    "  avery mail selftest         (Lesezugriff pruefen)",
];

fn failed(what: &str, path: &Path, error: &io::Error) -> Refusal {
    Refusal::new(format!("{what}: {}: {error}", path.display()))
}

fn yaml_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            c if c.is_control() => escaped.push_str(&format!("\\u{:04X}", u32::from(c))),
            c => escaped.push(c),
        }
    }
    escaped
}

fn quoted_part(path: &Path) -> Result<String, Refusal> {
    path.to_str()
        .map(yaml_escape)
        .ok_or_else(|| Refusal::new(format!("Pfad ist nicht UTF-8: {}", path.display())))
}

fn render(template: &str, root: &Path, exe: &Path) -> Result<String, Refusal> {
    Ok(template
        .replace("@GOOSE_ROOT@", &quoted_part(root)?)
        .replace("@AVERY@", &quoted_part(exe)?))
}

fn stamp(seconds: i64) -> String {
    let (year, month, day) = civil_from_days(seconds.div_euclid(86_400));
    let rest = seconds.rem_euclid(86_400);
    format!(
        "{year:04}{month:02}{day:02}{:02}{:02}{:02}",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

fn backup_path(dest: &Path) -> Result<PathBuf, Refusal> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let stamp = stamp(i64::try_from(seconds).unwrap_or(0));
    let name = dest
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Refusal::new(format!("Dateiname ist nicht UTF-8: {}", dest.display())))?;
    for attempt in 0..=MAX_BACKUPS {
        let suffix = if attempt == 0 {
            String::new()
        } else {
            format!("-{attempt}")
        };
        let candidate = dest.with_file_name(format!("{name}.bak-{stamp}{suffix}"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(Refusal::new(format!(
        "Zu viele Sicherungen: {}",
        dest.display()
    )))
}

fn copy_new(from: &Path, to: &Path) -> io::Result<()> {
    let mut source = File::open(from)?;
    let mut target = File::create_new(to)?;
    io::copy(&mut source, &mut target)?;
    target.sync_all()
}

fn write_atomic(dest: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = dest
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    let temp = dest.with_file_name(format!(".{name}.new-{}", std::process::id()));
    let result = File::create_new(&temp).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, dest)
    });
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn install(dest: &Path, text: &str, force: bool, out: &mut dyn Write) -> Result<(), Refusal> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| failed("Ordner nicht anlegbar", parent, &error))?;
    }
    if dest.exists() {
        if !force {
            say(
                out,
                &format!("  vorhanden, nicht ueberschrieben: {}", dest.display()),
            );
            return Ok(());
        }
        let backup = backup_path(dest)?;
        copy_new(dest, &backup)
            .map_err(|error| failed("Sicherung fehlgeschlagen", &backup, &error))?;
        say(out, &format!("  Sicherung: {}", backup.display()));
    }
    write_atomic(dest, text.as_bytes())
        .map_err(|error| failed("Datei nicht schreibbar", dest, &error))?;
    say(out, &format!("  geschrieben: {}", dest.display()));
    Ok(())
}

pub(crate) fn init(l: &Launcher, args: &[String], out: &mut dyn Write) -> Result<u8, Refusal> {
    let mut force = false;
    for arg in args {
        match arg.as_str() {
            "--force" => force = true,
            "-h" | "--help" => {
                say(
                    out,
                    "Aufruf: avery init [--force]   (--force ueberschreibt Dateien, mit Sicherung)",
                );
                return Ok(0);
            }
            other => return Err(Refusal::new(format!("Unbekannte Option: {other}"))),
        }
    }
    if !cfg!(target_os = "macos") {
        return Err(Refusal::new("Avery laeuft nur auf macOS."));
    }
    if !l.exe.is_absolute() {
        return Err(Refusal::new("Der Pfad von avery ist nicht ermittelbar."));
    }
    let root = &l.env.goose_root;
    say(out, &format!("Avery-Profil einrichten: {}", root.display()));
    let files = [
        (
            root.join("config/config.yaml"),
            render(CONFIG, root, &l.exe)?,
        ),
        (root.join("config/permission.yaml"), PERMISSION.to_string()),
        (
            root.join("config/custom_providers/custom_splash.json"),
            PROVIDER.to_string(),
        ),
        (root.join("top-of-mind.md"), TOP_OF_MIND.to_string()),
        (l.env.home.join("avery.env"), ENV_TEMPLATE.to_string()),
    ];
    for (dest, text) in &files {
        install(dest, text, force, out)?;
    }
    let mail = l.env.home.join("mail");
    fs::create_dir_all(&mail).map_err(|error| failed("Ordner nicht anlegbar", &mail, &error))?;
    for dir in [&l.env.home, &mail] {
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(PRIVATE));
    }
    say(out, "");
    if !l.env.goose_app.is_dir() {
        say(
            out,
            "Hinweis: Goose Desktop fehlt. Installieren mit: brew install --cask block-goose",
        );
    }
    if l.system.find("splash").is_none() {
        say(
            out,
            "Hinweis: Splash fehlt. Installieren mit: brew install incoai/tap/splash",
        );
    }
    say(out, "");
    for line in NEXT {
        say(out, line);
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml_text_is_escaped_for_double_quoted_scalars() {
        assert_eq!(yaml_escape("a&b|c d"), "a&b|c d");
        assert_eq!(yaml_escape("a\"b\\c"), "a\\\"b\\\\c");
        assert_eq!(yaml_escape("a\nb\u{7f}"), "a\\u000Ab\\u007F");
        assert_eq!(yaml_escape("Grüße"), "Grüße");
    }

    #[test]
    fn rendering_replaces_every_placeholder_and_keeps_the_scalars_closed() -> Result<(), Refusal> {
        let text = render(
            "a: \"@GOOSE_ROOT@/x\"\nb: \"@AVERY@\"\nc: \"@GOOSE_ROOT@\"\n",
            Path::new("/r\"oot"),
            Path::new("/bin/ave\\ry"),
        )?;
        assert_eq!(
            text,
            "a: \"/r\\\"oot/x\"\nb: \"/bin/ave\\\\ry\"\nc: \"/r\\\"oot\"\n"
        );
        assert!(!text.contains('@'));
        Ok(())
    }

    #[test]
    fn the_embedded_templates_have_the_placeholders_and_no_old_names() {
        assert!(CONFIG.contains("@GOOSE_ROOT@") && CONFIG.contains("@AVERY@"));
        for template in [CONFIG, PERMISSION, PROVIDER, TOP_OF_MIND, ENV_TEMPLATE] {
            assert!(!template.contains("private-agent"));
        }
    }

    #[test]
    fn stamps_are_utc_and_sortable() {
        assert_eq!(stamp(0), "19700101000000");
        assert_eq!(stamp(1_700_000_000), "20231114221320");
        assert_eq!(stamp(86_399), "19700101235959");
    }
}
