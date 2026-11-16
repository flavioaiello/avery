use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::profile::{failed, write_atomic, DATA, PROGRAM};
use super::{read_limited, say, Launcher, Refusal};

const IDENTIFIER: &str = "io.github.flavioaiello.avery";
const BUNDLE: &str = "Avery.app";
const PLIST: &str = include_str!("../../assets/Info.plist");
const ICON: &[u8] = include_bytes!("../../assets/Avery.icns");
const NO_HOME: &str = "Hinweis: Das Dock-Symbol wurde nicht angelegt (HOME ist nicht gesetzt).";

fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn utf8(path: &Path) -> Result<&str, Refusal> {
    path.to_str()
        .ok_or_else(|| Refusal::new(format!("Pfad ist nicht UTF-8: {}", path.display())))
}

fn script(l: &Launcher) -> Result<String, Refusal> {
    let mut text = String::from("#!/bin/sh\n");
    let usual = l.env.user.as_ref().map(|user| user.join(".avery"));
    if usual.as_ref() != Some(&l.env.home) {
        text.push_str(&format!(
            "export AVERY_HOME={}\n",
            quoted(utf8(&l.env.home)?)
        ));
    }
    text.push_str(&format!("exec {} app --dialogs\n", quoted(utf8(&l.exe)?)));
    Ok(text)
}

fn folder(l: &Launcher) -> Option<PathBuf> {
    l.env.user.as_ref().map(|user| user.join("Applications"))
}

fn ours(app: &Path) -> bool {
    read_limited(&app.join("Contents/Info.plist"))
        .is_some_and(|text| text.contains(&format!("<string>{IDENTIFIER}</string>")))
}

pub(crate) fn present(l: &Launcher) -> bool {
    folder(l).is_some_and(|folder| ours(&folder.join(BUNDLE)))
}

fn stage(l: &Launcher, dir: &Path) -> Result<(), Refusal> {
    let script = script(l)?;
    let files: [(&str, &[u8], u32); 3] = [
        ("Contents/Info.plist", PLIST.as_bytes(), DATA),
        ("Contents/Resources/Avery.icns", ICON, DATA),
        ("Contents/MacOS/Avery", script.as_bytes(), PROGRAM),
    ];
    for (relative, bytes, mode) in files {
        let file = dir.join(relative);
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| failed("Ordner nicht anlegbar", parent, &error))?;
        }
        write_atomic(&file, bytes, mode)
            .map_err(|error| failed("Datei nicht schreibbar", &file, &error))?;
    }
    Ok(())
}

fn place(staged: &Path, app: &Path, old: &Path) -> io::Result<()> {
    let replacing = fs::symlink_metadata(app).is_ok();
    if replacing {
        fs::rename(app, old)?;
    }
    if let Err(error) = fs::rename(staged, app) {
        if replacing {
            let _ = fs::rename(old, app);
        }
        return Err(error);
    }
    if replacing {
        let _ = fs::remove_dir_all(old);
    }
    Ok(())
}

pub(crate) fn install(l: &Launcher, force: bool, out: &mut dyn Write) -> Result<(), Refusal> {
    let Some(folder) = folder(l) else {
        say(out, NO_HOME);
        return Ok(());
    };
    let app = folder.join(BUNDLE);
    if let Ok(meta) = fs::symlink_metadata(&app) {
        if !meta.is_dir() || !ours(&app) {
            return Err(Refusal::new(format!(
                "{} gehoert nicht zu Avery und bleibt unveraendert. Entfernen mit: rm -rf {}  und danach: avery init",
                app.display(),
                quoted(utf8(&app)?)
            )));
        }
        if !force {
            say(
                out,
                &format!("  vorhanden, nicht ueberschrieben: {}", app.display()),
            );
            return Ok(());
        }
    }
    fs::create_dir_all(&folder)
        .map_err(|error| failed("Ordner nicht anlegbar", &folder, &error))?;
    let pid = std::process::id();
    let staged = folder.join(format!(".{BUNDLE}.new-{pid}"));
    let old = folder.join(format!(".{BUNDLE}.old-{pid}"));
    let _ = fs::remove_dir_all(&staged);
    let built = stage(l, &staged).and_then(|()| {
        place(&staged, &app, &old).map_err(|error| failed("App nicht anlegbar", &app, &error))
    });
    if built.is_err() {
        let _ = fs::remove_dir_all(&staged);
    }
    built?;
    say(out, &format!("  geschrieben: {}", app.display()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_quotes_survive_the_shell_quoting() {
        assert_eq!(
            quoted("/opt/homebrew/bin/avery"),
            "'/opt/homebrew/bin/avery'"
        );
        assert_eq!(quoted("a'b"), "'a'\\''b'");
        assert_eq!(quoted("a b\"c$d`e\\f"), "'a b\"c$d`e\\f'");
        assert_eq!(quoted(""), "''");
    }

    #[test]
    fn the_property_list_names_this_program_and_this_icon() {
        assert!(PLIST.contains(&format!("<string>{IDENTIFIER}</string>")));
        assert!(PLIST.contains("<key>CFBundleExecutable</key>\n\t<string>Avery</string>"));
        assert!(PLIST.contains("<key>CFBundleIconFile</key>\n\t<string>Avery</string>"));
        assert!(PLIST.contains("<key>LSUIElement</key>\n\t<true/>"));
        assert!(PLIST.contains("<key>NSAppleEventsUsageDescription</key>"));
    }

    #[test]
    fn the_icon_is_a_whole_icns_container_with_the_sizes_the_dock_needs() {
        assert!(ICON.starts_with(b"icns"));
        let be = |at: usize| {
            ICON.get(at..at + 4)
                .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
                .map(u32::from_be_bytes)
                .and_then(|value| usize::try_from(value).ok())
        };
        assert_eq!(be(4), Some(ICON.len()));
        let mut at = 8;
        let mut tags: Vec<&[u8]> = Vec::new();
        while at < ICON.len() {
            let length = be(at + 4).unwrap_or(0);
            assert!(length > 8 && at + length <= ICON.len(), "entry at {at}");
            let tag = ICON.get(at..at + 4).unwrap_or_default();
            let start: &[u8] = match tag {
                b"info" => b"bpli",
                b"ic04" => b"ARGB",
                _ => b"\x89PNG",
            };
            assert_eq!(ICON.get(at + 8..at + 12), Some(start), "entry at {at}");
            tags.push(tag);
            at += length;
        }
        assert_eq!(at, ICON.len());
        for wanted in [&b"ic07"[..], b"ic09", b"ic11", b"ic13"] {
            assert!(tags.contains(&wanted), "{wanted:?}");
        }
    }
}
