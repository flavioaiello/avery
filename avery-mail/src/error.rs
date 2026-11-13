//! Error type shared by all tools.

use std::fmt;

/// Failure of a tool call. The variants mirror the three kinds of failure of the
/// Python reference: `ToolError`, `UncertainError` (a subclass) and any other
/// exception.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolError {
    /// Reported to the model as the text of a tool result with `isError`.
    Msg(String),
    /// The outcome of a send is unknown (for example Mail stopped answering after
    /// the send was requested). A send log entry stays `pending`.
    Uncertain(String),
    /// An unexpected failure. The model only learns the kind, never details.
    Internal(String),
}

pub type ToolResult<T> = Result<T, ToolError>;

impl ToolError {
    pub fn msg(text: impl Into<String>) -> Self {
        ToolError::Msg(text.into())
    }

    pub fn internal(kind: impl Into<String>) -> Self {
        ToolError::Internal(kind.into())
    }

    /// True for the errors a Python `except ToolError` clause would catch.
    pub fn is_tool_error(&self) -> bool {
        !matches!(self, ToolError::Internal(_))
    }

    /// Text shown to the model.
    pub fn message(&self) -> String {
        match self {
            ToolError::Msg(text) | ToolError::Uncertain(text) => text.clone(),
            ToolError::Internal(kind) => format!("Internal error ({kind}). Please retry."),
        }
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for ToolError {}
