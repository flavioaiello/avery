use std::time::Duration;

use crate::base64;
use crate::osascript::{run_fed, Finished};

pub(crate) const SECURITY: &str = "/usr/bin/security";
const SERVICE: &str = "avery-mail";
const TIMEOUT: Duration = Duration::from_secs(30);
const NAME_LIMIT: usize = 300;
const SECURITY_LINE: usize = 4095;
const LINE_MARGIN: usize = 95;
const LINE_LIMIT: usize = SECURITY_LINE - LINE_MARGIN;
const COMMAND_OVERHEAD: usize = "add-generic-password -U -s \"\" -a \"\" -l \"Avery \" -w \"\"\n"
    .len()
    + SERVICE.len()
    + 2 * NAME_LIMIT;
pub(crate) const MAX_RECORD: usize = (LINE_LIMIT - COMMAND_OVERHEAD) / 4 * 3;
const MAX_STORED: usize = MAX_RECORD.div_ceil(3) * 4;
const NOT_FOUND: i32 = 44;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum VaultError {
    Refused,
    TooLarge,
    Unreadable,
    Unavailable(String),
}

impl std::fmt::Display for VaultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VaultError::Refused => f.write_str("entry refused"),
            VaultError::TooLarge => f.write_str("entry too large for the Keychain"),
            VaultError::Unreadable => f.write_str("stored entry unreadable"),
            VaultError::Unavailable(text) => f.write_str(text),
        }
    }
}

impl std::error::Error for VaultError {}

pub(crate) trait Vault {
    fn load(&self, name: &str) -> Result<Option<String>, VaultError>;
    fn store(&self, name: &str, secret: &str) -> Result<(), VaultError>;
    fn delete(&self, name: &str) -> Result<(), VaultError>;
}

fn name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_LIMIT
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "@._+:-=".contains(c))
}

fn add_command(name: &str, encoded: &str) -> String {
    format!(
        "add-generic-password -U -s \"{SERVICE}\" -a \"{name}\" -l \"Avery {name}\" -w \"{encoded}\"\n"
    )
}

fn decode(text: &str) -> Option<String> {
    if text.is_empty() || text.len() > MAX_STORED {
        return None;
    }
    let bytes = base64::decode(text)?;
    if base64::encode(&bytes) != text {
        return None;
    }
    String::from_utf8(bytes).ok()
}

pub(crate) struct Keychain {
    program: String,
}

impl Keychain {
    pub(crate) fn system() -> Keychain {
        Keychain {
            program: SECURITY.to_string(),
        }
    }

    #[cfg(test)]
    pub(crate) fn at(program: &str) -> Keychain {
        Keychain {
            program: program.to_string(),
        }
    }

    fn run(&self, args: &[&str], input: &str) -> Result<(i32, String), VaultError> {
        let finished = run_fed(
            &self.program,
            args,
            input.as_bytes(),
            TIMEOUT,
            MAX_STORED + 1,
            MAX_STORED + 1,
        );
        match finished {
            Finished::Done {
                code: Some(code),
                stdout,
                ..
            } => Ok((code, stdout)),
            Finished::Done { code: None, .. } => {
                Err(VaultError::Unavailable("security was stopped".to_string()))
            }
            Finished::TimedOut => Err(VaultError::Unavailable(
                "the Keychain did not answer in time".to_string(),
            )),
            Finished::TooLarge => Err(VaultError::Unavailable(
                "unexpected answer from security".to_string(),
            )),
            Finished::NotStarted => Err(VaultError::Unavailable(
                "security could not be started".to_string(),
            )),
        }
    }
}

impl Vault for Keychain {
    fn load(&self, name: &str) -> Result<Option<String>, VaultError> {
        if !name_ok(name) {
            return Err(VaultError::Refused);
        }
        let (code, stdout) = self.run(
            &["find-generic-password", "-s", SERVICE, "-a", name, "-w"],
            "",
        )?;
        match code {
            0 => decode(stdout.strip_suffix('\n').unwrap_or(&stdout))
                .map(Some)
                .ok_or(VaultError::Unreadable),
            NOT_FOUND => Ok(None),
            other => Err(VaultError::Unavailable(format!(
                "the Keychain refused to read the entry (security exit {other})"
            ))),
        }
    }

    fn store(&self, name: &str, secret: &str) -> Result<(), VaultError> {
        if !name_ok(name) || secret.is_empty() {
            return Err(VaultError::Refused);
        }
        if secret.len() > MAX_RECORD {
            return Err(VaultError::TooLarge);
        }
        let command = add_command(name, &base64::encode(secret.as_bytes()));
        let (code, _) = self.run(&["-i"], &command)?;
        match code {
            0 => Ok(()),
            other => Err(VaultError::Unavailable(format!(
                "the Keychain refused to store the entry (security exit {other})"
            ))),
        }
    }

    fn delete(&self, name: &str) -> Result<(), VaultError> {
        if !name_ok(name) {
            return Err(VaultError::Refused);
        }
        let (code, _) = self.run(&["delete-generic-password", "-s", SERVICE, "-a", name], "")?;
        match code {
            0 | NOT_FOUND => Ok(()),
            other => Err(VaultError::Unavailable(format!(
                "the Keychain refused to delete the entry (security exit {other})"
            ))),
        }
    }
}

#[cfg(test)]
pub(crate) mod memory {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    pub(crate) struct Memory {
        entries: Mutex<HashMap<String, String>>,
        broken: bool,
        read_only: bool,
    }

    impl Memory {
        pub(crate) fn new() -> Memory {
            Memory::default()
        }

        pub(crate) fn broken() -> Memory {
            Memory {
                broken: true,
                ..Memory::default()
            }
        }

        pub(crate) fn read_only(mut self) -> Memory {
            self.read_only = true;
            self
        }

        pub(crate) fn with(self, name: &str, secret: &str) -> Memory {
            if let Ok(mut entries) = self.entries.lock() {
                entries.insert(name.to_string(), secret.to_string());
            }
            self
        }

        pub(crate) fn get(&self, name: &str) -> Option<String> {
            self.entries.lock().ok()?.get(name).cloned()
        }

        pub(crate) fn count(&self) -> usize {
            self.entries.lock().map_or(0, |entries| entries.len())
        }
    }

    pub(crate) struct Shared(pub(crate) Arc<Memory>);

    impl Vault for Shared {
        fn load(&self, name: &str) -> Result<Option<String>, VaultError> {
            self.0.load(name)
        }

        fn store(&self, name: &str, secret: &str) -> Result<(), VaultError> {
            self.0.store(name, secret)
        }

        fn delete(&self, name: &str) -> Result<(), VaultError> {
            self.0.delete(name)
        }
    }

    impl Vault for Memory {
        fn load(&self, name: &str) -> Result<Option<String>, VaultError> {
            if self.broken {
                return Err(VaultError::Unavailable("broken".to_string()));
            }
            Ok(self.get(name))
        }

        fn store(&self, name: &str, secret: &str) -> Result<(), VaultError> {
            if self.broken || self.read_only {
                return Err(VaultError::Unavailable("broken".to_string()));
            }
            if secret.len() > MAX_RECORD {
                return Err(VaultError::TooLarge);
            }
            self.entries
                .lock()
                .map_err(|_| VaultError::Unavailable("poisoned".to_string()))?
                .insert(name.to_string(), secret.to_string());
            Ok(())
        }

        fn delete(&self, name: &str) -> Result<(), VaultError> {
            if self.broken || self.read_only {
                return Err(VaultError::Unavailable("broken".to_string()));
            }
            self.entries
                .lock()
                .map_err(|_| VaultError::Unavailable("poisoned".to_string()))?
                .remove(name);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{TempDir, TestResult};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    const NAME: &str = "google:alice@gmail.example";
    const STATEFUL: &str = r#"echo run >> "$DIR/ran.txt"
case "$1" in
  -i) cat > "$DIR/input.txt"; sed -n 's/.* -w "\([^"]*\)".*/\1/p' "$DIR/input.txt" > "$DIR/item.txt" ;;
  find-generic-password) [ -s "$DIR/item.txt" ] || exit 44; cat "$DIR/item.txt" ;;
esac"#;

    struct Fake {
        keychain: Keychain,
        input: String,
        ran: String,
    }

    fn fake_security(dir: &TempDir, script: &str) -> Result<Keychain, Box<dyn std::error::Error>> {
        let path = dir.path("security.sh");
        fs::write(&path, format!("#!/bin/sh\n{script}\n"))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
        Ok(Keychain::at(&path))
    }

    fn stateful(dir: &TempDir) -> Result<Fake, Box<dyn std::error::Error>> {
        let keychain = fake_security(dir, &format!("DIR='{}'\n{STATEFUL}", dir.path("")))?;
        Ok(Fake {
            keychain,
            input: dir.path("input.txt"),
            ran: dir.path("ran.txt"),
        })
    }

    fn runs(fake: &Fake) -> usize {
        fs::read_to_string(&fake.ran).map_or(0, |text| text.lines().count())
    }

    #[test]
    fn names_are_restricted_to_a_safe_alphabet() {
        assert!(name_ok("google:alice@gmail.example"));
        assert!(name_ok("microsoft:a.b+c@outlook.example"));
        for bad in ["", "a b", "a\"b", "a\nb", "a;b", "ä", "a$b", "a'b"] {
            assert!(!name_ok(bad), "{bad:?}");
        }
        assert!(!name_ok(&"a".repeat(NAME_LIMIT + 1)));
    }

    #[test]
    fn the_record_limit_is_derived_from_the_command_it_must_fit_into() {
        let name = "a".repeat(NAME_LIMIT);
        assert_eq!(add_command(&name, "").len(), COMMAND_OVERHEAD);
        let full = add_command(&name, &base64::encode(&vec![0u8; MAX_RECORD]));
        assert_eq!(base64::encode(&vec![0u8; MAX_RECORD]).len(), MAX_STORED);
        assert!(full.len() <= LINE_LIMIT, "{}", full.len());
        assert!(full.len() + 4 > LINE_LIMIT, "{}", full.len());
        let over = add_command(&name, &base64::encode(&vec![0u8; MAX_RECORD + 1]));
        assert!(over.len() > LINE_LIMIT, "{}", over.len());
    }

    #[test]
    fn a_secret_is_stored_as_base64_through_standard_input() -> TestResult {
        let dir = TempDir::new()?;
        let args = dir.path("args.txt");
        let input = dir.path("input.txt");
        let keychain = fake_security(
            &dir,
            &format!("printf '%s ' \"$@\" > '{args}'\ncat > '{input}'"),
        )?;
        keychain.store(NAME, "{\"t\": \"SECRET\"}")?;
        assert_eq!(fs::read_to_string(&args)?.trim(), "-i");
        let text = fs::read_to_string(&input)?;
        assert_eq!(
            text,
            "add-generic-password -U -s \"avery-mail\" -a \"google:alice@gmail.example\" -l \"Avery google:alice@gmail.example\" -w \"eyJ0IjogIlNFQ1JFVCJ9\"\n"
        );
        assert!(!text.contains("SECRET"));
        assert!(!text.contains(" -X "));
        Ok(())
    }

    #[test]
    fn a_record_of_every_shape_survives_the_keychain_on_one_command_line() -> TestResult {
        let dir = TempDir::new()?;
        let fake = stateful(&dir)?;
        let long = "~".repeat(MAX_RECORD);
        for secret in [
            "x",
            "{\"t\": \"SECRET\"}",
            "-X evil -w \"quoted\"",
            "a\" -U -s \"other\n-a \"x\\",
            "line\nbreak\r\n\ttab \u{0} nul",
            "äöü €😀",
            long.as_str(),
        ] {
            fake.keychain.store(NAME, secret)?;
            let sent = fs::read_to_string(&fake.input)?;
            assert_eq!(sent.matches('\n').count(), 1, "{secret:?}");
            assert!(sent.ends_with("\"\n"), "{secret:?}");
            assert_eq!(sent.matches('"').count(), 8, "{secret:?}");
            assert_eq!(fake.keychain.load(NAME)?.as_deref(), Some(secret));
        }
        Ok(())
    }

    #[test]
    fn a_record_of_the_maximum_size_fits_below_the_line_limit_of_security() -> TestResult {
        let dir = TempDir::new()?;
        let fake = stateful(&dir)?;
        let name = format!("microsoft:{}", "a".repeat(NAME_LIMIT - "microsoft:".len()));
        assert_eq!(name.len(), NAME_LIMIT);
        assert!(name_ok(&name));
        let wide = "é".repeat(MAX_RECORD / 2);
        assert_eq!(wide.len(), MAX_RECORD);
        for record in ["~".repeat(MAX_RECORD), wide] {
            fake.keychain.store(&name, &record)?;
            let sent = fs::read_to_string(&fake.input)?;
            assert!(sent.lines().all(|line| line.len() <= 4095));
            assert!(sent.len() <= 4095, "{}", sent.len());
            assert!(sent.len() <= LINE_LIMIT && sent.len() + 4 > LINE_LIMIT);
            assert_eq!(fake.keychain.load(&name)?, Some(record));
        }
        Ok(())
    }

    #[test]
    fn an_oversize_record_is_refused_before_security_runs_and_the_entry_stays() -> TestResult {
        let dir = TempDir::new()?;
        let fake = stateful(&dir)?;
        fake.keychain.store(NAME, "old")?;
        let before = runs(&fake);
        assert_eq!(before, 1);
        for size in [MAX_RECORD + 1, MAX_RECORD + 2, 4096, 64 * 1024] {
            assert_eq!(
                fake.keychain.store(NAME, &"x".repeat(size)),
                Err(VaultError::TooLarge),
                "{size}"
            );
        }
        assert_eq!(
            fake.keychain.store(NAME, &"é".repeat(MAX_RECORD / 2 + 1)),
            Err(VaultError::TooLarge)
        );
        assert_eq!(runs(&fake), before);
        assert_eq!(fake.keychain.load(NAME)?.as_deref(), Some("old"));
        assert!(VaultError::TooLarge.to_string().contains("too large"));
        Ok(())
    }

    #[test]
    fn a_secret_is_read_and_a_missing_entry_is_none() -> TestResult {
        let dir = TempDir::new()?;
        let args = dir.path("args.txt");
        let found = fake_security(
            &dir,
            &format!("printf '%s ' \"$@\" > '{args}'\nprintf 'eyJhIjogMX0=\\n'"),
        )?;
        assert_eq!(found.load(NAME)?, Some("{\"a\": 1}".to_string()));
        assert_eq!(
            fs::read_to_string(&args)?.trim(),
            "find-generic-password -s avery-mail -a google:alice@gmail.example -w"
        );
        let missing = fake_security(&dir, "exit 44")?;
        assert_eq!(missing.load(NAME)?, None);
        assert_eq!(missing.delete(NAME), Ok(()));
        Ok(())
    }

    #[test]
    fn only_canonical_base64_of_text_is_decoded() {
        let long = "A".repeat(MAX_STORED);
        assert_eq!(decode("Zg=="), Some("f".to_string()));
        assert_eq!(decode("Zm9vYmFy"), Some("foobar".to_string()));
        assert_eq!(decode("Pz8/"), Some("???".to_string()));
        assert_eq!(decode(&long).map(|text| text.len()), Some(MAX_RECORD));
        for bad in [
            "",
            "Pz8_",
            "Zg",
            "Zg=",
            "Zg===",
            "Zg=a",
            "Zh==",
            "Zm9v ",
            " Zm9v",
            "Zm9v\n",
            "Zm9v\r",
            "Zm 9v",
            "Zm9v=",
            "Zm9v!",
            "=Zm9v",
            "ä",
            "//79",
            "Zm9vYg==Zm9v",
            &"A".repeat(MAX_STORED + 4),
        ] {
            assert_eq!(decode(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn hostile_or_old_stored_text_is_unreadable_and_never_guessed() -> TestResult {
        let dir = TempDir::new()?;
        let out = dir.path("out.txt");
        let keychain = fake_security(&dir, &format!("cat '{out}'"))?;
        fs::write(&out, "Pz8/\n")?;
        assert_eq!(keychain.load(NAME)?, Some("???".to_string()));
        for stdout in [
            "{\"client_id\": \"a\", \"refresh_token\": \"b\", \"scope\": \"\"}\n",
            "",
            "\n",
            "Pz8_\n",
            "Zg\n",
            "Zh==\n",
            "Zm9v Ymfy\n",
            " Zm9v\n",
            "Zm9v \n",
            "Zm9v\nYmFy\n",
            "Zm9v\r\n",
            "Zm9v\n\n",
            "\u{1b}[31mZm9v\n",
            "Zm9v\u{0}\n",
            "//79\n",
        ] {
            fs::write(&out, stdout)?;
            assert_eq!(
                keychain.load(NAME),
                Err(VaultError::Unreadable),
                "{stdout:?}"
            );
        }
        fs::write(&out, format!("{}\n", "A".repeat(MAX_STORED + 4)))?;
        assert!(matches!(
            keychain.load(NAME),
            Err(VaultError::Unavailable(_))
        ));
        Ok(())
    }

    #[test]
    fn failures_are_reported_without_the_secret() -> TestResult {
        let dir = TempDir::new()?;
        let failing = fake_security(&dir, "cat >/dev/null; echo 'the secret is X' >&2; exit 51")?;
        for result in [
            failing.load("google:a@b.example").map(|_| ()),
            failing.store("google:a@b.example", "x"),
            failing.delete("google:a@b.example"),
        ] {
            let Err(VaultError::Unavailable(text)) = result else {
                return Err("expected a failure".into());
            };
            assert!(text.contains("51"), "{text}");
            assert!(!text.contains("secret"), "{text}");
        }
        assert!(matches!(
            Keychain::at("/nonexistent/security").load("google:a@b.example"),
            Err(VaultError::Unavailable(_))
        ));
        Ok(())
    }

    #[test]
    fn odd_names_and_secrets_never_reach_the_program() -> TestResult {
        let dir = TempDir::new()?;
        let marker = dir.path("ran.txt");
        let keychain = fake_security(&dir, &format!("touch '{marker}'"))?;
        assert_eq!(keychain.load("a b"), Err(VaultError::Refused));
        assert_eq!(keychain.store("a\"b", "x"), Err(VaultError::Refused));
        assert_eq!(
            keychain.store("google:a@b.example", ""),
            Err(VaultError::Refused)
        );
        assert_eq!(
            keychain.store("google:a@b.example", &"x".repeat(MAX_RECORD + 1)),
            Err(VaultError::TooLarge)
        );
        assert_eq!(keychain.delete("a;b"), Err(VaultError::Refused));
        assert!(!std::path::Path::new(&marker).exists());
        Ok(())
    }

    #[test]
    fn the_memory_vault_behaves_like_a_store() -> TestResult {
        let vault = memory::Memory::new();
        assert_eq!(vault.load("x")?, None);
        vault.store("x", "1")?;
        assert_eq!(vault.load("x")?, Some("1".to_string()));
        assert_eq!(
            vault.store("x", &"y".repeat(MAX_RECORD + 1)),
            Err(VaultError::TooLarge)
        );
        assert_eq!(vault.load("x")?, Some("1".to_string()));
        vault.delete("x")?;
        assert_eq!(vault.count(), 0);
        Ok(())
    }
}
