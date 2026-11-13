use crate::account::{string_list, Account, MailAccount};
use crate::error::{ToolError, ToolResult};
use crate::json::{Json, Object};
use crate::text::text_of;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MessageQuery {
    pub(crate) mailbox: Option<String>,
    pub(crate) since_days: i64,
    pub(crate) unread_only: bool,
    pub(crate) query: Option<String>,
    pub(crate) field: String,
    pub(crate) limit: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MessageItem {
    pub(crate) id: String,
    pub(crate) subject: String,
    pub(crate) from: String,
    pub(crate) ts: i64,
    pub(crate) date: String,
    pub(crate) read: bool,
    pub(crate) flagged: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MessageList {
    pub(crate) mailbox: Option<String>,
    pub(crate) total: i64,
    pub(crate) items: Vec<MessageItem>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Mailbox {
    pub(crate) path: String,
    pub(crate) unread: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Person {
    pub(crate) name: String,
    pub(crate) address: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Attachment {
    pub(crate) name: String,
    pub(crate) size: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MessageBody {
    pub(crate) mailbox: String,
    pub(crate) subject: String,
    pub(crate) from: String,
    pub(crate) date: String,
    pub(crate) read: bool,
    pub(crate) to: Vec<Person>,
    pub(crate) cc: Vec<Person>,
    pub(crate) attachments: Vec<Attachment>,
    pub(crate) body_chars: Option<usize>,
    pub(crate) body: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Draft,
    Send,
}

impl Mode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Mode::Draft => "draft",
            Mode::Send => "send",
        }
    }

    pub(crate) fn parse(text: &str) -> Option<Mode> {
        match text {
            "draft" => Some(Mode::Draft),
            "send" => Some(Mode::Send),
            _ => None,
        }
    }
}

pub(crate) const MAX_MESSAGE_ID: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MessageId(String);

impl MessageId {
    fn token(text: &str) -> bool {
        !text.is_empty()
            && text.len() <= MAX_MESSAGE_ID
            && text
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "_=+/.-".contains(c))
    }

    pub(crate) fn parse(text: &str) -> Option<MessageId> {
        let text = text.trim();
        (!text.contains("..") && MessageId::token(text)).then(|| MessageId(text.to_string()))
    }

    pub(crate) fn logged(text: &str) -> Option<String> {
        let text = text.trim();
        MessageId::token(text).then(|| text.to_string())
    }

    pub(crate) fn argument(value: Option<&Json>, parameter: &str) -> ToolResult<Option<MessageId>> {
        let invalid = || {
            ToolError::msg(format!(
                "Parameter '{parameter}' must be a message id from list_messages or search_messages."
            ))
        };
        match value {
            None | Some(Json::Null) => Ok(None),
            Some(Json::Int(number)) if *number >= 1 => MessageId::parse(&number.to_string())
                .map(Some)
                .ok_or_else(invalid),
            Some(Json::Str(text)) => MessageId::parse(text).map(Some).ok_or_else(invalid),
            Some(_) => Err(invalid()),
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn number(&self) -> Option<i64> {
        self.0.parse::<i64>().ok().filter(|number| *number > 0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReplySpec {
    pub(crate) mailbox: Option<String>,
    pub(crate) id: MessageId,
    pub(crate) all: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ComposeRequest {
    pub(crate) mode: Mode,
    pub(crate) to: Vec<String>,
    pub(crate) cc: Vec<String>,
    pub(crate) subject: String,
    pub(crate) body: String,
    pub(crate) reply: Option<ReplySpec>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Preview {
    pub(crate) from: String,
    pub(crate) to: Vec<String>,
    pub(crate) cc: Vec<String>,
    pub(crate) subject: String,
    pub(crate) body: String,
    pub(crate) reply: Option<(String, String)>,
    pub(crate) status: String,
    pub(crate) saved: Option<bool>,
}

impl Preview {
    pub(crate) fn to_json(&self) -> Json {
        let mut object = Object::new();
        object.insert("from", Json::from(&self.from));
        object.insert("to", Json::from(self.to.as_slice()));
        object.insert("cc", Json::from(self.cc.as_slice()));
        object.insert("subject", Json::from(&self.subject));
        object.insert("body", Json::from(&self.body));
        object.insert(
            "reply",
            match &self.reply {
                Some((subject, from)) => Json::object(vec![
                    ("subject", Json::from(subject)),
                    ("from", Json::from(from)),
                ]),
                None => Json::Null,
            },
        );
        object.insert("status", Json::from(&self.status));
        if let Some(saved) = self.saved {
            object.insert("saved", Json::Bool(saved));
        }
        Json::Obj(object)
    }

    pub(crate) fn from_json(value: &Json) -> Option<Preview> {
        let object = value.as_object()?;
        let reply = match object.get("reply") {
            Some(Json::Obj(reply)) => {
                Some((text_of(reply.get("subject")), text_of(reply.get("from"))))
            }
            _ => None,
        };
        Some(Preview {
            from: text_of(object.get("from")),
            to: string_list(object.get("to")),
            cc: string_list(object.get("cc")),
            subject: text_of(object.get("subject")),
            body: text_of(object.get("body")),
            reply,
            status: text_of(object.get("status")),
            saved: object.get("saved").and_then(Json::as_bool),
        })
    }
}

pub(crate) trait Backend {
    fn all_accounts(&self) -> ToolResult<Vec<MailAccount>>;
    fn account_info(&self, account: &Account) -> ToolResult<MailAccount>;
    fn mailboxes(&self, account: &Account) -> ToolResult<Vec<Mailbox>>;
    fn messages(&self, account: &Account, query: &MessageQuery) -> ToolResult<MessageList>;
    fn read(
        &self,
        account: &Account,
        mailbox: Option<&str>,
        id: &MessageId,
        max_chars: usize,
    ) -> ToolResult<MessageBody>;
    fn compose(&self, account: &Account, request: &ComposeRequest) -> ToolResult<Preview>;
    fn approval_test(&self, seconds: u64) -> ToolResult<String>;
}

fn whole_number(value: Option<&Json>) -> Option<i64> {
    match value? {
        Json::Int(number) => Some(*number),
        Json::Str(text) => text.trim().parse::<i64>().ok(),
        _ => None,
    }
}

fn people(value: Option<&Json>) -> Vec<Person> {
    match value {
        Some(Json::Arr(items)) => items
            .iter()
            .filter_map(Json::as_object)
            .map(|entry| Person {
                name: text_of(entry.get("name")),
                address: text_of(entry.get("address")),
            })
            .collect(),
        _ => Vec::new(),
    }
}

impl MessageList {
    pub(crate) fn from_json(value: &Json) -> Option<MessageList> {
        let object = value.as_object()?;
        let mailbox = object
            .get("mailbox")
            .and_then(Json::as_str)
            .filter(|mailbox| !mailbox.is_empty())
            .map(str::to_string);
        let items = match object.get("items") {
            Some(Json::Arr(entries)) => entries
                .iter()
                .filter_map(Json::as_object)
                .map(|entry| MessageItem {
                    id: text_of(entry.get("id")),
                    subject: text_of(entry.get("subject")),
                    from: text_of(entry.get("from")),
                    ts: whole_number(entry.get("ts")).unwrap_or(0),
                    date: text_of(entry.get("date")),
                    read: entry.get("read").and_then(Json::as_bool).unwrap_or(false),
                    flagged: entry
                        .get("flagged")
                        .and_then(Json::as_bool)
                        .unwrap_or(false),
                })
                .collect(),
            _ => Vec::new(),
        };
        Some(MessageList {
            mailbox,
            total: whole_number(object.get("total")).unwrap_or(0),
            items,
        })
    }
}

impl Mailbox {
    pub(crate) fn list_from_json(value: &Json) -> Option<Vec<Mailbox>> {
        let entries = value.as_array()?;
        Some(
            entries
                .iter()
                .filter_map(Json::as_object)
                .map(|entry| Mailbox {
                    path: text_of(entry.get("path")),
                    unread: match entry.get("unread") {
                        None => "0".to_string(),
                        other => text_of(other),
                    },
                })
                .collect(),
        )
    }
}

impl MessageBody {
    pub(crate) fn from_json(value: &Json) -> Option<MessageBody> {
        let object = value.as_object()?;
        let attachments = match object.get("attachments") {
            Some(Json::Arr(entries)) => entries
                .iter()
                .filter_map(Json::as_object)
                .map(|entry| Attachment {
                    name: text_of(entry.get("name")),
                    size: whole_number(entry.get("size")),
                })
                .collect(),
            _ => Vec::new(),
        };
        Some(MessageBody {
            mailbox: text_of(object.get("mailbox")),
            subject: text_of(object.get("subject")),
            from: text_of(object.get("from")),
            date: text_of(object.get("date")),
            read: object.get("read").and_then(Json::as_bool).unwrap_or(false),
            to: people(object.get("to")),
            cc: people(object.get("cc")),
            attachments,
            body_chars: whole_number(object.get("bodyChars"))
                .and_then(|count| usize::try_from(count).ok()),
            body: text_of(object.get("body")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{j, TestResult};

    #[test]
    fn message_ids_are_plain_tokens_of_bounded_length() {
        for good in ["101", "18c3f0a1b2", "AAMkAGI2-_=", "a/b+c.d", " 7 "] {
            assert!(MessageId::parse(good).is_some(), "{good:?}");
        }
        assert_eq!(
            MessageId::parse(" 7 ").map(|id| id.as_str().to_string()),
            Some("7".to_string())
        );
        for bad in [
            "", "  ", "a b", "a\"b", "a\nb", "ä", "a;b", "a?b", "a#b", "a%b", "..", "a..b", "../x",
            "a/../b",
        ] {
            assert!(MessageId::parse(bad).is_none(), "{bad:?}");
        }
        assert!(MessageId::parse(&"a".repeat(MAX_MESSAGE_ID)).is_some());
        assert!(MessageId::parse(&"a".repeat(MAX_MESSAGE_ID + 1)).is_none());
        assert_eq!(
            MessageId::parse("101").and_then(|id| id.number()),
            Some(101)
        );
        assert_eq!(MessageId::parse("0").and_then(|id| id.number()), None);
        assert_eq!(MessageId::parse("-5").and_then(|id| id.number()), None);
        assert_eq!(MessageId::parse("18c3f").and_then(|id| id.number()), None);
    }

    #[test]
    fn message_id_arguments_accept_whole_numbers_and_tokens() -> TestResult {
        let id = |text: &str| -> TestResult {
            let value = j(text)?;
            let parsed = MessageId::argument(Some(&value), "id");
            assert_eq!(
                parsed.map(|found| found.map(|id| id.as_str().to_string())),
                Ok(Some("101".to_string())),
                "{text}"
            );
            Ok(())
        };
        id("101")?;
        id("\"101\"")?;
        id("\" 101 \"")?;
        assert_eq!(MessageId::argument(None, "id"), Ok(None));
        assert_eq!(MessageId::argument(Some(&Json::Null), "id"), Ok(None));
        for text in [
            "0", "-3", "1.5", "true", "[1]", "{}", "\"\"", "\"a b\"", "\"a;b\"",
        ] {
            let value = j(text)?;
            let Err(error) = MessageId::argument(Some(&value), "reply_to_id") else {
                return Err(format!("{text} was accepted").into());
            };
            assert!(error.message().contains("'reply_to_id'"), "{error}");
        }
        Ok(())
    }

    #[test]
    fn decodes_a_message_list() -> TestResult {
        let list = MessageList::from_json(&j(
            r#"{"mailbox": "INBOX", "total": 2, "items": [
                {"id": 5, "subject": "S", "from": "F", "ts": 1700000000, "date": "2023-11-14 23:13", "read": false, "flagged": true},
                {"id": 6}]}"#,
        )?)
        .ok_or("list")?;
        assert_eq!(list.mailbox.as_deref(), Some("INBOX"));
        assert_eq!(list.total, 2);
        assert_eq!(list.items[0].id, "5");
        assert!(!list.items[0].read);
        assert!(list.items[0].flagged);
        assert_eq!(list.items[1].subject, "");
        assert_eq!(list.items[1].date, "");
        assert_eq!(list.items[1].ts, 0);
        assert!(MessageList::from_json(&j("[]")?).is_none());
        Ok(())
    }

    #[test]
    fn decodes_mailboxes_and_bodies_leniently() -> TestResult {
        let boxes =
            Mailbox::list_from_json(&j(r#"[{"path": "INBOX", "unread": 3}, {"path": "x"}]"#)?)
                .ok_or("boxes")?;
        assert_eq!(boxes[0].unread, "3");
        assert_eq!(boxes[1].unread, "0");
        let body = MessageBody::from_json(&j(
            r#"{"mailbox": "INBOX", "subject": "S", "to": [{"name": null, "address": "a@example.org"}, 5],
                "attachments": [{"name": "f.pdf", "size": 2048}, {"name": "g", "size": "x"}], "bodyChars": 12, "body": "text"}"#,
        )?)
        .ok_or("body")?;
        assert_eq!(
            body.to,
            [Person {
                name: String::new(),
                address: "a@example.org".into()
            }]
        );
        assert_eq!(body.attachments[0].size, Some(2048));
        assert_eq!(body.attachments[1].size, None);
        assert_eq!(body.body_chars, Some(12));
        assert_eq!(body.date, "");
        Ok(())
    }

    #[test]
    fn preview_json_has_the_reference_key_order() -> TestResult {
        let preview = Preview {
            from: "Demo <d@example.org>".into(),
            to: vec!["a@example.org".into()],
            cc: vec![],
            subject: "S".into(),
            body: "B".into(),
            reply: None,
            status: "drafted".into(),
            saved: Some(true),
        };
        assert_eq!(
            crate::json::dumps(&preview.to_json(), false),
            r#"{"from": "Demo <d@example.org>", "to": ["a@example.org"], "cc": [], "subject": "S", "body": "B", "reply": null, "status": "drafted", "saved": true}"#
        );
        let decoded = Preview::from_json(&preview.to_json()).ok_or("preview")?;
        assert_eq!(decoded, preview);
        Ok(())
    }
}
