use serde::{Deserialize, Serialize};

use crate::tool_ui::ToolUiData;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssistantActivityCell {
    pub title: String,
    pub body_lines: Vec<String>,
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
