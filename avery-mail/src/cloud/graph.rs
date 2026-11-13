mod draft;
mod folders;
mod html;
mod stamp;

use std::cmp::Reverse;
use std::collections::HashSet;

use self::folders::Place;
use super::Api;
use crate::account::{Account, MailAccount};
use crate::backend::{
    Attachment, ComposeRequest, Mailbox, MessageBody, MessageId, MessageItem, MessageList,
    MessageQuery, Person, Preview, MAX_MESSAGE_ID,
};
use crate::error::{ToolError, ToolResult};
use crate::http::{encode_component, query};
use crate::journal;
use crate::json::{Json, Object};
use crate::localtime;
use crate::osascript::Host;
use crate::source::{plain_address, Source, MAX_ADDRESS};
use crate::text::{char_len, clean_str, one_line_str, take_chars};

const BASE: &str = "https://graph.microsoft.com/v1.0/me";
const LINK_PREFIX: &str = "https://graph.microsoft.com/v1.0/";
const IDS: &str = "IdType=\"ImmutableId\"";
const IDS_AND_TEXT: &str = "IdType=\"ImmutableId\", outlook.body-content-type=\"text\"";
const PROFILE_FIELDS: &str = "mail,userPrincipalName,displayName";
const LIST_FIELDS: &str = "id,subject,from,receivedDateTime,isRead,flag";
const READ_FIELDS: &str = concat!(
    "id,subject,from,toRecipients,ccRecipients,receivedDateTime,isRead,body,",
    "hasAttachments,parentFolderId"
);
const PAGE_SIZE: &str = "100";
const PAGE_ITEMS: usize = 100;
const MAX_PAGES: usize = 5;
const MAX_LIMIT: usize = 100;
const MAX_DAYS: i64 = 3650;
const MAX_ID: usize = MAX_MESSAGE_ID;
const MAX_LINK: usize = 2048;
const MAX_NAME: usize = 120;
const MAX_TEXT: usize = 300;
const MAX_PEOPLE: usize = 100;
const MAX_ATTACHMENTS: usize = 50;
const NO_ADDRESS: &str = "Outlook.com did not report an e-mail address for this account.";
const NO_MAILBOX: &str = "Mailbox not found. Use list_mailboxes to see the valid names.";
const NO_MESSAGE: &str = concat!(
    "Message not found. Use an id from list_messages or search_messages together with the ",
    "same account and mailbox."
);
const FOREIGN_LINK: &str = concat!(
    "The provider answered with a continuation link that does not belong to Microsoft Graph, ",
    "so the listing was stopped."
);

fn unexpected() -> ToolError {
    ToolError::msg("The provider gave an unexpected answer.")
}

fn extended<'a>(api: &Api<'a>, extra: &[(&str, &str)]) -> Api<'a> {
    let mut copy = Api::new(api.transport, api.token.clone(), api.source);
    let own = api
        .headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()));
    for (name, value) in own.chain(extra.iter().copied()) {
        copy = copy.with_header(name, value);
    }
    copy
}

fn url(path: &str, pairs: &[(&str, &str)]) -> String {
    if pairs.is_empty() {
        format!("{BASE}{path}")
    } else {
        format!("{BASE}{path}?{}", query(pairs))
    }
}

fn message_path(id: &str) -> String {
    format!("/messages/{}", encode_component(id))
}

fn field<'a>(object: &'a Object, key: &str) -> &'a str {
    object.get(key).and_then(Json::as_str).unwrap_or_default()
}

fn page_items(json: &Json) -> ToolResult<&Vec<Json>> {
    json.as_object()
        .and_then(|page| page.get("value"))
        .and_then(Json::as_array)
        .ok_or_else(unexpected)
}

fn next_link(json: &Json) -> ToolResult<Option<String>> {
    let link = json
        .as_object()
        .and_then(|page| page.get("@odata.nextLink"));
    match link {
        None | Some(Json::Null) => Ok(None),
        Some(Json::Str(text))
            if text.starts_with(LINK_PREFIX)
                && text.len() <= MAX_LINK
                && text.chars().all(|c| c.is_ascii_graphic()) =>
        {
            Ok(Some(text.clone()))
        }
        Some(_) => Err(ToolError::msg(FOREIGN_LINK)),
    }
}

fn email_field<'a>(address: Option<&'a Object>, key: &str) -> &'a str {
    address
        .and_then(|entry| entry.get(key))
        .and_then(Json::as_str)
        .unwrap_or_default()
}

fn email(value: Option<&Json>) -> (&str, &str) {
    let address = value
        .and_then(Json::as_object)
        .and_then(|recipient| recipient.get("emailAddress"))
        .and_then(Json::as_object);
    (
        email_field(address, "name"),
        email_field(address, "address"),
    )
}

fn person_text(name: &str, address: &str) -> String {
    let name = one_line_str(name, MAX_NAME);
    let address = one_line_str(address, MAX_ADDRESS);
    match (name.is_empty() || name == address, address.is_empty()) {
        (true, _) => address,
        (false, true) => name,
        (false, false) => format!("{name} <{address}>"),
    }
}

fn people(value: Option<&Json>) -> Vec<Person> {
    value
        .and_then(Json::as_array)
        .map_or_else(Vec::new, |items| {
            items
                .iter()
                .take(MAX_PEOPLE)
                .map(|item| {
                    let (name, address) = email(Some(item));
                    Person {
                        name: one_line_str(name, MAX_NAME),
                        address: one_line_str(address, MAX_ADDRESS),
                    }
                })
                .collect()
        })
}

fn usable_address(profile: &Object) -> Option<String> {
    ["mail", "userPrincipalName"]
        .iter()
        .map(|key| field(profile, key).trim())
        .find(|text| plain_address(text))
        .map(str::to_string)
}

pub(crate) fn account_info(api: &Api, account: &Account) -> ToolResult<MailAccount> {
    let session = extended(api, &[("Prefer", IDS)]);
    let profile = session.get(&url("", &[("$select", PROFILE_FIELDS)]))?;
    let address = profile
        .as_object()
        .and_then(usable_address)
        .ok_or_else(|| ToolError::msg(NO_ADDRESS))?;
    Ok(MailAccount {
        id: account.id.clone(),
        name: account.name.clone(),
        kind: Source::Microsoft.kind().to_string(),
        addresses: vec![address],
    })
}

pub(crate) fn mailboxes(api: &Api) -> ToolResult<Vec<Mailbox>> {
    folders::listing(&extended(api, &[("Prefer", IDS)]))
}

fn item_of(entry: &Json) -> Option<MessageItem> {
    let message = entry.as_object()?;
    let id = MessageId::parse(field(message, "id"))?;
    let (name, address) = email(message.get("from"));
    let flagged = message
        .get("flag")
        .and_then(Json::as_object)
        .map(|flag| field(flag, "flagStatus"))
        == Some("flagged");
    Some(MessageItem {
        id: id.as_str().to_string(),
        subject: one_line_str(field(message, "subject"), MAX_TEXT),
        from: person_text(name, address),
        ts: stamp::parse(field(message, "receivedDateTime")).unwrap_or(0),
        date: String::new(),
        read: message
            .get("isRead")
            .and_then(Json::as_bool)
            .unwrap_or(false),
        flagged,
    })
}

fn wanted(item: &MessageItem, needle: Option<&str>, scope: &str) -> bool {
    let Some(needle) = needle else {
        return true;
    };
    let subject = item.subject.to_lowercase().contains(needle);
    let sender = item.from.to_lowercase().contains(needle);
    match scope {
        "subject" => subject,
        "sender" => sender,
        _ => subject || sender,
    }
}

fn dated(mut item: MessageItem) -> MessageItem {
    if item.ts != 0 {
        item.date = localtime::minute(item.ts);
    }
    item
}

fn listing_url(place: &Place, since: i64, unread_only: bool) -> String {
    let mut filter = format!("receivedDateTime ge {}", stamp::iso(since));
    if unread_only {
        filter.push_str(" and isRead eq false");
    }
    url(
        &format!("/mailFolders/{}/messages", encode_component(&place.id)),
        &[
            ("$select", LIST_FIELDS),
            ("$orderby", "receivedDateTime desc"),
            ("$top", PAGE_SIZE),
            ("$filter", &filter),
        ],
    )
}

fn scan(
    api: &Api,
    first: String,
    needle: Option<&str>,
    scope: &str,
    limit: usize,
) -> ToolResult<Vec<MessageItem>> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut found: Vec<MessageItem> = Vec::new();
    let mut next = Some(first);
    for _ in 0..MAX_PAGES {
        let Some(address) = next.take() else {
            break;
        };
        let Some(page) = api.get_optional(&address)? else {
            return Err(ToolError::msg(NO_MAILBOX));
        };
        for entry in page_items(&page)?.iter().take(PAGE_ITEMS) {
            let Some(item) = item_of(entry) else {
                continue;
            };
            if wanted(&item, needle, scope) && seen.insert(item.id.clone()) {
                found.push(item);
            }
        }
        if found.len() >= limit {
            break;
        }
        next = next_link(&page)?;
    }
    Ok(found)
}

fn list(api: &Api, request: &MessageQuery, now: i64) -> ToolResult<MessageList> {
    let session = extended(api, &[("Prefer", IDS)]);
    let place = folders::resolve(&session, request.mailbox.as_deref())?;
    let limit = usize::try_from(request.limit)
        .unwrap_or(0)
        .clamp(1, MAX_LIMIT);
    let days = request.since_days.clamp(1, MAX_DAYS);
    let first = listing_url(
        &place,
        now.saturating_sub(days.saturating_mul(86_400)),
        request.unread_only,
    );
    let needle = request
        .query
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_lowercase);
    let mut found = scan(&session, first, needle.as_deref(), &request.field, limit)?;
    found.sort_by_key(|item| Reverse(item.ts));
    Ok(MessageList {
        mailbox: Some(place.name),
        total: i64::try_from(found.len()).unwrap_or(i64::MAX),
        items: found.into_iter().take(limit).map(dated).collect(),
    })
}

pub(crate) fn messages(api: &Api, request: &MessageQuery) -> ToolResult<MessageList> {
    list(api, request, journal::now_seconds())
}

fn attachments(api: &Api, id: &str) -> ToolResult<Vec<Attachment>> {
    let address = url(
        &format!("{}/attachments", message_path(id)),
        &[("$select", "name,size")],
    );
    let page = api.get(&address)?;
    Ok(page_items(&page)?
        .iter()
        .filter_map(Json::as_object)
        .take(MAX_ATTACHMENTS)
        .map(|entry| Attachment {
            name: one_line_str(field(entry, "name"), MAX_NAME),
            size: match entry.get("size") {
                Some(Json::Int(bytes)) if *bytes >= 0 => Some(*bytes),
                _ => None,
            },
        })
        .collect())
}

fn folder_name(api: &Api, folder: &str) -> String {
    if folder.is_empty() || folder.len() > MAX_ID {
        return String::new();
    }
    let path = format!("/mailFolders/{}", encode_component(folder));
    match api.get_optional(&url(&path, &[("$select", "displayName")])) {
        Ok(Some(json)) => json
            .as_object()
            .map(|found| one_line_str(field(found, "displayName"), MAX_NAME))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn body_text(message: &Object) -> String {
    let Some(body) = message.get("body").and_then(Json::as_object) else {
        return String::new();
    };
    let content = field(body, "content");
    if field(body, "contentType").eq_ignore_ascii_case("html") {
        html::to_text(content)
    } else {
        content.to_string()
    }
}

pub(crate) fn read(api: &Api, id: &MessageId, max_chars: usize) -> ToolResult<MessageBody> {
    let session = extended(api, &[("Prefer", IDS_AND_TEXT)]);
    let address = url(&message_path(id.as_str()), &[("$select", READ_FIELDS)]);
    let Some(json) = session.get_optional(&address)? else {
        return Err(ToolError::msg(NO_MESSAGE));
    };
    let message = json.as_object().ok_or_else(unexpected)?;
    let (name, from) = email(message.get("from"));
    let listed = if message.get("hasAttachments") == Some(&Json::Bool(true)) {
        attachments(&session, id.as_str())?
    } else {
        Vec::new()
    };
    let text = clean_str(&body_text(message), None);
    let received = stamp::parse(field(message, "receivedDateTime"));
    Ok(MessageBody {
        mailbox: folder_name(&session, field(message, "parentFolderId")),
        subject: one_line_str(field(message, "subject"), MAX_TEXT),
        from: person_text(name, from),
        date: received.map_or_else(String::new, localtime::minute),
        read: message
            .get("isRead")
            .and_then(Json::as_bool)
            .unwrap_or(false),
        to: people(message.get("toRecipients")),
        cc: people(message.get("ccRecipients")),
        attachments: listed,
        body_chars: Some(char_len(&text)),
        body: take_chars(&text, max_chars),
    })
}

pub(crate) fn compose(
    api: &Api,
    host: &dyn Host,
    account: &Account,
    request: &ComposeRequest,
) -> ToolResult<Preview> {
    draft::compose(
        &extended(api, &[("Prefer", IDS_AND_TEXT)]),
        host,
        account,
        request,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::scripted::{ok, Scripted};
    use crate::http::{HttpError, Method, Request};
    use crate::json;
    use crate::testutil::{mid, TestResult};

    const NOW: i64 = 1_794_583_470;
    const LIST_URL: &str = concat!(
        "https://graph.microsoft.com/v1.0/me/mailFolders/inbox/messages",
        "?$select=id%2Csubject%2Cfrom%2CreceivedDateTime%2CisRead%2Cflag",
        "&$orderby=receivedDateTime%20desc&$top=100",
        "&$filter=receivedDateTime%20ge%202026-11-10T15%3A24%3A30Z"
    );
    const NEXT: &str =
        "https://graph.microsoft.com/v1.0/me/mailFolders/inbox/messages?%24skiptoken=";

    fn api(transport: &Scripted) -> Api<'_> {
        Api::new(transport, "TOKEN".to_string(), Source::Microsoft)
    }

    fn account() -> Account {
        Account {
            id: "microsoft:bob@outlook.example".to_string(),
            name: "Bob Outlook".to_string(),
            kind: "outlook".to_string(),
            addresses: vec!["bob@outlook.example".to_string()],
            label: "Bob Outlook".to_string(),
        }
    }

    fn header(request: &Request, name: &str) -> Option<String> {
        request
            .headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    }

    fn person(name: &str, address: &str) -> Json {
        Json::object(vec![(
            "emailAddress",
            Json::object(vec![
                ("name", Json::from(name)),
                ("address", Json::from(address)),
            ]),
        )])
    }

    fn row(id: &str, subject: &str, from: (&str, &str), received: &str, read: bool) -> Json {
        Json::object(vec![
            ("id", Json::from(id)),
            ("receivedDateTime", Json::from(received)),
            ("subject", Json::from(subject)),
            ("isRead", Json::Bool(read)),
            ("from", person(from.0, from.1)),
            (
                "flag",
                Json::object(vec![("flagStatus", Json::from("notFlagged"))]),
            ),
        ])
    }

    fn simple(id: &str, subject: &str) -> Json {
        row(
            id,
            subject,
            ("Alice Example", "alice@example.org"),
            "2026-11-13T14:09:25Z",
            true,
        )
    }

    fn page(rows: Vec<Json>, next: Option<&str>) -> String {
        let mut object = Object::new();
        object.insert(
            "@odata.context",
            "https://graph.microsoft.com/v1.0/$metadata#users('bb8775a4-4d8c-42cf-a1d4-4d58c2bb668f')/mailFolders('inbox')/messages(id,subject,from,receivedDateTime,isRead,flag)",
        );
        if let Some(link) = next {
            object.insert("@odata.nextLink", link);
        }
        object.insert("value", Json::Arr(rows));
        json::dumps(&Json::Obj(object), false)
    }

    fn request(text: Option<&str>, field: &str) -> MessageQuery {
        MessageQuery {
            mailbox: None,
            since_days: 3,
            unread_only: false,
            query: text.map(str::to_string),
            field: field.to_string(),
            limit: 20,
        }
    }

    fn ids(list: &MessageList) -> Vec<&str> {
        list.items.iter().map(|item| item.id.as_str()).collect()
    }

    fn failed(result: ToolResult<MessageList>) -> Result<ToolError, String> {
        match result {
            Err(error) => Ok(error),
            Ok(list) => Err(format!("expected an error, got {list:?}")),
        }
    }

    #[test]
    fn the_address_comes_from_mail_or_the_principal_name() -> TestResult {
        let transport = Scripted::new().json(
            Method::Get,
            "/v1.0/me?",
            200,
            r#"{"@odata.context": "https://graph.microsoft.com/v1.0/$metadata#users(mail,userPrincipalName,displayName)/$entity", "displayName": "Bob", "mail": "bob@outlook.example", "userPrincipalName": "bob.upn@live.example"}"#,
        );
        let info = account_info(&api(&transport), &account())?;
        assert_eq!(
            info,
            MailAccount {
                id: "microsoft:bob@outlook.example".to_string(),
                name: "Bob Outlook".to_string(),
                kind: "outlook".to_string(),
                addresses: vec!["bob@outlook.example".to_string()],
            }
        );
        let seen = transport.seen();
        let [call] = seen.as_slice() else {
            return Err("one request".into());
        };
        assert_eq!(call.method, Method::Get);
        assert_eq!(
            call.url,
            "https://graph.microsoft.com/v1.0/me?$select=mail%2CuserPrincipalName%2CdisplayName"
        );
        assert_eq!(header(call, "Prefer").as_deref(), Some(IDS));
        assert!(header(call, "Authorization").is_some_and(|value| value.ends_with(" TOKEN")));
        assert_eq!(call.body, None);
        Ok(())
    }

    #[test]
    fn a_missing_or_odd_mail_falls_back_to_the_principal_name() -> TestResult {
        for (body, wanted) in [
            (
                r#"{"mail": null, "userPrincipalName": "bob@live.example"}"#,
                Some("bob@live.example"),
            ),
            (
                r#"{"userPrincipalName": " bob@live.example "}"#,
                Some("bob@live.example"),
            ),
            (
                r#"{"mail": "not an address", "userPrincipalName": "bob@live.example"}"#,
                Some("bob@live.example"),
            ),
            (
                r#"{"mail": "bob@outlook.example\nBcc: x@y.example", "userPrincipalName": "bob@live.example"}"#,
                Some("bob@live.example"),
            ),
            (
                r#"{"mail": "bob@outlook.example", "userPrincipalName": "x#EXT#@y.example"}"#,
                Some("bob@outlook.example"),
            ),
            (
                r#"{"mail": null, "userPrincipalName": "x#EXT#@y.example"}"#,
                None,
            ),
            (r#"{"mail": 5, "userPrincipalName": ["a@b.example"]}"#, None),
            (r#"{}"#, None),
            (r#"[]"#, None),
            (r#"null"#, None),
        ] {
            let transport = Scripted::new().json(Method::Get, "/v1.0/me?", 200, body);
            let result = account_info(&api(&transport), &account());
            match wanted {
                Some(address) => {
                    assert_eq!(result?.addresses, [address.to_string()], "{body}");
                }
                None => assert_eq!(result, Err(ToolError::msg(NO_ADDRESS)), "{body}"),
            }
        }
        Ok(())
    }

    #[test]
    fn profile_errors_are_passed_on() {
        for (status, wanted) in [(401, "no longer accepts"), (403, "403"), (429, "429")] {
            let transport = Scripted::new().json(Method::Get, "/v1.0/me?", status, "{}");
            let result = account_info(&api(&transport), &account());
            assert!(
                matches!(&result, Err(ToolError::Msg(text)) if text.contains(wanted)),
                "{status}: {result:?}"
            );
        }
    }

    #[test]
    fn the_default_listing_asks_for_the_newest_inbox_messages_of_the_window() -> TestResult {
        let transport = Scripted::new().json(
            Method::Get,
            "$orderby",
            200,
            &page(
                vec![
                    row(
                        "AAkALgAAAAAAHYQDEapmEc2byACqAC-EWg0A_1=",
                        "Ihr Paket wird morgen geliefert",
                        ("DHL", "noreply@dhl.example"),
                        "2026-11-13T14:09:25Z",
                        false,
                    ),
                    row(
                        "AAkALgAAAAAAHYQDEapmEc2byACqAC-EWg0A_2=",
                        "Rechnung Oktober",
                        ("", "rechnung@stadtwerke.example"),
                        "2026-11-12T08:00:00.123Z",
                        true,
                    ),
                ],
                None,
            ),
        );
        let list = list(&api(&transport), &request(None, "both"), NOW)?;
        assert_eq!(list.mailbox.as_deref(), Some("Inbox"));
        assert_eq!(list.total, 2);
        assert_eq!(
            list.items,
            vec![
                MessageItem {
                    id: "AAkALgAAAAAAHYQDEapmEc2byACqAC-EWg0A_1=".to_string(),
                    subject: "Ihr Paket wird morgen geliefert".to_string(),
                    from: "DHL <noreply@dhl.example>".to_string(),
                    ts: 1_794_578_965,
                    date: localtime::minute(1_794_578_965),
                    read: false,
                    flagged: false,
                },
                MessageItem {
                    id: "AAkALgAAAAAAHYQDEapmEc2byACqAC-EWg0A_2=".to_string(),
                    subject: "Rechnung Oktober".to_string(),
                    from: "rechnung@stadtwerke.example".to_string(),
                    ts: 1_794_470_400,
                    date: localtime::minute(1_794_470_400),
                    read: true,
                    flagged: false,
                },
            ]
        );
        let seen = transport.seen();
        let [call] = seen.as_slice() else {
            return Err("one request".into());
        };
        assert_eq!(call.method, Method::Get);
        assert_eq!(call.url, LIST_URL);
        assert_eq!(header(call, "Prefer").as_deref(), Some(IDS));
        assert_eq!(call.body, None);
        assert_eq!(header(call, "Content-Type"), None);
        Ok(())
    }

    #[test]
    fn unread_messages_and_the_window_end_up_in_the_filter() -> TestResult {
        let transport = Scripted::new().json(Method::Get, "$orderby", 200, &page(vec![], None));
        let mut wanted = request(None, "both");
        wanted.unread_only = true;
        wanted.since_days = 90;
        let list = list(&api(&transport), &wanted, NOW)?;
        assert_eq!(list.total, 0);
        assert!(list.items.is_empty());
        let seen = transport.seen();
        let [call] = seen.as_slice() else {
            return Err("one request".into());
        };
        assert!(
            call.url.ends_with(concat!(
                "&$filter=receivedDateTime%20ge%202026-08-15T15%3A24%3A30Z",
                "%20and%20isRead%20eq%20false"
            )),
            "{}",
            call.url
        );
        Ok(())
    }

    #[test]
    fn days_and_limits_are_clamped_before_they_reach_the_url() -> TestResult {
        for (days, stamp) in [
            (0, "2026-11-12T15%3A24%3A30Z"),
            (-7, "2026-11-12T15%3A24%3A30Z"),
            (10_000_000, "2016-11-15T15%3A24%3A30Z"),
        ] {
            let transport = Scripted::new().json(Method::Get, "$orderby", 200, &page(vec![], None));
            let mut wanted = request(None, "both");
            wanted.since_days = days;
            list(&api(&transport), &wanted, NOW)?;
            let seen = transport.seen();
            let call = seen.first().ok_or("request")?;
            assert!(
                call.url.contains(&format!("%20ge%20{stamp}")),
                "{days}: {}",
                call.url
            );
        }
        let rows: Vec<Json> = (0..30)
            .map(|number| simple(&format!("m{number:02}"), "s"))
            .collect();
        for (limit, shown) in [(-5, 1), (0, 1), (1, 1), (7, 7), (30, 30), (1000, 30)] {
            let transport =
                Scripted::new().json(Method::Get, "$orderby", 200, &page(rows.clone(), None));
            let mut wanted = request(None, "both");
            wanted.limit = limit;
            assert_eq!(
                list(&api(&transport), &wanted, NOW)?.items.len(),
                shown,
                "{limit}"
            );
        }
        Ok(())
    }

    #[test]
    fn the_far_past_and_the_epoch_do_not_break_the_filter() -> TestResult {
        let transport = Scripted::new().json(Method::Get, "$orderby", 200, &page(vec![], None));
        list(&api(&transport), &request(None, "both"), 5)?;
        let seen = transport.seen();
        let call = seen.first().ok_or("request")?;
        assert!(
            call.url.contains("%20ge%201970-01-01T00%3A00%3A00Z"),
            "{}",
            call.url
        );
        Ok(())
    }

    #[test]
    fn newest_messages_come_first_whatever_the_server_order() -> TestResult {
        let transport = Scripted::new().json(
            Method::Get,
            "$orderby",
            200,
            &page(
                vec![
                    row(
                        "old",
                        "a",
                        ("A", "a@example.org"),
                        "2026-11-11T20:15:00Z",
                        true,
                    ),
                    row(
                        "new",
                        "b",
                        ("B", "b@example.org"),
                        "2026-11-13T14:09:25Z",
                        true,
                    ),
                    row(
                        "mid",
                        "c",
                        ("C", "c@example.org"),
                        "2026-11-12T08:00:00Z",
                        true,
                    ),
                    row("none", "d", ("D", "d@example.org"), "garbage", true),
                ],
                None,
            ),
        );
        let list = list(&api(&transport), &request(None, "both"), NOW)?;
        assert_eq!(ids(&list), ["new", "mid", "old", "none"]);
        let undated = list.items.last().ok_or("last")?;
        assert_eq!((undated.ts, undated.date.as_str()), (0, ""));
        Ok(())
    }

    #[test]
    fn flags_and_unknown_read_states_are_mapped() -> TestResult {
        let flagged = Json::object(vec![
            ("id", Json::from("f1")),
            ("receivedDateTime", Json::from("2026-11-13T14:09:25Z")),
            (
                "flag",
                Json::object(vec![("flagStatus", Json::from("flagged"))]),
            ),
        ]);
        let complete = Json::object(vec![
            ("id", Json::from("f2")),
            ("receivedDateTime", Json::from("2026-11-13T14:09:24Z")),
            ("isRead", Json::from("yes")),
            (
                "flag",
                Json::object(vec![("flagStatus", Json::from("complete"))]),
            ),
        ]);
        let odd = Json::object(vec![
            ("id", Json::from("f3")),
            ("receivedDateTime", Json::from("2026-11-13T14:09:23Z")),
            ("flag", Json::from("flagged")),
        ]);
        let transport = Scripted::new().json(
            Method::Get,
            "$orderby",
            200,
            &page(vec![flagged, complete, odd], None),
        );
        let list = list(&api(&transport), &request(None, "both"), NOW)?;
        let states: Vec<(bool, bool)> = list
            .items
            .iter()
            .map(|item| (item.read, item.flagged))
            .collect();
        assert_eq!(states, [(false, true), (false, false), (false, false)]);
        let first = list.items.first().ok_or("first")?;
        assert_eq!((first.subject.as_str(), first.from.as_str()), ("", ""));
        Ok(())
    }

    #[test]
    fn the_text_is_searched_locally_in_the_subject_the_sender_or_both() -> TestResult {
        let rows = || {
            vec![
                row(
                    "1",
                    "Rechnung Oktober",
                    ("Stadtwerke", "service@strom.example"),
                    "2026-11-13T14:09:25Z",
                    true,
                ),
                row(
                    "2",
                    "Hallo",
                    ("Anna Rechnung", "anna@example.org"),
                    "2026-11-13T14:09:24Z",
                    true,
                ),
                row(
                    "3",
                    "Newsletter",
                    ("Shop", "RECHNUNG@shop.example"),
                    "2026-11-13T14:09:23Z",
                    true,
                ),
                row(
                    "4",
                    "Unrelated",
                    ("Bob", "bob@example.org"),
                    "2026-11-13T14:09:22Z",
                    true,
                ),
            ]
        };
        for (text, field, wanted) in [
            (Some("rechnung"), "subject", vec!["1"]),
            (Some("RECHNUNG"), "sender", vec!["2", "3"]),
            (Some("Rechnung"), "both", vec!["1", "2", "3"]),
            (Some("rechnung"), "anything", vec!["1", "2", "3"]),
            (Some("  rechnung  "), "subject", vec!["1"]),
            (Some("strom.example"), "sender", vec!["1"]),
            (Some("<anna@example.org>"), "sender", vec!["2"]),
            (Some("Anna Rechnung <anna"), "sender", vec!["2"]),
            (Some("nothing like this"), "both", vec![]),
            (Some(""), "subject", vec!["1", "2", "3", "4"]),
            (Some("   "), "subject", vec!["1", "2", "3", "4"]),
            (None, "subject", vec!["1", "2", "3", "4"]),
        ] {
            let transport = Scripted::new().json(Method::Get, "$orderby", 200, &page(rows(), None));
            let list = list(&api(&transport), &request(text, field), NOW)?;
            assert_eq!(ids(&list), wanted, "{text:?} {field}");
            assert_eq!(list.total, i64::try_from(wanted.len())?, "{text:?} {field}");
        }
        Ok(())
    }

    #[test]
    fn unicode_text_is_matched_without_regard_to_case() -> TestResult {
        let transport = Scripted::new().json(
            Method::Get,
            "$orderby",
            200,
            &page(
                vec![row(
                    "u1",
                    "GR\u{dc}\u{df}E aus Z\u{dc}RICH",
                    ("J\u{fc}rgen", "j@example.org"),
                    "2026-11-13T14:09:25Z",
                    true,
                )],
                None,
            ),
        );
        let hits = |text: &str, field: &str| -> Result<usize, ToolError> {
            list(&api(&transport), &request(Some(text), field), NOW).map(|list| list.items.len())
        };
        assert_eq!(hits("z\u{fc}rich", "subject")?, 1);
        assert_eq!(hits("J\u{dc}RGEN", "sender")?, 1);
        assert_eq!(hits("j\u{fc}rgen", "subject")?, 0);
        Ok(())
    }

    #[test]
    fn paging_stops_as_soon_as_enough_matches_are_found() -> TestResult {
        let transport = Scripted::new()
            .json(
                Method::Get,
                "$orderby",
                200,
                &page(
                    vec![simple("1", "a"), simple("2", "b"), simple("3", "c")],
                    Some(&format!("{NEXT}P2")),
                ),
            )
            .json(
                Method::Get,
                "skiptoken=P2",
                200,
                &page(vec![simple("4", "d")], None),
            );
        let mut wanted = request(None, "both");
        wanted.limit = 2;
        let list = list(&api(&transport), &wanted, NOW)?;
        assert_eq!(ids(&list), ["1", "2"]);
        assert_eq!(list.total, 3);
        assert_eq!(transport.seen().len(), 1);
        Ok(())
    }

    #[test]
    fn further_pages_are_fetched_from_the_link_with_the_same_headers() -> TestResult {
        let transport = Scripted::new()
            .json(
                Method::Get,
                "$orderby",
                200,
                &page(vec![simple("1", "a")], Some(&format!("{NEXT}P2"))),
            )
            .json(
                Method::Get,
                "skiptoken=P2",
                200,
                &page(
                    vec![simple("1", "a"), simple("2", "b")],
                    Some(&format!("{NEXT}P3")),
                ),
            )
            .json(
                Method::Get,
                "skiptoken=P3",
                200,
                &page(vec![simple("3", "c")], None),
            );
        let mut wanted = request(None, "both");
        wanted.limit = 10;
        let list = list(&api(&transport), &wanted, NOW)?;
        assert_eq!(ids(&list), ["1", "2", "3"]);
        assert_eq!(list.total, 3);
        let seen = transport.seen();
        let urls: Vec<&str> = seen.iter().map(|call| call.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                LIST_URL.to_string(),
                format!("{NEXT}P2"),
                format!("{NEXT}P3")
            ]
        );
        for call in &seen {
            assert_eq!(call.method, Method::Get);
            assert_eq!(header(call, "Prefer").as_deref(), Some(IDS));
            assert!(header(call, "Authorization").is_some());
        }
        Ok(())
    }

    #[test]
    fn at_most_five_pages_are_scanned() -> TestResult {
        let mut transport = Scripted::new().json(
            Method::Get,
            "$orderby",
            200,
            &page(vec![simple("p1", "other")], Some(&format!("{NEXT}P2"))),
        );
        for number in 2..=7 {
            transport = transport.json(
                Method::Get,
                &format!("skiptoken=P{number}"),
                200,
                &page(
                    vec![simple(&format!("p{number}"), "other")],
                    Some(&format!("{NEXT}P{}", number + 1)),
                ),
            );
        }
        let list = list(&api(&transport), &request(Some("zzz"), "both"), NOW)?;
        assert_eq!(list.total, 0);
        let seen = transport.seen();
        let urls: Vec<&str> = seen.iter().map(|call| call.url.as_str()).collect();
        let mut wanted = vec![LIST_URL.to_string()];
        wanted.extend((2..=MAX_PAGES).map(|number| format!("{NEXT}P{number}")));
        assert_eq!(urls, wanted);
        assert_eq!(MAX_PAGES, 5);
        Ok(())
    }

    #[test]
    fn only_the_first_hundred_entries_of_a_page_are_looked_at() -> TestResult {
        let rows: Vec<Json> = (0..150)
            .map(|number| simple(&format!("m{number:03}"), "s"))
            .collect();
        let transport = Scripted::new().json(Method::Get, "$orderby", 200, &page(rows, None));
        let mut wanted = request(None, "both");
        wanted.limit = 50;
        let list = list(&api(&transport), &wanted, NOW)?;
        assert_eq!(list.total, 100);
        assert_eq!(list.items.len(), 50);
        Ok(())
    }

    #[test]
    fn links_that_do_not_belong_to_graph_are_refused_without_a_request() -> TestResult {
        let long = format!("{LINK_PREFIX}me/messages?{}", "a".repeat(MAX_LINK));
        for link in [
            "https://evil.example/v1.0/me/messages?x=1",
            "https://graph.microsoft.com.evil.example/v1.0/me/messages",
            "https://graph.microsoft.com@evil.example/v1.0/me/messages",
            "http://graph.microsoft.com/v1.0/me/messages",
            "https://graph.microsoft.com/beta/me/messages",
            "https://graph.microsoft.com/v1.0",
            "https://login.microsoftonline.com/v1.0/me",
            "/v1.0/me/messages?skip=1",
            "https://graph.microsoft.com/v1.0/me/messages?a b",
            "https://graph.microsoft.com/v1.0/me/messages?\u{e4}",
            "https://graph.microsoft.com/v1.0/me/messages?\n",
            "",
            long.as_str(),
        ] {
            let transport = Scripted::new().json(
                Method::Get,
                "$orderby",
                200,
                &page(vec![simple("1", "a")], Some(link)),
            );
            let mut wanted = request(Some("zzz"), "both");
            wanted.limit = 5;
            let error = failed(list(&api(&transport), &wanted, NOW))?;
            assert_eq!(error, ToolError::msg(FOREIGN_LINK), "{link:?}");
            assert_eq!(transport.seen().len(), 1, "{link:?}");
        }
        let numeric = r#"{"value": [], "@odata.nextLink": 5}"#;
        let transport = Scripted::new().json(Method::Get, "$orderby", 200, numeric);
        let error = failed(list(&api(&transport), &request(None, "both"), NOW))?;
        assert_eq!(error, ToolError::msg(FOREIGN_LINK));
        Ok(())
    }

    #[test]
    fn a_null_link_ends_the_listing_and_the_check_is_only_made_when_following() -> TestResult {
        let transport = Scripted::new().json(
            Method::Get,
            "$orderby",
            200,
            r#"{"value": [], "@odata.nextLink": null}"#,
        );
        assert_eq!(
            list(&api(&transport), &request(None, "both"), NOW)?.total,
            0
        );
        let transport = Scripted::new().json(
            Method::Get,
            "$orderby",
            200,
            &page(vec![simple("1", "a")], Some("https://evil.example/x")),
        );
        let mut wanted = request(None, "both");
        wanted.limit = 1;
        assert_eq!(list(&api(&transport), &wanted, NOW)?.items.len(), 1);
        Ok(())
    }

    #[test]
    fn entries_without_a_usable_id_or_object_shape_are_skipped() -> TestResult {
        let transport = Scripted::new().json(
            Method::Get,
            "$orderby",
            200,
            &page(
                vec![
                    simple("good", "kept"),
                    Json::from("text"),
                    Json::Arr(vec![]),
                    Json::Null,
                    Json::object(vec![("subject", Json::from("no id"))]),
                    simple("", "empty id"),
                    simple("with space", "bad id"),
                    simple("semi;colon", "bad id"),
                    simple(&"a".repeat(600), "long id"),
                ],
                None,
            ),
        );
        let list = list(&api(&transport), &request(None, "both"), NOW)?;
        assert_eq!(ids(&list), ["good"]);
        Ok(())
    }

    #[test]
    fn a_page_without_a_value_array_is_an_unexpected_answer() -> TestResult {
        for body in [
            r#"{}"#,
            r#"{"value": {}}"#,
            r#"{"value": "x"}"#,
            r#"[]"#,
            r#"null"#,
            "",
        ] {
            let transport = Scripted::new().json(Method::Get, "$orderby", 200, body);
            let error = failed(list(&api(&transport), &request(None, "both"), NOW))?;
            assert_eq!(error, unexpected(), "{body:?}");
        }
        Ok(())
    }

    #[test]
    fn hostile_text_is_cleaned_and_bounded() -> TestResult {
        let hidden = "Pay\u{200b}ment\u{202e} due\u{feff}";
        let subject = format!("{hidden}\r\nBcc: evil@example.org\n{}", "x".repeat(1000));
        let sender = format!("Mallory\n<ceo@bank.example>{}", "y".repeat(500));
        let transport = Scripted::new().json(
            Method::Get,
            "$orderby",
            200,
            &page(
                vec![row(
                    "h1",
                    &subject,
                    (&sender, "mallory@evil.example"),
                    "2026-11-13T14:09:25Z",
                    true,
                )],
                None,
            ),
        );
        let list = list(&api(&transport), &request(None, "both"), NOW)?;
        let item = list.items.first().ok_or("item")?;
        assert!(item
            .subject
            .starts_with("Payment due Bcc: evil@example.org xxx"));
        assert!(item.subject.ends_with(" [...]"));
        assert_eq!(item.subject.chars().count(), MAX_TEXT + 6);
        assert!(!item.subject.contains('\n'));
        assert!(item.from.starts_with("Mallory <ceo@bank.example>yyy"));
        assert!(item.from.contains(" [...] <mallory@evil.example>"));
        assert!(!item.from.contains('\n'));
        Ok(())
    }

    #[test]
    fn a_search_in_a_named_folder_resolves_the_folder_first() -> TestResult {
        let transport = Scripted::new()
            .json(Method::Get, "/mailFolders/AAMkProjects%3D/messages", 200, &page(vec![simple("p1", "Plan")], None))
            .json(
                Method::Get,
                "/mailFolders/AAMkInbox%3D/childFolders",
                200,
                r#"{"value": [{"id": "AAMkProjects=", "displayName": "Projects", "unreadItemCount": 2, "childFolderCount": 0}]}"#,
            )
            .json(
                Method::Get,
                "/mailFolders?",
                200,
                r#"{"value": [{"id": "AAMkInbox=", "displayName": "Inbox", "unreadItemCount": 5, "childFolderCount": 1}]}"#,
            );
        let mut wanted = request(None, "both");
        wanted.mailbox = Some("inbox/PROJECTS".to_string());
        let list = list(&api(&transport), &wanted, NOW)?;
        assert_eq!(list.mailbox.as_deref(), Some("Inbox/Projects"));
        assert_eq!(ids(&list), ["p1"]);
        let seen = transport.seen();
        let last = seen.last().ok_or("last")?;
        assert!(last.url.starts_with(
            "https://graph.microsoft.com/v1.0/me/mailFolders/AAMkProjects%3D/messages?$select="
        ));
        for call in &seen {
            assert_eq!(header(call, "Prefer").as_deref(), Some(IDS));
        }
        Ok(())
    }

    #[test]
    fn well_known_names_need_no_folder_lookup() -> TestResult {
        for (name, folder, shown) in [
            ("Sent", "sentitems", "Sent Items"),
            ("DRAFTS", "drafts", "Drafts"),
            (" trash ", "deleteditems", "Deleted Items"),
            ("Spam", "junkemail", "Junk Email"),
            ("archive", "archive", "Archive"),
            ("INBOX", "inbox", "Inbox"),
        ] {
            let transport = Scripted::new().json(
                Method::Get,
                &format!("/mailFolders/{folder}/messages"),
                200,
                &page(vec![], None),
            );
            let mut wanted = request(None, "both");
            wanted.mailbox = Some(name.to_string());
            let list = list(&api(&transport), &wanted, NOW)?;
            assert_eq!(list.mailbox.as_deref(), Some(shown), "{name}");
            assert_eq!(transport.seen().len(), 1, "{name}");
        }
        Ok(())
    }

    #[test]
    fn an_unknown_mailbox_is_reported_with_the_standard_text() -> TestResult {
        let transport = Scripted::new().json(
            Method::Get,
            "/mailFolders?",
            200,
            r#"{"value": [{"id": "AAMkInbox=", "displayName": "Inbox", "childFolderCount": 0}]}"#,
        );
        let mut wanted = request(None, "both");
        wanted.mailbox = Some("Nowhere".to_string());
        let error = failed(list(&api(&transport), &wanted, NOW))?;
        assert_eq!(
            error,
            ToolError::msg("Mailbox not found. Use list_mailboxes to see the valid names.")
        );
        Ok(())
    }

    #[test]
    fn a_well_known_folder_that_does_not_exist_is_an_unknown_mailbox() -> TestResult {
        let transport = Scripted::new().json(
            Method::Get,
            "/mailFolders/archive/messages",
            404,
            r#"{"error": {"code": "ErrorFolderNotFound", "message": "The specified folder could not be found in the store."}}"#,
        );
        let mut wanted = request(None, "both");
        wanted.mailbox = Some("archive".to_string());
        let error = failed(list(&api(&transport), &wanted, NOW))?;
        assert_eq!(error, ToolError::msg(NO_MAILBOX));
        assert_eq!(transport.seen().len(), 1);
        Ok(())
    }

    #[test]
    fn provider_failures_while_listing_are_model_friendly() -> TestResult {
        for (status, wanted) in [
            (401, "no longer accepts"),
            (403, "403"),
            (500, "temporary problem"),
        ] {
            let transport = Scripted::new().json(Method::Get, "$orderby", status, "{}");
            let error = failed(list(&api(&transport), &request(None, "both"), NOW))?;
            assert!(
                matches!(&error, ToolError::Msg(text) if text.contains(wanted)),
                "{error}"
            );
        }
        let transport =
            Scripted::new().reply(Method::Get, "$orderby", vec![Err(HttpError::TimedOut)]);
        let error = failed(list(&api(&transport), &request(None, "both"), NOW))?;
        assert!(error.message().contains("did not answer in time"));
        let transport = Scripted::new().reply(Method::Get, "$orderby", vec![ok("<html>")]);
        assert!(list(&api(&transport), &request(None, "both"), NOW).is_err());
        Ok(())
    }

    #[test]
    fn the_public_listing_uses_the_clock() -> TestResult {
        let transport = Scripted::new().json(Method::Get, "$orderby", 200, &page(vec![], None));
        let list = messages(&api(&transport), &request(None, "both"))?;
        assert_eq!(list.total, 0);
        let seen = transport.seen();
        let call = seen.first().ok_or("request")?;
        let stamp_text = call
            .url
            .split("%20ge%20")
            .nth(1)
            .ok_or("filter")?
            .replace("%3A", ":");
        let cutoff = stamp::parse(&stamp_text).ok_or("cutoff")?;
        let expected = journal::now_seconds() - 3 * 86_400;
        assert!((cutoff - expected).abs() < 60, "{cutoff} {expected}");
        Ok(())
    }

    fn message(extra: Vec<(&str, Json)>) -> String {
        let mut object = Object::new();
        object.insert("@odata.context", "https://graph.microsoft.com/v1.0/$metadata#users('bb8775a4-4d8c-42cf-a1d4-4d58c2bb668f')/messages(id,subject)/$entity");
        object.insert(
            "@odata.etag",
            "W/\"CQAAABYAAADHcgC8Hl9tRZ/hc1wEUs1TAAAwR4Hg\"",
        );
        object.insert("id", "AAMkMSG=");
        object.insert("receivedDateTime", "2026-11-13T14:09:25Z");
        object.insert("hasAttachments", false);
        object.insert("subject", "Rechnung Oktober");
        object.insert("isRead", false);
        object.insert("parentFolderId", "AAMkFOLDER=");
        object.insert(
            "body",
            Json::object(vec![
                ("contentType", Json::from("text")),
                (
                    "content",
                    Json::from("Guten Tag,\r\n\r\nIhre Rechnung liegt bei.\r\n"),
                ),
            ]),
        );
        object.insert("from", person("Stadtwerke", "rechnung@stadtwerke.example"));
        object.insert(
            "toRecipients",
            Json::Arr(vec![person("Bob", "bob@outlook.example")]),
        );
        object.insert("ccRecipients", Json::Arr(vec![]));
        for (key, value) in extra {
            object.insert(key, value);
        }
        json::dumps(&Json::Obj(object), false)
    }

    const MESSAGE_URL: &str = concat!(
        "https://graph.microsoft.com/v1.0/me/messages/AAMkMSG%3D",
        "?$select=id%2Csubject%2Cfrom%2CtoRecipients%2CccRecipients%2CreceivedDateTime",
        "%2CisRead%2Cbody%2ChasAttachments%2CparentFolderId"
    );

    fn reading(body: &str) -> Scripted {
        Scripted::new()
            .json(
                Method::Get,
                "/mailFolders/AAMkFOLDER%3D",
                200,
                r#"{"id": "AAMkFOLDER=", "displayName": "Inbox"}"#,
            )
            .json(Method::Get, "/messages/AAMkMSG%3D?", 200, body)
    }

    #[test]
    fn a_message_is_read_with_text_bodies_and_the_folder_name() -> TestResult {
        let transport = reading(&message(vec![]));
        let body = read(&api(&transport), &mid("AAMkMSG=")?, 6000)?;
        assert_eq!(
            body,
            MessageBody {
                mailbox: "Inbox".to_string(),
                subject: "Rechnung Oktober".to_string(),
                from: "Stadtwerke <rechnung@stadtwerke.example>".to_string(),
                date: localtime::minute(1_794_578_965),
                read: false,
                to: vec![Person {
                    name: "Bob".to_string(),
                    address: "bob@outlook.example".to_string()
                }],
                cc: vec![],
                attachments: vec![],
                body_chars: Some(36),
                body: "Guten Tag,\n\nIhre Rechnung liegt bei.".to_string(),
            }
        );
        let seen = transport.seen();
        let [first, second] = seen.as_slice() else {
            return Err("two requests".into());
        };
        assert_eq!(
            (first.method, first.url.as_str()),
            (Method::Get, MESSAGE_URL)
        );
        assert_eq!(
            (second.method, second.url.as_str()),
            (
                Method::Get,
                "https://graph.microsoft.com/v1.0/me/mailFolders/AAMkFOLDER%3D?$select=displayName"
            )
        );
        for call in &seen {
            assert_eq!(header(call, "Prefer").as_deref(), Some(IDS_AND_TEXT));
            assert_eq!(call.body, None);
            assert_eq!(header(call, "Content-Type"), None);
        }
        Ok(())
    }

    #[test]
    fn long_bodies_are_cut_and_the_total_is_reported() -> TestResult {
        let long = "word ".repeat(1000);
        let transport = reading(&message(vec![(
            "body",
            Json::object(vec![
                ("contentType", Json::from("text")),
                ("content", Json::from(long.as_str())),
            ]),
        )]));
        let body = read(&api(&transport), &mid("AAMkMSG=")?, 500)?;
        assert_eq!(body.body_chars, Some(4999));
        assert_eq!(body.body.chars().count(), 500);
        assert!(long.starts_with(&body.body));
        let transport = reading(&message(vec![]));
        let body = read(&api(&transport), &mid("AAMkMSG=")?, 5)?;
        assert_eq!(body.body, "Guten");
        assert_eq!(body.body_chars, Some(36));
        Ok(())
    }

    #[test]
    fn html_bodies_are_converted_when_the_server_ignores_the_text_preference() -> TestResult {
        let html = concat!(
            "<html><head><style>p {color: red}</style></head><body>",
            "<div>Hi Bob,</div><div><br></div><div>the invoice: 84,20&nbsp;&euro;<script>alert(1)</script></div>",
            "<p>Best,<br>Alice</p></body></html>"
        );
        let transport = reading(&message(vec![(
            "body",
            Json::object(vec![
                ("contentType", Json::from("html")),
                ("content", Json::from(html)),
            ]),
        )]));
        let body = read(&api(&transport), &mid("AAMkMSG=")?, 6000)?;
        assert_eq!(
            body.body,
            "Hi Bob,\n\nthe invoice: 84,20 \u{20ac}\n\nBest,\nAlice"
        );
        assert_eq!(body.body_chars, Some(body.body.chars().count()));
        let upper = reading(&message(vec![(
            "body",
            Json::object(vec![
                ("contentType", Json::from("HTML")),
                ("content", Json::from("<b>x</b>")),
            ]),
        )]));
        assert_eq!(read(&api(&upper), &mid("AAMkMSG=")?, 6000)?.body, "x");
        let text_with_tags = reading(&message(vec![(
            "body",
            Json::object(vec![
                ("contentType", Json::from("text")),
                ("content", Json::from("a <b>b</b> &amp; c")),
            ]),
        )]));
        assert_eq!(
            read(&api(&text_with_tags), &mid("AAMkMSG=")?, 6000)?.body,
            "a <b>b</b> &amp; c"
        );
        Ok(())
    }

    #[test]
    fn missing_bodies_and_odd_shapes_give_empty_text() -> TestResult {
        for extra in [
            vec![("body", Json::Null)],
            vec![("body", Json::from("text"))],
            vec![(
                "body",
                Json::object(vec![("contentType", Json::from("text"))]),
            )],
            vec![("body", Json::object(vec![("content", Json::Int(5))]))],
        ] {
            let transport = reading(&message(extra));
            let body = read(&api(&transport), &mid("AAMkMSG=")?, 6000)?;
            assert_eq!((body.body.as_str(), body.body_chars), ("", Some(0)));
        }
        Ok(())
    }

    #[test]
    fn body_text_is_cleaned_like_all_other_untrusted_text() -> TestResult {
        let hostile =
            "Hello\u{200b}\u{202e}\r\n\r\n\r\n\r\nsecret\u{e0041}\u{e0042}   \r\nend\u{feff}";
        let transport = reading(&message(vec![(
            "body",
            Json::object(vec![
                ("contentType", Json::from("text")),
                ("content", Json::from(hostile)),
            ]),
        )]));
        let body = read(&api(&transport), &mid("AAMkMSG=")?, 6000)?;
        assert_eq!(body.body, "Hello\n\nsecret\nend");
        assert_eq!(body.body_chars, Some(17));
        Ok(())
    }

    #[test]
    fn attachments_are_listed_with_their_sizes() -> TestResult {
        let transport = reading(&message(vec![("hasAttachments", Json::Bool(true))])).json(
            Method::Get,
            "/messages/AAMkMSG%3D/attachments",
            200,
            r##"{"value": [
                {"@odata.type": "#microsoft.graph.fileAttachment", "id": "AAMkATT1=", "name": "rechnung-oktober.pdf", "size": 48213},
                {"@odata.type": "#microsoft.graph.itemAttachment", "id": "AAMkATT2=", "name": "Forwarded\nmail", "size": 1024},
                {"@odata.type": "#microsoft.graph.referenceAttachment", "id": "AAMkATT3=", "name": "link.url", "size": -1},
                {"id": "AAMkATT4=", "size": "big"},
                "junk",
                {"name": "photo.jpg", "size": 5.5}
            ]}"##,
        );
        let body = read(&api(&transport), &mid("AAMkMSG=")?, 6000)?;
        let listed: Vec<(&str, Option<i64>)> = body
            .attachments
            .iter()
            .map(|file| (file.name.as_str(), file.size))
            .collect();
        assert_eq!(
            listed,
            [
                ("rechnung-oktober.pdf", Some(48213)),
                ("Forwarded mail", Some(1024)),
                ("link.url", None),
                ("", None),
                ("photo.jpg", None),
            ]
        );
        let seen = transport.seen();
        let attachments_call = seen
            .iter()
            .find(|call| call.url.contains("/attachments"))
            .ok_or("attachments call")?;
        assert_eq!(
            attachments_call.url,
            "https://graph.microsoft.com/v1.0/me/messages/AAMkMSG%3D/attachments?$select=name%2Csize"
        );
        assert_eq!(
            header(attachments_call, "Prefer").as_deref(),
            Some(IDS_AND_TEXT)
        );
        Ok(())
    }

    #[test]
    fn only_fifty_attachments_are_shown_and_none_are_asked_for_without_the_flag() -> TestResult {
        let many: Vec<String> = (0..80)
            .map(|number| format!(r#"{{"name": "f{number:02}.txt", "size": {number}}}"#))
            .collect();
        let transport = reading(&message(vec![("hasAttachments", Json::Bool(true))])).json(
            Method::Get,
            "/attachments",
            200,
            &format!(r#"{{"value": [{}]}}"#, many.join(", ")),
        );
        let body = read(&api(&transport), &mid("AAMkMSG=")?, 6000)?;
        assert_eq!(body.attachments.len(), MAX_ATTACHMENTS);
        assert_eq!(
            body.attachments.last().map(|file| file.name.as_str()),
            Some("f49.txt")
        );
        let transport = reading(&message(vec![]));
        read(&api(&transport), &mid("AAMkMSG=")?, 6000)?;
        assert!(transport
            .seen()
            .iter()
            .all(|call| !call.url.contains("/attachments")));
        let transport = reading(&message(vec![("hasAttachments", Json::from("true"))]));
        read(&api(&transport), &mid("AAMkMSG=")?, 6000)?;
        assert!(transport
            .seen()
            .iter()
            .all(|call| !call.url.contains("/attachments")));
        Ok(())
    }

    #[test]
    fn a_failing_attachment_list_fails_the_read_instead_of_hiding_files() -> TestResult {
        for (status, body) in [
            (500, "{}"),
            (403, "{}"),
            (200, r#"{"value": 5}"#),
            (200, "[]"),
        ] {
            let transport = reading(&message(vec![("hasAttachments", Json::Bool(true))])).json(
                Method::Get,
                "/attachments",
                status,
                body,
            );
            assert!(
                read(&api(&transport), &mid("AAMkMSG=")?, 6000).is_err(),
                "{status} {body}"
            );
        }
        Ok(())
    }

    #[test]
    fn the_folder_name_is_best_effort() -> TestResult {
        for status in [404, 403, 500, 429] {
            let transport = Scripted::new()
                .json(Method::Get, "/mailFolders/", status, "{}")
                .json(Method::Get, "/messages/AAMkMSG%3D?", 200, &message(vec![]));
            assert_eq!(
                read(&api(&transport), &mid("AAMkMSG=")?, 6000)?.mailbox,
                "",
                "{status}"
            );
        }
        let transport = Scripted::new()
            .reply(Method::Get, "/mailFolders/", vec![Err(HttpError::TimedOut)])
            .json(Method::Get, "/messages/AAMkMSG%3D?", 200, &message(vec![]));
        assert_eq!(read(&api(&transport), &mid("AAMkMSG=")?, 6000)?.mailbox, "");
        for extra in [
            vec![("parentFolderId", Json::Null)],
            vec![("parentFolderId", Json::from(""))],
            vec![("parentFolderId", Json::from("x".repeat(MAX_ID + 1)))],
            vec![("parentFolderId", Json::Int(5))],
        ] {
            let transport =
                Scripted::new().json(Method::Get, "/messages/AAMkMSG%3D?", 200, &message(extra));
            assert_eq!(read(&api(&transport), &mid("AAMkMSG=")?, 6000)?.mailbox, "");
            assert_eq!(transport.seen().len(), 1);
        }
        let transport = Scripted::new()
            .json(
                Method::Get,
                "/mailFolders/",
                200,
                r#"{"displayName": "Posteingang\n\u202eX"}"#,
            )
            .json(Method::Get, "/messages/AAMkMSG%3D?", 200, &message(vec![]));
        assert_eq!(
            read(&api(&transport), &mid("AAMkMSG=")?, 6000)?.mailbox,
            "Posteingang X"
        );
        let transport = Scripted::new()
            .json(Method::Get, "/mailFolders/", 200, "[]")
            .json(Method::Get, "/messages/AAMkMSG%3D?", 200, &message(vec![]));
        assert_eq!(read(&api(&transport), &mid("AAMkMSG=")?, 6000)?.mailbox, "");
        Ok(())
    }

    #[test]
    fn an_unknown_message_is_reported_with_the_standard_text() -> TestResult {
        let transport = Scripted::new().json(
            Method::Get,
            "/messages/",
            404,
            r#"{"error": {"code": "ErrorItemNotFound", "message": "The specified object was not found in the store."}}"#,
        );
        let result = read(&api(&transport), &mid("AAMkGONE=")?, 6000);
        assert_eq!(
            result,
            Err(ToolError::msg(
                "Message not found. Use an id from list_messages or search_messages together with the same account and mailbox."
            ))
        );
        assert_eq!(transport.seen().len(), 1);
        Ok(())
    }

    #[test]
    fn other_read_failures_are_passed_on() -> TestResult {
        for (status, wanted) in [
            (401, "no longer accepts"),
            (403, "403"),
            (400, "Id is malformed"),
        ] {
            let transport = Scripted::new().json(
                Method::Get,
                "/messages/",
                status,
                r#"{"error": {"code": "ErrorInvalidIdMalformed", "message": "Id is malformed."}}"#,
            );
            let result = read(&api(&transport), &mid("101")?, 6000);
            assert!(
                matches!(&result, Err(ToolError::Msg(text)) if text.contains(wanted)),
                "{status}: {result:?}"
            );
        }
        for body in ["[]", "null", "\"x\"", "5"] {
            let transport = Scripted::new().json(Method::Get, "/messages/", 200, body);
            assert_eq!(
                read(&api(&transport), &mid("AAMkMSG=")?, 6000),
                Err(unexpected()),
                "{body}"
            );
        }
        Ok(())
    }

    #[test]
    fn ids_are_percent_encoded_in_the_path() -> TestResult {
        for (id, encoded) in [
            ("a+b/c=d.e-f_g", "a%2Bb%2Fc%3Dd.e-f_g"),
            ("../../me", "..%2F..%2Fme"),
            ("a//b", "a%2F%2Fb"),
        ] {
            let transport = Scripted::new().json(Method::Get, "/messages/", 200, &message(vec![]));
            read(&api(&transport), &mid(id)?, 6000)?;
            let seen = transport.seen();
            let call = seen.first().ok_or("request")?;
            let wanted = format!("https://graph.microsoft.com/v1.0/me/messages/{encoded}?$select=");
            assert!(call.url.starts_with(&wanted), "{}", call.url);
        }
        Ok(())
    }

    #[test]
    fn recipients_and_header_texts_are_cleaned_and_bounded() -> TestResult {
        let crowd: Vec<Json> = (0..150)
            .map(|number| person(&format!("P{number}"), &format!("p{number}@example.org")))
            .collect();
        let transport = reading(&message(vec![
            (
                "subject",
                Json::from(format!("Re:\r\n{}\u{200b}", "s".repeat(500))),
            ),
            (
                "from",
                person("Eve\n<boss@corp.example>", "eve@evil.example"),
            ),
            ("toRecipients", Json::Arr(crowd)),
            (
                "ccRecipients",
                Json::Arr(vec![
                    person("", "only@example.org"),
                    person("Only Name", ""),
                    person("same@example.org", "same@example.org"),
                    Json::from("junk"),
                    Json::object(vec![("emailAddress", Json::Null)]),
                    Json::Obj(Object::new()),
                ]),
            ),
            ("isRead", Json::Null),
            ("receivedDateTime", Json::from("yesterday")),
        ]));
        let body = read(&api(&transport), &mid("AAMkMSG=")?, 6000)?;
        assert!(body.subject.starts_with("Re: sss"));
        assert!(body.subject.ends_with(" [...]"));
        assert_eq!(body.from, "Eve <boss@corp.example> <eve@evil.example>");
        assert_eq!(body.to.len(), MAX_PEOPLE);
        assert_eq!(
            body.to.first().map(|p| p.address.as_str()),
            Some("p0@example.org")
        );
        let cc: Vec<(&str, &str)> = body
            .cc
            .iter()
            .map(|p| (p.name.as_str(), p.address.as_str()))
            .collect();
        assert_eq!(
            cc,
            [
                ("", "only@example.org"),
                ("Only Name", ""),
                ("same@example.org", "same@example.org"),
                ("", ""),
                ("", ""),
                ("", ""),
            ]
        );
        assert!(!body.read);
        assert_eq!(body.date, "");
        Ok(())
    }

    #[test]
    fn mailboxes_ask_for_immutable_ids_too() -> TestResult {
        let transport = Scripted::new().json(
            Method::Get,
            "/mailFolders?",
            200,
            r#"{"value": [{"id": "AAMkInbox=", "displayName": "Inbox", "unreadItemCount": 3, "childFolderCount": 0}]}"#,
        );
        let boxes = mailboxes(&api(&transport))?;
        assert_eq!(
            boxes,
            vec![Mailbox {
                path: "Inbox".to_string(),
                unread: "3".to_string()
            }]
        );
        let seen = transport.seen();
        let [call] = seen.as_slice() else {
            return Err("one request".into());
        };
        assert_eq!(header(call, "Prefer").as_deref(), Some(IDS));
        Ok(())
    }

    #[test]
    fn the_preference_header_is_added_next_to_existing_headers() -> TestResult {
        let transport = Scripted::new().json(Method::Get, "/x", 200, "{}");
        let base = Api::new(&transport, "TOKEN".to_string(), Source::Microsoft)
            .with_header("X-First", "1");
        let derived = extended(&base, &[("Prefer", "a=\"b\""), ("X-Second", "2")]);
        derived.get("https://graph.microsoft.com/v1.0/me/x")?;
        let seen = transport.seen();
        let call = seen.first().ok_or("request")?;
        let names: Vec<&str> = call.headers.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["Authorization", "X-First", "Prefer", "X-Second"]);
        assert_eq!(header(call, "Prefer").as_deref(), Some("a=\"b\""));
        Ok(())
    }

    #[test]
    fn the_requested_page_size_matches_the_number_of_entries_that_are_read() {
        assert_eq!(PAGE_SIZE, PAGE_ITEMS.to_string());
        assert_eq!(MAX_PAGES * PAGE_ITEMS, 500);
    }

    #[test]
    fn urls_are_built_with_literal_option_names_and_encoded_values() {
        assert_eq!(url("", &[]), "https://graph.microsoft.com/v1.0/me");
        assert_eq!(
            url("/messages", &[("$select", "a,b"), ("$filter", "x eq 'y z'")]),
            "https://graph.microsoft.com/v1.0/me/messages?$select=a%2Cb&$filter=x%20eq%20%27y%20z%27"
        );
        assert_eq!(message_path("a b/c"), "/messages/a%20b%2Fc");
    }

    #[test]
    fn a_missing_profile_is_reported() {
        let transport = Scripted::new().json(Method::Get, "/v1.0/me?", 404, "{}");
        let result = account_info(&api(&transport), &account());
        assert!(matches!(result, Err(ToolError::Msg(_))));
    }

    struct Shared(std::sync::Arc<Scripted>);

    impl crate::http::Transport for Shared {
        fn send(&self, request: &Request) -> Result<crate::http::Response, HttpError> {
            self.0.send(request)
        }
    }

    struct Declining;

    impl Host for Declining {
        fn ensure_running(&self, _app: crate::osascript::App) {}

        fn run(
            &self,
            _app: crate::osascript::App,
            _payload: &str,
            _timeout: std::time::Duration,
        ) -> crate::osascript::Finished {
            crate::osascript::Finished::Done {
                code: Some(0),
                stdout: r#"{"ok": true, "data": {"decision": "declined"}}"#.to_string(),
                stderr: String::new(),
            }
        }
    }

    #[test]
    fn the_cloud_backend_hands_outlook_accounts_to_this_module_with_the_real_token() -> TestResult {
        use crate::backend::{Backend, Mode};
        use crate::cloud::CloudMail;
        use crate::vault::memory::Memory;
        use std::sync::Arc;

        let record = r#"{"client_id": "ms-client", "refresh_token": "M.R3_old", "scope": "offline_access User.Read Mail.Read Calendars.Read Mail.ReadWrite mail.send"}"#;
        let transport = Arc::new(
            Scripted::new()
                .json(
                    Method::Post,
                    "/token",
                    200,
                    r#"{"access_token": "ACCESS-1", "token_type": "Bearer", "expires_in": 3600, "scope": "Mail.Read"}"#,
                )
                .json(Method::Delete, "/messages/AAMkDRAFT%3D", 204, "")
                .json(
                    Method::Get,
                    "/messages/AAMkDRAFT%3D",
                    200,
                    r#"{"id": "AAMkDRAFT=", "subject": "Hallo", "hasAttachments": false, "body": {"contentType": "text", "content": "Kurzer Text."}, "toRecipients": [{"emailAddress": {"address": "anna@example.org"}}], "ccRecipients": [], "bccRecipients": []}"#,
                )
                .json(Method::Post, "/v1.0/me/messages", 201, r#"{"id": "AAMkDRAFT="}"#)
                .json(Method::Get, "/messages/AAMkMSG%3D?", 200, &message(vec![]))
                .json(Method::Get, "/mailFolders/AAMkFOLDER%3D", 200, r#"{"displayName": "Inbox"}"#)
                .json(Method::Get, "/mailFolders/inbox/messages", 200, &page(vec![simple("m1", "Hallo")], None))
                .json(
                    Method::Get,
                    "/mailFolders?",
                    200,
                    r#"{"value": [{"id": "AAMkInbox=", "displayName": "Inbox", "unreadItemCount": 2, "childFolderCount": 0}]}"#,
                )
                .json(Method::Get, "/v1.0/me?", 200, r#"{"mail": "bob@outlook.example"}"#),
        );
        let vault = Memory::new().with("microsoft:bob@outlook.example", record);
        let backend = CloudMail::new(
            Box::new(Shared(Arc::clone(&transport))),
            Box::new(vault),
            Box::new(Declining),
        );
        let who = account();
        assert_eq!(
            backend.account_info(&who)?.addresses,
            ["bob@outlook.example"]
        );
        assert_eq!(backend.mailboxes(&who)?.len(), 1);
        assert_eq!(
            ids(&backend.messages(&who, &request(None, "both"))?),
            ["m1"]
        );
        let body = backend.read(&who, None, &mid("AAMkMSG=")?, 6000)?;
        assert_eq!(body.mailbox, "Inbox");
        let preview = backend.compose(
            &who,
            &ComposeRequest {
                mode: Mode::Send,
                to: vec!["anna@example.org".to_string()],
                cc: vec![],
                subject: "Hallo".to_string(),
                body: "Kurzer Text.".to_string(),
                reply: None,
            },
        )?;
        assert_eq!(preview.status, "declined");
        let seen = transport.seen();
        let graph: Vec<&Request> = seen
            .iter()
            .filter(|call| call.url.starts_with("https://graph.microsoft.com/"))
            .collect();
        assert_eq!(graph.len(), 8);
        for call in &graph {
            assert!(
                header(call, "Authorization").is_some_and(|value| value.ends_with(" ACCESS-1")),
                "{}",
                call.url
            );
            assert!(header(call, "Prefer").is_some_and(|value| value.starts_with(IDS)));
        }
        let tokens = seen
            .iter()
            .filter(|call| {
                call.url
                    .starts_with("https://login.microsoftonline.com/consumers/")
            })
            .count();
        assert_eq!(tokens, 5);
        assert!(graph
            .iter()
            .all(|call| call.method != Method::Post || call.url.ends_with("/messages")));
        Ok(())
    }
}
