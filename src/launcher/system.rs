use std::fs;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::osascript::{run_command, Finished};

const QUERY_TIMEOUT: Duration = Duration::from_secs(10);
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const NOT_FOUND: u8 = 127;
const NOT_RUNNABLE: u8 = 126;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Plan {
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
    pub(crate) env: Vec<(String, String)>,
}

impl Plan {
    pub(crate) fn new(program: impl Into<String>, args: Vec<String>) -> Plan {
        Plan {
            program: program.into(),
            args,
            env: Vec::new(),
        }
    }

    pub(crate) fn with_env(mut self, key: &str, value: String) -> Plan {
        self.env.push((key.to_string(), value));
        self
    }

    pub(crate) fn line(&self) -> String {
        let mut words = vec![self.program.as_str()];
        words.extend(self.args.iter().map(String::as_str));
        words.join(" ")
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Ran {
    pub(crate) code: Option<i32>,
    pub(crate) stdout: String,
}

pub(crate) trait System {
    fn find(&self, program: &str) -> Option<PathBuf>;
    fn run(&self, program: &str, args: &[&str]) -> Option<Ran>;
    fn healthy(&self, url: &str) -> bool;
    fn running(&self, needle: &str) -> bool;
    fn start(&self, plan: &Plan) -> u8;
}

pub(crate) struct RealSystem {
    search: String,
}

impl RealSystem {
    pub(crate) fn from_lookup(lookup: &dyn Fn(&str) -> Option<String>) -> RealSystem {
        RealSystem {
            search: lookup("PATH").unwrap_or_default(),
        }
    }
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

impl System for RealSystem {
    fn find(&self, program: &str) -> Option<PathBuf> {
        if program.contains('/') {
            return is_executable(Path::new(program)).then(|| PathBuf::from(program));
        }
        std::env::split_paths(&self.search)
            .map(|dir| dir.join(program))
            .find(|candidate| is_executable(candidate))
    }

    fn run(&self, program: &str, args: &[&str]) -> Option<Ran> {
        match run_command(program, args, QUERY_TIMEOUT) {
            Finished::Done { code, stdout, .. } => Some(Ran { code, stdout }),
            _ => None,
        }
    }

    fn healthy(&self, url: &str) -> bool {
        endpoint(url).is_some_and(|endpoint| probe(&endpoint))
    }

    fn running(&self, needle: &str) -> bool {
        self.run("ps", &["-axww", "-o", "command="])
            .is_some_and(|ran| {
                ran.code == Some(0) && ran.stdout.lines().any(|line| line.contains(needle))
            })
    }

    fn start(&self, plan: &Plan) -> u8 {
        let mut command = Command::new(&plan.program);
        command.args(&plan.args);
        for (key, value) in &plan.env {
            command.env(key, value);
        }
        let error = command.exec();
        let _ = writeln!(
            std::io::stderr(),
            "avery: {} nicht gestartet: {error}",
            plan.program
        );
        if error.kind() == std::io::ErrorKind::NotFound {
            NOT_FOUND
        } else {
            NOT_RUNNABLE
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Endpoint {
    address: SocketAddr,
    authority: String,
    path: String,
}

fn plain(text: &str) -> bool {
    text.bytes().all(|byte| byte > b' ' && byte < 0x7f)
}

fn split_authority(authority: &str) -> Option<(&str, u16)> {
    if let Some(bracketed) = authority.strip_prefix('[') {
        let (host, tail) = bracketed.split_once(']')?;
        return match tail.strip_prefix(':') {
            Some(port) => Some((host, port.parse().ok()?)),
            None if tail.is_empty() => Some((host, 80)),
            None => None,
        };
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => Some((host, port.parse().ok()?)),
        None => Some((authority, 80)),
    }
}

fn endpoint(url: &str) -> Option<Endpoint> {
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/"),
    };
    if !plain(authority) || !plain(path) {
        return None;
    }
    let (host, port) = split_authority(authority)?;
    let ip = if host == "localhost" {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    } else {
        host.parse().ok()?
    };
    Some(Endpoint {
        address: SocketAddr::new(ip, port),
        authority: authority.to_string(),
        path: path.to_string(),
    })
}

fn probe(endpoint: &Endpoint) -> bool {
    let Ok(mut stream) = TcpStream::connect_timeout(&endpoint.address, PROBE_TIMEOUT) else {
        return false;
    };
    if stream.set_read_timeout(Some(PROBE_TIMEOUT)).is_err()
        || stream.set_write_timeout(Some(PROBE_TIMEOUT)).is_err()
    {
        return false;
    }
    let request = format!(
        "GET {} HTTP/1.0\r\nHost: {}\r\nConnection: close\r\n\r\n",
        endpoint.path, endpoint.authority
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut head = [0u8; 5];
    stream.read_exact(&mut head).is_ok() && &head == b"HTTP/"
}

pub(crate) fn stable_path(exe: &Path) -> PathBuf {
    let real = fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    let parts: Vec<_> = real.components().collect();
    let Some(cellar) = parts.iter().position(|part| part.as_os_str() == "Cellar") else {
        return real;
    };
    let Some(name) = real.file_name() else {
        return real;
    };
    let mut link: PathBuf = parts[..cellar].iter().collect();
    link.push("bin");
    link.push(name);
    if fs::canonicalize(&link).is_ok_and(|resolved| resolved == real) {
        link
    } else {
        real
    }
}

pub(crate) fn own_path() -> PathBuf {
    std::env::current_exe()
        .map(|exe| stable_path(&exe))
        .unwrap_or_else(|_| PathBuf::from("avery"))
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::os::unix::fs::symlink;
    use std::thread;

    use super::*;
    use crate::testutil::TempDir;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn root(dir: &TempDir) -> Result<PathBuf, std::io::Error> {
        fs::canonicalize(dir.path(""))
    }

    #[test]
    fn plans_print_as_one_line() {
        let plan = Plan::new(
            "open",
            vec![
                "-n".to_string(),
                "-a".to_string(),
                "/Opt/Mein Goose.app".to_string(),
            ],
        )
        .with_env("X", "1".to_string());
        assert_eq!(plan.line(), "open -n -a /Opt/Mein Goose.app");
        assert_eq!(plan.env, vec![("X".to_string(), "1".to_string())]);
    }

    #[test]
    fn only_plain_http_endpoints_on_literal_addresses_are_probed() {
        let found = endpoint("http://127.0.0.1:8000/health");
        assert_eq!(
            found,
            Some(Endpoint {
                address: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8000),
                authority: "127.0.0.1:8000".to_string(),
                path: "/health".to_string(),
            })
        );
        assert_eq!(
            endpoint("http://localhost/").map(|e| e.address.port()),
            Some(80)
        );
        assert_eq!(
            endpoint("http://[::1]:9/x").map(|e| e.address.port()),
            Some(9)
        );
        for refused in [
            "https://127.0.0.1:8000/health",
            "http://example.org/health",
            "http://127.0.0.1:notaport/",
            "http://127.0.0.1:8000/a b",
            "http://127.0.0.1:8000/a\r\nHost: x",
            "http://[::1/",
            "127.0.0.1:8000",
            "",
        ] {
            assert_eq!(endpoint(refused), None, "{refused}");
        }
    }

    fn serve_once(answer: &'static [u8]) -> Result<u16, Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut line = String::new();
                let _ = BufReader::new(&stream).read_line(&mut line);
                let _ = stream.write_all(answer);
            }
        });
        Ok(port)
    }

    #[test]
    fn any_http_answer_counts_as_healthy_and_silence_does_not() -> TestResult {
        let system = RealSystem {
            search: String::new(),
        };
        let port = serve_once(b"HTTP/1.1 404 Not Found\r\n\r\n")?;
        assert!(system.healthy(&format!("http://127.0.0.1:{port}/health")));
        let port = serve_once(b"garbage")?;
        assert!(!system.healthy(&format!("http://127.0.0.1:{port}/health")));
        let closed = TcpListener::bind("127.0.0.1:0")?;
        let port = closed.local_addr()?.port();
        drop(closed);
        assert!(!system.healthy(&format!("http://127.0.0.1:{port}/health")));
        Ok(())
    }

    #[test]
    fn programs_are_found_on_the_search_path_when_they_are_executable() -> TestResult {
        let dir = TempDir::new()?;
        let bin = root(&dir)?.join("bin");
        fs::create_dir_all(&bin)?;
        let tool = bin.join("tool");
        fs::write(&tool, "#!/bin/sh\n")?;
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755))?;
        fs::write(bin.join("plain"), "x")?;
        let system = RealSystem {
            search: format!("/nonexistent:{}", bin.display()),
        };
        assert_eq!(system.find("tool"), Some(tool.clone()));
        assert_eq!(system.find("plain"), None);
        assert_eq!(system.find("missing"), None);
        assert_eq!(system.find(&tool.to_string_lossy()), Some(tool));
        Ok(())
    }

    #[test]
    fn a_homebrew_keg_path_becomes_the_linked_path() -> TestResult {
        let dir = TempDir::new()?;
        let base = root(&dir)?;
        let keg = base.join("Cellar/avery/9.9.9/bin");
        fs::create_dir_all(&keg)?;
        fs::create_dir_all(base.join("bin"))?;
        let real = keg.join("avery");
        fs::write(&real, "x")?;
        let link = base.join("bin/avery");
        symlink(&real, &link)?;
        assert_eq!(stable_path(&real), link);
        assert_eq!(stable_path(&link), link);
        fs::remove_file(&link)?;
        assert_eq!(stable_path(&real), real);
        Ok(())
    }

    #[test]
    fn a_path_outside_a_keg_stays_as_it_is() -> TestResult {
        let dir = TempDir::new()?;
        let target = root(&dir)?.join("target/debug");
        fs::create_dir_all(&target)?;
        let exe = target.join("avery");
        fs::write(&exe, "x")?;
        assert_eq!(stable_path(&exe), exe);
        Ok(())
    }
}
