use std::time::Duration;

use crate::base64;
use crate::lock::{self, FileLock, LockError};
use crate::osascript::{run_fed, Finished};

pub(crate) const SECURITY: &str = "/usr/bin/security";
const SERVICE: &str = "avery-mail";
const TIMEOUT: Duration = Duration::from_secs(30);
const OUTPUT_LIMIT: usize = 16 * 1024;
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
    Busy,
    Unavailable(String),
}

impl std::fmt::Display for VaultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VaultError::Refused => f.write_str("entry refused"),
            VaultError::TooLarge => f.write_str("entry too large"),
            VaultError::Unreadable => f.write_str("entry unreadable"),
            VaultError::Busy => f.write_str(
                "another Avery command is using the Keychain (vault.lock), try again in a moment",
            ),
            VaultError::Unavailable(text) => f.write_str(text),
        }
    }
}

impl std::error::Error for VaultError {}

pub(crate) trait Vault {
    fn load(&self, name: &str) -> Result<Option<String>, VaultError>;
    fn store(&self, name: &str, secret: &str) -> Result<(), VaultError>;
    fn replace(&self, name: &str, expected: &str, secret: &str) -> Result<bool, VaultError>;
    fn delete(&self, name: &str) -> Result<(), VaultError>;
}

fn name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_LIMIT
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "@._+:-=".contains(c))
}

fn checked(name: &str, secret: &str) -> Result<(), VaultError> {
    if !name_ok(name) || secret.is_empty() {
        return Err(VaultError::Refused);
    }
    if secret.len() > MAX_RECORD {
        return Err(VaultError::TooLarge);
    }
    Ok(())
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
    lock: String,
    wait: Duration,
}

impl Keychain {
    pub(crate) fn system(lock: &str) -> Keychain {
        Keychain {
            program: SECURITY.to_string(),
            lock: lock.to_string(),
            wait: lock::WAIT,
        }
    }

    #[cfg(test)]
    pub(crate) fn at(program: &str, lock: &str) -> Keychain {
        Keychain {
            program: program.to_string(),
            lock: lock.to_string(),
            wait: lock::WAIT,
        }
    }

    fn locked(&self) -> Result<FileLock, VaultError> {
        lock::acquire(&self.lock, self.wait).map_err(|error| match error {
            LockError::Busy => VaultError::Busy,
            LockError::Io(kind) => {
                VaultError::Unavailable(format!("the Keychain lock could not be taken ({kind:?})"))
            }
        })
    }

    fn run(&self, args: &[&str], input: &str) -> Result<(i32, String), VaultError> {
        let finished = run_fed(
            &self.program,
            args,
            input.as_bytes(),
            TIMEOUT,
            OUTPUT_LIMIT,
            OUTPUT_LIMIT,
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

    fn read(&self, name: &str) -> Result<Option<String>, VaultError> {
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

    fn write(&self, name: &str, secret: &str) -> Result<(), VaultError> {
        let command = add_command(name, &base64::encode(secret.as_bytes()));
        let (code, _) = self.run(&["-i"], &command)?;
        match code {
            0 => Ok(()),
            other => Err(VaultError::Unavailable(format!(
                "the Keychain refused to store the entry (security exit {other})"
            ))),
        }
    }
}

impl Vault for Keychain {
    fn load(&self, name: &str) -> Result<Option<String>, VaultError> {
        if !name_ok(name) {
            return Err(VaultError::Refused);
        }
        self.read(name)
    }

    fn store(&self, name: &str, secret: &str) -> Result<(), VaultError> {
        checked(name, secret)?;
        let _lock = self.locked()?;
        self.write(name, secret)
    }

    fn replace(&self, name: &str, expected: &str, secret: &str) -> Result<bool, VaultError> {
        checked(name, secret)?;
        let _lock = self.locked()?;
        match self.read(name) {
            Ok(Some(current)) if current == expected => {}
            Ok(_) | Err(VaultError::Unreadable) => return Ok(false),
            Err(error) => return Err(error),
        }
        self.write(name, secret)?;
        Ok(true)
    }

    fn delete(&self, name: &str) -> Result<(), VaultError> {
        if !name_ok(name) {
            return Err(VaultError::Refused);
        }
        let _lock = self.locked()?;
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

        fn replace(&self, name: &str, expected: &str, secret: &str) -> Result<bool, VaultError> {
            self.0.replace(name, expected, secret)
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

        fn replace(&self, name: &str, expected: &str, secret: &str) -> Result<bool, VaultError> {
            if self.broken || self.read_only {
                return Err(VaultError::Unavailable("broken".to_string()));
            }
            if secret.len() > MAX_RECORD {
                return Err(VaultError::TooLarge);
            }
            let mut entries = self
                .entries
                .lock()
                .map_err(|_| VaultError::Unavailable("poisoned".to_string()))?;
            if entries.get(name).map(String::as_str) != Some(expected) {
                return Ok(false);
            }
            entries.insert(name.to_string(), secret.to_string());
            Ok(true)
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
    use std::sync::Barrier;
    use std::thread;
    use std::time::Instant;

    const NAME: &str = "google:alice@gmail.example";
    const OTHER: &str = "microsoft:bob@outlook.example";
    const STATEFUL: &str = r#"echo run >> "$DIR/ran.txt"
case "$1" in
  -i) cat > "$DIR/input.txt"; sed -n 's/.* -w "\([^"]*\)".*/\1/p' "$DIR/input.txt" > "$DIR/item.txt" ;;
  find-generic-password) [ -s "$DIR/item.txt" ] || exit 44; cat "$DIR/item.txt" ;;
esac"#;

    const RACY: &str = r#"echo "start $1" >> "$DIR/log.txt"
trap 'echo "end $1" >> "$DIR/log.txt"' EXIT
case "$1" in
  -i) cat > "$DIR/input.txt"; if mkdir "$DIR/slow" 2>/dev/null; then sleep 0.2; fi; sed -n 's/.* -w "\([^"]*\)".*/\1/p' "$DIR/input.txt" > "$DIR/item.txt" ;;
  find-generic-password) [ -s "$DIR/item.txt" ] || exit 44; cat "$DIR/item.txt" ;;
  delete-generic-password) rm -f "$DIR/item.txt" ;;
esac"#;
    const SLOW: &str = r#"echo "start $1" >> "$DIR/log.txt"
trap 'echo "end $1" >> "$DIR/log.txt"' EXIT
sleep 0.1
case "$1" in
  -i) cat > "$DIR/input.txt"; sed -n 's/.* -w "\([^"]*\)".*/\1/p' "$DIR/input.txt" > "$DIR/item.txt" ;;
  find-generic-password) [ -s "$DIR/item.txt" ] || exit 44; cat "$DIR/item.txt" ;;
  delete-generic-password) rm -f "$DIR/item.txt" ;;
esac"#;

    struct Fake {
        keychain: Keychain,
        input: String,
        ran: String,
    }

    fn scripted(dir: &TempDir, script: &str) -> Result<Keychain, Box<dyn std::error::Error>> {
        fake_security(dir, &format!("DIR='{}'\n{script}", dir.path("")))
    }

    fn seed(dir: &TempDir, text: &str) -> std::io::Result<()> {
        fs::write(
            dir.path("item.txt"),
            format!("{}\n", base64::encode(text.as_bytes())),
        )
    }

    fn item_exists(dir: &TempDir) -> bool {
        std::path::Path::new(&dir.path("item.txt")).exists()
    }

    fn lock_is_free(dir: &TempDir) -> bool {
        lock::acquire(&dir.path("vault.lock"), Duration::ZERO).is_ok()
    }

    fn fake_security(dir: &TempDir, script: &str) -> Result<Keychain, Box<dyn std::error::Error>> {
        let path = dir.path("security.sh");
        fs::write(&path, format!("#!/bin/sh\n{script}\n"))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
        Ok(Keychain::at(&path, &dir.path("vault.lock")))
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
        assert_eq!(keychain.load(NAME), Err(VaultError::Unreadable));
        fs::write(&out, format!("{}\n", "A".repeat(OUTPUT_LIMIT + 4)))?;
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
            Keychain::at("/nonexistent/security", "/nonexistent/vault.lock")
                .load("google:a@b.example"),
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

    #[test]
    fn the_memory_vault_replaces_only_the_expected_text() -> TestResult {
        let vault = memory::Memory::new();
        assert_eq!(vault.replace("x", "1", "2"), Ok(false));
        assert_eq!(vault.count(), 0);
        vault.store("x", "1")?;
        assert_eq!(vault.replace("x", "other", "2"), Ok(false));
        assert_eq!(vault.load("x")?, Some("1".to_string()));
        assert_eq!(vault.replace("x", "1", "2"), Ok(true));
        assert_eq!(vault.load("x")?, Some("2".to_string()));
        assert_eq!(vault.replace("x", "1", "3"), Ok(false));
        assert_eq!(
            vault.replace("x", "2", &"y".repeat(MAX_RECORD + 1)),
            Err(VaultError::TooLarge)
        );
        assert_eq!(vault.load("x")?, Some("2".to_string()));
        let shared = memory::Shared(std::sync::Arc::new(vault));
        assert_eq!(shared.replace("x", "2", "4"), Ok(true));
        assert_eq!(shared.load("x")?, Some("4".to_string()));
        let read_only = memory::Memory::new().with("x", "1").read_only();
        assert!(matches!(
            read_only.replace("x", "1", "2"),
            Err(VaultError::Unavailable(_))
        ));
        assert!(matches!(
            memory::Memory::broken().replace("x", "1", "2"),
            Err(VaultError::Unavailable(_))
        ));
        Ok(())
    }

    #[test]
    fn replace_stores_when_the_stored_text_equals_the_expected_text() -> TestResult {
        let dir = TempDir::new()?;
        let fake = stateful(&dir)?;
        fake.keychain.store(NAME, "old")?;
        assert_eq!(fake.keychain.replace(NAME, "old", "new"), Ok(true));
        assert_eq!(fake.keychain.load(NAME)?.as_deref(), Some("new"));
        let sent = fs::read_to_string(&fake.input)?;
        assert_eq!(sent, add_command(NAME, &base64::encode("new".as_bytes())));
        assert_eq!(sent.matches('\n').count(), 1);
        assert_eq!(sent.matches('"').count(), 8);
        assert!(!sent.contains("new"));
        assert_eq!(fake.keychain.replace(NAME, "new", "newer"), Ok(true));
        assert_eq!(fake.keychain.load(NAME)?.as_deref(), Some("newer"));
        assert!(lock_is_free(&dir));
        Ok(())
    }

    #[test]
    fn replace_refuses_when_the_stored_text_differs_and_writes_nothing() -> TestResult {
        let dir = TempDir::new()?;
        let fake = stateful(&dir)?;
        fake.keychain.store(NAME, "old")?;
        let before = fs::read_to_string(&fake.input)?;
        let spawned = runs(&fake);
        for expected in ["Old", "old ", "o\nld"] {
            assert_eq!(
                fake.keychain.replace(NAME, expected, "new"),
                Ok(false),
                "{expected:?}"
            );
        }
        assert_eq!(fake.keychain.load(NAME)?.as_deref(), Some("old"));
        assert_eq!(fs::read_to_string(&fake.input)?, before);
        assert_eq!(runs(&fake), spawned + 3 + 1);
        Ok(())
    }

    #[test]
    fn replace_refuses_a_missing_entry_and_creates_nothing() -> TestResult {
        let dir = TempDir::new()?;
        let fake = stateful(&dir)?;
        assert_eq!(fake.keychain.replace(NAME, "old", "new"), Ok(false));
        assert_eq!(runs(&fake), 1);
        assert!(!std::path::Path::new(&fake.input).exists());
        assert!(!item_exists(&dir));
        assert_eq!(fake.keychain.load(NAME)?, None);
        Ok(())
    }

    #[test]
    fn replace_treats_an_unreadable_stored_text_as_a_different_text() -> TestResult {
        let dir = TempDir::new()?;
        let marker = dir.path("added.txt");
        let keychain = scripted(
            &dir,
            &format!(
                "case \"$1\" in\n  -i) cat > '{marker}' ;;\n  find-generic-password) echo 'not base64 at all!' ;;\nesac"
            ),
        )?;
        assert_eq!(keychain.replace(NAME, "old", "new"), Ok(false));
        assert!(!std::path::Path::new(&marker).exists());
        Ok(())
    }

    #[test]
    fn replace_reports_an_unavailable_keychain_without_any_secret() -> TestResult {
        let dir = TempDir::new()?;
        let failing = fake_security(&dir, "cat >/dev/null; echo 'the secret is X' >&2; exit 51")?;
        let Err(VaultError::Unavailable(text)) =
            failing.replace(NAME, "EXPECTED-TEXT", "SECRET-TEXT")
        else {
            return Err("expected a failure".into());
        };
        assert!(text.contains("51"), "{text}");
        for hidden in ["secret", "SECRET-TEXT", "EXPECTED-TEXT", "X"] {
            assert!(!text.contains(hidden), "{hidden}: {text}");
        }
        let half = scripted(
            &dir,
            "case \"$1\" in\n  find-generic-password) printf 'RVhQRUNURUQ=\\n' ;;\n  -i) cat >/dev/null; exit 52 ;;\nesac",
        )?;
        let Err(VaultError::Unavailable(text)) = half.replace(NAME, "EXPECTED", "SECRET-TEXT")
        else {
            return Err("expected a failure".into());
        };
        assert!(
            text.contains("52") && !text.contains("SECRET-TEXT"),
            "{text}"
        );
        assert!(lock_is_free(&dir));
        Ok(())
    }

    #[test]
    fn replace_checks_name_and_size_before_the_lock_and_before_anything_runs() -> TestResult {
        let dir = TempDir::new()?;
        let marker = dir.path("ran.txt");
        let keychain = fake_security(&dir, &format!("touch '{marker}'"))?;
        assert_eq!(keychain.replace("a b", "x", "y"), Err(VaultError::Refused));
        assert_eq!(keychain.replace("a\"b", "x", "y"), Err(VaultError::Refused));
        assert_eq!(keychain.replace(NAME, "x", ""), Err(VaultError::Refused));
        assert_eq!(
            keychain.replace(NAME, "x", &"y".repeat(MAX_RECORD + 1)),
            Err(VaultError::TooLarge)
        );
        assert_eq!(
            keychain.replace(NAME, "x", &"é".repeat(MAX_RECORD / 2 + 1)),
            Err(VaultError::TooLarge)
        );
        assert!(!std::path::Path::new(&marker).exists());
        assert!(!std::path::Path::new(&dir.path("vault.lock")).exists());
        Ok(())
    }

    #[test]
    fn a_record_of_the_maximum_size_can_be_replaced_on_one_command_line() -> TestResult {
        let dir = TempDir::new()?;
        let fake = stateful(&dir)?;
        let name = format!("microsoft:{}", "a".repeat(NAME_LIMIT - "microsoft:".len()));
        fake.keychain.store(&name, "old")?;
        let record = "~".repeat(MAX_RECORD);
        assert_eq!(fake.keychain.replace(&name, "old", &record), Ok(true));
        let sent = fs::read_to_string(&fake.input)?;
        assert!(sent.len() <= LINE_LIMIT && sent.len() + 4 > LINE_LIMIT);
        assert_eq!(sent.matches('\n').count(), 1);
        assert_eq!(fake.keychain.load(&name)?, Some(record.clone()));
        assert_eq!(fake.keychain.replace(&name, &record, "small"), Ok(true));
        assert_eq!(fake.keychain.load(&name)?.as_deref(), Some("small"));
        Ok(())
    }

    #[test]
    fn a_racing_delete_never_lets_a_replace_bring_the_entry_back() -> TestResult {
        for round in 0..1 {
            let dir = TempDir::new()?;
            let keychain = scripted(&dir, RACY)?;
            seed(&dir, "old")?;
            let (replaced, deleted) = thread::scope(|scope| {
                let replacing = scope.spawn(|| keychain.replace(NAME, "old", "new"));
                thread::sleep(Duration::from_millis(70));
                let deleting = scope.spawn(|| keychain.delete(NAME));
                (replacing.join(), deleting.join())
            });
            assert_eq!(deleted.map_err(|_| "thread")?, Ok(()), "round {round}");
            let replaced = replaced.map_err(|_| "thread")?;
            assert!(matches!(replaced, Ok(true) | Ok(false)), "{replaced:?}");
            assert_eq!(keychain.load(NAME)?, None, "round {round}");
            assert!(!item_exists(&dir), "round {round}");
        }
        Ok(())
    }

    #[test]
    fn a_racing_store_is_never_overwritten_by_an_older_replace() -> TestResult {
        for round in 0..1 {
            let dir = TempDir::new()?;
            let keychain = scripted(&dir, RACY)?;
            seed(&dir, "old")?;
            let (replaced, stored) = thread::scope(|scope| {
                let replacing = scope.spawn(|| keychain.replace(NAME, "old", "renewed"));
                thread::sleep(Duration::from_millis(70));
                let relinking = scope.spawn(|| keychain.store(NAME, "relinked"));
                (replacing.join(), relinking.join())
            });
            assert_eq!(stored.map_err(|_| "thread")?, Ok(()), "round {round}");
            let replaced = replaced.map_err(|_| "thread")?;
            assert!(matches!(replaced, Ok(true) | Ok(false)), "{replaced:?}");
            assert_eq!(
                keychain.load(NAME)?.as_deref(),
                Some("relinked"),
                "round {round}"
            );
        }
        Ok(())
    }

    #[test]
    fn store_delete_and_replace_serialise_on_the_vault_lock() -> TestResult {
        let dir = TempDir::new()?;
        let keychain = scripted(&dir, SLOW)?;
        let barrier = Barrier::new(4);
        thread::scope(|scope| -> Result<(), Box<dyn std::error::Error>> {
            let workers = [
                scope.spawn(|| {
                    barrier.wait();
                    keychain.store(NAME, "a").map(|_| ())
                }),
                scope.spawn(|| {
                    barrier.wait();
                    keychain.delete(NAME)
                }),
                scope.spawn(|| {
                    barrier.wait();
                    keychain.replace(NAME, "a", "b").map(|_| ())
                }),
                scope.spawn(|| {
                    barrier.wait();
                    keychain.store(OTHER, "c")
                }),
            ];
            for worker in workers {
                worker.join().map_err(|_| "thread")??;
            }
            Ok(())
        })?;
        let log = fs::read_to_string(dir.path("log.txt"))?;
        let lines: Vec<&str> = log.lines().collect();
        assert!(lines.len() >= 8 && lines.len().is_multiple_of(2), "{log}");
        for pair in lines.chunks(2) {
            let [start, end] = pair else {
                return Err(format!("odd log: {log}").into());
            };
            let (Some(first), Some(last)) =
                (start.strip_prefix("start "), end.strip_prefix("end "))
            else {
                return Err(format!("two commands overlapped:\n{log}").into());
            };
            assert_eq!(first, last, "{log}");
        }
        assert!(lock_is_free(&dir));
        Ok(())
    }

    #[test]
    fn a_busy_lock_fails_closed_with_a_clear_message_and_runs_nothing() -> TestResult {
        let dir = TempDir::new()?;
        let marker = dir.path("ran.txt");
        let mut keychain = fake_security(
            &dir,
            &format!(
                "echo run >> '{marker}'\ncase \"$1\" in\n  find-generic-password) exit 44 ;;\n  *) cat >/dev/null ;;\nesac"
            ),
        )?;
        keychain.wait = Duration::from_millis(150);
        let held = lock::acquire(&dir.path("vault.lock"), Duration::ZERO)
            .map_err(|error| format!("{error:?}"))?;
        let started = Instant::now();
        for result in [
            keychain.store(NAME, "SECRET-TEXT"),
            keychain.delete(NAME),
            keychain
                .replace(NAME, "EXPECTED-TEXT", "SECRET-TEXT")
                .map(|_| ()),
        ] {
            assert_eq!(result, Err(VaultError::Busy));
        }
        let waited = started.elapsed();
        assert!(
            waited >= Duration::from_millis(450) && waited < Duration::from_secs(10),
            "{waited:?}"
        );
        assert!(!std::path::Path::new(&marker).exists());
        let text = VaultError::Busy.to_string();
        assert!(
            text.contains("another Avery command") && text.contains("vault.lock"),
            "{text}"
        );
        for hidden in ["SECRET-TEXT", "EXPECTED-TEXT", NAME] {
            assert!(!text.contains(hidden), "{text}");
        }
        assert_eq!(keychain.load(NAME)?, None);
        assert_eq!(fs::read_to_string(&marker)?.lines().count(), 1);
        drop(held);
        assert_eq!(keychain.delete(NAME), Ok(()));
        assert_eq!(keychain.store(NAME, "x"), Ok(()));
        Ok(())
    }

    #[test]
    fn the_lock_is_taken_for_one_command_and_never_kept_between_commands() -> TestResult {
        let dir = TempDir::new()?;
        let fake = stateful(&dir)?;
        assert!(lock_is_free(&dir));
        fake.keychain.store(NAME, "old")?;
        assert!(lock_is_free(&dir));
        assert_eq!(fake.keychain.replace(NAME, "old", "new"), Ok(true));
        assert!(lock_is_free(&dir));
        assert_eq!(fake.keychain.replace(NAME, "old", "new"), Ok(false));
        assert!(lock_is_free(&dir));
        fake.keychain.delete(NAME)?;
        assert!(lock_is_free(&dir));
        let failing = fake_security(&dir, "cat >/dev/null; exit 51")?;
        assert!(failing.store(NAME, "x").is_err());
        assert!(lock_is_free(&dir));
        assert!(failing.delete(NAME).is_err());
        assert!(lock_is_free(&dir));
        assert!(failing.replace(NAME, "x", "y").is_err());
        assert!(lock_is_free(&dir));
        let mode = fs::metadata(dir.path("vault.lock"))?.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(fs::metadata(dir.path("vault.lock"))?.len(), 0);
        Ok(())
    }

    #[test]
    fn a_lock_that_cannot_be_taken_is_reported_without_running_security() -> TestResult {
        let dir = TempDir::new()?;
        let marker = dir.path("ran.txt");
        let path = dir.path("security.sh");
        fs::write(&path, format!("#!/bin/sh\ntouch '{marker}'\n"))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
        fs::write(dir.path("file"), "x")?;
        let keychain = Keychain::at(&path, &dir.path("file/vault.lock"));
        for result in [
            keychain.store(NAME, "SECRET-TEXT"),
            keychain.delete(NAME),
            keychain
                .replace(NAME, "EXPECTED-TEXT", "SECRET-TEXT")
                .map(|_| ()),
        ] {
            let Err(VaultError::Unavailable(text)) = result else {
                return Err("expected a lock failure".into());
            };
            assert!(text.contains("lock could not be taken"), "{text}");
            assert!(
                !text.contains("SECRET-TEXT") && !text.contains("EXPECTED-TEXT"),
                "{text}"
            );
        }
        assert!(!std::path::Path::new(&marker).exists());
        Ok(())
    }

    #[test]
    fn the_keychain_lock_and_the_config_lock_are_independent_files() -> TestResult {
        let dir = TempDir::new()?;
        let settings = crate::testutil::settings_at(&dir.path("sub/config.json"));
        assert_eq!(settings.vault_lock_path(), dir.path("sub/vault.lock"));
        assert_eq!(settings.lock_path(), dir.path("sub/config.lock"));
        assert_ne!(settings.vault_lock_path(), settings.lock_path());
        assert_ne!(settings.vault_lock_path(), settings.config_path);
        let script = dir.path("security.sh");
        fs::write(&script, "#!/bin/sh\ncat >/dev/null\nexit 0\n")?;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755))?;
        let keychain = Keychain::at(&script, &settings.vault_lock_path());
        let config = lock::acquire(&settings.lock_path(), Duration::ZERO)
            .map_err(|error| format!("{error:?}"))?;
        keychain.store(NAME, "x")?;
        keychain.delete(NAME)?;
        drop(config);
        let vault = lock::acquire(&settings.vault_lock_path(), Duration::ZERO)
            .map_err(|error| format!("{error:?}"))?;
        assert!(
            lock::acquire(&settings.lock_path(), Duration::ZERO).is_ok(),
            "holding vault.lock must not hold config.lock"
        );
        drop(vault);
        let mode = fs::metadata(settings.vault_lock_path())?
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(
            fs::metadata(dir.path("sub"))?.permissions().mode() & 0o777,
            0o700
        );
        Ok(())
    }

    #[test]
    fn errors_of_the_vault_never_contain_the_secret_or_the_expected_text() {
        for error in [
            VaultError::Refused,
            VaultError::TooLarge,
            VaultError::Unreadable,
            VaultError::Busy,
            VaultError::Unavailable("the Keychain lock could not be taken (NotFound)".to_string()),
        ] {
            let text = error.to_string();
            assert!(!text.is_empty());
            for hidden in ["SECRET-TEXT", "EXPECTED-TEXT", "refresh", "{"] {
                assert!(!text.contains(hidden), "{text}");
            }
        }
    }
}
