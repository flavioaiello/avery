use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::thread;
use std::time::{Duration, Instant};

use crate::config::{dirname, make_dirs};

pub(crate) const WAIT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(50);

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LockError {
    Busy,
    Io(io::ErrorKind),
}

pub(crate) struct FileLock {
    file: File,
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

pub(crate) fn acquire(path: &str, wait: Duration) -> Result<FileLock, LockError> {
    let failed = |error: io::Error| LockError::Io(error.kind());
    make_dirs(&dirname(path), 0o700).map_err(failed)?;
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map_err(failed)?;
    let started = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(FileLock { file }),
            Err(TryLockError::Error(error)) => return Err(failed(error)),
            Err(TryLockError::WouldBlock) => {
                let left = wait.saturating_sub(started.elapsed());
                if left.is_zero() {
                    return Err(LockError::Busy);
                }
                thread::sleep(POLL.min(left));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{TempDir, TestResult};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::mpsc;

    fn taken(path: &str) -> Result<FileLock, String> {
        acquire(path, Duration::ZERO).map_err(|error| format!("{error:?}"))
    }

    #[test]
    fn a_lock_is_exclusive_until_it_is_dropped() -> TestResult {
        let dir = TempDir::new()?;
        let path = dir.path("a.lock");
        let first = taken(&path)?;
        assert!(matches!(
            acquire(&path, Duration::ZERO),
            Err(LockError::Busy)
        ));
        drop(first);
        let second = taken(&path)?;
        assert!(matches!(
            acquire(&path, Duration::ZERO),
            Err(LockError::Busy)
        ));
        drop(second);
        drop(taken(&path)?);
        Ok(())
    }

    #[test]
    fn different_lock_files_never_exclude_each_other() -> TestResult {
        let dir = TempDir::new()?;
        let first = taken(&dir.path("config.lock"))?;
        let second = taken(&dir.path("vault.lock"))?;
        drop(first);
        drop(second);
        Ok(())
    }

    #[test]
    fn dropping_the_lock_releases_it_even_while_a_copy_of_the_descriptor_lives_on() -> TestResult {
        let dir = TempDir::new()?;
        let path = dir.path("a.lock");
        let first = taken(&path)?;
        let inherited = first.file.try_clone()?;
        drop(first);
        drop(taken(&path)?);
        drop(inherited);
        Ok(())
    }

    #[test]
    fn the_lock_file_and_its_directories_are_private_and_stay_empty() -> TestResult {
        let dir = TempDir::new()?;
        let path = dir.path("sub/dir/a.lock");
        drop(taken(&path)?);
        let file = fs::metadata(&path)?;
        assert_eq!(file.permissions().mode() & 0o777, 0o600);
        assert_eq!(file.len(), 0);
        for sub in ["sub", "sub/dir"] {
            let mode = fs::metadata(dir.path(sub))?.permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{sub}");
        }
        drop(taken(&path)?);
        assert_eq!(fs::metadata(&path)?.len(), 0);
        Ok(())
    }

    #[test]
    fn waiting_is_bounded_and_ends_busy() -> TestResult {
        let dir = TempDir::new()?;
        let path = dir.path("a.lock");
        let held = taken(&path)?;
        let started = Instant::now();
        let outcome = acquire(&path, Duration::from_millis(200));
        let waited = started.elapsed();
        assert!(matches!(outcome, Err(LockError::Busy)));
        assert!(
            waited >= Duration::from_millis(200) && waited < Duration::from_secs(5),
            "{waited:?}"
        );
        drop(held);
        Ok(())
    }

    #[test]
    fn a_lock_that_is_released_in_time_is_taken() -> TestResult {
        let dir = TempDir::new()?;
        let path = dir.path("a.lock");
        let (locked, ready) = mpsc::channel();
        let holder = {
            let path = path.clone();
            thread::spawn(move || -> Result<(), String> {
                let held = taken(&path)?;
                locked.send(()).map_err(|_| "send".to_string())?;
                thread::sleep(Duration::from_millis(250));
                drop(held);
                Ok(())
            })
        };
        ready.recv_timeout(Duration::from_secs(5))?;
        let started = Instant::now();
        let lock = acquire(&path, Duration::from_secs(10));
        assert!(lock.is_ok());
        assert!(started.elapsed() >= Duration::from_millis(100));
        holder.join().map_err(|_| "thread")??;
        Ok(())
    }

    #[test]
    fn a_lock_that_cannot_be_created_is_an_error_and_not_a_wait() -> TestResult {
        let dir = TempDir::new()?;
        fs::create_dir(dir.path("is-a-directory"))?;
        fs::write(dir.path("file"), "x")?;
        let started = Instant::now();
        for path in [dir.path("is-a-directory"), dir.path("file/a.lock")] {
            assert!(
                matches!(
                    acquire(&path, Duration::from_secs(30)),
                    Err(LockError::Io(_))
                ),
                "{path}"
            );
        }
        assert!(started.elapsed() < Duration::from_secs(1));
        Ok(())
    }

    #[test]
    fn the_default_wait_is_bounded() {
        assert_eq!(WAIT, Duration::from_secs(5));
    }
}
