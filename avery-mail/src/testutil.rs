//! Helpers for unit tests.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A private directory that is removed when the value is dropped.
pub struct TempDir {
    root: PathBuf,
}

impl TempDir {
    pub fn new() -> TempDir {
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
        fs::create_dir_all(&root).expect("create test directory");
        TempDir { root }
    }

    /// The path of `sub` inside the directory (the directory itself for `""`).
    pub fn path(&self, sub: &str) -> String {
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
