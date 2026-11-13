use crate::config::Settings;
use crate::json::{self, Json, ParseError};
use std::error::Error;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

pub(crate) type TestResult = Result<(), Box<dyn Error>>;

pub(crate) fn j(text: &str) -> Result<Json, ParseError> {
    json::parse(text)
}

pub(crate) struct TempDir {
    root: PathBuf,
}

impl TempDir {
    pub(crate) fn new() -> io::Result<TempDir> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let name = format!(
            "avery-mail-test-{}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst),
            nanos
        );
        let root = std::env::temp_dir().join(name);
        fs::create_dir_all(&root)?;
        Ok(TempDir { root })
    }

    pub(crate) fn path(&self, sub: &str) -> String {
        if sub.is_empty() {
            self.root.to_string_lossy().into_owned()
        } else {
            self.root.join(sub).to_string_lossy().into_owned()
        }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub(crate) fn settings_at(path: &str) -> Settings {
    Settings {
        config_path: path.to_string(),
        fake: false,
        fake_write: String::new(),
    }
}
