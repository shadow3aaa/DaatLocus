use serde::{Deserialize, Serialize};

use crate::app::AppId;
use crate::tool_ui::{
    CodingEditUiData, CodingReviewUiData, CodingToolGroupUiData, PatchFileUiData, ToolUiData,
};

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
    /// Full user message body. Used by TUI/WebUI to render long inputs without truncation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_body: Option<String>,
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
pub struct CodingReviewActivityCell {
    pub title: String,
    pub summary: String,
    pub review_pending: bool,
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
        full_body: None,
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
        generic_app_cell(
            render_exposed_tool_names(&data.title),
            render_exposed_tool_names_in_lines(data.body_lines),
        )
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
                    tool_name: AppId::render_exposed_tool_name(&call.tool_name),
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

impl From<CodingReviewUiData> for CodingReviewActivityCell {
    fn from(data: CodingReviewUiData) -> Self {
        Self {
            title: data.title,
            summary: data.summary,
            review_pending: data.review_pending,
        }
    }
}

impl From<ToolUiData> for ErrorActivityCell {
    fn from(data: ToolUiData) -> Self {
        error_cell(
            render_exposed_tool_names(&data.title),
            render_exposed_tool_names_in_lines(data.body_lines),
        )
    }
}

pub fn render_exposed_tool_names(text: &str) -> String {
    let mut rendered = String::with_capacity(text.len());
    let mut token = String::new();
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !token.is_empty() {
                rendered.push_str(&render_exposed_tool_name_token(&token));
                token.clear();
            }
            rendered.push(ch);
        } else {
            token.push(ch);
        }
    }
    if !token.is_empty() {
        rendered.push_str(&render_exposed_tool_name_token(&token));
    }
    rendered
}

pub fn render_exposed_tool_names_in_lines(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .map(|line| render_exposed_tool_names(&line))
        .collect()
}

fn render_exposed_tool_name_token(token: &str) -> String {
    let start = token
        .char_indices()
        .find(|(_, ch)| ch.is_ascii_alphanumeric() || *ch == '_')
        .map(|(index, _)| index)
        .unwrap_or(token.len());
    let end = token
        .char_indices()
        .rev()
        .find(|(_, ch)| ch.is_ascii_alphanumeric() || *ch == '_')
        .map(|(index, ch)| index + ch.len_utf8())
        .unwrap_or(start);
    if start >= end {
        return token.to_string();
    }

    let candidate = &token[start..end];
    let rendered = AppId::render_exposed_tool_name(candidate);
    if rendered == candidate {
        token.to_string()
    } else {
        format!("{}{}{}", &token[..start], rendered, &token[end..])
    }
}
