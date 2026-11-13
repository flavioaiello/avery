use super::dialog::{approve_change, Verdict};
use super::Api;
use crate::calendar::{
    Approval, Calendar, CalendarInfo, Change, ChangeResult, EventDetail, RawEvent, Window,
};
use crate::error::{ToolError, ToolResult};
use crate::osascript::Host;

fn unavailable() -> ToolError {
    ToolError::msg("Outlook.com calendar is not available yet.")
}

pub(crate) fn calendars(_api: &Api) -> ToolResult<Vec<CalendarInfo>> {
    Err(unavailable())
}

pub(crate) fn events(
    _api: &Api,
    _calendar: &str,
    _window: Window,
    _zone: &str,
) -> ToolResult<Vec<RawEvent>> {
    Err(unavailable())
}

pub(crate) fn event(
    _api: &Api,
    _calendar: &str,
    _uid: &str,
    _zone: &str,
) -> ToolResult<EventDetail> {
    Err(unavailable())
}

pub(crate) fn change(
    _api: &Api,
    host: &dyn Host,
    calendar: &Calendar,
    _provider_calendar: &str,
    _zone: &str,
    change: &Change,
    approval: &Approval,
) -> ToolResult<ChangeResult> {
    match approve_change(host, &calendar.label, change, approval)? {
        Verdict::Approved | Verdict::Declined | Verdict::TimedOut => Err(unavailable()),
    }
}
