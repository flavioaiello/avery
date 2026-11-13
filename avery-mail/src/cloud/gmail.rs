use super::dialog::{approve_send, Verdict};
use super::Api;
use crate::account::{Account, MailAccount};
use crate::backend::{
    ComposeRequest, Mailbox, MessageBody, MessageId, MessageList, MessageQuery, Preview,
};
use crate::error::{ToolError, ToolResult};
use crate::osascript::Host;

fn unavailable() -> ToolError {
    ToolError::msg("Gmail is not available yet.")
}

pub(crate) fn account_info(_api: &Api, _account: &Account) -> ToolResult<MailAccount> {
    Err(unavailable())
}

pub(crate) fn mailboxes(_api: &Api) -> ToolResult<Vec<Mailbox>> {
    Err(unavailable())
}

pub(crate) fn messages(_api: &Api, _query: &MessageQuery) -> ToolResult<MessageList> {
    Err(unavailable())
}

pub(crate) fn read(_api: &Api, _id: &MessageId, _max_chars: usize) -> ToolResult<MessageBody> {
    Err(unavailable())
}

pub(crate) fn compose(
    _api: &Api,
    host: &dyn Host,
    _account: &Account,
    _request: &ComposeRequest,
) -> ToolResult<Preview> {
    let preview = Preview {
        from: String::new(),
        to: Vec::new(),
        cc: Vec::new(),
        subject: String::new(),
        body: String::new(),
        reply: None,
        status: String::new(),
        saved: None,
    };
    match approve_send(host, &preview, 0)? {
        Verdict::Approved | Verdict::Declined | Verdict::TimedOut => Err(unavailable()),
    }
}
