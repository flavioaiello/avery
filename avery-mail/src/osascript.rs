use std::io::{self, Read};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

pub(crate) const OSASCRIPT: &str = "/usr/bin/osascript";
pub(crate) const LAUNCHCTL: &str = "/bin/launchctl";
pub(crate) const COMMON_SCRIPT: &str = include_str!("../jxa/common.js");
pub(crate) const DIALOGS_SCRIPT: &str = include_str!("../jxa/dialogs.js");
pub(crate) const DIALOG_SCRIPT: &str = include_str!("../jxa/dialog.js");
pub(crate) const MAIL_SCRIPT: &str = include_str!("../jxa/mail.js");
pub(crate) const CALENDAR_SCRIPT: &str = include_str!("../jxa/calendar.js");

fn assemble(libraries: &[&str], body: &str) -> String {
    format!(
        "var INVISIBLE_RANGES = {};\n{}\n{body}",
        crate::text::hidden_ranges_js(),
        libraries.join("\n")
    )
}

pub(crate) fn mail_script() -> String {
    assemble(&[COMMON_SCRIPT, DIALOGS_SCRIPT], MAIL_SCRIPT)
}

pub(crate) fn calendar_script() -> String {
    assemble(&[COMMON_SCRIPT, DIALOGS_SCRIPT], CALENDAR_SCRIPT)
}

pub(crate) fn dialog_script() -> String {
    assemble(&[COMMON_SCRIPT], DIALOG_SCRIPT)
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
            App::Dialog => "osascript",
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

fn is_gui_session(managername: &str) -> bool {
    managername.trim() == "Aqua"
}

pub(crate) trait Host {
    fn ensure_running(&self, app: App);
    fn run(&self, app: App, payload: &str, timeout: Duration) -> Finished;
}

pub(crate) struct OsascriptHost;

pub(crate) struct Programs {
    osascript: Option<String>,
    launchctl: Option<String>,
}

pub(crate) struct ScreenHost<'a> {
    osascript: Option<&'a str>,
}

#[cfg(debug_assertions)]
fn overridden(lookup: &dyn Fn(&str) -> Option<String>, key: &str) -> Option<String> {
    lookup(key).filter(|value| !value.is_empty())
}

#[cfg(not(debug_assertions))]
fn overridden(_lookup: &dyn Fn(&str) -> Option<String>, _key: &str) -> Option<String> {
    None
}

impl Programs {
    pub(crate) fn from_lookup(lookup: &dyn Fn(&str) -> Option<String>) -> Programs {
        let program = |key: &str, system: &str| match overridden(lookup, key) {
            None => Some(system.to_string()),
            Some(path) if path.starts_with('/') => Some(path),
            Some(_) => None,
        };
        Programs {
            osascript: program("AVERY_OSASCRIPT", OSASCRIPT),
            launchctl: program("AVERY_LAUNCHCTL", LAUNCHCTL),
        }
    }

    pub(crate) fn from_env() -> Programs {
        Programs::from_lookup(&|key| {
            std::env::var_os(key).map(|value| value.to_string_lossy().into_owned())
        })
    }

    pub(crate) fn host(&self) -> ScreenHost<'_> {
        ScreenHost {
            osascript: self.osascript.as_deref(),
        }
    }

    pub(crate) fn graphical(&self) -> bool {
        self.launchctl.as_deref().is_some_and(|program| {
            matches!(
                run_command(program, &["managername"], Duration::from_secs(5)),
                Finished::Done { code: Some(0), stdout, .. } if is_gui_session(&stdout)
            )
        })
    }
}

fn run_script(program: &str, app: App, payload: &str, timeout: Duration) -> Finished {
    let script = app.script();
    run_command(
        program,
        &["-l", "JavaScript", "-e", script.as_str(), payload],
        timeout,
    )
}

impl Host for ScreenHost<'_> {
    fn ensure_running(&self, _app: App) {}

    fn run(&self, app: App, payload: &str, timeout: Duration) -> Finished {
        match (app, self.osascript) {
            (App::Dialog, Some(program)) => run_script(program, app, payload, timeout),
            _ => Finished::NotStarted,
        }
    }
}

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
        run_script(OSASCRIPT, app, payload, timeout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{TempDir, TestResult};
    use std::fs;
    #[cfg(debug_assertions)]
    use std::os::unix::fs::PermissionsExt;

    fn programs(pairs: &[(&str, &str)]) -> Programs {
        Programs::from_lookup(&|key| {
            pairs
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.to_string())
        })
    }

    #[cfg(debug_assertions)]
    fn stand_in(dir: &TempDir, name: &str, body: &str) -> Result<String, std::io::Error> {
        let path = dir.path(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n"))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
        Ok(path)
    }

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

    #[cfg(not(debug_assertions))]
    #[test]
    fn release_builds_ignore_the_screen_program_overrides() {
        let replaced = programs(&[
            ("AVERY_OSASCRIPT", "/stand/in/osascript"),
            ("AVERY_LAUNCHCTL", "/stand/in/launchctl"),
        ]);
        assert_eq!(replaced.osascript.as_deref(), Some(OSASCRIPT));
        assert_eq!(replaced.launchctl.as_deref(), Some(LAUNCHCTL));
        let refused = programs(&[("AVERY_OSASCRIPT", "osascript")]);
        assert_eq!(refused.osascript.as_deref(), Some(OSASCRIPT));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn the_screen_programs_are_the_system_ones_unless_an_absolute_path_replaces_them() {
        let system = programs(&[]);
        assert_eq!(system.osascript.as_deref(), Some(OSASCRIPT));
        assert_eq!(system.launchctl.as_deref(), Some(LAUNCHCTL));
        let unset = programs(&[("AVERY_OSASCRIPT", ""), ("AVERY_LAUNCHCTL", "")]);
        assert_eq!(unset.osascript.as_deref(), Some(OSASCRIPT));
        assert_eq!(unset.launchctl.as_deref(), Some(LAUNCHCTL));
        let replaced = programs(&[
            ("AVERY_OSASCRIPT", "/stand/in/osascript"),
            ("AVERY_LAUNCHCTL", "/stand/in/launchctl"),
        ]);
        assert_eq!(replaced.osascript.as_deref(), Some("/stand/in/osascript"));
        assert_eq!(replaced.launchctl.as_deref(), Some("/stand/in/launchctl"));
        let one = programs(&[("AVERY_OSASCRIPT", "/stand/in/osascript")]);
        assert_eq!(one.launchctl.as_deref(), Some(LAUNCHCTL));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn a_program_that_is_not_an_absolute_path_never_reaches_the_path_lookup() {
        for bad in [
            "osascript",
            "./osascript",
            "../osascript",
            " /x/osascript",
            "~/x",
            "-l",
        ] {
            let refused = programs(&[("AVERY_OSASCRIPT", bad), ("AVERY_LAUNCHCTL", bad)]);
            assert_eq!(refused.osascript, None, "{bad:?}");
            assert_eq!(refused.launchctl, None, "{bad:?}");
            assert!(!refused.graphical(), "{bad:?}");
            assert_eq!(
                refused
                    .host()
                    .run(App::Dialog, "{}", Duration::from_secs(1)),
                Finished::NotStarted,
                "{bad:?}"
            );
        }
    }

    #[cfg(debug_assertions)]
    #[test]
    fn the_screen_host_runs_the_dialog_script_through_its_own_program_and_nothing_else(
    ) -> TestResult {
        let dir = TempDir::new()?;
        let log = dir.path("calls.log");
        let osascript = stand_in(
            &dir,
            "osascript",
            &format!(
                "printf '%s|%s|%s|%s\\n' \"$1\" \"$2\" \"$3\" \"$5\" >> '{log}'\nprintf '{{\"ok\": true, \"data\": {{\"result\": \"ok\"}}}}'"
            ),
        )?;
        let programs = programs(&[("AVERY_OSASCRIPT", &osascript)]);
        let host = programs.host();
        let answered = host.run(App::Dialog, "{\"op\": \"ping\"}", Duration::from_secs(10));
        assert!(
            matches!(&answered, Finished::Done { code: Some(0), stdout, .. } if stdout.contains("\"ok\": true")),
            "{answered:?}"
        );
        for app in [App::Mail, App::Calendar] {
            assert_eq!(
                host.run(app, "{}", Duration::from_secs(10)),
                Finished::NotStarted,
                "{app:?}"
            );
        }
        host.ensure_running(App::Mail);
        assert_eq!(
            fs::read_to_string(&log)?,
            "-l|JavaScript|-e|{\"op\": \"ping\"}\n"
        );
        Ok(())
    }

    #[cfg(debug_assertions)]
    #[test]
    fn only_a_launchctl_that_says_aqua_makes_a_graphical_session() -> TestResult {
        let dir = TempDir::new()?;
        for (index, (body, expected)) in [
            ("echo Aqua", true),
            ("echo ' Aqua '", true),
            ("echo Background", false),
            ("echo Aqua; exit 1", false),
            ("exit 0", false),
        ]
        .into_iter()
        .enumerate()
        {
            let path = stand_in(&dir, &format!("launchctl-{index}"), body)?;
            let programs = programs(&[("AVERY_LAUNCHCTL", &path)]);
            assert_eq!(programs.graphical(), expected, "{body}");
        }
        let missing = programs(&[("AVERY_LAUNCHCTL", "/nonexistent/launchctl")]);
        assert!(!missing.graphical());
        Ok(())
    }

    fn alive(pid: &str) -> bool {
        matches!(
            shell(&format!("kill -0 {pid}"), 5000),
            Finished::Done { code: Some(0), .. }
        )
    }

    #[test]
    fn a_child_that_is_stopped_for_the_deadline_or_the_output_limit_is_gone_afterwards(
    ) -> TestResult {
        for (command, limit, expected) in [
            ("sleep 30", STDOUT_LIMIT, Finished::TimedOut),
            ("yes abcdefghij", 1024, Finished::TooLarge),
        ] {
            let dir = TempDir::new()?;
            let pid_file = dir.path("pid");
            let script = format!("echo $$ > '{pid_file}'; exec {command}");
            let finished = run_limited(
                "/bin/sh",
                &["-c", &script],
                Duration::from_millis(1000),
                limit,
                1024,
            );
            assert_eq!(finished, expected, "{command}");
            let pid = fs::read_to_string(&pid_file)?.trim().to_string();
            assert!(
                !pid.is_empty() && pid.chars().all(|c| c.is_ascii_digit()),
                "{pid:?}"
            );
            let mut lingering = true;
            for _ in 0..40 {
                lingering = alive(&pid);
                if !lingering {
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
            assert!(!lingering, "{command}: process {pid} is still running");
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn kinds(script: &str, names: &[&str]) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let expressions: Vec<String> = names.iter().map(|name| format!("typeof {name}")).collect();
        let reporter = format!(
            "function run(argv) {{ return JSON.stringify([{}]); }}",
            expressions.join(", ")
        );
        let output = Command::new(OSASCRIPT)
            .args([
                "-l",
                "JavaScript",
                "-e",
                &format!("{script}\n{reporter}"),
                "{}",
            ])
            .output()?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
        }
        let answer = crate::json::parse(String::from_utf8_lossy(&output.stdout).trim())?;
        Ok(answer
            .as_array()
            .ok_or("array")?
            .iter()
            .filter_map(|kind| kind.as_str().map(str::to_string))
            .collect())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn each_script_carries_only_the_libraries_it_uses() -> TestResult {
        let names = [
            "askApproval",
            "changeDecision",
            "dialogLines",
            "askButtons",
            "chooseList",
            "notify",
            "shown",
            "hidden",
            "flat",
            "showDialog",
        ];
        let approvals_only: Vec<&str> = [
            "function",
            "function",
            "function",
            "undefined",
            "undefined",
            "undefined",
            "function",
            "function",
            "function",
            "function",
        ]
        .to_vec();
        let setup_only: Vec<&str> = [
            "undefined",
            "undefined",
            "undefined",
            "function",
            "function",
            "function",
            "function",
            "function",
            "function",
            "function",
        ]
        .to_vec();
        assert_eq!(
            kinds(&dialog_script(), &names)?,
            setup_only,
            "setup dialogs"
        );
        assert_eq!(kinds(&mail_script(), &names)?, approvals_only, "mail");
        assert_eq!(
            kinds(&calendar_script(), &names)?,
            approvals_only,
            "calendar"
        );
        Ok(())
    }

    #[test]
    fn invalid_utf8_is_replaced_not_fatal() {
        assert!(matches!(
            shell("printf '\\377\\376ok'", 5000),
            Finished::Done { stdout, .. } if stdout.ends_with("ok")
        ));
    }
}
