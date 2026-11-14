use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

type TestResult = Result<(), Box<dyn Error>>;

const ROOT: &str = env!("CARGO_MANIFEST_DIR");
const THIS_FILE: &str = "tests/repo.rs";
const MAX_FILE: u64 = 4 * 1024 * 1024;

fn root() -> PathBuf {
    PathBuf::from(ROOT)
}

fn read(relative: &str) -> Result<String, Box<dyn Error>> {
    Ok(fs::read_to_string(root().join(relative))?)
}

fn walk(dir: &Path, found: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            if entry.file_name() == ".git" || entry.file_name() == "target" {
                continue;
            }
            walk(&entry.path(), found)?;
        } else if kind.is_file() {
            found.push(entry.path());
        }
    }
    Ok(())
}

fn project_texts() -> Result<Vec<(String, String)>, Box<dyn Error>> {
    let base = root();
    let mut paths = Vec::new();
    walk(&base, &mut paths)?;
    let mut texts = Vec::new();
    for path in paths {
        let relative = path.strip_prefix(&base)?.to_string_lossy().into_owned();
        if relative == THIS_FILE || fs::metadata(&path)?.len() > MAX_FILE {
            continue;
        }
        let bytes = fs::read(&path)?;
        if bytes.contains(&0) {
            continue;
        }
        texts.push((relative, String::from_utf8_lossy(&bytes).into_owned()));
    }
    Ok(texts)
}

fn run_of(text: &str, at: usize, wanted: impl Fn(char) -> bool, forward: bool) -> String {
    if forward {
        text[at..].chars().take_while(|c| wanted(*c)).collect()
    } else {
        let mut reversed: Vec<char> = text[..at]
            .chars()
            .rev()
            .take_while(|c| wanted(*c))
            .collect();
        reversed.reverse();
        reversed.into_iter().collect()
    }
}

fn mail_domain(run: &str) -> Option<&str> {
    let bytes = run.as_bytes();
    for (at, byte) in bytes.iter().enumerate().rev() {
        if *byte != b'.' || at == 0 {
            continue;
        }
        let letters = bytes[at + 1..]
            .iter()
            .take_while(|c| c.is_ascii_alphabetic())
            .count();
        if letters >= 2 {
            return Some(&run[..at + 1 + letters]);
        }
    }
    None
}

fn is_retina_name(domain: &str) -> bool {
    let bytes = domain.as_bytes();
    bytes.len() == 6 && bytes[0].is_ascii_digit() && &domain[1..] == "x.png"
}

fn allowed_domain(domain: &str) -> bool {
    let domain = domain.to_lowercase();
    domain.ends_with(".example")
        || ["example.org", "example.com", "example.invalid"].contains(&domain.as_str())
        || is_retina_name(&domain)
}

fn has_token(text: &str, prefix: &str, length: usize) -> bool {
    text.match_indices(prefix).any(|(at, _)| {
        text[at + prefix.len()..]
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .count()
            >= length
    })
}

fn has_private_key(text: &str) -> bool {
    let marker = "-----BEGIN ";
    text.match_indices(marker).any(|(at, _)| {
        let words: String = text[at + marker.len()..]
            .chars()
            .take_while(|c| c.is_ascii_uppercase() || *c == ' ')
            .collect();
        words.ends_with("PRIVATE KEY")
    })
}

fn findings(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let literals = [
        "/Users/".to_string(),
        ["github", "_pat_"].concat(),
        ["aiello", ".ch"].concat(),
        ["@micro", "soft.com"].concat(),
    ];
    for literal in &literals {
        if text.contains(literal.as_str()) {
            found.push(literal.clone());
        }
    }
    for prefix in [["gh", "o_"].concat(), ["gh", "p_"].concat()] {
        if has_token(text, &prefix, 20) {
            found.push(prefix);
        }
    }
    if has_private_key(text) {
        found.push("private key".to_string());
    }
    for (at, _) in text.match_indices('@') {
        let local = run_of(
            text,
            at,
            |c| c.is_ascii_alphanumeric() || "._%+-".contains(c),
            false,
        );
        let run = run_of(
            text,
            at + 1,
            |c| c.is_ascii_alphanumeric() || c == '.' || c == '-',
            true,
        );
        if local.is_empty() {
            continue;
        }
        if let Some(domain) = mail_domain(&run) {
            if !allowed_domain(domain) {
                found.push(format!("{local}@{domain}"));
            }
        }
    }
    found
}

#[test]
fn the_scanner_finds_planted_offences_and_skips_what_it_should() {
    let token = ["gh", "p_", "abcdefghijklmnopqrst"].concat();
    let key = ["-----", "BEGIN RSA PRIVATE KEY-----"].concat();
    for (text, wanted) in [
        ("pfad /Users/jane/Documents", "/Users/"),
        (token.as_str(), "ghp_"),
        (key.as_str(), "private key"),
        ("mail someone@evil.test", "someone@evil.test"),
    ] {
        assert!(
            findings(text).iter().any(|found| found.contains(wanted)),
            "{text}"
        );
    }
    let benign =
        "/home/x/.avery demo@gmail.example me@example.org icon_16x16@2x.png a@b me@example.org.";
    assert_eq!(findings(benign), Vec::<String>::new());
    assert!(findings("x@foo.bar9")
        .iter()
        .any(|found| found == "x@foo.bar"));
    assert!(findings("no at sign here").is_empty());
}

#[test]
fn no_personal_paths_addresses_or_credentials_are_in_the_repository() -> TestResult {
    let mut offences = Vec::new();
    for (relative, text) in project_texts()? {
        for found in findings(&text) {
            offences.push(format!("{relative}: {found}"));
        }
    }
    assert!(offences.is_empty(), "{offences:?}");
    Ok(())
}

#[test]
fn there_is_no_python_in_the_repository() -> TestResult {
    let mut offences = Vec::new();
    for (relative, text) in project_texts()? {
        if text.to_lowercase().contains("python") {
            offences.push(relative);
        }
    }
    assert!(offences.is_empty(), "{offences:?}");
    Ok(())
}

#[test]
fn the_launcher_lives_in_rust_only() {
    for gone in ["bin", "libexec", "share", "tools", "Casks"] {
        assert!(!root().join(gone).exists(), "{gone}");
    }
    for needed in [
        "Cargo.toml",
        "Cargo.lock",
        "profile/config.yaml.tmpl",
        "jxa/mail.js",
    ] {
        assert!(root().join(needed).is_file(), "{needed}");
    }
}

fn quoted_after(text: &str, key: &str) -> Option<String> {
    let line = text
        .lines()
        .find(|line| line.trim_start().starts_with(key))?;
    let start = line.find('"')? + 1;
    let end = line[start..].find('"')? + start;
    Some(line[start..end].to_string())
}

#[test]
fn the_versions_in_cargo_and_the_formula_agree() -> TestResult {
    let manifest = read("Cargo.toml")?;
    let version = quoted_after(&manifest, "version =").ok_or("no version in Cargo.toml")?;
    let lock = read("Cargo.lock")?;
    let after_name = lock
        .split("name = \"avery\"\n")
        .nth(1)
        .ok_or("avery is not in Cargo.lock")?;
    assert_eq!(
        quoted_after(after_name, "version =").as_deref(),
        Some(version.as_str())
    );
    let formula = read("Formula/avery.rb")?;
    let tag = quoted_after(&formula, "tag:").ok_or("no tag in the formula")?;
    assert_eq!(tag, format!("v{version}"));
    assert_eq!(avery::VERSION, version);
    Ok(())
}

#[test]
fn the_formula_builds_the_crate_and_runs_the_model_service() -> TestResult {
    let formula = read("Formula/avery.rb")?;
    for wanted in [
        "service do",
        "run [opt_bin/\"avery\", \"model\"]",
        "keep_alive crashed: true",
        "std_service_path_env",
        "depends_on \"rust\" => :build",
        "system \"cargo\", \"install\", *std_cargo_args",
        "url \"https://github.com/flavioaiello/avery.git\"",
        "head \"https://github.com/flavioaiello/avery.git\"",
        "avery serve",
    ] {
        assert!(formula.contains(wanted), "{wanted}");
    }
    for gone in [
        "git@github.com",
        "ssh://",
        "path: \"avery-mail\"",
        "bin.install",
        "libexec",
        "pkgshare",
        "avery-mail",
    ] {
        assert!(!formula.contains(gone), "{gone}");
    }
    assert!(!formula.to_lowercase().contains("python"));
    for text in [formula.as_str(), read("README.md")?.as_str()] {
        assert!(!text.contains("brew install --cask flavioaiello"));
    }
    Ok(())
}

#[test]
fn the_formula_is_valid_ruby() -> TestResult {
    let Ok(output) = Command::new("ruby")
        .arg("-c")
        .arg(root().join("Formula/avery.rb"))
        .output()
    else {
        return Ok(());
    };
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[test]
fn the_docs_explain_the_setup_dialogs_the_switch_and_the_permission() -> TestResult {
    let formula = read("Formula/avery.rb")?;
    let readme = read("README.md")?;
    for (name, text) in [("Formula", &formula), ("README", &readme)] {
        for wanted in [
            "avery mail onboard",
            "AVERY_NO_ONBOARD=1",
            "macOS may ask for permission to show notifications",
            "no dialog and no notification appears",
            "Automation (Mail and Calendar)",
        ] {
            assert!(text.contains(wanted), "{name}: {wanted}");
        }
        for gone in ["Automation later", "System Events"] {
            assert!(!text.contains(gone), "{name}: {gone}");
        }
    }
    assert!(formula.contains("macOS asks once for"));
    assert!(readme.contains("macOS asks once for Automation (Mail and Calendar)"));
    assert!(
        readme.contains("avery mail onboard --if-needed") && readme.contains("onboard.dismissed")
    );
    assert!(read("profile/avery.env")?.contains("AVERY_NO_ONBOARD"));
    Ok(())
}
