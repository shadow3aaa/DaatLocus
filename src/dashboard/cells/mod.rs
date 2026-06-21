mod apps;
mod common;
mod exec;
mod highlight;
pub(crate) mod markdown;
mod messages;
mod plan;
mod primitive;
mod tui;
mod web_activity;

use serde::{Deserialize, Serialize};

use crate::{
    events::{EventPayload, EventView},
    reasoning::runtime::{AgentContent, AgentContentPart, AgentMessage, HistoryMessage},
    tool_ui::{BrowserUiData, TerminalUiAction, ToolUiEvent},
};

use super::DashboardState;
use apps::{BrowserActivityCell, LiveBrowserActivityCell, WebSearchActivityCell};
#[cfg(test)]
pub(crate) use common::ExploredCallActivityCell;
use common::{
    AssistantActivityCell, ErrorActivityCell, GenericAppActivityCell, MessageImageAttachment,
    RuntimeStatusActivityCell, TerminalWaitActivityCell, UserActivityCell,
    assistant_cell_with_body, error_cell, final_message_separator_cell, terminal_wait_cell,
    user_cell,
};
use common::{
    CodingEditActivityCell, CodingOpenProjectActivityCell, CodingReviewActivityCell,
    ExploredActivityCell, ThinkingActivityCell,
};
use common::{render_exposed_tool_names, render_exposed_tool_names_in_lines, thinking_cell};
use exec::{ExecResultActivityCell, LiveExecActivityCell, is_output_metadata_line, live_exec_cell};
use messages::{PatchActivityCell, ReplyActivityCell, TelegramActivityCell};
use plan::PlanActivityCell;
use primitive::{ActivatePrimitiveActivityCell, CreatePrimitiveSpecActivityCell};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LiveActivityCell {
    pub key: String,
    pub cell: ActivityCell,
}

pub(super) use tui::activity_transcript_lines;
pub use tui::render_activity_feed_cached;
pub use tui::{ActivityFeedRenderArgs, CachedActivityLines};
pub use web_activity::{
    LiveWebActivityItem, WebActivityActor, WebActivityItem, WebActivityKind, WebActivityStatus,
    default_web_activity_version, sync_web_activity_state, web_activity_item_from_cell,
};

pub use common::ReducedMotion;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ActivityCell {
    Assistant(AssistantActivityCell),
    FinalMessageSeparator(common::FinalMessageSeparatorActivityCell),
    User(UserActivityCell),
    Browser(BrowserActivityCell),
    LiveBrowser(LiveBrowserActivityCell),
    WebSearch(WebSearchActivityCell),
    CodingOpenProject(CodingOpenProjectActivityCell),
    Explored(ExploredActivityCell),
    CodingEdit(CodingEditActivityCell),
    CodingReview(CodingReviewActivityCell),
    #[serde(alias = "ToolResult")]
    GenericApp(GenericAppActivityCell),
    PlanResult(PlanActivityCell),
    CreatePrimitiveSpecResult(CreatePrimitiveSpecActivityCell),
    ActivatePrimitiveResult(ActivatePrimitiveActivityCell),
    ExecResult(ExecResultActivityCell),
    LiveExec(LiveExecActivityCell),
    Patch(PatchActivityCell),
    Telegram(TelegramActivityCell),
    Reply(ReplyActivityCell),
    TerminalWait(TerminalWaitActivityCell),
    Warning(ErrorActivityCell),
    Error(ErrorActivityCell),
    Thinking(ThinkingActivityCell),
    RuntimeStatus(RuntimeStatusActivityCell),
}

const RUNTIME_STATUS_LIVE_CELL_KEY: &str = "runtime-status";

pub(super) fn append_runtime_status_live_cell(
    live_cells: &mut Vec<LiveActivityCell>,
    state: &DashboardState,
) {
    if !state.runtime_activity.active_runtime_turn {
        return;
    }

    live_cells.push(LiveActivityCell {
        key: RUNTIME_STATUS_LIVE_CELL_KEY.to_string(),
        cell: ActivityCell::RuntimeStatus(RuntimeStatusActivityCell {
            label: "Working".to_string(),
            detail: state.runtime_activity.detail.clone(),
            active_runtime_started_at_ms: state.runtime_activity.active_runtime_started_at_ms,
            reduced_motion: state.reduced_motion.clone(),
        }),
    });
}

#[derive(Clone)]
pub enum DashboardActivityEvent {
    AppendCommittedCells {
        cells: Vec<ActivityCell>,
    },
    ExecBegin {
        key: String,
        title: String,
        call_lines: Vec<String>,
    },
    BrowserBegin {
        key: String,
        event: BrowserUiData,
    },
    ExecUpdate {
        key: String,
        meta: Option<String>,
        output_lines: Vec<String>,
    },
    ExecEnd {
        key: String,
    },
    BrowserEnd {
        key: String,
    },
}

pub fn render_activity_from_messages(messages: Vec<HistoryMessage>) -> Vec<ActivityCell> {
    let cells = messages
        .into_iter()
        .filter(|message| !message.is_system() && !is_runtime_context_history_message(message))
        .rev()
        .take(12)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .flat_map(activity_cells_from_prompt_message)
        .collect::<Vec<_>>();
    coalesce_activity_cells(cells)
}

pub fn activity_cells_from_history_items(items: &[WebActivityItem]) -> Vec<ActivityCell> {
    coalesce_activity_cells(
        items
            .iter()
            .filter_map(|item| item.cell.clone())
            .collect::<Vec<_>>(),
    )
}

pub fn apply_activity_event(state: &mut DashboardState, event: DashboardActivityEvent) {
    match event {
        DashboardActivityEvent::AppendCommittedCells { mut cells } => {
            state.activity_cells.append(&mut cells);
            state.activity_cells = coalesce_activity_cells(state.activity_cells.clone());
            let history_cells = activity_cells_from_history_items(&state.activity_history.items);
            if !history_cells.is_empty() {
                state.activity_cells = history_cells;
            }
        }
        DashboardActivityEvent::ExecBegin {
            key,
            title,
            call_lines,
        } => upsert_live_activity_cell(
            &mut state.live_activity_cells,
            LiveActivityCell {
                key,
                cell: ActivityCell::LiveExec(live_exec_cell(
                    title,
                    call_lines,
                    Some(current_time_ms()),
                )),
            },
        ),
        DashboardActivityEvent::ExecUpdate {
            key,
            meta,
            output_lines,
        } => upsert_live_activity_cell(
            &mut state.live_activity_cells,
            LiveActivityCell {
                key,
                cell: ActivityCell::LiveExec(LiveExecActivityCell {
                    title: String::new(),
                    call_lines: Vec::new(),
                    meta,
                    output_lines,
                    started_at_ms: None,
                }),
            },
        ),
        DashboardActivityEvent::ExecEnd { key } => {
            state.live_activity_cells.retain(|cell| cell.key != key);
        }
        DashboardActivityEvent::BrowserBegin { key, event } => upsert_live_activity_cell(
            &mut state.live_activity_cells,
            LiveActivityCell {
                key,
                cell: ActivityCell::LiveBrowser(event.into()),
            },
        ),
        DashboardActivityEvent::BrowserEnd { key } => {
            state.live_activity_cells.retain(|cell| cell.key != key);
        }
    }
    sync_web_activity_state(state);
}

pub fn assistant_activity_cell(content: &str) -> Option<ActivityCell> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }
    if assistant_text_is_error(trimmed) {
        let title =
            render_exposed_tool_names(first_line_or_fallback(trimmed, "tool invocation error"));
        return Some(ActivityCell::Error(error_cell(
            title,
            render_exposed_tool_names_in_lines(remaining_lines_with_limit(trimmed, 24)),
        )));
    }
    Some(ActivityCell::Assistant(assistant_cell_with_body(
        first_line_or_fallback(trimmed, "assistant"),
        remaining_lines_with_limit(trimmed, 8),
        Some(trimmed.to_string()),
    )))
}

pub fn final_message_separator_activity_cell(elapsed_seconds: Option<u64>) -> ActivityCell {
    ActivityCell::FinalMessageSeparator(final_message_separator_cell(elapsed_seconds))
}

pub fn thinking_activity_cell(reasoning_content: &str) -> Option<ActivityCell> {
    let trimmed = reasoning_content.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut lines: Vec<&str> = trimmed.lines().collect();
    let full_body = if lines.len() > 3 {
        let full = trimmed.to_string();
        lines.truncate(2);
        lines.push("...");
        Some(full)
    } else {
        None
    };
    let body_lines: Vec<String> = lines.into_iter().map(|s| s.to_string()).collect();
    let title = "Thinking".to_string();
    Some(ActivityCell::Thinking(thinking_cell(
        title, body_lines, full_body,
    )))
}

pub fn user_activity_cell_from_event(event: &EventView) -> Option<ActivityCell> {
    let content = user_agent_content_from_event(event)?;
    Some(ActivityCell::User(user_cell_from_agent_content(&content)))
}

pub fn activity_cell_from_tool_ui_event(ui_event: ToolUiEvent) -> Option<ActivityCell> {
    match ui_event {
        ToolUiEvent::Exec(event) => Some(ActivityCell::ExecResult(event.into())),
        ToolUiEvent::Terminal(event) => {
            if matches!(event.action, TerminalUiAction::Poll) {
                terminal_wait_activity_cell_from_terminal_event(event)
            } else {
                Some(ActivityCell::ExecResult(event.into()))
            }
        }
        ToolUiEvent::Browser(event) => match event.action {
            crate::tool_ui::BrowserUiAction::Snapshot => Some(ActivityCell::Browser(event.into())),
            crate::tool_ui::BrowserUiAction::OpenPage
            | crate::tool_ui::BrowserUiAction::Wait
            | crate::tool_ui::BrowserUiAction::Click
            | crate::tool_ui::BrowserUiAction::Fill
            | crate::tool_ui::BrowserUiAction::Back
            | crate::tool_ui::BrowserUiAction::Forward
            | crate::tool_ui::BrowserUiAction::Reload
            | crate::tool_ui::BrowserUiAction::ClosePage => None,
        },
        ToolUiEvent::WebSearch(event) => Some(ActivityCell::WebSearch(event.into())),
        ToolUiEvent::CodingOpenProject(event) => {
            Some(ActivityCell::CodingOpenProject(event.into()))
        }
        ToolUiEvent::Explored(event) => Some(ActivityCell::Explored(event.into())),
        ToolUiEvent::CodingEdit(event) => Some(ActivityCell::CodingEdit(event.into())),
        ToolUiEvent::CodingReview(event) => Some(ActivityCell::CodingReview(event.into())),
        ToolUiEvent::Patch(event) => Some(ActivityCell::Patch(event.into())),
        ToolUiEvent::Telegram(event) => Some(ActivityCell::Telegram(event.into())),
        ToolUiEvent::Reply(event) => Some(ActivityCell::Reply(event.into())),
        ToolUiEvent::Plan(event) if event.steps.is_empty() => None,
        ToolUiEvent::Plan(event) => Some(ActivityCell::PlanResult(event.into())),
        ToolUiEvent::CreatePrimitiveSpec(event) => {
            Some(ActivityCell::CreatePrimitiveSpecResult(event.into()))
        }
        ToolUiEvent::ActivatePrimitive(event) => {
            Some(ActivityCell::ActivatePrimitiveResult(event.into()))
        }
        ToolUiEvent::App(event) => Some(ActivityCell::GenericApp(event.into())),
        ToolUiEvent::Warning(event) => Some(ActivityCell::Warning(event.into())),
        ToolUiEvent::Error(event) => Some(ActivityCell::Error(event.into())),
    }
}

fn terminal_wait_activity_cell_from_terminal_event(
    event: crate::tool_ui::TerminalUiData,
) -> Option<ActivityCell> {
    let mut body_lines = event.body_lines;
    let meta = body_lines
        .first()
        .filter(|line| is_terminal_poll_meta_line(line))
        .cloned();
    if meta.is_some() {
        body_lines.remove(0);
    }
    body_lines.retain(|line| !is_output_metadata_line(line));
    if body_lines.is_empty() {
        return None;
    }
    Some(ActivityCell::TerminalWait(terminal_wait_cell(
        event.title,
        meta,
        body_lines,
    )))
}

fn is_terminal_poll_meta_line(line: &str) -> bool {
    let line = line.trim();
    line.starts_with("terminal-session-") && line.contains("  exit=") && line.contains("  cwd=")
}

fn user_agent_content_from_event(event: &EventView) -> Option<AgentContent> {
    let (text, parts) = match &event.payload {
        EventPayload::TelegramIncoming(payload) => (
            payload.incoming_text.clone(),
            payload
                .attachments
                .iter()
                .map(|attachment| match attachment.kind {
                    crate::events::TelegramIncomingAttachmentKind::Image => {
                        AgentContentPart::Image {
                            path: attachment.local_path.clone(),
                            media_type: attachment.media_type.clone(),
                            description: attachment.description.clone(),
                        }
                    }
                })
                .collect::<Vec<_>>(),
        ),
        EventPayload::TerminalIncoming(payload) => (
            payload.incoming_text.clone(),
            payload
                .attachments
                .iter()
                .map(|attachment| match attachment.kind {
                    crate::events::TerminalIncomingAttachmentKind::Image => {
                        AgentContentPart::Image {
                            path: attachment.local_path.clone(),
                            media_type: attachment.media_type.clone(),
                            description: attachment.description.clone(),
                        }
                    }
                })
                .collect::<Vec<_>>(),
        ),
    };

    if text.trim().is_empty() && parts.is_empty() {
        None
    } else if parts.is_empty() {
        Some(AgentContent::text(text))
    } else {
        Some(AgentContent::multimodal(text, parts))
    }
}

fn activity_cells_from_prompt_message(message: HistoryMessage) -> Vec<ActivityCell> {
    match &message.message {
        AgentMessage::Assistant { content } => {
            let mut cells = Vec::new();
            let is_tool_protocol_placeholder =
                content.trim().starts_with("assistant tool-call protocol:");
            if assistant_text_is_error(content.trim()) {
                let title = render_exposed_tool_names(first_line_or_fallback(
                    content,
                    "tool invocation error",
                ));
                return vec![ActivityCell::Error(error_cell(
                    title,
                    render_exposed_tool_names_in_lines(remaining_lines_with_limit(content, 24)),
                ))];
            }
            if !content.trim().is_empty() && !is_tool_protocol_placeholder {
                cells.push(ActivityCell::Assistant(assistant_cell_with_body(
                    first_line_or_fallback(content, "assistant"),
                    remaining_lines_with_limit(content, 8),
                    Some(content.trim().to_string()),
                )));
            }
            cells
        }
        AgentMessage::AssistantToolCallProtocol { .. } => Vec::new(),
        AgentMessage::Tool { .. } => message
            .tool_ui_event
            .and_then(activity_cell_from_tool_ui_event)
            .into_iter()
            .collect(),
        AgentMessage::User { content } => {
            vec![ActivityCell::User(user_cell_from_agent_content(content))]
        }
        AgentMessage::System { .. } => Vec::new(),
    }
}

fn assistant_text_is_error(trimmed: &str) -> bool {
    trimmed.starts_with("tool invocation failed")
        || trimmed.starts_with("tool loop failed")
        || trimmed.starts_with("agent turn failed")
}

fn user_cell_from_agent_content(content: &AgentContent) -> UserActivityCell {
    let full_body = content.as_text().trim().to_string();
    let mut cell = user_cell(
        first_line_or_fallback(content.as_text(), "user"),
        remaining_lines(content.as_text()),
    );
    if !full_body.is_empty() {
        cell.full_body = Some(full_body);
    }
    cell.image_attachments = content
        .parts()
        .iter()
        .filter_map(message_image_attachment_from_part)
        .collect();
    cell
}

fn message_image_attachment_from_part(part: &AgentContentPart) -> Option<MessageImageAttachment> {
    let AgentContentPart::Image {
        path,
        media_type,
        description,
    } = part
    else {
        return None;
    };
    if path.trim().is_empty() || !media_type.trim().starts_with("image/") {
        return None;
    }
    let label = description
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| {
            std::path::Path::new(path)
                .file_name()
                .and_then(|value| value.to_str())
                .filter(|value| !value.trim().is_empty())
                .unwrap_or("image")
                .to_string()
        });
    Some(MessageImageAttachment {
        label,
        uri: dashboard_attachment_uri(path),
        mime_type: media_type.trim().to_string(),
        description: description.clone(),
    })
}

fn dashboard_attachment_uri(path: &str) -> String {
    format!(
        "/dashboard/attachments/{}",
        path.as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

fn is_runtime_context_history_message(message: &HistoryMessage) -> bool {
    let Some(content) = message.text_content() else {
        return false;
    };
    let content = content.trim_start();
    content.starts_with("<preturn_context") || content.starts_with("<afterclaim_context")
}

fn first_line_or_fallback<'a>(content: &'a str, fallback: &'a str) -> &'a str {
    content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or(fallback)
}

fn remaining_lines_with_limit(content: &str, limit: usize) -> Vec<String> {
    remaining_lines(content).into_iter().take(limit).collect()
}

fn remaining_lines(content: &str) -> Vec<String> {
    let mut lines: Vec<&str> = content.lines().collect();
    // drop first line (used as title)
    if !lines.is_empty() {
        lines.remove(0);
    }
    // trim leading blank lines
    while lines.first().is_some_and(|l| l.trim().is_empty()) {
        lines.remove(0);
    }
    // trim trailing blank lines
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    lines
        .into_iter()
        .map(str::trim)
        .map(ToString::to_string)
        .collect()
}

fn upsert_live_activity_cell(cells: &mut Vec<LiveActivityCell>, incoming: LiveActivityCell) {
    if let Some(existing) = cells.iter_mut().find(|cell| cell.key == incoming.key) {
        match (&mut existing.cell, incoming.cell) {
            (ActivityCell::LiveExec(existing_exec), ActivityCell::LiveExec(incoming_exec)) => {
                if !incoming_exec.title.is_empty() {
                    existing_exec.title = incoming_exec.title;
                }
                if !incoming_exec.call_lines.is_empty() {
                    existing_exec.call_lines = incoming_exec.call_lines;
                }
                if incoming_exec.meta.is_some() {
                    existing_exec.meta = incoming_exec.meta;
                }
                if !incoming_exec.output_lines.is_empty() {
                    existing_exec.output_lines = incoming_exec.output_lines;
                }
                if existing_exec.started_at_ms.is_none() {
                    existing_exec.started_at_ms = incoming_exec.started_at_ms;
                }
            }
            (
                ActivityCell::LiveBrowser(existing_browser),
                ActivityCell::LiveBrowser(incoming_browser),
            ) => {
                *existing_browser = incoming_browser;
            }
            (slot, cell) => *slot = cell,
        }
    } else {
        cells.push(incoming);
    }
}

fn coalesce_activity_cells(cells: Vec<ActivityCell>) -> Vec<ActivityCell> {
    let mut merged: Vec<ActivityCell> = Vec::new();
    for cell in cells {
        if let ActivityCell::Explored(new_group) = &cell
            && let Some(ActivityCell::Explored(existing_group)) = merged.last_mut()
            && existing_group.stable_id == new_group.stable_id
        {
            existing_group.title = new_group.title.clone();
            existing_group.calls.extend(new_group.calls.clone());
            continue;
        }

        if let Some(last) = merged.last_mut() {
            let same_exact = *last == cell;
            let same_exec_pair = matches!(
                (&mut *last, &cell),
                (
                    ActivityCell::ExecResult(last_exec),
                    ActivityCell::ExecResult(new_exec)
                )
                    if last_exec.title == new_exec.title
            );
            let same_error_family = matches!(
                (&*last, &cell),
                (ActivityCell::Error(last_error), ActivityCell::Error(new_error))
                    if strip_repeated_suffix(&last_error.title) == new_error.title
            );
            if same_exact || same_error_family || same_exec_pair {
                if same_exec_pair {
                    if let (
                        ActivityCell::ExecResult(last_exec),
                        ActivityCell::ExecResult(new_exec),
                    ) = (&mut *last, cell)
                    {
                        if new_exec.meta.is_some() {
                            last_exec.meta = new_exec.meta;
                        }
                        last_exec.output_lines = new_exec.output_lines;
                    }
                    continue;
                }
                if let ActivityCell::Error(last_error) = last {
                    if let Some((base, count)) = parse_repeated_suffix(&last_error.title) {
                        last_error.title = format!("{base} (x{})", count + 1);
                    } else {
                        last_error.title = format!("{} (x2)", last_error.title);
                    }
                    if same_error_family && let ActivityCell::Error(new_error) = cell {
                        last_error.body_lines = new_error.body_lines;
                    }
                }
                continue;
            }
        }
        merged.push(cell);
    }
    merged
}

fn parse_repeated_suffix(title: &str) -> Option<(String, usize)> {
    let marker = " (x";
    let start = title.rfind(marker)?;
    if !title.ends_with(')') {
        return None;
    }
    let count = title[start + marker.len()..title.len() - 1]
        .parse::<usize>()
        .ok()?;
    Some((title[..start].to_string(), count))
}

fn strip_repeated_suffix(title: &str) -> String {
    parse_repeated_suffix(title)
        .map(|(base, _)| base)
        .unwrap_or_else(|| title.to_string())
}

fn current_time_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_ui::{PlanUiData, TerminalUiAction, TerminalUiData, ToolUiEvent};

    fn terminal_event_view_with_attachment() -> EventView {
        EventView {
            event_id: uuid::Uuid::parse_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa")
                .expect("valid uuid"),
            source: crate::events::EventSource::Terminal,
            status: crate::events::EventStatus::Claimed,
            reply_message: None,
            arrived_at_ms: 1,
            payload: EventPayload::TerminalIncoming(crate::events::TerminalIncomingEvent {
                origin: "dashboard".to_string(),
                incoming_text: "show this".to_string(),
                attachments: vec![crate::events::TerminalIncomingAttachment {
                    kind: crate::events::TerminalIncomingAttachmentKind::Image,
                    media_type: "image/png".to_string(),
                    local_path: "/tmp/dashboard-image.png".to_string(),
                    description: Some("dashboard screenshot".to_string()),
                }],
            }),
            last_error: None,
        }
    }

    #[test]
    fn activity_feed_hides_runtime_context_messages_before_limit() {
        let mut messages = vec![HistoryMessage::user("real user message")];
        for _ in 0..20 {
            messages.push(HistoryMessage::user(
                "<preturn_context>\n<sensory>...</sensory>\n</preturn_context>",
            ));
        }
        messages.push(HistoryMessage::user(
            "<afterclaim_context>\n<claimed_input>...</claimed_input>\n</afterclaim_context>",
        ));

        let cells = render_activity_from_messages(messages);

        assert_eq!(cells.len(), 1);
        match &cells[0] {
            ActivityCell::User(cell) => assert_eq!(cell.title, "real user message"),
            _ => panic!("expected user activity cell"),
        }
    }

    #[test]
    fn empty_plan_ui_event_does_not_create_activity_cell() {
        let cell = activity_cell_from_tool_ui_event(ToolUiEvent::Plan(PlanUiData {
            kind: crate::tool_ui::PlanUiKind::Updated,
            explanation: None,
            steps: Vec::new(),
        }));

        assert!(cell.is_none());
    }

    #[test]
    fn terminal_poll_without_output_does_not_create_activity_cell() {
        let cell = activity_cell_from_tool_ui_event(ToolUiEvent::Terminal(TerminalUiData {
            action: TerminalUiAction::Poll,
            origin: None,
            title: "cargo test dashboard::command_render::tests".to_string(),
            body_lines: vec![
                r"terminal-session-8  running  exit=-  cwd=\\?\C:\Users\13940\DaatLocus"
                    .to_string(),
            ],
        }));

        assert!(cell.is_none());
    }

    #[test]
    fn terminal_poll_strips_session_metadata_from_visible_body() {
        let cell = activity_cell_from_tool_ui_event(ToolUiEvent::Terminal(TerminalUiData {
            action: TerminalUiAction::Poll,
            origin: None,
            title: "cargo test dashboard::command_render::tests".to_string(),
            body_lines: vec![
                r"terminal-session-8  running  exit=-  cwd=\\?\C:\Users\13940\DaatLocus"
                    .to_string(),
                "output_missed_bytes=0 output_dropped_bytes=12 output_retained_bytes=256 output_buffer_capacity=1024".to_string(),
                "Compiling reqwest v0.12.28".to_string(),
            ],
        }))
        .expect("terminal wait cell");

        match cell {
            ActivityCell::TerminalWait(cell) => {
                assert_eq!(
                    cell.meta.as_deref(),
                    Some(r"terminal-session-8  running  exit=-  cwd=\\?\C:\Users\13940\DaatLocus")
                );
                assert_eq!(cell.body_lines, vec!["Compiling reqwest v0.12.28"]);
            }
            _ => panic!("expected terminal wait activity cell"),
        }
    }

    #[test]
    fn user_activity_cell_preserves_long_multiline_input() {
        let message = (1..=12)
            .map(|index| format!("[定位段 {index:03}] marker-{index:03}"))
            .collect::<Vec<_>>()
            .join("\n");

        let cells = render_activity_from_messages(vec![HistoryMessage::user(message.clone())]);

        assert_eq!(cells.len(), 1);
        match &cells[0] {
            ActivityCell::User(cell) => {
                assert_eq!(cell.body_lines.len(), 11);
                assert!(
                    cell.body_lines
                        .iter()
                        .any(|line| line.contains("marker-012"))
                );
                assert_eq!(cell.full_body.as_deref(), Some(message.as_str()));
            }
            _ => panic!("expected user activity cell"),
        }
    }

    #[test]
    fn explored_calls_only_merge_while_contiguous() {
        let first_group = ActivityCell::Explored(ExploredActivityCell {
            stable_id: "explored".to_string(),
            title: "Explored".to_string(),
            calls: vec![ExploredCallActivityCell {
                tool_name: "Search".to_string(),
                action: None,
                target: None,
                secondary_target: None,
                summary: "first".to_string(),
                detail_lines: Vec::new(),
                detail_title: None,
            }],
        });
        let updated_group = ActivityCell::Explored(ExploredActivityCell {
            stable_id: "explored".to_string(),
            title: "Explored".to_string(),
            calls: vec![ExploredCallActivityCell {
                tool_name: "Read".to_string(),
                action: None,
                target: None,
                secondary_target: None,
                summary: "second".to_string(),
                detail_lines: Vec::new(),
                detail_title: None,
            }],
        });
        let boundary = ActivityCell::Assistant(AssistantActivityCell {
            title: "boundary".to_string(),
            body_lines: Vec::new(),
            full_body: None,
            rich_mode: true,
        });

        let contiguous = coalesce_activity_cells(vec![first_group.clone(), updated_group.clone()]);
        assert_eq!(contiguous.len(), 1);
        match &contiguous[0] {
            ActivityCell::Explored(group) => {
                assert_eq!(
                    group
                        .calls
                        .iter()
                        .map(|call| call.summary.as_str())
                        .collect::<Vec<_>>(),
                    vec!["first", "second"]
                );
            }
            _ => panic!("expected explored group"),
        }

        let separated = coalesce_activity_cells(vec![first_group, boundary, updated_group]);
        assert_eq!(separated.len(), 3);
    }

    #[test]
    fn explored_coalescing_preserves_all_calls() {
        let groups = (0..30)
            .map(|index| {
                ActivityCell::Explored(ExploredActivityCell {
                    stable_id: "explored".to_string(),
                    title: "Explored".to_string(),
                    calls: vec![ExploredCallActivityCell {
                        tool_name: "Search".to_string(),
                        action: None,
                        target: None,
                        secondary_target: None,
                        summary: format!("call-{index:02}"),
                        detail_lines: Vec::new(),
                        detail_title: None,
                    }],
                })
            })
            .collect::<Vec<_>>();

        let merged = coalesce_activity_cells(groups);

        assert_eq!(merged.len(), 1);
        let ActivityCell::Explored(group) = &merged[0] else {
            panic!("expected explored group");
        };
        assert_eq!(group.calls.len(), 30);
        assert_eq!(group.calls[0].summary, "call-00");
        assert_eq!(group.calls[29].summary, "call-29");
    }

    #[test]
    fn dashboard_error_cells_render_exposed_tool_names_as_app_scoped_display_names() {
        let cell = activity_cell_from_tool_ui_event(ToolUiEvent::error(
            "coding__edit_code failed",
            vec!["retry with coding__read_code first".to_string()],
        ))
        .expect("error cell");

        match cell {
            ActivityCell::Error(cell) => {
                assert_eq!(cell.title, "coding::edit_code failed");
                assert_eq!(cell.body_lines, vec!["retry with coding::read_code first"]);
            }
            _ => panic!("expected error activity cell"),
        }
    }

    #[test]
    fn assistant_tool_failures_render_exposed_tool_names_as_app_scoped_display_names() {
        let cell = assistant_activity_cell(
            "tool invocation failed: coding__edit_code\nhunk old text not found",
        )
        .expect("assistant error cell");

        match cell {
            ActivityCell::Error(cell) => {
                assert_eq!(cell.title, "tool invocation failed: coding::edit_code");
                assert_eq!(cell.body_lines, vec!["hunk old text not found"]);
            }
            _ => panic!("expected error activity cell"),
        }
    }

    #[test]
    fn assistant_model_request_failures_render_as_error_cells() {
        let cell = assistant_activity_cell(
            "agent turn failed: model provider returned HTTP 400 Bad Request\ninvalid schema",
        )
        .expect("assistant error cell");

        match cell {
            ActivityCell::Error(cell) => {
                assert_eq!(
                    cell.title,
                    "agent turn failed: model provider returned HTTP 400 Bad Request"
                );
                assert_eq!(cell.body_lines, vec!["invalid schema"]);
            }
            _ => panic!("expected error activity cell"),
        }
    }

    #[test]
    fn user_activity_cell_from_event_preserves_dashboard_attachments() {
        let cell = user_activity_cell_from_event(&terminal_event_view_with_attachment())
            .expect("user event cell");

        match cell {
            ActivityCell::User(cell) => {
                assert_eq!(cell.title, "show this");
                assert_eq!(cell.image_attachments.len(), 1);
                assert_eq!(cell.image_attachments[0].label, "dashboard screenshot");
                assert_eq!(cell.image_attachments[0].mime_type, "image/png");
                assert_eq!(
                    cell.image_attachments[0].uri,
                    "/dashboard/attachments/2f746d702f64617368626f6172642d696d6167652e706e67"
                );
            }
            _ => panic!("expected user activity cell"),
        }
    }
}
