//! avery-mail: MCP server (stdio) for Apple Mail: reading, drafts and sending with approval.
//!
//! A Rust port of `libexec/avery-mail`. Standard library only.

#![forbid(unsafe_code)]
#![allow(dead_code)]

mod account;
mod config;
mod error;
mod fake;
mod json;
mod pycompat;
mod sha256;
#[cfg(test)]
mod testutil;
mod text;
mod texts;

fn main() {
    println!("{}", texts::VERSION);
}
