mod charset;
mod header;
mod html;
mod message;
mod mime;
mod outgoing;
mod reply;
#[cfg(test)]
mod tests;

use super::dialog::{approve_send, Verdict};
use super::Api;
use crate::account::{Account, MailAccount};
use crate::backend::{
    ComposeRequest, Mailbox, MessageBody, MessageId, MessageItem, MessageList, MessageQuery, Mode,
    Person, Preview,
};
use crate::base64;
use crate::error::{ToolError, ToolResult};
use crate::http::{self, encode_component, Method};
use crate::json::{Json, Object};
use crate::localtime;
use crate::mailerror::map_error;
use crate::osascript::Host;
use crate::provider::Provider;
use crate::text::{
    char_len, clean_str, is_hidden, one_line_str, plain_address, take_chars, MAX_ADDRESS,
};
use crate::texts::{APPROVAL_SECONDS, MAX_RECIPIENTS, MAX_SUBJECT};
use message::Message;
use outgoing::Outgoing;

const BASE: &str = "https://gmail.googleapis.com/gmail/v1/users/me";
const SYSTEM: [(&str, &str, bool); 7] = [
    ("INBOX", "INBOX", true),
    ("SENT", "Sent", true),
    ("DRAFT", "Drafts", true),
    ("SPAM", "Spam", true),
    ("TRASH", "Trash", true),
    ("STARRED", "Starred", false),
    ("IMPORTANT", "Important", false),
];
const ARCHIVE: &str = "All Mail";
const MAX_SCANNED_LABELS: usize = 1000;
const MAX_SCANNED_IDS: usize = 500;
const MAX_MAILBOXES: usize = 200;
const MAX_COUNTS: usize = 40;
const MAX_LABEL_NAME: usize = 120;
const MAX_LABEL_ID: usize = 128;
const MAX_LIMIT: i64 = 50;
const MAX_SINCE_DAYS: i64 = 3650;
const MAX_QUERY_CHARS: usize = 200;
const MAX_SHOWN_SUBJECT: usize = 500;
const MAX_SHOWN_FROM: usize = 300;
const MAX_DRAFT_RECIPIENTS: usize = 50;
const MAX_OWN_ADDRESSES: usize = 20;
const MAX_BODY_BYTES: usize = 256 * 1024;
const LIST_METADATA: &str =
    "format=metadata&metadataHeaders=From&metadataHeaders=Subject&metadataHeaders=Date";
const REPLY_METADATA: &str = concat!(
    "format=metadata&metadataHeaders=From&metadataHeaders=To&metadataHeaders=Cc",
    "&metadataHeaders=Reply-To&metadataHeaders=Subject&metadataHeaders=Message-ID",
    "&metadataHeaders=References&metadataHeaders=In-Reply-To"
);

fn url(path: &str) -> String {
    format!("{BASE}{path}")
}

fn unexpected() -> ToolError {
    ToolError::msg("Gmail gave an unexpected answer.")
}

fn mailbox_missing() -> ToolError {
    ToolError::msg("Mailbox not found. Use list_mailboxes to see the valid names.")
}

fn message_missing() -> ToolError {
    ToolError::msg(concat!(
        "Message not found. Use an id from list_messages or search_messages together ",
        "with the same account and mailbox."
    ))
}

fn unconfirmed(what: &str, place: &str) -> ToolError {
    ToolError::Uncertain(format!(
        concat!(
            "Gmail answered without confirming {}. It may or may not exist: ask the user to ",
            "check {} in Gmail before you try again."
        ),
        what, place
    ))
}

fn usable_address(address: &str) -> bool {
    address.len() <= MAX_ADDRESS
        && address.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty() && !domain.is_empty() && !domain.contains('@')
        })
        && address
            .chars()
            .all(|c| c.is_ascii_graphic() && !"<>(),;:\"\\[]".contains(c))
}

fn profile_address(api: &Api) -> ToolResult<String> {
    let profile = api.get(&url("/profile"))?;
    profile
        .as_object()
        .and_then(|object| object.get("emailAddress"))
        .and_then(Json::as_str)
        .map(str::trim)
        .filter(|address| usable_address(address))
        .map(str::to_string)
        .ok_or_else(unexpected)
}

pub(crate) fn account_info(api: &Api, account: &Account) -> ToolResult<MailAccount> {
    Ok(MailAccount {
        id: account.id.clone(),
        name: account.name.clone(),
        kind: Provider::Google.kind().to_string(),
        addresses: vec![profile_address(api)?],
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Label {
    id: String,
    shown: String,
    system: bool,
}

fn label_id(entry: &Object) -> Option<&str> {
    entry.get("id").and_then(Json::as_str).filter(|id| {
        !id.is_empty() && id.len() <= MAX_LABEL_ID && id.chars().all(|c| c.is_ascii_graphic())
    })
}

fn taken(listed: &[Label], lower: &str) -> bool {
    listed.iter().any(|label| {
        label.shown.to_lowercase() == lower || (label.system && label.id.to_lowercase() == lower)
    })
}

fn labels(json: &Json) -> Vec<Label> {
    let entries: Vec<&Object> = json
        .as_object()
        .and_then(|object| object.get("labels"))
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .take(MAX_SCANNED_LABELS)
        .filter_map(Json::as_object)
        .collect();
    let mut listed: Vec<Label> = Vec::new();
    for (id, shown, _) in SYSTEM {
        if entries.iter().any(|entry| label_id(entry) == Some(id)) {
            listed.push(Label {
                id: id.to_string(),
                shown: shown.to_string(),
                system: true,
            });
        }
    }
    let mut users: Vec<Label> = entries
        .iter()
        .filter(|entry| entry.get("type").and_then(Json::as_str) == Some("user"))
        .filter_map(|entry| {
            let id = label_id(entry)?;
            let name = one_line_str(entry.get("name").and_then(Json::as_str)?, MAX_LABEL_NAME);
            (!name.is_empty()).then(|| Label {
                id: id.to_string(),
                shown: name,
                system: false,
            })
        })
        .collect();
    users.sort_by_key(|label| label.shown.to_lowercase());
    for label in users {
        if !taken(&listed, &label.shown.to_lowercase()) {
            listed.push(label);
        }
    }
    listed.truncate(MAX_MAILBOXES);
    listed
}

fn resolve<'a>(listed: &'a [Label], wanted: &str) -> Option<&'a Label> {
    let wanted = wanted.trim().to_lowercase();
    if wanted.is_empty() {
        return None;
    }
    listed.iter().find(|label| {
        label.shown.to_lowercase() == wanted || (label.system && label.id.to_lowercase() == wanted)
    })
}

fn unread_of(json: &Json) -> Option<i64> {
    message::whole(json.as_object()?.get("messagesUnread")).filter(|count| *count >= 0)
}

pub(crate) fn mailboxes(api: &Api) -> ToolResult<Vec<Mailbox>> {
    let listed = labels(&api.get(&url("/labels"))?);
    if listed.is_empty() {
        return Err(unexpected());
    }
    let urls: Vec<String> = listed
        .iter()
        .take(MAX_COUNTS)
        .map(|label| url(&format!("/labels/{}", encode_component(&label.id))))
        .collect();
    let counts = api.get_many(&urls);
    Ok(listed
        .iter()
        .enumerate()
        .map(|(index, label)| Mailbox {
            path: label.shown.clone(),
            unread: counts
                .get(index)
                .and_then(|result| result.as_ref().ok())
                .and_then(unread_of)
                .map_or_else(|| "?".to_string(), |count| count.to_string()),
        })
        .collect())
}

fn mailbox(api: &Api, wanted: Option<&str>) -> ToolResult<Label> {
    let name = wanted
        .map(str::trim)
        .filter(|name| !name.is_empty() && !name.eq_ignore_ascii_case("inbox"));
    let Some(name) = name else {
        return Ok(Label {
            id: "INBOX".to_string(),
            shown: "INBOX".to_string(),
            system: true,
        });
    };
    let listed = labels(&api.get(&url("/labels"))?);
    resolve(&listed, name).cloned().ok_or_else(mailbox_missing)
}

fn words(text: &str) -> String {
    let spaced: String = take_chars(text, 2 * MAX_QUERY_CHARS)
        .chars()
        .map(|c| {
            if c.is_control() || is_hidden(c) || "\"(){}\\:".contains(c) {
                ' '
            } else {
                c
            }
        })
        .collect();
    let collapsed = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    take_chars(&collapsed, MAX_QUERY_CHARS)
        .trim_end()
        .to_string()
}

fn search(query: &MessageQuery) -> ToolResult<String> {
    let mut parts: Vec<String> = Vec::new();
    if query.since_days >= 1 {
        parts.push(format!(
            "newer_than:{}d",
            query.since_days.min(MAX_SINCE_DAYS)
        ));
    }
    if query.unread_only {
        parts.push("is:unread".to_string());
    }
    if let Some(text) = &query.query {
        let words = words(text);
        if words.is_empty() {
            return Err(ToolError::msg("The search text is empty."));
        }
        parts.push(match query.field.as_str() {
            "subject" => format!("subject:({words})"),
            "sender" => format!("from:({words})"),
            "both" => format!("{{subject:({words}) from:({words})}}"),
            _ => {
                return Err(ToolError::msg(
                    "Parameter 'field' must be subject, sender or both.",
                ))
            }
        });
    }
    Ok(parts.join(" "))
}

fn shown_date(message: &Message<'_>) -> String {
    message.seconds().map(localtime::minute).unwrap_or_default()
}

fn text_of(message: &Message<'_>, name: &str, limit: usize) -> String {
    header::text(message.header(name).unwrap_or_default(), limit)
}

fn item_of(id: &str, json: &Json) -> Option<MessageItem> {
    let message = Message::parse(json)?;
    Some(MessageItem {
        id: id.to_string(),
        subject: text_of(&message, "Subject", MAX_SHOWN_SUBJECT),
        from: text_of(&message, "From", MAX_SHOWN_FROM),
        ts: message.seconds().unwrap_or(0),
        date: shown_date(&message),
        read: !message.has_label("UNREAD"),
        flagged: message.has_label("STARRED"),
    })
}

fn listed_ids(list: &Object, limit: usize) -> Vec<String> {
    list.get("messages")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .take(MAX_SCANNED_IDS)
        .filter_map(Json::as_object)
        .filter_map(|entry| entry.get("id").and_then(Json::as_str))
        .filter_map(MessageId::parse)
        .map(|id| id.as_str().to_string())
        .take(limit)
        .collect()
}

fn count(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

pub(crate) fn messages(api: &Api, query: &MessageQuery) -> ToolResult<MessageList> {
    let search = search(query)?;
    let label = mailbox(api, query.mailbox.as_deref())?;
    let limit = query.limit.clamp(1, MAX_LIMIT);
    let limit_text = limit.to_string();
    let mut pairs: Vec<(&str, &str)> = vec![("maxResults", &limit_text), ("labelIds", &label.id)];
    if !search.is_empty() {
        pairs.push(("q", &search));
    }
    if matches!(label.id.as_str(), "SPAM" | "TRASH") {
        pairs.push(("includeSpamTrash", "true"));
    }
    let answer = api.get(&url(&format!("/messages?{}", http::query(&pairs))))?;
    let list = answer.as_object().ok_or_else(unexpected)?;
    let ids = listed_ids(list, usize::try_from(limit).unwrap_or(1));
    let more = list
        .get("nextPageToken")
        .and_then(Json::as_str)
        .is_some_and(|token| !token.is_empty());
    let estimate = message::whole(list.get("resultSizeEstimate")).unwrap_or(0);
    let total = if more {
        estimate.max(count(ids.len()))
    } else {
        count(ids.len())
    };
    let urls: Vec<String> = ids
        .iter()
        .map(|id| {
            url(&format!(
                "/messages/{}?{LIST_METADATA}",
                encode_component(id)
            ))
        })
        .collect();
    let mut items: Vec<MessageItem> = Vec::new();
    let mut failure: Option<ToolError> = None;
    for (id, result) in ids.iter().zip(api.get_many(&urls)) {
        match result.and_then(|json| item_of(id, &json).ok_or_else(unexpected)) {
            Ok(item) => items.push(item),
            Err(error) => {
                failure.get_or_insert(error);
            }
        }
    }
    if let (true, Some(error)) = (items.is_empty(), failure) {
        return Err(error);
    }
    items.sort_by_key(|item| std::cmp::Reverse(item.ts));
    Ok(MessageList {
        mailbox: Some(label.shown),
        total,
        items,
    })
}

fn location(message: &Message<'_>) -> String {
    SYSTEM
        .iter()
        .find(|(id, _, folder)| *folder && message.has_label(id))
        .map_or(ARCHIVE, |(_, shown, _)| shown)
        .to_string()
}

fn people(message: &Message<'_>, name: &str) -> Vec<Person> {
    message.header(name).map(header::people).unwrap_or_default()
}

fn body_of(api: &Api, id: &MessageId, message: &Message<'_>) -> ToolResult<mime::Body> {
    let first = mime::extract(message.payload(), &[]);
    if !first.text.trim().is_empty() || first.missing.is_empty() {
        return Ok(first);
    }
    let urls: Vec<String> = first
        .missing
        .iter()
        .map(|part| {
            url(&format!(
                "/messages/{}/attachments/{}",
                encode_component(id.as_str()),
                encode_component(part)
            ))
        })
        .collect();
    let mut fetched: Vec<(String, Vec<u8>)> = Vec::new();
    for (part, result) in first.missing.iter().zip(api.get_many(&urls)) {
        let answer = result?;
        let bytes = answer
            .as_object()
            .and_then(|object| object.get("data"))
            .and_then(Json::as_str)
            .and_then(|data| mime::decode(data, mime::MAX_TEXT_BYTES))
            .ok_or_else(unexpected)?;
        fetched.push((part.clone(), bytes));
    }
    Ok(mime::extract(message.payload(), &fetched))
}

pub(crate) fn read(api: &Api, id: &MessageId, max_chars: usize) -> ToolResult<MessageBody> {
    let wanted = url(&format!(
        "/messages/{}?format=full",
        encode_component(id.as_str())
    ));
    let answer = api.get_optional(&wanted)?.ok_or_else(message_missing)?;
    let message = Message::parse(&answer).ok_or_else(unexpected)?;
    let content = body_of(api, id, &message)?;
    let text = clean_str(&content.text, None);
    Ok(MessageBody {
        mailbox: location(&message),
        subject: text_of(&message, "Subject", MAX_SHOWN_SUBJECT),
        from: text_of(&message, "From", MAX_SHOWN_FROM),
        date: shown_date(&message),
        read: !message.has_label("UNREAD"),
        to: people(&message, "To"),
        cc: people(&message, "Cc"),
        attachments: content.attachments,
        body_chars: Some(char_len(&text)),
        body: take_chars(&text, max_chars),
    })
}

struct Letter {
    to: Vec<String>,
    cc: Vec<String>,
    subject: String,
    in_reply_to: Option<String>,
    references: Vec<String>,
    thread_id: Option<String>,
    reply: Option<(String, String)>,
}

fn nothing_done(reason: &str) -> ToolError {
    ToolError::msg(format!("{reason} NOTHING was sent or saved."))
}

fn check_request(request: &ComposeRequest) -> ToolResult<()> {
    if request
        .subject
        .chars()
        .any(|c| c.is_control() || is_hidden(c))
    {
        return Err(nothing_done(
            "The subject must be a single line without control or invisible characters.",
        ));
    }
    if char_len(&request.subject) > MAX_SUBJECT {
        return Err(nothing_done(&format!(
            "The subject is too long (max {MAX_SUBJECT} characters)."
        )));
    }
    if request
        .to
        .iter()
        .chain(&request.cc)
        .any(|address| address.contains(['\r', '\n']))
    {
        return Err(nothing_done("An address contains a line break."));
    }
    if request.body.len() > MAX_BODY_BYTES {
        return Err(nothing_done("The text is too long."));
    }
    if request.body.chars().any(is_hidden) {
        return Err(nothing_done(
            "The text contains control or invisible characters that the approval dialog cannot show.",
        ));
    }
    Ok(())
}

fn own_addresses(account: &Account, sender: &str) -> ToolResult<Vec<String>> {
    let mut own: Vec<String> = account
        .addresses
        .iter()
        .map(|address| address.trim().to_string())
        .filter(|address| !address.is_empty())
        .take(MAX_OWN_ADDRESSES)
        .collect();
    let known = own
        .iter()
        .any(|address| address.eq_ignore_ascii_case(sender));
    if !own.is_empty() && !known {
        return Err(ToolError::msg(map_error(
            "sender_mismatch",
            Some("compose"),
        )));
    }
    if !known {
        own.push(sender.to_string());
    }
    Ok(own)
}

fn letter(api: &Api, request: &ComposeRequest, own: &[String]) -> ToolResult<Letter> {
    let Some(spec) = &request.reply else {
        let (to, cc) = reply::distinct(&request.to, &request.cc);
        return Ok(Letter {
            to,
            cc,
            subject: request.subject.clone(),
            in_reply_to: None,
            references: Vec::new(),
            thread_id: None,
            reply: None,
        });
    };
    let wanted = url(&format!(
        "/messages/{}?{REPLY_METADATA}",
        encode_component(spec.id.as_str())
    ));
    let answer = api.get_optional(&wanted)?.ok_or_else(message_missing)?;
    let original = Message::parse(&answer).ok_or_else(unexpected)?;
    let plan = reply::plan(&reply::Original::of(&original), own, spec.all);
    Ok(Letter {
        to: plan.to,
        cc: plan.cc,
        subject: plan.subject,
        in_reply_to: plan.in_reply_to,
        references: plan.references,
        thread_id: plan.thread_id,
        reply: Some((plan.original_subject, plan.original_from)),
    })
}

fn check_recipients(mode: Mode, letter: &Letter) -> ToolResult<()> {
    if letter.to.is_empty() {
        return Err(ToolError::msg(map_error("no_recipients", Some("compose"))));
    }
    let total = letter.to.len() + letter.cc.len();
    if mode == Mode::Send && total > MAX_RECIPIENTS {
        return Err(ToolError::msg(map_error(
            "too_many_recipients",
            Some("compose"),
        )));
    }
    if mode == Mode::Draft && total > MAX_DRAFT_RECIPIENTS {
        return Err(ToolError::msg(format!(
            "The draft would have more than {MAX_DRAFT_RECIPIENTS} recipients, so NOTHING was saved."
        )));
    }
    if !letter
        .to
        .iter()
        .chain(&letter.cc)
        .all(|address| plain_address(address))
    {
        return Err(ToolError::msg(match mode {
            Mode::Send => map_error("recipient_invalid", Some("compose")),
            Mode::Draft => concat!(
                "A recipient address is not a plain ASCII address (at most 254 characters), ",
                "so NOTHING was saved."
            )
            .to_string(),
        }));
    }
    Ok(())
}

fn raw_message(letter: &Letter, body: &str) -> ToolResult<String> {
    Outgoing {
        to: &letter.to,
        cc: &letter.cc,
        subject: &letter.subject,
        in_reply_to: letter.in_reply_to.as_deref(),
        references: &letter.references,
        body,
    }
    .raw()
    .ok_or_else(|| nothing_done("The message could not be built safely."))
}

fn payload(raw: &str, thread_id: Option<&str>) -> Object {
    let mut object = Object::new();
    object.insert("raw", raw);
    if let Some(thread) = thread_id {
        object.insert("threadId", thread);
    }
    object
}

fn confirmed(answer: &Json) -> bool {
    answer
        .as_object()
        .and_then(|object| object.get("id"))
        .and_then(Json::as_str)
        .is_some_and(|id| !id.is_empty())
}

pub(crate) fn compose(
    api: &Api,
    host: &dyn Host,
    account: &Account,
    request: &ComposeRequest,
) -> ToolResult<Preview> {
    check_request(request)?;
    let sender = profile_address(api)?;
    let own = own_addresses(account, &sender)?;
    let letter = letter(api, request, &own)?;
    check_recipients(request.mode, &letter)?;
    let raw = raw_message(&letter, &request.body)?;
    let body = payload(
        &base64::encode_url(raw.as_bytes()),
        letter.thread_id.as_deref(),
    );
    let mut preview = Preview {
        from: sender,
        to: letter.to,
        cc: letter.cc,
        subject: letter.subject,
        body: request.body.clone(),
        reply: letter.reply,
        status: String::new(),
        saved: None,
    };
    if request.mode == Mode::Draft {
        let wrapped = Json::object(vec![("message", Json::Obj(body))]);
        let answer = api.send(Method::Post, &url("/drafts"), Some(&wrapped))?;
        if !confirmed(&answer) {
            return Err(unconfirmed("the draft", "Drafts"));
        }
        preview.status = "drafted".to_string();
        preview.saved = Some(true);
        return Ok(preview);
    }
    match approve_send(host, &preview, APPROVAL_SECONDS)? {
        Verdict::Declined => {
            preview.status = "declined".to_string();
            return Ok(preview);
        }
        Verdict::TimedOut => {
            preview.status = "timeout".to_string();
            return Ok(preview);
        }
        Verdict::Approved => {}
    }
    let answer = api.send(Method::Post, &url("/messages/send"), Some(&Json::Obj(body)))?;
    if !confirmed(&answer) {
        return Err(unconfirmed("the message", "Sent"));
    }
    preview.status = "sent".to_string();
    Ok(preview)
}
