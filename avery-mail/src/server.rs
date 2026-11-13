use std::io::{self, BufRead, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};

use crate::calendar::{self, MAX_NOTES};
use crate::context::Context;
use crate::error::{ToolError, ToolResult};
use crate::json::{self, Json, Object};
use crate::read_tools;
use crate::rights::Level;
use crate::text::clean;
use crate::texts::{
    calendar_write_instructions, read_instructions, write_instructions, MAX_RECIPIENTS,
    MAX_SEND_CHARS, MAX_SEND_LINES, SERVER_NAME, VERSION,
};
use crate::write_tools;

pub(crate) const MAX_LINE_BYTES: usize = 1 << 20;
const DEFAULT_PROTOCOL: &str = "2024-11-05";

type Handler = fn(&Context, &Object) -> ToolResult<String>;

fn handler(name: &str) -> Option<Handler> {
    match name {
        "list_accounts" => Some(read_tools::list_accounts),
        "list_mailboxes" => Some(read_tools::list_mailboxes),
        "list_messages" => Some(read_tools::list_messages),
        "search_messages" => Some(read_tools::search_messages),
        "read_message" => Some(read_tools::read_message),
        "create_draft" => Some(write_tools::create_draft),
        "send_message" => Some(write_tools::send_message),
        "list_calendars" => Some(calendar::list_calendars),
        "list_events" => Some(calendar::list_events),
        "get_event" => Some(calendar::get_event),
        "create_event" => Some(calendar::create_event),
        "update_event" => Some(calendar::update_event),
        "delete_event" => Some(calendar::delete_event),
        _ => None,
    }
}

fn property(kind: &str, description: &str) -> Json {
    Json::object(vec![
        ("type", Json::from(kind)),
        ("description", Json::from(description)),
    ])
}

fn text_list(description: &str) -> Json {
    Json::object(vec![
        ("type", Json::from("array")),
        ("items", Json::object(vec![("type", Json::from("string"))])),
        ("description", Json::from(description)),
    ])
}

fn tool(
    name: &str,
    description: &str,
    properties: Vec<(&str, Json)>,
    required: &[&str],
    read_only: bool,
) -> Json {
    let mut schema = Object::new();
    schema.insert("type", "object");
    schema.insert("properties", Json::object(properties));
    schema.insert("additionalProperties", Json::Bool(false));
    if !required.is_empty() {
        schema.insert(
            "required",
            Json::Arr(required.iter().map(|name| Json::from(*name)).collect()),
        );
    }
    let mut annotations = Object::new();
    annotations.insert("title", name.replace('_', " ").as_str());
    annotations.insert("readOnlyHint", Json::Bool(read_only));
    if !read_only {
        annotations.insert(
            "destructiveHint",
            Json::Bool(matches!(name, "update_event" | "delete_event")),
        );
        annotations.insert("idempotentHint", Json::Bool(false));
        annotations.insert("openWorldHint", Json::Bool(name == "send_message"));
    }
    Json::object(vec![
        ("name", Json::from(name)),
        ("description", Json::from(description)),
        ("inputSchema", Json::Obj(schema)),
        ("annotations", Json::Obj(annotations)),
    ])
}

fn account() -> Json {
    property(
        "string",
        "Account name from list_accounts. Default: all enabled accounts.",
    )
}

fn mailbox() -> Json {
    property(
        "string",
        "Mailbox path from list_mailboxes. Default: the inbox.",
    )
}

fn unread_only() -> Json {
    property("boolean", "Only unread messages (default false).")
}

fn limit() -> Json {
    property(
        "integer",
        "Maximum number of messages (default 20, max 50).",
    )
}

fn read_tool_list() -> Vec<Json> {
    vec![
        tool(
            "list_accounts",
            "List the private mail accounts you may read (name, type, addresses).",
            vec![],
            &[],
            true,
        ),
        tool(
            "list_mailboxes",
            "List mailboxes (folders/labels) with unread counts. Use the paths as 'mailbox' in other tools.",
            vec![("account", account())],
            &[],
            true,
        ),
        tool(
            "list_messages",
            "List recent messages of a mailbox (default: inbox), newest first: id, date, sender, subject, unread flag. Use read_message to see the text.",
            vec![
                ("account", account()),
                ("mailbox", mailbox()),
                ("unread_only", unread_only()),
                (
                    "since_days",
                    property(
                        "integer",
                        "Look back this many days (default 3, max 365).",
                    ),
                ),
                ("limit", limit()),
            ],
            &[],
            true,
        ),
        tool(
            "search_messages",
            "Find messages whose subject and/or sender contains the text, newest first. Body text is not searched.",
            vec![
                ("query", property("string", "Text to look for.")),
                (
                    "field",
                    Json::object(vec![
                        ("type", Json::from("string")),
                        (
                            "enum",
                            Json::Arr(vec![
                                Json::from("subject"),
                                Json::from("sender"),
                                Json::from("both"),
                            ]),
                        ),
                        ("description", Json::from("Where to look (default both).")),
                    ]),
                ),
                ("account", account()),
                ("mailbox", mailbox()),
                ("unread_only", unread_only()),
                (
                    "since_days",
                    property(
                        "integer",
                        "Look back this many days (default 90, max 3650).",
                    ),
                ),
                ("limit", limit()),
            ],
            &["query"],
            true,
        ),
        tool(
            "read_message",
            "Read one message: headers, attachment names and plain-text body (truncated). Needs the id from list_messages or search_messages; pass the same account and mailbox.",
            vec![
                ("id", property("string", "Message id exactly as shown by list_messages or search_messages.")),
                ("account", account()),
                ("mailbox", mailbox()),
                (
                    "max_chars",
                    property(
                        "integer",
                        "Maximum body characters (default 6000, max 20000).",
                    ),
                ),
            ],
            &["id"],
            true,
        ),
    ]
}

fn calendar_argument() -> Json {
    property(
        "string",
        "Calendar name from list_calendars. Default: all enabled calendars.",
    )
}

fn calendar_read_tool_list() -> Vec<Json> {
    vec![
        tool(
            "list_calendars",
            "List the private calendars you may use (name and level: read or write).",
            vec![],
            &[],
            true,
        ),
        tool(
            "list_events",
            "List events in a time range, earliest first: calendar, id, time, title, place, attendees, repeat. Recurring events are expanded. Times are local. Use get_event for notes and attendees.",
            vec![
                ("calendar", calendar_argument()),
                (
                    "start",
                    property(
                        "string",
                        "First moment, local time: 2026-11-20 or 2026-11-20 09:30 (default: today).",
                    ),
                ),
                (
                    "days",
                    property("integer", "Length of the range in days (default 7, max 60)."),
                ),
                (
                    "limit",
                    property("integer", "Maximum number of events (default 30, max 100)."),
                ),
                (
                    "query",
                    property("string", "Only events whose title or place contains this text."),
                ),
            ],
            &[],
            true,
        ),
        tool(
            "get_event",
            "Read one event: time, place, repeat rule, attendees, link and notes (untrusted). Needs the id from list_events; pass the same calendar.",
            vec![
                ("id", property("string", "Event id from list_events.")),
                ("calendar", calendar_argument()),
            ],
            &["id"],
            true,
        ),
    ]
}

fn event_properties() -> Vec<(&'static str, Json)> {
    vec![
        (
            "summary",
            property("string", "Title of the event, one line."),
        ),
        (
            "start",
            property(
                "string",
                "Start, local time: 2026-11-20 09:30, or a date 2026-11-20 for an all-day event.",
            ),
        ),
        (
            "end",
            property(
                "string",
                "End, local time (or the last day for an all-day event). Not together with duration_minutes.",
            ),
        ),
        (
            "duration_minutes",
            property(
                "integer",
                "Length in minutes (default 60 for a new event). Not together with end.",
            ),
        ),
        (
            "location",
            property("string", "Place, one line. An empty text clears it."),
        ),
        (
            "notes",
            property(
                "string",
                &format!("Notes, plain text up to {MAX_NOTES} characters. An empty text clears them."),
            ),
        ),
    ]
}

fn calendar_write_tool_list() -> Vec<Json> {
    let mut create = vec![(
        "calendar",
        property(
            "string",
            "Calendar name from list_calendars. Optional if only one calendar may be changed.",
        ),
    )];
    create.extend(event_properties());
    create.push((
        "all_day",
        property(
            "boolean",
            "All-day event (default false): start is then a date and end the last day.",
        ),
    ));
    let mut update = vec![
        ("id", property("string", "Event id from list_events.")),
        (
            "calendar",
            property(
                "string",
                "Calendar of the event. Optional if only one calendar may be changed.",
            ),
        ),
    ];
    update.extend(event_properties());
    vec![
        tool(
            "create_event",
            "Create a single event. The user must approve it in a macOS dialog that shows the details; if they decline, nothing is created. Needs summary and start (local time). Timed: end or duration_minutes (default 60). All day: all_day=true and a date. No attendees, no repetition, no alerts.",
            create,
            &["summary", "start"],
            false,
        ),
        tool(
            "update_event",
            "Change a single event without attendees (not recurring). Only the fields you pass change. The user must approve in a macOS dialog that shows before and after. Needs the id from list_events. To move an event pass start (the length stays) or start and end.",
            update,
            &["id"],
            false,
        ),
        tool(
            "delete_event",
            "Delete a single event without attendees (not recurring). The user must approve in a macOS dialog; the deletion is logged and the user can undo it. Needs the id from list_events.",
            vec![
                ("id", property("string", "Event id from list_events.")),
                (
                    "calendar",
                    property(
                        "string",
                        "Calendar of the event. Optional if only one calendar may be changed.",
                    ),
                ),
            ],
            &["id"],
            false,
        ),
    ]
}

fn compose_properties() -> Vec<(&'static str, Json)> {
    vec![
        (
            "account",
            property(
                "string",
                "Account name from list_accounts to write from. Optional if only one account may write.",
            ),
        ),
        (
            "to",
            text_list(&format!(
                "Recipient addresses (plain name@example.com, max {MAX_RECIPIENTS} with cc). Not for answers."
            )),
        ),
        ("cc", text_list("Cc addresses. Not for answers.")),
        (
            "subject",
            property("string", "Subject, one line. Not for answers."),
        ),
        ("body", property("string", "Plain text of the mail.")),
        (
            "reply_to_id",
            property(
                "string",
                "Answer to the message with this id (exactly as shown by list_messages / search_messages). The recipients and the subject are taken from it.",
            ),
        ),
        ("mailbox", mailbox()),
        (
            "reply_all",
            property(
                "boolean",
                "With reply_to_id: answer all recipients (default false).",
            ),
        ),
    ]
}

fn write_tool_list() -> Vec<Json> {
    vec![
        tool(
            "create_draft",
            "Save a draft for the user to review in their mail app (Apple Mail opens it, Gmail and Outlook.com keep it in Drafts); NOTHING is sent. New mail: to, subject, body. Answer: reply_to_id (+ account and mailbox of the original) and body. Plain text, no Bcc, no attachments.",
            compose_properties(),
            &["body"],
            false,
        ),
        tool(
            "send_message",
            &format!(
                "Send a mail. The user must approve each mail in a macOS dialog showing recipients and text; if they decline, nothing is sent. New mail: to, subject, body. Answer: reply_to_id (+ account and mailbox of the original) and body. Plain text up to {MAX_SEND_CHARS} characters and {MAX_SEND_LINES} lines, no Bcc, no attachments; longer texts must be drafts."
            ),
            compose_properties(),
            &["body"],
            false,
        ),
    ]
}

struct Exposure {
    mail_read: bool,
    mail_write: bool,
    calendar_read: bool,
    calendar_write: bool,
    problem: Option<String>,
}

fn exposure(ctx: &Context) -> Exposure {
    match ctx.settings.rights() {
        Ok(rights) => Exposure {
            mail_read: !rights.mail_at(Level::Read).is_empty(),
            mail_write: !rights.mail_at(Level::Write).is_empty(),
            calendar_read: !rights.calendars_at(Level::Read).is_empty(),
            calendar_write: !rights.calendars_at(Level::Write).is_empty(),
            problem: None,
        },
        Err(error) => Exposure {
            mail_read: true,
            mail_write: false,
            calendar_read: true,
            calendar_write: false,
            problem: Some(error.message()),
        },
    }
}

pub(crate) fn tools_list(ctx: &Context) -> Json {
    let exposure = exposure(ctx);
    let mut tools = Vec::new();
    if exposure.mail_read {
        tools.extend(read_tool_list());
    }
    if exposure.mail_write {
        tools.extend(write_tool_list());
    }
    if exposure.calendar_read {
        tools.extend(calendar_read_tool_list());
    }
    if exposure.calendar_write {
        tools.extend(calendar_write_tool_list());
    }
    Json::Arr(tools)
}

pub(crate) fn instructions(ctx: &Context) -> String {
    let exposure = exposure(ctx);
    let mut text = read_instructions(exposure.mail_read, exposure.calendar_read);
    if exposure.mail_write {
        text.push_str(&write_instructions());
    }
    if exposure.calendar_write {
        text.push_str(&calendar_write_instructions());
    }
    if let Some(problem) = &exposure.problem {
        text.push_str(&format!(" The configuration cannot be used: {problem}"));
    }
    text
}

fn text_result(text: &str, is_error: bool) -> Json {
    Json::object(vec![
        (
            "content",
            Json::Arr(vec![Json::object(vec![
                ("type", Json::from("text")),
                ("text", Json::from(text)),
            ])]),
        ),
        ("isError", Json::Bool(is_error)),
    ])
}

fn invoke(ctx: &Context, name: &str, run: Handler, args: &Object) -> Json {
    let outcome = catch_unwind(AssertUnwindSafe(|| run(ctx, args)))
        .unwrap_or_else(|_| Err(ToolError::internal("panic")));
    match outcome {
        Ok(text) => text_result(&text, false),
        Err(error) => {
            if let ToolError::Internal(kind) = &error {
                eprintln!("internal error in {name}: {kind}");
            }
            text_result(&error.message(), true)
        }
    }
}

pub(crate) fn call_tool(ctx: &Context, params: &Object) -> Json {
    let requested = params.get("name");
    let name = requested.and_then(Json::as_str).unwrap_or_default();
    let Some(run) = handler(name) else {
        return text_result(
            &format!("Unknown tool: {}", clean(requested, Some(40))),
            true,
        );
    };
    let empty = Object::new();
    let args = match params.get("arguments") {
        Some(Json::Obj(object)) => object,
        _ => &empty,
    };
    invoke(ctx, name, run, args)
}

fn reply(id: &Json, result: Result<Json, (i64, &str)>) -> Json {
    let mut payload = Object::new();
    payload.insert("jsonrpc", "2.0");
    payload.insert("id", id.clone());
    match result {
        Ok(value) => payload.insert("result", value),
        Err((code, message)) => payload.insert(
            "error",
            Json::object(vec![
                ("code", Json::Int(code)),
                ("message", Json::from(message)),
            ]),
        ),
    }
    Json::Obj(payload)
}

pub(crate) fn handle(ctx: &Context, message: &Json) -> Option<Json> {
    let object = message.as_object()?;
    let method = object.get("method").filter(|value| **value != Json::Null)?;
    let id = object.get("id").filter(|value| **value != Json::Null)?;
    let empty = Object::new();
    let params = match object.get("params") {
        Some(Json::Obj(params)) => params,
        _ => &empty,
    };
    let result = match method.as_str() {
        Some("initialize") => {
            let version = match params.get("protocolVersion") {
                Some(Json::Str(version)) if !version.is_empty() => version.as_str(),
                _ => DEFAULT_PROTOCOL,
            };
            Ok(Json::object(vec![
                ("protocolVersion", Json::from(version)),
                (
                    "capabilities",
                    Json::object(vec![(
                        "tools",
                        Json::object(vec![("listChanged", Json::Bool(false))]),
                    )]),
                ),
                (
                    "serverInfo",
                    Json::object(vec![
                        ("name", Json::from(SERVER_NAME)),
                        ("version", Json::from(VERSION)),
                    ]),
                ),
                ("instructions", Json::from(instructions(ctx).as_str())),
            ]))
        }
        Some("ping") => Ok(Json::Obj(Object::new())),
        Some("tools/list") => Ok(Json::object(vec![("tools", tools_list(ctx))])),
        Some("tools/call") => Ok(call_tool(ctx, params)),
        Some("resources/list") => Ok(Json::object(vec![("resources", Json::Arr(Vec::new()))])),
        Some("resources/templates/list") => Ok(Json::object(vec![(
            "resourceTemplates",
            Json::Arr(Vec::new()),
        )])),
        Some("prompts/list") => Ok(Json::object(vec![("prompts", Json::Arr(Vec::new()))])),
        _ => Err((-32601, "Method not found")),
    };
    Some(reply(id, result))
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Line {
    Text(Vec<u8>),
    TooLong,
    Eof,
}

pub(crate) fn next_line(input: &mut dyn BufRead, max: usize) -> io::Result<Line> {
    let mut line: Vec<u8> = Vec::new();
    let mut too_long = false;
    let mut seen = false;
    loop {
        let available = input.fill_buf()?;
        if available.is_empty() {
            break;
        }
        seen = true;
        let end = available.iter().position(|byte| *byte == b'\n');
        let chunk = &available[..end.unwrap_or(available.len())];
        if !too_long {
            if line.len() + chunk.len() > max {
                too_long = true;
                line = Vec::new();
            } else {
                line.extend_from_slice(chunk);
            }
        }
        let consumed = end.map_or(available.len(), |position| position + 1);
        input.consume(consumed);
        if end.is_some() {
            return Ok(if too_long {
                Line::TooLong
            } else {
                Line::Text(line)
            });
        }
    }
    Ok(match (seen, too_long) {
        (false, _) => Line::Eof,
        (true, true) => Line::TooLong,
        (true, false) => Line::Text(line),
    })
}

pub(crate) fn serve(
    ctx: &Context,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> io::Result<()> {
    loop {
        let bytes = match next_line(input, MAX_LINE_BYTES)? {
            Line::Eof => return Ok(()),
            Line::TooLong => {
                eprintln!("request line too long, ignored");
                continue;
            }
            Line::Text(bytes) => bytes,
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }
        let Ok(message) = json::parse(text.trim()) else {
            continue;
        };
        if let Some(response) = handle(ctx, &message) {
            let mut line = json::dumps(&response, true);
            line.push('\n');
            output.write_all(line.as_bytes())?;
            output.flush()?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::string_list;
    use crate::fake::FakeOptions;
    use crate::testutil::{j, settings_at, TempDir, TestResult};
    use std::fs;
    use std::io::Cursor;

    const READ_TOOLS: [&str; 5] = [
        "list_accounts",
        "list_mailboxes",
        "list_messages",
        "search_messages",
        "read_message",
    ];
    const CALENDAR_TOOLS: [&str; 3] = ["list_calendars", "list_events", "get_event"];
    const CALENDAR_WRITE_TOOLS: [&str; 3] = ["create_event", "update_event", "delete_event"];

    fn read_and_calendar() -> Vec<&'static str> {
        READ_TOOLS
            .iter()
            .chain(CALENDAR_TOOLS.iter())
            .copied()
            .collect()
    }

    fn demo(dir: &TempDir, write: &str, approve: &str) -> Context {
        let mut settings = settings_at(&dir.path("config.json"));
        settings.fake = true;
        settings.fake_write = write.to_string();
        let options = FakeOptions {
            approve: approve.to_string(),
            outbox: Some(dir.path("outbox.jsonl")),
            ..FakeOptions::default()
        };
        Context::demo(settings, options)
    }

    fn real_mode(dir: &TempDir, config: &str) -> Result<Context, std::io::Error> {
        let settings = settings_at(&dir.path("config.json"));
        fs::write(&settings.config_path, config)?;
        Ok(Context::demo(settings, FakeOptions::default()))
    }

    fn rpc(ctx: &Context, text: &str) -> Result<Json, Box<dyn std::error::Error>> {
        handle(ctx, &j(text)?).ok_or_else(|| "no response".into())
    }

    fn names(ctx: &Context) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let response = rpc(
            ctx,
            r#"{"jsonrpc": "2.0", "id": 1, "method": "tools/list"}"#,
        )?;
        let tools = response
            .as_object()
            .and_then(|o| o.get("result"))
            .and_then(Json::as_object)
            .and_then(|o| o.get("tools"))
            .and_then(Json::as_array)
            .ok_or("no tools")?;
        Ok(tools
            .iter()
            .map(|tool| crate::text::text_of(tool.as_object().and_then(|o| o.get("name"))))
            .collect())
    }

    fn call(
        ctx: &Context,
        name: &str,
        arguments: &str,
    ) -> Result<(bool, String), Box<dyn std::error::Error>> {
        let request = format!(
            r#"{{"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {{"name": "{name}", "arguments": {arguments}}}}}"#
        );
        let response = rpc(ctx, &request)?;
        let result = response
            .as_object()
            .and_then(|o| o.get("result"))
            .and_then(Json::as_object)
            .ok_or("no result")?;
        let text = result
            .get("content")
            .and_then(Json::as_array)
            .and_then(|items| items.first())
            .and_then(Json::as_object)
            .and_then(|o| o.get("text"))
            .and_then(Json::as_str)
            .ok_or("no text")?
            .to_string();
        Ok((result.get("isError") == Some(&Json::Bool(true)), text))
    }

    #[test]
    fn without_write_rights_only_the_read_tools_are_offered() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = demo(&dir, "", "no");
        assert_eq!(names(&ctx)?, read_and_calendar());
        assert!(!instructions(&ctx).contains("send_message"));
        let (error, text) = call(
            &ctx,
            "send_message",
            r#"{"to": ["a@example.org"], "subject": "S", "body": "B"}"#,
        )?;
        assert!(error && text.contains("not enabled"));
        let (error, text) = call(&ctx, "create_draft", r#"{"body": "B"}"#)?;
        assert!(error && text.contains("not enabled"));
        Ok(())
    }

    #[test]
    fn with_write_rights_the_write_tools_appear_with_honest_annotations() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = demo(&dir, "FAKE-GMAIL", "no");
        let names = names(&ctx)?;
        assert_eq!(names[..5], READ_TOOLS);
        assert_eq!(names[5..7], ["create_draft", "send_message"]);
        assert_eq!(names[7..], CALENDAR_TOOLS);
        assert!(instructions(&ctx).contains("macOS dialog"));
        let Json::Arr(tools) = tools_list(&ctx) else {
            return Err("not a list".into());
        };
        for tool in &tools[5..7] {
            let object = tool.as_object().ok_or("tool")?;
            let annotations = object
                .get("annotations")
                .and_then(Json::as_object)
                .ok_or("annotations")?;
            assert_eq!(annotations.get("readOnlyHint"), Some(&Json::Bool(false)));
            assert_eq!(annotations.get("destructiveHint"), Some(&Json::Bool(false)));
            let schema = object
                .get("inputSchema")
                .and_then(Json::as_object)
                .ok_or("schema")?;
            let properties = schema
                .get("properties")
                .and_then(Json::as_object)
                .ok_or("properties")?;
            assert!(properties.get("bcc").is_none());
            assert!(properties.get("attachments").is_none());
            assert_eq!(schema.get("additionalProperties"), Some(&Json::Bool(false)));
            assert_eq!(string_list(schema.get("required")), ["body"]);
        }
        let send = tools[6].as_object().ok_or("send")?;
        let open_world = send
            .get("annotations")
            .and_then(Json::as_object)
            .and_then(|a| a.get("openWorldHint"));
        assert_eq!(open_world, Some(&Json::Bool(true)));
        let draft = tools[5].as_object().ok_or("draft")?;
        let open_world = draft
            .get("annotations")
            .and_then(Json::as_object)
            .and_then(|a| a.get("openWorldHint"));
        assert_eq!(open_world, Some(&Json::Bool(false)));
        Ok(())
    }

    #[test]
    fn with_calendar_write_rights_the_change_tools_appear_with_honest_annotations() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = demo(&dir, "FAKE-CAL-PRIVAT", "no");
        let all = names(&ctx)?;
        assert_eq!(all[..8], read_and_calendar());
        assert_eq!(all[8..], CALENDAR_WRITE_TOOLS);
        assert!(instructions(&ctx).contains("Calendar changes: create_event"));
        assert!(!instructions(&ctx).contains("send_message"));
        let Json::Arr(tools) = tools_list(&ctx) else {
            return Err("not a list".into());
        };
        let hints = |tool: &Json| -> Result<(Json, Json, Json), Box<dyn std::error::Error>> {
            let annotations = tool
                .as_object()
                .and_then(|o| o.get("annotations"))
                .and_then(Json::as_object)
                .ok_or("annotations")?;
            Ok((
                annotations.get("readOnlyHint").cloned().ok_or("read")?,
                annotations
                    .get("destructiveHint")
                    .cloned()
                    .ok_or("destructive")?,
                annotations.get("openWorldHint").cloned().ok_or("open")?,
            ))
        };
        let false_ = Json::Bool(false);
        let true_ = Json::Bool(true);
        assert_eq!(
            hints(&tools[8])?,
            (false_.clone(), false_.clone(), false_.clone())
        );
        assert_eq!(
            hints(&tools[9])?,
            (false_.clone(), true_.clone(), false_.clone())
        );
        assert_eq!(hints(&tools[10])?, (false_.clone(), true_, false_));
        for tool in &tools[8..] {
            let schema = tool
                .as_object()
                .and_then(|o| o.get("inputSchema"))
                .and_then(Json::as_object)
                .ok_or("schema")?;
            assert_eq!(schema.get("additionalProperties"), Some(&Json::Bool(false)));
            let properties = schema
                .get("properties")
                .and_then(Json::as_object)
                .ok_or("properties")?;
            assert!(
                properties.get("attendees").is_none() && properties.get("recurrence").is_none()
            );
        }
        let create = tools[8].as_object().ok_or("create")?;
        let required = create
            .get("inputSchema")
            .and_then(Json::as_object)
            .and_then(|s| s.get("required"));
        assert_eq!(string_list(required), ["summary", "start"]);
        Ok(())
    }

    #[test]
    fn real_mode_configuration_decides_which_tools_exist() -> TestResult {
        let dir = TempDir::new()?;
        let account = |level: &str| {
            format!(
                r#"{{"id": "ACC-1", "name": "Privat Gmail", "type": "imap", "addresses": ["me@gmail.example"], "level": "{level}"}}"#
            )
        };
        let with = |level: &str| {
            format!(
                r#"{{"version": 3, "apple": {{"mail": [{}]}}}}"#,
                account(level)
            )
        };
        assert_eq!(names(&real_mode(&dir, &with("read"))?)?, READ_TOOLS);
        assert_eq!(names(&real_mode(&dir, &with("write"))?)?.len(), 7);
        let calendar = r#"{"version": 3, "apple": {"calendars": [{"id": "C", "name": "K", "level": "read"}]}}"#;
        assert_eq!(names(&real_mode(&dir, calendar)?)?, CALENDAR_TOOLS);
        let both = r#"{"version": 3,
            "apple": {"mail": [{"id": "A", "level": "read"}],
                      "calendars": [{"id": "C", "level": "write"}]}}"#;
        let remote = r#"{"version": 3, "accounts": [{"provider": "google", "address": "alice@gmail.example", "mail": "write"}]}"#;
        assert_eq!(names(&real_mode(&dir, remote)?)?.len(), 7);
        let linked_only = r#"{"version": 3, "accounts": [{"provider": "google", "address": "alice@gmail.example"}]}"#;
        assert!(names(&real_mode(&dir, linked_only)?)?.is_empty());
        let mut expected = read_and_calendar();
        expected.extend(CALENDAR_WRITE_TOOLS);
        assert_eq!(names(&real_mode(&dir, both)?)?, expected);
        let empty = r#"{"version": 3}"#;
        let ctx = real_mode(&dir, empty)?;
        assert!(names(&ctx)?.is_empty());
        assert!(instructions(&ctx).contains("Nothing is enabled yet"));
        for broken in [
            r#"{"version": 2, "allowed_accounts": []}"#,
            r#"{"version": 3, "apple": {"mail": [{"id": "A", "level": "root"}]}}"#,
            r#"{"version": 3, "mail": {"accounts": [{"id": "A", "level": "read"}]}}"#,
            "not json",
        ] {
            let ctx = real_mode(&dir, broken)?;
            assert_eq!(names(&ctx)?, read_and_calendar(), "{broken}");
            assert!(instructions(&ctx).contains("The configuration cannot be used"));
            let (error, text) = call(&ctx, "list_accounts", "{}")?;
            assert!(error && text.contains("config.json"), "{text}");
            let (error, text) = call(&ctx, "list_events", "{}")?;
            assert!(error && text.contains("config.json"), "{text}");
        }
        Ok(())
    }

    #[test]
    fn the_handshake_echoes_the_protocol_version_and_names_the_server() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = demo(&dir, "", "no");
        let response = rpc(
            &ctx,
            r#"{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26"}}"#,
        )?;
        let result = response
            .as_object()
            .and_then(|o| o.get("result"))
            .and_then(Json::as_object)
            .ok_or("result")?;
        assert_eq!(
            result.get("protocolVersion"),
            Some(&Json::from("2025-03-26"))
        );
        let info = result
            .get("serverInfo")
            .and_then(Json::as_object)
            .ok_or("info")?;
        assert_eq!(info.get("name"), Some(&Json::from("avery-mail")));
        assert_eq!(info.get("version"), Some(&Json::from(VERSION)));
        assert_eq!(
            result.get("capabilities"),
            Some(&j(r#"{"tools": {"listChanged": false}}"#)?)
        );
        let fallback = rpc(
            &ctx,
            r#"{"jsonrpc": "2.0", "id": 2, "method": "initialize", "params": {"protocolVersion": 5}}"#,
        )?;
        let version = fallback
            .as_object()
            .and_then(|o| o.get("result"))
            .and_then(Json::as_object)
            .and_then(|o| o.get("protocolVersion"));
        assert_eq!(version, Some(&Json::from("2024-11-05")));
        Ok(())
    }

    #[test]
    fn simple_methods_answer_and_unknown_ones_are_an_error() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = demo(&dir, "", "no");
        let reply_to = |text: &str| rpc(&ctx, text);
        assert_eq!(
            reply_to(r#"{"jsonrpc": "2.0", "id": "x", "method": "ping"}"#)?,
            j(r#"{"jsonrpc": "2.0", "id": "x", "result": {}}"#)?
        );
        assert_eq!(
            reply_to(r#"{"jsonrpc": "2.0", "id": 3, "method": "resources/list"}"#)?,
            j(r#"{"jsonrpc": "2.0", "id": 3, "result": {"resources": []}}"#)?
        );
        assert_eq!(
            reply_to(r#"{"jsonrpc": "2.0", "id": 4, "method": "resources/templates/list"}"#)?,
            j(r#"{"jsonrpc": "2.0", "id": 4, "result": {"resourceTemplates": []}}"#)?
        );
        assert_eq!(
            reply_to(r#"{"jsonrpc": "2.0", "id": 5, "method": "prompts/list"}"#)?,
            j(r#"{"jsonrpc": "2.0", "id": 5, "result": {"prompts": []}}"#)?
        );
        for method in ["\"nope\"", "5", "[]"] {
            assert_eq!(
                reply_to(&format!(
                    r#"{{"jsonrpc": "2.0", "id": 6, "method": {method}}}"#
                ))?,
                j(
                    r#"{"jsonrpc": "2.0", "id": 6, "error": {"code": -32601, "message": "Method not found"}}"#
                )?
            );
        }
        Ok(())
    }

    #[test]
    fn notifications_and_malformed_messages_get_no_answer() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = demo(&dir, "", "no");
        for text in [
            r#"{"jsonrpc": "2.0", "method": "notifications/initialized"}"#,
            r#"{"jsonrpc": "2.0", "id": null, "method": "ping"}"#,
            r#"{"jsonrpc": "2.0", "id": 1}"#,
            r#"{"jsonrpc": "2.0", "id": 1, "method": null}"#,
            r#"[1, 2]"#,
            r#""ping""#,
            r#"5"#,
        ] {
            assert_eq!(handle(&ctx, &j(text)?), None, "{text}");
        }
        Ok(())
    }

    #[test]
    fn tool_calls_report_errors_as_results_not_as_protocol_errors() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = demo(&dir, "", "no");
        let (error, text) = call(&ctx, "nope", "{}")?;
        assert!(error && text == "Unknown tool: nope");
        let (error, text) = call(&ctx, "list_accounts", "{}")?;
        assert!(!error && text.contains("Demo Gmail"));
        let (error, text) = call(&ctx, "list_accounts", "[1]")?;
        assert!(!error && text.contains("Demo Outlook"));
        let (error, text) = call(&ctx, "search_messages", "{}")?;
        assert!(error && text.contains("'query' is required"));
        let request = rpc(
            &ctx,
            r#"{"jsonrpc": "2.0", "id": 8, "method": "tools/call", "params": {"name": ["x"]}}"#,
        )?;
        assert!(format!("{request:?}").contains("Unknown tool"));
        let request = rpc(
            &ctx,
            &format!(
                r#"{{"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {{"name": "{}"}}}}"#,
                "n".repeat(500)
            ),
        )?;
        assert!(format!("{request:?}").len() < 600);
        Ok(())
    }

    #[test]
    fn a_crash_in_a_tool_becomes_a_short_error_and_the_server_goes_on() -> TestResult {
        let dir = TempDir::new()?;
        let mut settings = settings_at(&dir.path("config.json"));
        settings.fake = true;
        settings.fake_write = "FAKE-GMAIL".into();
        let options = FakeOptions {
            crash: true,
            approve: "yes".into(),
            ..FakeOptions::default()
        };
        let ctx = Context::demo(settings, options);
        let (error, text) = call(
            &ctx,
            "create_draft",
            r#"{"account": "Demo Gmail", "to": ["a@example.org"], "subject": "S", "body": "B"}"#,
        )?;
        assert!(error);
        assert_eq!(text, "Internal error (RuntimeError). Please retry.");
        let (error, _) = call(&ctx, "list_accounts", "{}")?;
        assert!(!error);
        Ok(())
    }

    fn explode(_: &Context, _: &Object) -> ToolResult<String> {
        let numbers: Vec<i32> = Vec::new();
        Ok(numbers[3].to_string())
    }

    #[test]
    fn a_panic_is_caught_and_leaks_nothing() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = demo(&dir, "", "no");
        let result = invoke(&ctx, "explode", explode, &Object::new());
        assert_eq!(
            result,
            text_result("Internal error (panic). Please retry.", true)
        );
        Ok(())
    }

    #[test]
    fn serve_answers_each_request_and_survives_garbage() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = demo(&dir, "", "no");
        let mut input = Vec::new();
        input.extend_from_slice(b"\n   \nnot json\n");
        input.extend_from_slice(&[0xff, 0xfe, b'\n']);
        input.extend_from_slice(b"{\"jsonrpc\": \"2.0\", \"id\": 1, \"method\": \"ping\"}\n");
        input.extend_from_slice(
            b"{\"jsonrpc\": \"2.0\", \"method\": \"notifications/initialized\"}\n",
        );
        input.extend_from_slice(b"[1]\n");
        input.extend_from_slice(b"{\"jsonrpc\": \"2.0\", \"id\": 2, \"method\": \"ping\"}");
        let mut output = Vec::new();
        serve(&ctx, &mut Cursor::new(input), &mut output)?;
        let text = String::from_utf8(output)?;
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            j(lines[0])?,
            j(r#"{"jsonrpc": "2.0", "id": 1, "result": {}}"#)?
        );
        assert_eq!(
            j(lines[1])?,
            j(r#"{"jsonrpc": "2.0", "id": 2, "result": {}}"#)?
        );
        assert!(text.is_ascii());
        Ok(())
    }

    #[test]
    fn responses_are_ascii_even_for_non_ascii_mail() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = demo(&dir, "", "no");
        let mut output = Vec::new();
        let request = "{\"jsonrpc\": \"2.0\", \"id\": 1, \"method\": \"tools/call\", \"params\": {\"name\": \"list_messages\", \"arguments\": {\"account\": \"Demo Gmail\", \"since_days\": 30}}}\n";
        serve(
            &ctx,
            &mut Cursor::new(request.as_bytes().to_vec()),
            &mut output,
        )?;
        let text = String::from_utf8(output)?;
        assert!(text.is_ascii());
        assert!(text.contains("best\\u00e4tigen"));
        Ok(())
    }

    #[test]
    fn the_line_reader_is_bounded_and_keeps_its_place() -> TestResult {
        let mut input = Cursor::new(b"short\n0123456789ABC\nnext\n".to_vec());
        assert_eq!(next_line(&mut input, 10)?, Line::Text(b"short".to_vec()));
        assert_eq!(next_line(&mut input, 10)?, Line::TooLong);
        assert_eq!(next_line(&mut input, 10)?, Line::Text(b"next".to_vec()));
        assert_eq!(next_line(&mut input, 10)?, Line::Eof);
        let mut exact = Cursor::new(b"0123456789\n".to_vec());
        assert_eq!(
            next_line(&mut exact, 10)?,
            Line::Text(b"0123456789".to_vec())
        );
        let mut unterminated = Cursor::new(b"0123456789ABC".to_vec());
        assert_eq!(next_line(&mut unterminated, 10)?, Line::TooLong);
        assert_eq!(next_line(&mut unterminated, 10)?, Line::Eof);
        let mut empty = Cursor::new(Vec::new());
        assert_eq!(next_line(&mut empty, 10)?, Line::Eof);
        let mut blank = Cursor::new(b"\n".to_vec());
        assert_eq!(next_line(&mut blank, 10)?, Line::Text(Vec::new()));
        Ok(())
    }

    #[test]
    fn a_huge_line_is_skipped_without_being_held_in_memory() -> TestResult {
        let dir = TempDir::new()?;
        let ctx = demo(&dir, "", "no");
        let mut input = vec![b'x'; MAX_LINE_BYTES + 5000];
        input.push(b'\n');
        input.extend_from_slice(b"{\"jsonrpc\": \"2.0\", \"id\": 1, \"method\": \"ping\"}\n");
        let mut output = Vec::new();
        serve(&ctx, &mut Cursor::new(input), &mut output)?;
        assert_eq!(String::from_utf8(output)?.lines().count(), 1);
        Ok(())
    }
}
