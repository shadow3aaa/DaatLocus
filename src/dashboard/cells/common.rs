use serde::{Deserialize, Serialize};

use crate::activity_event::{
    CodingEditActivityDescriptor, CodingReviewActivityDescriptor, ExploredActivityDescriptor,
    ExploredCallActivityAction, PatchFileActivityDescriptor, TextActivityDescriptor,
};
use crate::app::AppId;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssistantActivityData {
    pub content: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FinalMessageSeparatorActivityData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_seconds: Option<u64>,
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
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeStatusActivityData {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_runtime_started_at_ms: Option<i64>,
    #[serde(default)]
    pub reduced_motion: ReducedMotion,
}

/// Thinking / reasoning content produced by the model.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThinkingActivityData {
    pub content: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserActivityData {
    pub content: String,
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
pub struct GenericAppActivityData {
    pub title: String,
    pub body_lines: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodingOpenProjectActivityData {
    pub project_root: String,
    pub detail_lines: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExploredActivityData {
    pub stable_id: String,
    pub title: String,
    pub calls: Vec<ExploredCallActivityData>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodingEditActivityData {
    pub stable_id: String,
    pub title: String,
    pub tool_name: Option<String>,
    pub tool_app: Option<String>,
    pub selector: String,
    pub file: Option<String>,
    pub added_lines: usize,
    pub removed_lines: usize,
    pub propagation_count: usize,
    pub impact_lines: Vec<String>,
    pub diff_files: Vec<PatchFileActivityDescriptor>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodingReviewActivityData {
    pub title: String,
    pub summary: String,
    pub review_pending: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExploredCallActivityData {
    pub tool_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<ExploredCallActivityAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secondary_target: Option<String>,
    pub summary: String,
    pub detail_lines: Vec<String>,
    pub detail_title: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminalWaitActivityData {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<String>,
    pub body_lines: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorActivityData {
    pub title: String,
    pub body_lines: Vec<String>,
}

pub fn assistant_message_data(content: impl Into<String>) -> AssistantActivityData {
    AssistantActivityData {
        content: content.into(),
    }
}

pub fn final_message_separator_cell(
    elapsed_seconds: Option<u64>,
) -> FinalMessageSeparatorActivityData {
    FinalMessageSeparatorActivityData { elapsed_seconds }
}

pub fn user_message_data(content: impl Into<String>) -> UserActivityData {
    UserActivityData {
        content: content.into(),
        image_attachments: Vec::new(),
    }
}

pub fn generic_app_cell(
    title: impl Into<String>,
    body_lines: Vec<String>,
) -> GenericAppActivityData {
    GenericAppActivityData {
        title: title.into(),
        body_lines,
    }
}

pub fn terminal_wait_cell(
    title: impl Into<String>,
    meta: Option<String>,
    body_lines: Vec<String>,
) -> TerminalWaitActivityData {
    TerminalWaitActivityData {
        title: title.into(),
        meta,
        body_lines,
    }
}

pub fn error_cell(title: impl Into<String>, body_lines: Vec<String>) -> ErrorActivityData {
    let title = title.into();
    ErrorActivityData {
        title: render_exposed_tool_names(&title),
        body_lines: render_exposed_tool_names_in_lines(body_lines),
    }
}

pub fn thinking_cell(content: impl Into<String>) -> ThinkingActivityData {
    ThinkingActivityData {
        content: normalize_thinking_markdown_sections(&content.into()),
    }
}

fn normalize_thinking_markdown_sections(content: &str) -> String {
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    let split = split_embedded_thinking_headings(&normalized);
    add_blank_lines_around_thinking_headings(&split)
}

fn split_embedded_thinking_headings(content: &str) -> String {
    let mut output = String::with_capacity(content.len());
    let mut cursor = 0;

    while let Some(relative_start) = content[cursor..].find("**") {
        let start = cursor + relative_start;
        let heading_body_start = start + 2;
        let Some(relative_end) = content[heading_body_start..].find("**") else {
            break;
        };
        let end = heading_body_start + relative_end + 2;
        let candidate = &content[start..end];

        output.push_str(&content[cursor..start]);
        if embedded_thinking_heading_needs_break(content, start, end, candidate) {
            push_paragraph_break(&mut output);
        }
        output.push_str(candidate);
        cursor = end;
    }

    output.push_str(&content[cursor..]);
    output
}

fn embedded_thinking_heading_needs_break(
    content: &str,
    start: usize,
    end: usize,
    candidate: &str,
) -> bool {
    if start == 0 || content[..start].ends_with('\n') || !content[end..].starts_with("\n\n") {
        return false;
    }
    let Some(previous) = content[..start].chars().next_back() else {
        return false;
    };
    !previous.is_whitespace() && is_standalone_thinking_heading(candidate)
}

fn add_blank_lines_around_thinking_headings(content: &str) -> String {
    let lines = content.lines().collect::<Vec<_>>();
    let mut output = Vec::with_capacity(lines.len());

    for (index, line) in lines.iter().enumerate() {
        let heading = is_standalone_thinking_heading(line);
        if heading && !last_line_is_blank(&output) {
            output.push(String::new());
        }
        output.push((*line).to_string());
        if heading && !next_line_is_blank(&lines, index) {
            output.push(String::new());
        }
    }

    output.join("\n")
}

fn is_standalone_thinking_heading(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() || is_markdown_block_syntax(trimmed) {
        return false;
    }

    let text = strip_markdown_emphasis(trimmed);
    let ends_with_sentence_punctuation = text
        .chars()
        .next_back()
        .is_some_and(|ch| matches!(ch, '.' | '!' | '?' | '。' | '！' | '？'));
    if text.is_empty() || text.chars().count() > 88 || ends_with_sentence_punctuation {
        return false;
    }

    if is_strong_markdown(trimmed) {
        return true;
    }

    let word_count = text
        .split_whitespace()
        .filter(|word| !word.is_empty())
        .count();
    if word_count > 9 {
        return false;
    }

    text.chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_uppercase() || is_common_han(ch))
}

fn strip_markdown_emphasis(value: &str) -> &str {
    let trimmed = value.trim();
    for marker in ["**", "__", "*", "_"] {
        if let Some(inner) = trimmed
            .strip_prefix(marker)
            .and_then(|inner| inner.strip_suffix(marker))
        {
            return inner.trim();
        }
    }
    trimmed
        .strip_prefix('#')
        .map(|inner| inner.trim_start_matches('#'))
        .unwrap_or(trimmed)
        .trim()
}

fn is_strong_markdown(value: &str) -> bool {
    (value.starts_with("**") && value.ends_with("**"))
        || (value.starts_with("__") && value.ends_with("__"))
}

fn is_markdown_block_syntax(value: &str) -> bool {
    value.starts_with("# ")
        || value.starts_with("## ")
        || value.starts_with("### ")
        || value.starts_with("#### ")
        || value.starts_with("##### ")
        || value.starts_with("###### ")
        || value.starts_with("- ")
        || value.starts_with("* ")
        || value.starts_with("+ ")
        || value.starts_with("> ")
        || value.starts_with("```")
        || value.starts_with("~~~")
        || value.starts_with('|')
        || ordered_list_prefix(value)
}

fn ordered_list_prefix(value: &str) -> bool {
    let mut chars = value.chars().peekable();
    let mut saw_digit = false;
    while chars.peek().is_some_and(|ch| ch.is_ascii_digit()) {
        saw_digit = true;
        chars.next();
    }
    saw_digit
        && matches!(chars.next(), Some('.') | Some(')'))
        && chars.next().is_some_and(|ch| ch.is_whitespace())
}

fn is_common_han(ch: char) -> bool {
    ('\u{4E00}'..='\u{9FFF}').contains(&ch)
}

fn last_line_is_blank(lines: &[String]) -> bool {
    lines.last().is_none_or(|line| line.trim().is_empty())
}

fn next_line_is_blank(lines: &[&str], index: usize) -> bool {
    lines
        .get(index + 1)
        .is_none_or(|line| line.trim().is_empty())
}

fn push_paragraph_break(output: &mut String) {
    if output.ends_with("\n\n") {
        return;
    }
    if output.ends_with('\n') {
        output.push('\n');
    } else {
        output.push_str("\n\n");
    }
}

impl From<TextActivityDescriptor> for GenericAppActivityData {
    fn from(data: TextActivityDescriptor) -> Self {
        generic_app_cell(
            render_exposed_tool_names(&data.title),
            render_exposed_tool_names_in_lines(data.body_lines),
        )
    }
}

impl From<crate::activity_event::CodingOpenProjectActivityDescriptor>
    for CodingOpenProjectActivityData
{
    fn from(data: crate::activity_event::CodingOpenProjectActivityDescriptor) -> Self {
        Self {
            project_root: data.project_root,
            detail_lines: data.detail_lines,
        }
    }
}

impl From<ExploredActivityDescriptor> for ExploredActivityData {
    fn from(data: ExploredActivityDescriptor) -> Self {
        Self {
            stable_id: data.stable_id,
            title: data.title,
            calls: data
                .calls
                .into_iter()
                .map(|call| ExploredCallActivityData {
                    tool_name: AppId::render_exposed_tool_name(&call.tool_name),
                    action: call.action,
                    target: call.target,
                    secondary_target: call.secondary_target,
                    summary: call.summary,
                    detail_lines: call.detail_lines,
                    detail_title: None,
                })
                .collect(),
        }
    }
}

impl From<CodingEditActivityDescriptor> for CodingEditActivityData {
    fn from(data: CodingEditActivityDescriptor) -> Self {
        Self {
            stable_id: data.stable_id,
            title: data.title,
            tool_name: data.tool_name,
            tool_app: data.tool_app,
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

impl From<CodingReviewActivityDescriptor> for CodingReviewActivityData {
    fn from(data: CodingReviewActivityDescriptor) -> Self {
        Self {
            title: data.title,
            summary: data.summary,
            review_pending: data.review_pending,
        }
    }
}

impl From<TextActivityDescriptor> for ErrorActivityData {
    fn from(data: TextActivityDescriptor) -> Self {
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

#[cfg(test)]
mod tests {
    use super::{normalize_thinking_markdown_sections, thinking_cell};

    #[test]
    fn thinking_markdown_splits_embedded_bold_headings() {
        let content = "Everything needs careful path syntax.**Considering project operations**\n\nI am checking project state.";

        assert_eq!(
            normalize_thinking_markdown_sections(content),
            "Everything needs careful path syntax.\n\n**Considering project operations**\n\nI am checking project state."
        );
    }

    #[test]
    fn thinking_cell_normalizes_persisted_content() {
        let cell = thinking_cell("Intro.**Setting up for git commands**\n\nBody");

        assert_eq!(
            cell.content,
            "Intro.\n\n**Setting up for git commands**\n\nBody"
        );
    }

    #[test]
    fn thinking_markdown_does_not_split_inline_bold_phrases() {
        let content = "I should keep **important phrase**\n\nas normal prose.";

        assert_eq!(normalize_thinking_markdown_sections(content), content);
    }
}
