use crate::account::{string_list, Account, MailAccount};
use crate::error::ToolResult;
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReplySpec {
    pub(crate) mailbox: Option<String>,
    pub(crate) id: i64,
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
        id: i64,
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
