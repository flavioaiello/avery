use std::io::{self, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

pub(crate) const OSASCRIPT: &str = "/usr/bin/osascript";
pub(crate) const COMMON_SCRIPT: &str = include_str!("../jxa/common.js");
pub(crate) const DIALOGS_SCRIPT: &str = include_str!("../jxa/dialogs.js");
pub(crate) const DIALOG_SCRIPT: &str = include_str!("../jxa/dialog.js");
pub(crate) const MAIL_SCRIPT: &str = include_str!("../jxa/mail.js");
pub(crate) const CALENDAR_SCRIPT: &str = include_str!("../jxa/calendar.js");

fn assemble(body: &str) -> String {
    format!(
        "var INVISIBLE_RANGES = {};\n{COMMON_SCRIPT}\n{DIALOGS_SCRIPT}\n{body}",
        crate::text::hidden_ranges_js()
    )
}

pub(crate) fn mail_script() -> String {
    assemble(MAIL_SCRIPT)
}

pub(crate) fn calendar_script() -> String {
    assemble(CALENDAR_SCRIPT)
}

pub(crate) fn dialog_script() -> String {
    assemble(DIALOG_SCRIPT)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum App {
    Mail,
    Calendar,
    Dialog,
}

impl App {
    fn name(self) -> &'static str {
        match self {
            App::Mail => "Mail",
            App::Calendar => "Calendar",
            App::Dialog => "System Events",
        }
    }

    fn script(self) -> String {
        match self {
            App::Mail => mail_script(),
            App::Calendar => calendar_script(),
            App::Dialog => dialog_script(),
        }
    }
}
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
    run_fed(program, args, &[], timeout, stdout_limit, stderr_limit)
}

pub(crate) fn run_fed(
    program: &str,
    args: &[&str],
    input: &[u8],
    timeout: Duration,
    stdout_limit: usize,
    stderr_limit: usize,
) -> Finished {
    let spawned = Command::new(program)
        .args(args)
        .stdin(if input.is_empty() {
            Stdio::null()
        } else {
            Stdio::piped()
        })
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
    if let Some(mut stdin) = child.stdin.take() {
        let bytes = input.to_vec();
        thread::spawn(move || {
            let _ = stdin.write_all(&bytes);
        });
    }
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

fn is_gui_session(managername: &str) -> bool {
    managername.trim() == "Aqua"
}

pub(crate) fn gui_session() -> bool {
    matches!(
        run_command("/bin/launchctl", &["managername"], Duration::from_secs(5)),
        Finished::Done { code: Some(0), stdout, .. } if is_gui_session(&stdout)
    )
}

pub(crate) trait Host {
    fn ensure_running(&self, app: App);
    fn run(&self, app: App, payload: &str, timeout: Duration) -> Finished;
}

pub(crate) struct OsascriptHost;

fn running(app: App) -> bool {
    matches!(
        run_command(
            "/usr/bin/pgrep",
            &["-x", app.name()],
            Duration::from_secs(5)
        ),
        Finished::Done { code: Some(0), .. }
    )
}

impl Host for OsascriptHost {
    fn ensure_running(&self, app: App) {
        if app == App::Dialog || running(app) {
            return;
        }
        let _ = run_command(
            "/usr/bin/open",
            &["-g", "-j", "-a", app.name()],
            Duration::from_secs(20),
        );
        for _ in 0..40 {
            if running(app) {
                break;
            }
            thread::sleep(Duration::from_millis(500));
        }
        thread::sleep(Duration::from_secs(2));
    }

    fn run(&self, app: App, payload: &str, timeout: Duration) -> Finished {
        let script = app.script();
        run_command(
            OSASCRIPT,
            &["-l", "JavaScript", "-e", script.as_str(), payload],
            timeout,
        )
    }
}

#[cfg(test)]
pub(crate) mod clicks {
    use super::*;
    use crate::json::{self, Json};
    use std::sync::Mutex;

    #[derive(Clone)]
    pub(crate) struct Clicks {
        click: &'static str,
        seen: Arc<Mutex<Vec<String>>>,
    }

    impl Clicks {
        pub(crate) fn new(click: &'static str) -> Clicks {
            Clicks {
                click,
                seen: Arc::new(Mutex::new(Vec::new())),
            }
        }

        pub(crate) fn dialogs(&self) -> Vec<String> {
            self.seen
                .lock()
                .map(|seen| seen.clone())
                .unwrap_or_default()
        }
    }

    impl Host for Clicks {
        fn ensure_running(&self, _app: App) {}

        fn run(&self, _app: App, payload: &str, _timeout: Duration) -> Finished {
            if let Ok(mut seen) = self.seen.lock() {
                seen.push(payload.to_string());
            }
            let op = match json::parse(payload) {
                Ok(Json::Obj(object)) => object
                    .get("op")
                    .and_then(Json::as_str)
                    .unwrap_or_default()
                    .to_string(),
                _ => String::new(),
            };
            let decision = match (op.as_str(), self.click) {
                ("approve_send", "yes") => "send",
                ("approve_change", "yes") => "approved",
                (_, "timeout") => "timeout",
                _ => "declined",
            };
            Finished::Done {
                code: Some(0),
                stdout: format!("{{\"ok\": true, \"data\": {{\"decision\": \"{decision}\"}}}}"),
                stderr: String::new(),
            }
        }
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
    fn input_reaches_standard_input_and_is_optional() {
        assert_eq!(
            run_fed(
                "/bin/cat",
                &[],
                b"hello",
                Duration::from_secs(5),
                1024,
                1024
            ),
            Finished::Done {
                code: Some(0),
                stdout: "hello".into(),
                stderr: String::new()
            }
        );
        assert_eq!(
            run_fed("/bin/cat", &[], b"", Duration::from_secs(5), 1024, 1024),
            Finished::Done {
                code: Some(0),
                stdout: String::new(),
                stderr: String::new()
            }
        );
    }

    #[test]
    fn a_child_that_ignores_its_input_does_not_block_the_caller() {
        let big = vec![b'x'; 4 * 1024 * 1024];
        let started = Instant::now();
        let finished = run_fed(
            "/bin/sh",
            &["-c", "printf done"],
            &big,
            Duration::from_secs(10),
            1024,
            1024,
        );
        assert_eq!(
            finished,
            Finished::Done {
                code: Some(0),
                stdout: "done".into(),
                stderr: String::new()
            }
        );
        assert!(started.elapsed() < Duration::from_secs(5));
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
    fn only_the_aqua_session_counts_as_a_graphical_session() {
        for (name, expected) in [
            ("Aqua\n", true),
            (" Aqua ", true),
            ("Background\n", false),
            ("StandardIO\n", false),
            ("System", false),
            ("LoginWindow", false),
            ("aqua", false),
            ("Aqua Background", false),
            ("", false),
        ] {
            assert_eq!(is_gui_session(name), expected, "{name:?}");
        }
    }

    #[test]
    fn invalid_utf8_is_replaced_not_fatal() {
        assert!(matches!(
            shell("printf '\\377\\376ok'", 5000),
            Finished::Done { stdout, .. } if stdout.ends_with("ok")
        ));
    }
}
