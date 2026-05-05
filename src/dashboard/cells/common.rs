use serde::{Deserialize, Serialize};

use crate::tool_ui::ToolUiData;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssistantActivityCell {
    pub title: String,
    pub body_lines: Vec<String>,
}

/// Thinking / reasoning content produced by the model.
/// Rendered truncated in TUI and collapsible in WebUI.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThinkingActivityCell {
    pub title: String,
    pub body_lines: Vec<String>,
    /// Full reasoning text (may be very long for DeepSeek-style thinking).
    /// Kept for WebUI expand-to-view; TUI uses body_lines (truncated).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_body: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserActivityCell {
    pub title: String,
    pub body_lines: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_attachments: Vec<MessageImageAttachment>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageImageAttachment {
    pub label: String,
    pub uri: String,
    pub mime_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GenericAppActivityCell {
    pub title: String,
    pub body_lines: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminalWaitActivityCell {
    pub title: String,
    pub body_lines: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorActivityCell {
    pub title: String,
    pub body_lines: Vec<String>,
}

pub fn assistant_cell(title: impl Into<String>, body_lines: Vec<String>) -> AssistantActivityCell {
    AssistantActivityCell {
        title: title.into(),
        body_lines,
    }
}

pub fn user_cell(title: impl Into<String>, body_lines: Vec<String>) -> UserActivityCell {
    UserActivityCell {
        title: title.into(),
        body_lines,
        image_attachments: Vec::new(),
    }
}

pub fn generic_app_cell(
    title: impl Into<String>,
    body_lines: Vec<String>,
) -> GenericAppActivityCell {
    GenericAppActivityCell {
        title: title.into(),
        body_lines,
    }
}

pub fn terminal_wait_cell(
    title: impl Into<String>,
    body_lines: Vec<String>,
) -> TerminalWaitActivityCell {
    TerminalWaitActivityCell {
        title: title.into(),
        body_lines,
    }
}

pub fn error_cell(title: impl Into<String>, body_lines: Vec<String>) -> ErrorActivityCell {
    ErrorActivityCell {
        title: title.into(),
        body_lines,
    }
}

pub fn thinking_cell(
    title: impl Into<String>,
    body_lines: Vec<String>,
    full_body: Option<String>,
) -> ThinkingActivityCell {
    ThinkingActivityCell {
        title: title.into(),
        body_lines,
        full_body,
    }
}

impl From<ToolUiData> for GenericAppActivityCell {
    fn from(data: ToolUiData) -> Self {
        generic_app_cell(data.title, data.body_lines)
    }
}

impl From<ToolUiData> for ErrorActivityCell {
    fn from(data: ToolUiData) -> Self {
        error_cell(data.title, data.body_lines)
    }
}
