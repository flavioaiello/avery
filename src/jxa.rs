use std::time::Duration;

use crate::error::{ToolError, ToolResult};
use crate::json::{self, Json, Object};
use crate::osascript::{App, Finished, Host};
use crate::text::text_of;

pub(crate) type Failure = fn(&str, Option<&str>, bool) -> ToolError;

pub(crate) fn exchange(
    host: &dyn Host,
    app: App,
    request: Object,
    timeout_seconds: u64,
    failure: Failure,
) -> ToolResult<Json> {
    let op = request
        .get("op")
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_string();
    let op = op.as_str();
    let payload = json::dumps(&Json::Obj(request), true);
    let finished = host.run(app, &payload, Duration::from_secs(timeout_seconds));
    let (stdout, stderr) = match finished {
        Finished::TimedOut => return Err(failure("timed out", Some(op), false)),
        Finished::NotStarted => return Err(failure("empty answer from osascript", Some(op), true)),
        Finished::TooLarge => {
            return Err(failure("unexpected answer from osascript", Some(op), true))
        }
        Finished::Done { stdout, stderr, .. } => (stdout, stderr),
    };
    let reason = |fallback: &str| {
        if stderr.is_empty() {
            fallback.to_string()
        } else {
            stderr.clone()
        }
    };
    let answer = stdout.trim();
    if answer.is_empty() {
        return Err(failure(
            &reason("empty answer from osascript"),
            Some(op),
            true,
        ));
    }
    let Ok(Json::Obj(result)) = json::parse(answer) else {
        return Err(failure(
            &reason("unexpected answer from osascript"),
            Some(op),
            true,
        ));
    };
    if result.get("ok") != Some(&Json::Bool(true)) {
        return Err(failure(&text_of(result.get("error")), Some(op), false));
    }
    Ok(result.get("data").cloned().unwrap_or(Json::Null))
}
