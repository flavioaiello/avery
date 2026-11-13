use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::backend::{ComposeRequest, Mode, Preview};
use crate::config::{dirname, make_dirs, Settings};
use crate::error::{ToolError, ToolResult};
use crate::json::{self, Json, Object};
use crate::sha256::sha256_hex;
use crate::text::{char_len, one_line_str};
use crate::texts::{
    LOG_BLOCKED, LOG_TOO_LARGE, LOG_UNREADABLE, MAX_ATTEMPTS_PER_10_MIN, MAX_DRAFTS_PER_HOUR,
    MAX_SUBJECT,
};

pub(crate) const LOG_VERSION: i64 = 1;
const TAIL_BYTES: u64 = 1024 * 1024;
pub(crate) const MAX_LOGGED_RECIPIENTS: usize = 10;
const HOUR: i64 = 3600;
const TEN_MINUTES: i64 = 600;
const COOL_DOWN: i64 = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Pending,
    Sent,
    Drafted,
    Declined,
    Timeout,
    Error,
}

impl Outcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Outcome::Pending => "pending",
            Outcome::Sent => "sent",
            Outcome::Drafted => "drafted",
            Outcome::Declined => "declined",
            Outcome::Timeout => "timeout",
            Outcome::Error => "error",
        }
    }

    fn parse(text: &str) -> Option<Outcome> {
        match text {
            "pending" => Some(Outcome::Pending),
            "sent" => Some(Outcome::Sent),
            "drafted" => Some(Outcome::Drafted),
            "declined" => Some(Outcome::Declined),
            "timeout" => Some(Outcome::Timeout),
            "error" => Some(Outcome::Error),
            _ => None,
        }
    }

    pub(crate) fn from_status(status: &str) -> Outcome {
        match status {
            "sent" => Outcome::Sent,
            "declined" => Outcome::Declined,
            "timeout" => Outcome::Timeout,
            _ => Outcome::Error,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LogEntry {
    pub(crate) ts: i64,
    pub(crate) mode: Mode,
    pub(crate) outcome: Outcome,
    pub(crate) account: String,
    pub(crate) from: String,
    pub(crate) to: Vec<String>,
    pub(crate) cc: Vec<String>,
    pub(crate) subject: String,
    pub(crate) reply_to_id: Option<i64>,
    pub(crate) recipients: usize,
    pub(crate) chars: usize,
    pub(crate) sha256: String,
    pub(crate) id: Option<String>,
}

fn logged(addresses: &[String]) -> Vec<String> {
    addresses
        .iter()
        .take(MAX_LOGGED_RECIPIENTS)
        .map(|address| one_line_str(address, 254))
        .collect()
}

impl LogEntry {
    pub(crate) fn for_attempt(
        ts: i64,
        account_label: &str,
        request: &ComposeRequest,
        outcome: Outcome,
        preview: Option<&Preview>,
        id: Option<&str>,
    ) -> LogEntry {
        let (from, to, cc, subject) = match preview {
            Some(preview) => (
                preview.from.as_str(),
                if preview.to.is_empty() {
                    &request.to
                } else {
                    &preview.to
                },
                if preview.cc.is_empty() {
                    &request.cc
                } else {
                    &preview.cc
                },
                if preview.subject.is_empty() {
                    request.subject.as_str()
                } else {
                    preview.subject.as_str()
                },
            ),
            None => ("", &request.to, &request.cc, request.subject.as_str()),
        };
        let digest = sha256_hex(request.body.as_bytes());
        LogEntry {
            ts,
            mode: request.mode,
            outcome,
            account: account_label.to_string(),
            from: one_line_str(from, 120),
            to: logged(to),
            cc: logged(cc),
            subject: one_line_str(subject, MAX_SUBJECT),
            reply_to_id: request.reply.as_ref().map(|reply| reply.id),
            recipients: to.len() + cc.len(),
            chars: char_len(&request.body),
            sha256: digest.chars().take(16).collect(),
            id: id.map(str::to_string),
        }
    }

    pub(crate) fn to_json(&self) -> Json {
        let mut object = Object::new();
        object.insert("v", Json::Int(LOG_VERSION));
        object.insert("ts", Json::Int(self.ts));
        object.insert("mode", Json::from(self.mode.as_str()));
        object.insert("outcome", Json::from(self.outcome.as_str()));
        object.insert("account", Json::from(&self.account));
        object.insert("from", Json::from(&self.from));
        object.insert("to", Json::from(self.to.as_slice()));
        object.insert("cc", Json::from(self.cc.as_slice()));
        object.insert("subject", Json::from(&self.subject));
        object.insert(
            "reply_to_id",
            self.reply_to_id.map_or(Json::Null, Json::Int),
        );
        object.insert(
            "recipients",
            Json::Int(i64::try_from(self.recipients).unwrap_or(i64::MAX)),
        );
        object.insert(
            "chars",
            Json::Int(i64::try_from(self.chars).unwrap_or(i64::MAX)),
        );
        object.insert("sha256", Json::from(&self.sha256));
        if let Some(id) = &self.id {
            object.insert("id", Json::from(id));
        }
        Json::Obj(object)
    }

    pub(crate) fn from_json(value: &Json) -> Option<LogEntry> {
        let object = value.as_object()?;
        if object.get("v") != Some(&Json::Int(LOG_VERSION)) {
            return None;
        }
        let text = |key: &str| object.get(key).and_then(Json::as_str).map(str::to_string);
        let strings = |key: &str| -> Option<Vec<String>> {
            object
                .get(key)?
                .as_array()?
                .iter()
                .map(|item| item.as_str().map(str::to_string))
                .collect()
        };
        let whole = |key: &str| match object.get(key) {
            Some(Json::Int(number)) => Some(*number),
            _ => None,
        };
        Some(LogEntry {
            ts: whole("ts")?,
            mode: Mode::parse(object.get("mode")?.as_str()?)?,
            outcome: Outcome::parse(object.get("outcome")?.as_str()?)?,
            account: text("account")?,
            from: text("from")?,
            to: strings("to")?,
            cc: strings("cc")?,
            subject: text("subject")?,
            reply_to_id: match object.get("reply_to_id") {
                Some(Json::Null) => None,
                Some(Json::Int(number)) => Some(*number),
                _ => return None,
            },
            recipients: usize::try_from(whole("recipients")?).ok()?,
            chars: usize::try_from(whole("chars")?).ok()?,
            sha256: text("sha256")?,
            id: match object.get("id") {
                None => None,
                Some(Json::Str(id)) => Some(id.clone()),
                Some(_) => return None,
            },
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct LogView {
    pub(crate) entries: Vec<LogEntry>,
    pub(crate) unreadable: usize,
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

fn parse_lines(text: &str, limit: usize) -> LogView {
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let first = lines.len().saturating_sub(limit);
    let mut view = LogView::default();
    for line in lines.iter().skip(first) {
        match json::parse(line)
            .ok()
            .and_then(|value| LogEntry::from_json(&value))
        {
            Some(entry) => view.entries.push(entry),
            None => view.unreadable += 1,
        }
    }
    view
}

pub(crate) fn read_entries(settings: &Settings, limit: usize) -> ToolResult<LogView> {
    match tail(&settings.sendlog_path()) {
        Ok((text, _)) => Ok(parse_lines(&text, limit)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(LogView::default()),
        Err(_) => Err(ToolError::msg(LOG_BLOCKED)),
    }
}

pub(crate) fn read_window(settings: &Settings, now: i64) -> ToolResult<LogView> {
    let (text, cut) = match tail(&settings.sendlog_path()) {
        Ok(found) => found,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(LogView::default()),
        Err(_) => return Err(ToolError::msg(LOG_BLOCKED)),
    };
    let view = parse_lines(&text, usize::MAX);
    if view.unreadable > 0 {
        return Err(ToolError::msg(LOG_UNREADABLE));
    }
    if cut {
        let covers_the_hour = view
            .entries
            .first()
            .is_some_and(|oldest| now.saturating_sub(oldest.ts) >= HOUR);
        if !covers_the_hour {
            return Err(ToolError::msg(LOG_TOO_LARGE));
        }
    }
    Ok(view)
}

pub(crate) fn ensure_writable(settings: &Settings) -> ToolResult<()> {
    let path = settings.sendlog_path();
    make_dirs(&dirname(&path), 0o700).map_err(|_| ToolError::msg(LOG_BLOCKED))?;
    OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(&path)
        .map(drop)
        .map_err(|_| ToolError::msg(LOG_BLOCKED))
}

pub(crate) fn append(settings: &Settings, entry: &LogEntry, strict: bool) -> ToolResult<()> {
    let mut line = json::dumps(&entry.to_json(), false);
    line.push('\n');
    let written = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .mode(0o600)
        .open(settings.sendlog_path())
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
        Err(_) if strict => Err(ToolError::msg(LOG_BLOCKED)),
        Err(_) => {
            eprintln!("send log not writable");
            Ok(())
        }
    }
}

pub(crate) fn attempts(entries: &[LogEntry]) -> Vec<&LogEntry> {
    let finals: HashSet<&str> = entries
        .iter()
        .filter(|entry| entry.outcome != Outcome::Pending)
        .filter_map(|entry| entry.id.as_deref())
        .collect();
    entries
        .iter()
        .filter(|entry| {
            !(entry.outcome == Outcome::Pending
                && entry.id.as_deref().is_some_and(|id| finals.contains(id)))
        })
        .collect()
}

pub(crate) fn check_draft_limit(entries: &[LogEntry], now: i64) -> ToolResult<()> {
    let recent = entries
        .iter()
        .filter(|entry| entry.mode == Mode::Draft)
        .filter(|entry| now.saturating_sub(entry.ts) < HOUR)
        .count();
    if recent >= MAX_DRAFTS_PER_HOUR {
        return Err(ToolError::msg(format!(
            "Draft limit reached ({MAX_DRAFTS_PER_HOUR} per hour). NOTHING was created. Tell the user."
        )));
    }
    Ok(())
}

pub(crate) fn check_send_limits(
    entries: &[LogEntry],
    now: i64,
    max_per_hour: i64,
) -> ToolResult<()> {
    let sends: Vec<LogEntry> = entries
        .iter()
        .filter(|entry| entry.mode == Mode::Send)
        .cloned()
        .collect();
    let attempts = attempts(&sends);
    let within = |entry: &LogEntry, window: i64| now.saturating_sub(entry.ts) < window;
    let counted = attempts
        .iter()
        .filter(|entry| matches!(entry.outcome, Outcome::Sent | Outcome::Pending))
        .filter(|entry| within(entry, HOUR))
        .count();
    if i64::try_from(counted).unwrap_or(i64::MAX) >= max_per_hour {
        return Err(ToolError::msg(format!(
            "Send limit reached ({max_per_hour} per hour). NOTHING was sent. Tell the user."
        )));
    }
    let recent = attempts
        .iter()
        .filter(|entry| within(entry, TEN_MINUTES))
        .count();
    if recent >= MAX_ATTEMPTS_PER_10_MIN {
        return Err(ToolError::msg(
            "Too many send requests in the last minutes. NOTHING was sent. Ask the user before you try again.",
        ));
    }
    if let Some(last) = attempts.last() {
        if matches!(last.outcome, Outcome::Declined | Outcome::Timeout) && within(last, COOL_DOWN) {
            return Err(ToolError::msg(
                "The user has just declined or ignored a send request. NOTHING was sent. Do not retry unless the user asks you to.",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::ReplySpec;
    use crate::testutil::{j, log_entry as entry, settings_at, TempDir, TestResult};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn request(mode: Mode) -> ComposeRequest {
        ComposeRequest {
            mode,
            to: vec!["anna@example.org".into()],
            cc: vec![],
            subject: "Hallo".into(),
            body: "Kurzer Text.".into(),
            reply: None,
        }
    }

    #[test]
    fn entries_round_trip_and_carry_the_format_version() {
        let mut reply = request(Mode::Send);
        reply.reply = Some(ReplySpec {
            mailbox: None,
            id: 7,
            all: false,
        });
        let original = LogEntry::for_attempt(
            1_700_000_000,
            "Privat",
            &reply,
            Outcome::Pending,
            None,
            Some("abc"),
        );
        let value = original.to_json();
        assert_eq!(
            value.as_object().and_then(|o| o.get("v")),
            Some(&Json::Int(1))
        );
        assert_eq!(LogEntry::from_json(&value), Some(original.clone()));
        assert_eq!(original.chars, 12);
        assert_eq!(original.sha256.len(), 16);
        assert_eq!(original.reply_to_id, Some(7));
        assert!(!json::dumps(&value, false).contains("Kurzer Text"));
    }

    #[test]
    fn the_preview_wins_over_the_request_unless_it_is_empty() {
        let preview = Preview {
            from: "Demo <d@example.org>".into(),
            to: vec!["real@example.org".into()],
            cc: vec![],
            subject: "Re: Hallo".into(),
            body: "x".into(),
            reply: None,
            status: "sent".into(),
            saved: None,
        };
        let logged = LogEntry::for_attempt(
            1,
            "A",
            &request(Mode::Send),
            Outcome::Sent,
            Some(&preview),
            None,
        );
        assert_eq!(logged.to, ["real@example.org"]);
        assert_eq!(logged.subject, "Re: Hallo");
        assert_eq!(logged.from, "Demo <d@example.org>");
        let empty = Preview {
            to: vec![],
            subject: String::new(),
            ..preview
        };
        let logged = LogEntry::for_attempt(
            1,
            "A",
            &request(Mode::Send),
            Outcome::Sent,
            Some(&empty),
            None,
        );
        assert_eq!(logged.to, ["anna@example.org"]);
        assert_eq!(logged.subject, "Hallo");
    }

    #[test]
    fn unknown_versions_modes_and_shapes_are_not_read() -> TestResult {
        let good = r#"{"v": 1, "ts": 5, "mode": "send", "outcome": "sent", "account": "A", "from": "", "to": [], "cc": [], "subject": "", "reply_to_id": null, "recipients": 0, "chars": 1, "sha256": "x"}"#;
        assert!(LogEntry::from_json(&j(good)?).is_some());
        for bad in [
            good.replace(r#""v": 1"#, r#""v": 2"#),
            good.replace(r#""v": 1, "#, ""),
            good.replace(r#""send""#, r#""wipe""#),
            good.replace(r#""sent""#, r#""vanished""#),
            good.replace(r#""ts": 5"#, r#""ts": "5""#),
            good.replace(r#""to": []"#, r#""to": [1]"#),
            good.replace(r#""chars": 1"#, r#""chars": -1"#),
            good.replace(r#""recipients": 0"#, r#""recipients": -1"#),
            good.replace(r#""recipients": 0, "#, ""),
            good.replace(r#""reply_to_id": null"#, r#""reply_to_id": "x""#),
        ] {
            assert_eq!(LogEntry::from_json(&j(&bad)?), None, "{bad}");
        }
        assert_eq!(LogEntry::from_json(&j("[]")?), None);
        Ok(())
    }

    #[test]
    fn append_writes_private_lines_and_read_returns_them() -> TestResult {
        let dir = TempDir::new()?;
        let settings = settings_at(&dir.path("mail/config.json"));
        make_dirs(&dir.path("mail"), 0o700)?;
        let first = entry(10, Outcome::Pending, Some("a"));
        let second = entry(11, Outcome::Sent, Some("a"));
        append(&settings, &first, true)?;
        append(&settings, &second, true)?;
        let mode = fs::metadata(settings.sendlog_path())?.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let text = fs::read_to_string(settings.sendlog_path())?;
        assert_eq!(text.lines().count(), 2);
        assert!(text.ends_with('\n'));
        let view = read_entries(&settings, 300)?;
        assert_eq!(view.entries, [first, second]);
        assert_eq!(view.unreadable, 0);
        Ok(())
    }

    #[test]
    fn ensure_writable_creates_the_private_folder_and_blocks_on_failure() -> TestResult {
        let dir = TempDir::new()?;
        let settings = settings_at(&dir.path("fresh/mail/config.json"));
        ensure_writable(&settings)?;
        let mode = fs::metadata(dir.path("fresh/mail"))?.permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        let blocked = settings_at(&dir.path("blocked/config.json"));
        fs::create_dir_all(blocked.sendlog_path())?;
        assert_eq!(ensure_writable(&blocked), Err(ToolError::msg(LOG_BLOCKED)));
        assert_eq!(
            append(&blocked, &entry(1, Outcome::Pending, None), true),
            Err(ToolError::msg(LOG_BLOCKED))
        );
        assert_eq!(
            append(&blocked, &entry(1, Outcome::Pending, None), false),
            Ok(())
        );
        Ok(())
    }

    fn padded(ts: i64, outcome: Outcome) -> String {
        let mut value = entry(ts, outcome, None).to_json();
        if let Json::Obj(object) = &mut value {
            object.insert("subject", Json::from("x".repeat(200).as_str()));
        }
        let mut line = json::dumps(&value, false);
        line.push('\n');
        line
    }

    #[test]
    fn reading_is_bounded_and_counts_unreadable_lines() -> TestResult {
        let dir = TempDir::new()?;
        let settings = settings_at(&dir.path("config.json"));
        let mut text = String::from("not json\n{\"v\": 9}\n");
        for ts in 0..6000 {
            text.push_str(&padded(ts, Outcome::Sent));
        }
        fs::write(settings.sendlog_path(), &text)?;
        assert!(u64::try_from(text.len())? > TAIL_BYTES * 2);
        let view = read_entries(&settings, 300)?;
        assert_eq!(view.entries.len(), 300);
        assert_eq!(view.unreadable, 0);
        assert_eq!(view.entries.last().map(|e| e.ts), Some(5999));
        let all = read_entries(&settings, 100_000)?;
        assert!(all.entries.len() < 6000);
        assert!(all.entries.first().is_some_and(|e| e.ts > 0));
        let small = TempDir::new()?;
        let settings = settings_at(&small.path("config.json"));
        fs::write(settings.sendlog_path(), "garbage\n\n{\"v\": 9}\n")?;
        let view = read_entries(&settings, 300)?;
        assert_eq!((view.entries.len(), view.unreadable), (0, 2));
        assert_eq!(
            read_entries(&settings_at(&small.path("nothing/c.json")), 5),
            Ok(LogView::default())
        );
        Ok(())
    }

    #[test]
    fn the_window_for_the_limits_must_reach_back_an_hour_or_sending_is_blocked() -> TestResult {
        let now = 1_700_000_000;
        let recent_only = TempDir::new()?;
        let settings = settings_at(&recent_only.path("config.json"));
        let mut text = String::new();
        for index in 0..4000 {
            text.push_str(&padded(now - 3000 + index % 100, Outcome::Drafted));
        }
        fs::write(settings.sendlog_path(), &text)?;
        assert_eq!(
            read_window(&settings, now),
            Err(ToolError::msg(LOG_TOO_LARGE))
        );

        let reaching_back = TempDir::new()?;
        let settings = settings_at(&reaching_back.path("config.json"));
        let mut text = String::new();
        for index in 0..3000 {
            text.push_str(&padded(now - 9000 - index, Outcome::Sent));
        }
        for index in 0..100 {
            text.push_str(&padded(now - 100 + index, Outcome::Sent));
        }
        fs::write(settings.sendlog_path(), &text)?;
        let view = read_window(&settings, now)?;
        assert!(view.entries.len() > 100);
        assert!(view.entries.last().is_some_and(|last| last.ts >= now - 100));

        let small = TempDir::new()?;
        let settings = settings_at(&small.path("config.json"));
        fs::write(settings.sendlog_path(), padded(now - 5, Outcome::Sent))?;
        assert_eq!(read_window(&settings, now)?.entries.len(), 1);
        assert_eq!(
            read_window(&settings_at(&small.path("none/c.json")), now),
            Ok(LogView::default())
        );
        Ok(())
    }

    #[test]
    fn a_log_that_cannot_be_trusted_blocks_instead_of_counting_as_empty() -> TestResult {
        let now = 1_700_000_000;
        let unreadable = TempDir::new()?;
        let settings = settings_at(&unreadable.path("config.json"));
        fs::write(
            settings.sendlog_path(),
            format!("{}garbage\n", padded(now - 5, Outcome::Sent)),
        )?;
        assert_eq!(
            read_window(&settings, now),
            Err(ToolError::msg(LOG_UNREADABLE))
        );
        fs::write(settings.sendlog_path(), "{\"v\": 1, \"ts\": 5}\n")?;
        assert_eq!(
            read_window(&settings, now),
            Err(ToolError::msg(LOG_UNREADABLE))
        );

        let directory = TempDir::new()?;
        let settings = settings_at(&directory.path("config.json"));
        fs::create_dir_all(settings.sendlog_path())?;
        assert_eq!(
            read_window(&settings, now),
            Err(ToolError::msg(LOG_BLOCKED))
        );
        assert_eq!(
            read_entries(&settings, 10),
            Err(ToolError::msg(LOG_BLOCKED))
        );

        let protected = TempDir::new()?;
        let settings = settings_at(&protected.path("config.json"));
        fs::write(settings.sendlog_path(), padded(now - 5, Outcome::Sent))?;
        fs::set_permissions(settings.sendlog_path(), fs::Permissions::from_mode(0o200))?;
        assert_eq!(
            read_window(&settings, now),
            Err(ToolError::msg(LOG_BLOCKED))
        );
        assert_eq!(
            append(&settings, &entry(now, Outcome::Pending, None), true),
            Err(ToolError::msg(LOG_BLOCKED)),
            "the append needs to read the last byte, so a log that cannot be read blocks too"
        );
        Ok(())
    }

    #[test]
    fn a_torn_last_line_does_not_swallow_the_next_entry() -> TestResult {
        let dir = TempDir::new()?;
        let settings = settings_at(&dir.path("config.json"));
        fs::write(settings.sendlog_path(), "{\"v\": 1, \"ts\": 17000")?;
        append(&settings, &entry(18_000, Outcome::Pending, Some("a")), true)?;
        let view = read_entries(&settings, 10)?;
        assert_eq!(view.unreadable, 1);
        assert_eq!(view.entries.len(), 1);
        assert_eq!(view.entries[0].ts, 18_000);
        append(&settings, &entry(18_001, Outcome::Sent, Some("a")), true)?;
        assert_eq!(read_entries(&settings, 10)?.entries.len(), 2);
        Ok(())
    }

    #[test]
    fn logged_recipients_are_cleaned_capped_and_counted() {
        let many: Vec<String> = (0..40)
            .map(|index| format!("p{index}@example.org\u{9b}\u{e0041}\n"))
            .collect();
        let preview = Preview {
            from: "Me <me@example.org>\u{b}".into(),
            to: many.clone(),
            cc: many,
            subject: "S\u{85}x".into(),
            body: String::new(),
            reply: None,
            status: "sent".into(),
            saved: None,
        };
        let logged = LogEntry::for_attempt(
            1,
            "A",
            &request(Mode::Send),
            Outcome::Sent,
            Some(&preview),
            None,
        );
        assert_eq!(logged.to.len(), MAX_LOGGED_RECIPIENTS);
        assert_eq!(logged.cc.len(), MAX_LOGGED_RECIPIENTS);
        assert_eq!(logged.recipients, 80);
        assert_eq!(logged.to[0], "p0@example.org");
        assert_eq!(logged.from, "Me <me@example.org>");
        assert!(json::dumps(&logged.to_json(), false).len() < 5000);
    }

    #[test]
    fn drafts_are_limited_per_hour_and_sends_do_not_count() {
        let now = 1_700_000_000;
        let drafts = |count: i64, age: i64| -> Vec<LogEntry> {
            (0..count)
                .map(|index| {
                    let mut draft = entry(now - age - index, Outcome::Drafted, None);
                    draft.mode = Mode::Draft;
                    draft
                })
                .collect()
        };
        assert_eq!(check_draft_limit(&drafts(29, 10), now), Ok(()));
        assert!(matches!(
            check_draft_limit(&drafts(30, 10), now),
            Err(ToolError::Msg(m)) if m.starts_with("Draft limit reached (30 per hour)")
        ));
        assert_eq!(check_draft_limit(&drafts(300, 4000), now), Ok(()));
        let sends: Vec<LogEntry> = (0..50)
            .map(|index| entry(now - 10 - index, Outcome::Sent, None))
            .collect();
        assert_eq!(check_draft_limit(&sends, now), Ok(()));
    }

    #[test]
    fn a_final_entry_replaces_the_pending_one_with_the_same_id() {
        let entries = [
            entry(1, Outcome::Pending, Some("a")),
            entry(2, Outcome::Sent, Some("a")),
            entry(3, Outcome::Pending, Some("b")),
            entry(4, Outcome::Pending, None),
        ];
        let kept: Vec<(i64, Outcome)> = attempts(&entries)
            .iter()
            .map(|e| (e.ts, e.outcome))
            .collect();
        assert_eq!(
            kept,
            [
                (2, Outcome::Sent),
                (3, Outcome::Pending),
                (4, Outcome::Pending)
            ]
        );
    }

    fn many(count: i64, start_age: i64, outcome: Outcome, now: i64) -> Vec<LogEntry> {
        (0..count)
            .map(|index| entry(now - start_age - index, outcome, None))
            .collect()
    }

    #[test]
    fn five_sends_in_the_last_hour_block_and_older_ones_do_not_count() {
        let now = 1_700_000_000;
        let limit = |entries: &[LogEntry]| check_send_limits(entries, now, 5);
        assert_eq!(limit(&many(5, 4000, Outcome::Sent, now)), Ok(()));
        let blocked = limit(&many(5, 1000, Outcome::Sent, now));
        assert!(
            matches!(blocked, Err(ToolError::Msg(m)) if m.starts_with("Send limit reached (5 per hour)"))
        );
        assert_eq!(limit(&many(4, 1000, Outcome::Sent, now)), Ok(()));
    }

    #[test]
    fn unresolved_attempts_count_as_sent() {
        let now = 1_700_000_000;
        let pendings: Vec<LogEntry> = (0..5)
            .map(|index| {
                entry(
                    now - 900 - index,
                    Outcome::Pending,
                    Some(&format!("p{index}")),
                )
            })
            .collect();
        assert!(
            matches!(check_send_limits(&pendings, now, 5), Err(ToolError::Msg(m)) if m.starts_with("Send limit reached"))
        );
        assert_eq!(check_send_limits(&pendings[..4], now, 5), Ok(()));
    }

    #[test]
    fn a_pending_and_its_result_count_once() {
        let now = 1_700_000_000;
        let mut entries = Vec::new();
        for index in 0..4 {
            let id = format!("q{index}");
            entries.push(entry(now - 1500 - index, Outcome::Pending, Some(&id)));
            entries.push(entry(now - 1490 - index, Outcome::Sent, Some(&id)));
        }
        assert_eq!(check_send_limits(&entries, now, 5), Ok(()));
        entries.push(entry(now - 10, Outcome::Sent, Some("fifth")));
        assert!(
            matches!(check_send_limits(&entries, now, 5), Err(ToolError::Msg(m)) if m.starts_with("Send limit reached"))
        );
    }

    #[test]
    fn six_attempts_in_ten_minutes_block_even_when_declined() {
        let now = 1_700_000_000;
        let declines = many(6, 100, Outcome::Declined, now);
        let reason = check_send_limits(&declines, now, 5);
        assert!(
            matches!(reason, Err(ToolError::Msg(m)) if m.starts_with("Too many send requests"))
        );
        assert_eq!(check_send_limits(&declines[..5], now, 5), Ok(()));
        let spread = many(6, 700, Outcome::Declined, now);
        assert_eq!(check_send_limits(&spread, now, 5), Ok(()));
    }

    #[test]
    fn a_fresh_decline_or_timeout_cools_down_for_twenty_seconds() {
        let now = 1_700_000_000;
        for outcome in [Outcome::Declined, Outcome::Timeout] {
            let reason = check_send_limits(&[entry(now - 5, outcome, None)], now, 5);
            assert!(
                matches!(reason, Err(ToolError::Msg(m)) if m.starts_with("The user has just declined or ignored"))
            );
            assert_eq!(
                check_send_limits(&[entry(now - 25, outcome, None)], now, 5),
                Ok(())
            );
        }
        assert_eq!(
            check_send_limits(&[entry(now - 5, Outcome::Error, None)], now, 5),
            Ok(())
        );
    }

    #[test]
    fn drafts_never_count_against_the_send_limits() {
        let now = 1_700_000_000;
        let mut drafts = many(10, 10, Outcome::Drafted, now);
        for draft in &mut drafts {
            draft.mode = Mode::Draft;
        }
        assert_eq!(check_send_limits(&drafts, now, 5), Ok(()));
    }

    #[test]
    fn entries_from_the_future_count_as_recent() {
        let now = 1_700_000_000;
        let future = many(5, -500, Outcome::Sent, now);
        assert!(check_send_limits(&future, now, 5).is_err());
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
    fn outcomes_map_from_backend_status() {
        assert_eq!(Outcome::from_status("sent"), Outcome::Sent);
        assert_eq!(Outcome::from_status("declined"), Outcome::Declined);
        assert_eq!(Outcome::from_status("timeout"), Outcome::Timeout);
        assert_eq!(Outcome::from_status("drafted"), Outcome::Error);
        assert_eq!(Outcome::from_status(""), Outcome::Error);
    }
}
