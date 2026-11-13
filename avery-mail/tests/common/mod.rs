#[cfg(not(debug_assertions))]
compile_error!(
    "the binary-level tests need the debug profile: release builds ignore the screen stand-ins and would reach the real dialogs"
);

use std::error::Error;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub(crate) type TestResult = Result<(), Box<dyn Error>>;

const WAIT: Duration = Duration::from_secs(20);
static COUNTER: AtomicUsize = AtomicUsize::new(0);

pub(crate) struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    pub(crate) fn new() -> io::Result<Sandbox> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.subsec_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!(
            "avery-it-{}-{}-{nanos}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&root)?;
        let sandbox = Sandbox { root };
        sandbox.stand_ins()?;
        Ok(sandbox)
    }

    fn stand_ins(&self) -> io::Result<()> {
        let log = self.calls_path();
        let quoted = log.to_string_lossy().into_owned();
        if quoted.contains('\'') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the sandbox path has a quote",
            ));
        }
        let scripts = [
            (
                "osascript",
                format!("#!/bin/sh\nprintf 'osascript %s\\n' \"$5\" >> '{quoted}'\nexit 3\n"),
            ),
            (
                "launchctl",
                format!("#!/bin/sh\nprintf 'launchctl %s\\n' \"$*\" >> '{quoted}'\nexit 1\n"),
            ),
            (
                "launchctl-aqua",
                format!("#!/bin/sh\nprintf 'launchctl %s\\n' \"$*\" >> '{quoted}'\necho Aqua\n"),
            ),
        ];
        fs::create_dir_all(self.screen_dir())?;
        for (name, text) in scripts {
            let path = self.screen_dir().join(name);
            fs::write(&path, text)?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
        }
        Ok(())
    }

    pub(crate) fn screen_dir(&self) -> PathBuf {
        self.root.join("screen")
    }

    fn calls_path(&self) -> PathBuf {
        self.screen_dir().join("calls.log")
    }

    pub(crate) fn screen_program(&self, name: &str) -> PathBuf {
        self.screen_dir().join(name)
    }

    fn forget_calls(&self) -> io::Result<()> {
        fs::write(self.calls_path(), "")
    }

    fn calls(&self) -> String {
        fs::read_to_string(self.calls_path()).unwrap_or_default()
    }

    pub(crate) fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    pub(crate) fn write(&self, name: &str, text: &str) -> io::Result<()> {
        fs::write(self.path(name), text)
    }

    pub(crate) fn read(&self, name: &str) -> String {
        fs::read_to_string(self.path(name)).unwrap_or_default()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn command(sandbox: &Sandbox, env: &[(&str, &str)]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_avery-mail"));
    command
        .env_clear()
        .env("HOME", &sandbox.root)
        .env("MAIL_MCP_CONFIG", sandbox.path("config.json"))
        .env("AVERY_OSASCRIPT", sandbox.screen_program("osascript"))
        .env("AVERY_LAUNCHCTL", sandbox.screen_program("launchctl"))
        .envs(env.iter().copied())
        .stderr(Stdio::null());
    command
}

pub(crate) struct Output {
    pub(crate) code: Option<i32>,
    pub(crate) stdout: String,
    pub(crate) calls: String,
}

pub(crate) fn run_cli_allowing_the_screen(
    sandbox: &Sandbox,
    env: &[(&str, &str)],
    args: &[&str],
    input: &str,
) -> Result<Output, Box<dyn Error>> {
    sandbox.forget_calls()?;
    let mut child = command(sandbox, env)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input.as_bytes())?;
    }
    let output = child.wait_with_output()?;
    Ok(Output {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        calls: sandbox.calls(),
    })
}

pub(crate) fn run_cli(
    sandbox: &Sandbox,
    env: &[(&str, &str)],
    args: &[&str],
    input: &str,
) -> Result<Output, Box<dyn Error>> {
    let output = run_cli_allowing_the_screen(sandbox, env, args, input)?;
    if output.calls.is_empty() {
        Ok(output)
    } else {
        Err(format!(
            "{args:?} reached the screen: {}(output: {})",
            output.calls, output.stdout
        )
        .into())
    }
}

pub(crate) struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    next_id: u64,
    calls: PathBuf,
}

impl Server {
    pub(crate) fn start(sandbox: &Sandbox, env: &[(&str, &str)]) -> Result<Server, Box<dyn Error>> {
        let mut child = command(sandbox, env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        sandbox.forget_calls()?;
        Ok(Server {
            child,
            stdin,
            lines,
            next_id: 0,
            calls: sandbox.calls_path(),
        })
    }

    pub(crate) fn send_raw(&mut self, bytes: &[u8]) -> io::Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| io::Error::other("stdin closed"))?;
        stdin.write_all(bytes)?;
        stdin.flush()
    }

    pub(crate) fn next_line(&mut self) -> Result<String, Box<dyn Error>> {
        Ok(self.lines.recv_timeout(WAIT)?)
    }

    pub(crate) fn rpc(&mut self, method: &str, params: &str) -> Result<String, Box<dyn Error>> {
        self.next_id += 1;
        let request = format!(
            "{{\"jsonrpc\": \"2.0\", \"id\": {}, \"method\": \"{method}\", \"params\": {params}}}\n",
            self.next_id
        );
        self.send_raw(request.as_bytes())?;
        self.next_line()
    }

    pub(crate) fn tool_names(&mut self) -> Result<Vec<String>, Box<dyn Error>> {
        let answer = self.rpc("tools/list", "{}")?;
        Ok(values_after(&answer, "\"name\": \""))
    }

    pub(crate) fn call(
        &mut self,
        name: &str,
        arguments: &str,
    ) -> Result<(bool, String), Box<dyn Error>> {
        let answer = self.rpc(
            "tools/call",
            &format!("{{\"name\": \"{name}\", \"arguments\": {arguments}}}"),
        )?;
        let text = values_after(&answer, "\"text\": \"")
            .into_iter()
            .next()
            .ok_or("no text in the answer")?;
        Ok((answer.contains("\"isError\": true"), text))
    }

    pub(crate) fn finish(mut self) -> Result<Option<i32>, Box<dyn Error>> {
        self.stdin.take();
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = self.child.try_wait()? {
                let calls = fs::read_to_string(&self.calls).unwrap_or_default();
                if !calls.is_empty() {
                    return Err(format!("the server reached the screen: {calls}").into());
                }
                return Ok(status.code());
            }
            if Instant::now() >= deadline {
                return Err("the server did not stop".into());
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn values_after(text: &str, marker: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(position) = rest.find(marker) {
        let start = position + marker.len();
        let tail = &rest[start..];
        let mut value = String::new();
        let mut units: Vec<u16> = Vec::new();
        let mut chars = tail.chars();
        let mut consumed = 0usize;
        while let Some(c) = chars.next() {
            consumed += c.len_utf8();
            if c == '"' {
                break;
            }
            if c != '\\' {
                flush(&mut units, &mut value);
                value.push(c);
                continue;
            }
            let Some(escape) = chars.next() else { break };
            consumed += escape.len_utf8();
            match escape {
                'n' => push_plain(&mut units, &mut value, '\n'),
                'r' => push_plain(&mut units, &mut value, '\r'),
                't' => push_plain(&mut units, &mut value, '\t'),
                'u' => {
                    let digits: String = chars.by_ref().take(4).collect();
                    consumed += digits.len();
                    if let Ok(unit) = u16::from_str_radix(&digits, 16) {
                        units.push(unit);
                    }
                }
                other => push_plain(&mut units, &mut value, other),
            }
        }
        flush(&mut units, &mut value);
        found.push(value);
        rest = &tail[consumed.min(tail.len())..];
    }
    found
}

fn flush(units: &mut Vec<u16>, value: &mut String) {
    value.extend(char::decode_utf16(units.drain(..)).map(|c| c.unwrap_or('\u{fffd}')));
}

fn push_plain(units: &mut Vec<u16>, value: &mut String, c: char) {
    flush(units, value);
    value.push(c);
}
