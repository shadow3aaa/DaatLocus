use serde::{Deserialize, Serialize};

use crate::tool_ui::{CodingEditUiData, CodingToolGroupUiData, PatchFileUiData, ToolUiData};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssistantActivityCell {
    pub title: String,
    pub body_lines: Vec<String>,
    /// Full assistant message body. Used for width-aware per-cell rendering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_body: Option<String>,
    /// Rich (markdown) vs Raw (plain text) display mode.
    #[serde(default = "default_rich_mode")]
    pub rich_mode: bool,
}

fn default_rich_mode() -> bool {
    true
}

/// Controls animation behaviour in the TUI dashboard.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum ReducedMotion {
    /// Full animations enabled (spinners, transitions).
    #[default]
    Full,
    /// Animations mostly disabled; static indicators preferred.
    Reduced,
}

/// Thinking / reasoning content produced by the model.
/// Rendered truncated by default; press Enter to expand.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThinkingActivityCell {
    pub title: String,
    pub body_lines: Vec<String>,
    /// Full reasoning text. Rendered when `expanded` is true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_body: Option<String>,
    /// Whether the cell is expanded (toggle via Enter key).
    #[serde(default)]
    pub expanded: bool,
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
pub struct CodingOpenProjectActivityCell {
    pub project_root: String,
    pub language: Option<String>,
    pub detail_lines: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodingToolGroupActivityCell {
    pub stable_id: String,
    pub title: String,
    pub calls: Vec<CodingToolCallActivityCell>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodingEditActivityCell {
    pub stable_id: String,
    pub title: String,
    pub selector: String,
    pub file: Option<String>,
    pub added_lines: usize,
    pub removed_lines: usize,
    pub propagation_count: usize,
    pub impact_lines: Vec<String>,
    pub diff_files: Vec<PatchFileUiData>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodingToolCallActivityCell {
    pub tool_name: String,
    pub summary: String,
    pub detail_lines: Vec<String>,
    pub detail_title: Option<String>,
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

pub fn assistant_cell_with_body(
    title: impl Into<String>,
    body_lines: Vec<String>,
    full_body: Option<String>,
) -> AssistantActivityCell {
    AssistantActivityCell {
        title: title.into(),
        body_lines,
        full_body,
        rich_mode: true,
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
        expanded: false,
    }
}

impl From<ToolUiData> for GenericAppActivityCell {
    fn from(data: ToolUiData) -> Self {
        generic_app_cell(data.title, data.body_lines)
    }
}

impl From<crate::tool_ui::CodingOpenProjectUiData> for CodingOpenProjectActivityCell {
    fn from(data: crate::tool_ui::CodingOpenProjectUiData) -> Self {
        Self {
            project_root: data.project_root,
            language: data.language,
            detail_lines: data.detail_lines,
        }
    }
}

impl From<CodingToolGroupUiData> for CodingToolGroupActivityCell {
    fn from(data: CodingToolGroupUiData) -> Self {
        Self {
            stable_id: data.stable_id,
            title: data.title,
            calls: data
                .calls
                .into_iter()
                .map(|call| CodingToolCallActivityCell {
                    tool_name: call.tool_name,
                    summary: call.summary,
                    detail_lines: call.detail_lines,
                    detail_title: None,
                })
                .collect(),
        }
    }
}

impl From<CodingEditUiData> for CodingEditActivityCell {
    fn from(data: CodingEditUiData) -> Self {
        Self {
            stable_id: data.stable_id,
            title: data.title,
            selector: data.selector,
            file: data.file,
            added_lines: data.added_lines,
            removed_lines: data.removed_lines,
            propagation_count: data.propagation_count,
            impact_lines: data.impact_lines,
            diff_files: data.diff_files,
        }
    }
}

impl From<ToolUiData> for ErrorActivityCell {
    fn from(data: ToolUiData) -> Self {
        error_cell(data.title, data.body_lines)
    }
}
