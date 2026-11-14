mod account;
mod backend;
mod calendar;
mod changelog;
mod civil;
mod cli;
mod config;
mod console;
mod context;
mod error;
mod fake;
mod journal;
mod json;
mod jxa;
mod launcher;
mod localtime;
mod lock;
mod mailerror;
mod move_tools;
mod osascript;
mod read_tools;
mod real;
mod recurrence;
mod rights;
mod sendlog;
mod server;
mod sha256;
#[cfg(test)]
mod testutil;
mod text;
mod texts;
mod write_tools;

use std::io::{self, Write};

pub use texts::VERSION;

pub fn run(args: Vec<String>) -> u8 {
    std::panic::set_hook(Box::new(|_| {
        let _ = writeln!(io::stderr(), "avery: internal error");
    }));
    let lookup =
        |key: &str| std::env::var_os(key).map(|value| value.to_string_lossy().into_owned());
    let programs = osascript::Programs::from_env();
    let host = programs.host();
    let graphical = || programs.graphical();
    let screen = console::Screen {
        host: &host,
        gui: &graphical,
    };
    let system = launcher::RealSystem::from_lookup(&lookup);
    let call = launcher::Call {
        args: &args,
        lookup: &lookup,
        system: &system,
        screen: &screen,
        exe: launcher::own_path(),
    };
    let stdin = io::stdin();
    let stdout = io::stdout();
    let stderr = io::stderr();
    launcher::dispatch(
        &call,
        &mut stdin.lock(),
        &mut stdout.lock(),
        &mut stderr.lock(),
    )
}
