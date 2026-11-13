mod account;
mod backend;
mod cli;
mod config;
mod context;
mod error;
mod fake;
mod json;
mod localtime;
mod mailerror;
mod osascript;
mod read_tools;
mod real;
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
        let _ = writeln!(io::stderr(), "avery-mail: internal error");
    }));
    let ctx = context::Context::from_env();
    let stdin = io::stdin();
    let stdout = io::stdout();
    cli::dispatch(&args, &ctx, &mut stdin.lock(), &mut stdout.lock())
}
