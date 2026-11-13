use std::io::{self, Read};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

pub(crate) const OSASCRIPT: &str = "/usr/bin/osascript";
pub(crate) const MAIL_SCRIPT: &str = include_str!("../jxa/mail.js");
pub(crate) const STDOUT_LIMIT: usize = 8 * 1024 * 1024;
pub(crate) const STDERR_LIMIT: usize = 64 * 1024;
const POLL: Duration = Duration::from_millis(20);
const COLLECT: Duration = Duration::from_secs(2);

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Finished {
    Done {
        code: Option<i32>,
        stdout: String,
        stderr: String,
    },
    TimedOut,
    TooLarge,
    NotStarted,
}

struct Reader {
    bytes: Receiver<Vec<u8>>,
    over: Arc<AtomicBool>,
}

fn drain<R: Read + Send + 'static>(mut source: R, limit: usize) -> Reader {
    let (sender, bytes) = mpsc::channel();
    let over = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&over);
    thread::spawn(move || {
        let mut kept = Vec::new();
        let cap = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
        let _ = (&mut source).take(cap).read_to_end(&mut kept);
        if kept.len() > limit {
            flag.store(true, Ordering::SeqCst);
            kept.truncate(limit);
            let _ = io::copy(&mut source, &mut io::sink());
        }
        let _ = sender.send(kept);
    });
    Reader { bytes, over }
}

fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

pub(crate) fn run_limited(
    program: &str,
    args: &[&str],
    timeout: Duration,
    stdout_limit: usize,
    stderr_limit: usize,
) -> Finished {
    let spawned = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let Ok(mut child) = spawned else {
        return Finished::NotStarted;
    };
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        stop(&mut child);
        return Finished::NotStarted;
    };
    let out = drain(stdout, stdout_limit);
    let err = drain(stderr, stderr_limit);
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(_) => {
                stop(&mut child);
                return Finished::NotStarted;
            }
        }
        if out.over.load(Ordering::SeqCst) {
            stop(&mut child);
            return Finished::TooLarge;
        }
        if Instant::now() >= deadline {
            stop(&mut child);
            return Finished::TimedOut;
        }
        thread::sleep(POLL);
    };
    let stdout = out.bytes.recv_timeout(COLLECT).unwrap_or_default();
    let stderr = err.bytes.recv_timeout(COLLECT).unwrap_or_default();
    if out.over.load(Ordering::SeqCst) {
        return Finished::TooLarge;
    }
    Finished::Done {
        code: status.code(),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    }
}

pub(crate) fn run_command(program: &str, args: &[&str], timeout: Duration) -> Finished {
    run_limited(program, args, timeout, STDOUT_LIMIT, STDERR_LIMIT)
}

pub(crate) trait Host {
    fn ensure_mail_running(&self);
    fn run(&self, payload: &str, timeout: Duration) -> Finished;
}

pub(crate) struct OsascriptHost;

fn mail_running() -> bool {
    matches!(
        run_command("/usr/bin/pgrep", &["-x", "Mail"], Duration::from_secs(5)),
        Finished::Done { code: Some(0), .. }
    )
}

impl Host for OsascriptHost {
    fn ensure_mail_running(&self) {
        if mail_running() {
            return;
        }
        let _ = run_command(
            "/usr/bin/open",
            &["-g", "-j", "-a", "Mail"],
            Duration::from_secs(20),
        );
        for _ in 0..40 {
            if mail_running() {
                break;
            }
            thread::sleep(Duration::from_millis(500));
        }
        thread::sleep(Duration::from_secs(2));
    }

    fn run(&self, payload: &str, timeout: Duration) -> Finished {
        run_command(
            OSASCRIPT,
            &["-l", "JavaScript", "-e", MAIL_SCRIPT, payload],
            timeout,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(script: &str, timeout_ms: u64) -> Finished {
        run_command(
            "/bin/sh",
            &["-c", script],
            Duration::from_millis(timeout_ms),
        )
    }

    #[test]
    fn output_and_exit_code_are_returned() {
        assert_eq!(
            shell("printf hello; printf oops >&2; exit 3", 5000),
            Finished::Done {
                code: Some(3),
                stdout: "hello".into(),
                stderr: "oops".into()
            }
        );
    }

    #[test]
    fn a_missing_program_is_reported_as_not_started() {
        assert_eq!(
            run_command("/nonexistent/program", &[], Duration::from_secs(1)),
            Finished::NotStarted
        );
    }

    #[test]
    fn a_slow_process_is_killed_at_the_deadline() {
        let started = Instant::now();
        assert_eq!(shell("sleep 30", 300), Finished::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn too_much_output_stops_the_process() {
        let started = Instant::now();
        let finished = run_limited(
            "/usr/bin/yes",
            &["abcdefghij"],
            Duration::from_secs(20),
            1024 * 1024,
            1024,
        );
        assert_eq!(finished, Finished::TooLarge);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn output_just_under_the_limit_is_kept() {
        let finished = run_limited(
            "/bin/sh",
            &["-c", "printf 12345678"],
            Duration::from_secs(5),
            8,
            8,
        );
        assert_eq!(
            finished,
            Finished::Done {
                code: Some(0),
                stdout: "12345678".into(),
                stderr: String::new()
            }
        );
    }

    #[test]
    fn noisy_stderr_is_cut_but_does_not_block_the_process() {
        let finished = run_limited(
            "/bin/sh",
            &["-c", "i=0; while [ $i -lt 2000 ]; do printf 'xxxxxxxxxx' >&2; i=$((i+1)); done; printf done"],
            Duration::from_secs(10),
            1024,
            100,
        );
        assert!(matches!(
            finished,
            Finished::Done { code: Some(0), ref stdout, ref stderr }
                if stdout == "done" && stderr.len() == 100
        ));
    }

    #[test]
    fn a_process_killed_by_a_signal_has_no_exit_code() {
        assert!(matches!(
            shell("kill -9 $$", 5000),
            Finished::Done { code: None, .. }
        ));
    }

    #[test]
    fn invalid_utf8_is_replaced_not_fatal() {
        assert!(matches!(
            shell("printf '\\377\\376ok'", 5000),
            Finished::Done { stdout, .. } if stdout.ends_with("ok")
        ));
    }
}
