use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ToolError {
    Msg(String),
    Uncertain(String),
    Internal(String),
}

pub(crate) type ToolResult<T> = Result<T, ToolError>;

impl ToolError {
    pub(crate) fn msg(text: impl Into<String>) -> Self {
        ToolError::Msg(text.into())
    }

    pub(crate) fn internal(kind: impl Into<String>) -> Self {
        ToolError::Internal(kind.into())
    }

    pub(crate) fn is_tool_error(&self) -> bool {
        !matches!(self, ToolError::Internal(_))
    }

    pub(crate) fn message(&self) -> String {
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
