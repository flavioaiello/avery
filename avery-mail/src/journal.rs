use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::{dirname, make_dirs};
use crate::error::{ToolError, ToolResult};
use crate::json::{self, Json};

pub(crate) const HOUR: i64 = 3600;
pub(crate) const TEN_MINUTES: i64 = 600;
pub(crate) const COOL_DOWN: i64 = 20;
const TAIL_BYTES: u64 = 1024 * 1024;

pub(crate) struct Messages {
    pub(crate) blocked: &'static str,
    pub(crate) unreadable: &'static str,
    pub(crate) too_large: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct View<T> {
    pub(crate) entries: Vec<T>,
    pub(crate) unreadable: usize,
}

impl<T> Default for View<T> {
    fn default() -> Self {
        View {
            entries: Vec::new(),
            unreadable: 0,
        }
    }
}

pub(crate) struct Journal {
    path: String,
    messages: &'static Messages,
}

pub(crate) fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .unwrap_or(0)
}

pub(crate) fn attempt_id() -> ToolResult<String> {
    let mut bytes = [0u8; 6];
    File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut bytes))
        .map_err(|_| ToolError::internal("random"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn tail(path: &str) -> io::Result<(String, bool)> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let start = length.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(TAIL_BYTES).read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if start == 0 {
        return Ok((text, false));
    }
    Ok((
        match text.split_once('\n') {
            Some((_, rest)) => rest.to_string(),
            None => String::new(),
        },
        true,
    ))
}

fn parse_lines<T>(text: &str, limit: usize, parse: fn(&Json) -> Option<T>) -> View<T> {
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let first = lines.len().saturating_sub(limit);
    let mut view = View::default();
    for line in lines.iter().skip(first) {
        match json::parse(line).ok().and_then(|value| parse(&value)) {
            Some(entry) => view.entries.push(entry),
            None => view.unreadable += 1,
        }
    }
    view
}

impl Journal {
    pub(crate) fn new(path: String, messages: &'static Messages) -> Journal {
        Journal { path, messages }
    }

    #[cfg(test)]
    pub(crate) fn path(&self) -> &str {
        &self.path
    }

    pub(crate) fn recent<T>(
        &self,
        limit: usize,
        parse: fn(&Json) -> Option<T>,
    ) -> ToolResult<View<T>> {
        match tail(&self.path) {
            Ok((text, _)) => Ok(parse_lines(&text, limit, parse)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(View::default()),
            Err(_) => Err(ToolError::msg(self.messages.blocked)),
        }
    }

    pub(crate) fn window<T>(
        &self,
        now: i64,
        parse: fn(&Json) -> Option<T>,
        timestamp: fn(&T) -> i64,
    ) -> ToolResult<View<T>> {
        let (text, cut) = match tail(&self.path) {
            Ok(found) => found,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(View::default()),
            Err(_) => return Err(ToolError::msg(self.messages.blocked)),
        };
        let view = parse_lines(&text, usize::MAX, parse);
        if view.unreadable > 0 {
            return Err(ToolError::msg(self.messages.unreadable));
        }
        if cut {
            let covers_the_hour = view
                .entries
                .first()
                .is_some_and(|oldest| now.saturating_sub(timestamp(oldest)) >= HOUR);
            if !covers_the_hour {
                return Err(ToolError::msg(self.messages.too_large));
            }
        }
        Ok(view)
    }

    pub(crate) fn ensure_writable(&self) -> ToolResult<()> {
        make_dirs(&dirname(&self.path), 0o700)
            .map_err(|_| ToolError::msg(self.messages.blocked))?;
        OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(&self.path)
            .map(drop)
            .map_err(|_| ToolError::msg(self.messages.blocked))
    }

    pub(crate) fn append(&self, value: &Json, strict: bool) -> ToolResult<()> {
        let mut line = json::dumps(value, false);
        line.push('\n');
        let written = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .mode(0o600)
            .open(&self.path)
            .and_then(|mut file| {
                let mut data = Vec::new();
                if file.metadata()?.len() > 0 {
                    let mut last = [0u8; 1];
                    file.seek(SeekFrom::End(-1))?;
                    file.read_exact(&mut last)?;
                    if last != *b"\n" {
                        data.push(b'\n');
                    }
                }
                data.extend_from_slice(line.as_bytes());
                file.write_all(&data)?;
                file.sync_all()
            });
        match written {
            Ok(()) => Ok(()),
            Err(_) if strict => Err(ToolError::msg(self.messages.blocked)),
            Err(_) => {
                eprintln!("log not writable: {}", self.path);
                Ok(())
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

    static MESSAGES: Messages = Messages {
        blocked: "blocked",
        unreadable: "unreadable",
        too_large: "too large",
    };

    fn stamp(value: &Json) -> Option<i64> {
        let object = value.as_object()?;
        if object.get("v") != Some(&Json::Int(1)) {
            return None;
        }
        match object.get("ts") {
            Some(Json::Int(ts)) => Some(*ts),
            _ => None,
        }
    }

    fn at(entry: &i64) -> i64 {
        *entry
    }

    fn line(ts: i64) -> Json {
        Json::object(vec![
            ("v", Json::Int(1)),
            ("ts", Json::Int(ts)),
            ("pad", Json::from("x".repeat(500).as_str())),
        ])
    }

    fn padded(ts: i64) -> String {
        let mut text = json::dumps(&line(ts), false);
        text.push('\n');
        text
    }

    fn journal_in(dir: &TempDir, name: &str) -> Journal {
        Journal::new(dir.path(name), &MESSAGES)
    }

    #[test]
    fn append_writes_private_lines_and_recent_returns_them() -> TestResult {
        let dir = TempDir::new()?;
        make_dirs(&dir.path("mail"), 0o700)?;
        let journal = journal_in(&dir, "mail/log.jsonl");
        journal.append(&line(10), true)?;
        journal.append(&line(11), true)?;
        let mode = fs::metadata(journal.path())?.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let text = fs::read_to_string(journal.path())?;
        assert_eq!(text.lines().count(), 2);
        assert!(text.ends_with('\n'));
        let view = journal.recent(300, stamp)?;
        assert_eq!(view.entries, [10, 11]);
        assert_eq!(view.unreadable, 0);
        assert_eq!(journal.recent(1, stamp)?.entries, [11]);
        Ok(())
    }

    #[test]
    fn ensure_writable_creates_the_private_folder_and_blocks_on_failure() -> TestResult {
        let dir = TempDir::new()?;
        let fresh = journal_in(&dir, "fresh/mail/log.jsonl");
        fresh.ensure_writable()?;
        let mode = fs::metadata(dir.path("fresh/mail"))?.permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        let blocked = journal_in(&dir, "blocked/log.jsonl");
        fs::create_dir_all(blocked.path())?;
        assert_eq!(blocked.ensure_writable(), Err(ToolError::msg("blocked")));
        assert_eq!(
            blocked.append(&line(1), true),
            Err(ToolError::msg("blocked"))
        );
        assert_eq!(blocked.append(&line(1), false), Ok(()));
        Ok(())
    }

    #[test]
    fn reading_is_bounded_and_counts_unreadable_lines() -> TestResult {
        let dir = TempDir::new()?;
        let journal = journal_in(&dir, "log.jsonl");
        let mut text = String::from("not json\n{\"v\": 9}\n");
        for ts in 0..6000 {
            text.push_str(&padded(ts));
        }
        fs::write(journal.path(), &text)?;
        assert!(u64::try_from(text.len())? > TAIL_BYTES * 2);
        let view = journal.recent(300, stamp)?;
        assert_eq!(view.entries.len(), 300);
        assert_eq!(view.unreadable, 0);
        assert_eq!(view.entries.last(), Some(&5999));
        let all = journal.recent(100_000, stamp)?;
        assert!(all.entries.len() < 6000);
        assert!(all.entries.first().is_some_and(|ts| *ts > 0));
        let small = TempDir::new()?;
        let journal = journal_in(&small, "log.jsonl");
        fs::write(journal.path(), "garbage\n\n{\"v\": 9}\n")?;
        let view = journal.recent(300, stamp)?;
        assert_eq!((view.entries.len(), view.unreadable), (0, 2));
        assert_eq!(
            journal_in(&small, "nothing/log.jsonl").recent(5, stamp),
            Ok(View::default())
        );
        Ok(())
    }

    #[test]
    fn the_window_must_reach_back_an_hour_or_the_caller_is_blocked() -> TestResult {
        let now = 1_700_000_000;
        let recent_only = TempDir::new()?;
        let journal = journal_in(&recent_only, "log.jsonl");
        let mut text = String::new();
        for index in 0..4000 {
            text.push_str(&padded(now - 3000 + index % 100));
        }
        fs::write(journal.path(), &text)?;
        assert_eq!(
            journal.window(now, stamp, at),
            Err(ToolError::msg("too large"))
        );

        let reaching_back = TempDir::new()?;
        let journal = journal_in(&reaching_back, "log.jsonl");
        let mut text = String::new();
        for index in 0..3000 {
            text.push_str(&padded(now - 9000 - index));
        }
        for index in 0..100 {
            text.push_str(&padded(now - 100 + index));
        }
        fs::write(journal.path(), &text)?;
        let view = journal.window(now, stamp, at)?;
        assert!(view.entries.len() > 100);
        assert!(view.entries.last().is_some_and(|last| *last >= now - 100));

        let small = TempDir::new()?;
        let journal = journal_in(&small, "log.jsonl");
        fs::write(journal.path(), padded(now - 5))?;
        assert_eq!(journal.window(now, stamp, at)?.entries.len(), 1);
        assert_eq!(
            journal_in(&small, "none/log.jsonl").window(now, stamp, at),
            Ok(View::default())
        );
        Ok(())
    }

    #[test]
    fn a_journal_that_cannot_be_trusted_blocks_instead_of_counting_as_empty() -> TestResult {
        let now = 1_700_000_000;
        let unreadable = TempDir::new()?;
        let journal = journal_in(&unreadable, "log.jsonl");
        fs::write(journal.path(), format!("{}garbage\n", padded(now - 5)))?;
        assert_eq!(
            journal.window(now, stamp, at),
            Err(ToolError::msg("unreadable"))
        );
        fs::write(journal.path(), "{\"v\": 1, \"ts\": \"5\"}\n")?;
        assert_eq!(
            journal.window(now, stamp, at),
            Err(ToolError::msg("unreadable"))
        );

        let directory = TempDir::new()?;
        let journal = journal_in(&directory, "log.jsonl");
        fs::create_dir_all(journal.path())?;
        assert_eq!(
            journal.window(now, stamp, at),
            Err(ToolError::msg("blocked"))
        );
        assert_eq!(journal.recent(10, stamp), Err(ToolError::msg("blocked")));

        let protected = TempDir::new()?;
        let journal = journal_in(&protected, "log.jsonl");
        fs::write(journal.path(), padded(now - 5))?;
        fs::set_permissions(journal.path(), fs::Permissions::from_mode(0o200))?;
        assert_eq!(
            journal.window(now, stamp, at),
            Err(ToolError::msg("blocked"))
        );
        assert_eq!(
            journal.append(&line(now), true),
            Err(ToolError::msg("blocked")),
            "the append needs to read the last byte, so a journal that cannot be read blocks too"
        );
        Ok(())
    }

    #[test]
    fn a_torn_last_line_does_not_swallow_the_next_entry() -> TestResult {
        let dir = TempDir::new()?;
        let journal = journal_in(&dir, "log.jsonl");
        fs::write(journal.path(), "{\"v\": 1, \"ts\": 17000")?;
        journal.append(&line(18_000), true)?;
        let view = journal.recent(10, stamp)?;
        assert_eq!(view.unreadable, 1);
        assert_eq!(view.entries, [18_000]);
        journal.append(&line(18_001), true)?;
        assert_eq!(journal.recent(10, stamp)?.entries.len(), 2);
        Ok(())
    }

    #[test]
    fn attempt_ids_are_twelve_hex_characters_and_differ() -> TestResult {
        let first = attempt_id()?;
        let second = attempt_id()?;
        assert_eq!(first.len(), 12);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(first, second);
        Ok(())
    }

    #[test]
    fn the_clock_is_plausible() {
        assert!(now_seconds() > 1_700_000_000);
    }
}
