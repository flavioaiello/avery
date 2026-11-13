use std::time::Duration;

use crate::osascript::{run_fed, Finished};

pub(crate) const SECURITY: &str = "/usr/bin/security";
const SERVICE: &str = "avery-mail";
const TIMEOUT: Duration = Duration::from_secs(30);
const SECRET_LIMIT: usize = 16 * 1024;
const NAME_LIMIT: usize = 300;
const NOT_FOUND: i32 = 44;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum VaultError {
    Refused,
    Unavailable(String),
}

impl std::fmt::Display for VaultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VaultError::Refused => f.write_str("entry refused"),
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

fn hex(text: &str) -> String {
    text.bytes().map(|byte| format!("{byte:02x}")).collect()
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
            SECRET_LIMIT,
            SECRET_LIMIT,
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
            0 => Ok(Some(
                stdout.strip_suffix('\n').unwrap_or(&stdout).to_string(),
            )),
            NOT_FOUND => Ok(None),
            other => Err(VaultError::Unavailable(format!(
                "the Keychain refused to read the entry (security exit {other})"
            ))),
        }
    }

    fn store(&self, name: &str, secret: &str) -> Result<(), VaultError> {
        if !name_ok(name) || secret.is_empty() || secret.len() > SECRET_LIMIT / 2 {
            return Err(VaultError::Refused);
        }
        let command = format!(
            "add-generic-password -U -s \"{SERVICE}\" -a \"{name}\" -l \"Avery {name}\" -X \"{}\"\n",
            hex(secret)
        );
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

    fn fake_security(dir: &TempDir, script: &str) -> Result<Keychain, Box<dyn std::error::Error>> {
        let path = dir.path("security.sh");
        fs::write(&path, format!("#!/bin/sh\n{script}\n"))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
        Ok(Keychain::at(&path))
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
    fn a_secret_is_stored_as_hex_through_standard_input() -> TestResult {
        let dir = TempDir::new()?;
        let args = dir.path("args.txt");
        let input = dir.path("input.txt");
        let keychain = fake_security(
            &dir,
            &format!("printf '%s ' \"$@\" > '{args}'\ncat > '{input}'"),
        )?;
        keychain.store("google:alice@gmail.example", "{\"t\": \"SECRET\"}")?;
        assert_eq!(fs::read_to_string(&args)?.trim(), "-i");
        let text = fs::read_to_string(&input)?;
        assert!(text.starts_with(
            "add-generic-password -U -s \"avery-mail\" -a \"google:alice@gmail.example\""
        ));
        assert!(text.contains("-X \"7b2274223a2022534543524554227d\"\n"));
        assert!(!text.contains("SECRET"));
        assert_eq!(text.matches('\n').count(), 1);
        Ok(())
    }

    #[test]
    fn a_secret_is_read_and_a_missing_entry_is_none() -> TestResult {
        let dir = TempDir::new()?;
        let args = dir.path("args.txt");
        let found = fake_security(
            &dir,
            &format!("printf '%s ' \"$@\" > '{args}'\nprintf '{{\"a\": 1}}\\n'"),
        )?;
        assert_eq!(
            found.load("google:alice@gmail.example")?,
            Some("{\"a\": 1}".to_string())
        );
        assert_eq!(
            fs::read_to_string(&args)?.trim(),
            "find-generic-password -s avery-mail -a google:alice@gmail.example -w"
        );
        let missing = fake_security(&dir, "exit 44")?;
        assert_eq!(missing.load("google:alice@gmail.example")?, None);
        assert_eq!(missing.delete("google:alice@gmail.example"), Ok(()));
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
            keychain.store("google:a@b.example", &"x".repeat(SECRET_LIMIT)),
            Err(VaultError::Refused)
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
        vault.delete("x")?;
        assert_eq!(vault.count(), 0);
        Ok(())
    }
}
