mod apps;
mod common;
mod exec;
mod highlight;
mod messages;
mod plan;
mod primitives;
mod workflow;

use ratatui::{
    prelude::*,
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::{Paragraph, Wrap},
};
use serde::{Deserialize, Serialize};

use crate::{
    reasoning::runtime::{AgentMessage, HistoryMessage},
    tool_ui::{AppAttentionUiAction, BrowserUiData, TerminalUiAction, ToolUiEvent},
};

use super::DashboardState;
use apps::{AppAttentionActivityCell, BrowserActivityCell, LiveBrowserActivityCell};
use common::{
    AssistantActivityCell, ErrorActivityCell, GenericAppActivityCell, TerminalWaitActivityCell,
    UserActivityCell, assistant_cell, error_cell, terminal_wait_cell, user_cell,
};
use exec::{ExecResultActivityCell, LiveExecActivityCell, live_exec_cell};
use messages::{PatchActivityCell, ReplyActivityCell, TelegramActivityCell};
use plan::PlanActivityCell;
use primitives::Cell;
use workflow::{ActivateWorkflowActivityCell, CreateWorkflowActivityCell, DeepRecallActivityCell};

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LiveActivityCell {
    pub key: String,
    pub cell: ActivityCell,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ActivityCell {
    Assistant(AssistantActivityCell),
    User(UserActivityCell),
    AppAttention(AppAttentionActivityCell),
    Browser(BrowserActivityCell),
    LiveBrowser(LiveBrowserActivityCell),
    #[serde(alias = "ToolResult")]
    GenericApp(GenericAppActivityCell),
    PlanResult(PlanActivityCell),
    CreateWorkflowResult(CreateWorkflowActivityCell),
    ActivateWorkflowResult(ActivateWorkflowActivityCell),
    DeepRecallResult(DeepRecallActivityCell),
    ExecResult(ExecResultActivityCell),
    LiveExec(LiveExecActivityCell),
    Patch(PatchActivityCell),
    Telegram(TelegramActivityCell),
    Reply(ReplyActivityCell),
    TerminalWait(TerminalWaitActivityCell),
    Error(ErrorActivityCell),
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
        .filter(|message| !message.is_system())
        .rev()
        .take(12)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .flat_map(activity_cells_from_prompt_message)
        .collect::<Vec<_>>();
    coalesce_activity_cells(cells)
}

pub fn render_activity_feed(
    f: &mut Frame,
    area: Rect,
    cells: &[ActivityCell],
    live_cells: &[LiveActivityCell],
) {
    let lines = if cells.is_empty() && live_cells.is_empty() {
        vec![Line::from(vec![Span::styled(
            "No activity yet",
            Style::default().fg(Color::DarkGray),
        )])]
    } else {
        let mut visible_cells = cells.to_vec();
        visible_cells.extend(live_cells.iter().map(|cell| cell.cell.clone()));
        let mut lines = Vec::new();
        for (idx, cell) in visible_cells.iter().enumerate() {
            lines.extend(render_activity_cell_lines(cell));
            if idx + 1 < visible_cells.len() {
                lines.push(Line::from(""));
            }
        }
        lines
    };
    let text = if lines.is_empty() {
        Text::from(Line::from(vec![Span::styled(
            "No activity yet",
            Style::default().fg(Color::DarkGray),
        )]))
    } else {
        Text::from(lines)
    };
    let inner = Rect {
        x: area.x.saturating_add(1),
        y: area.y,
        width: area.width.saturating_sub(2),
        height: area.height,
    };
    let max_scroll = text
        .lines
        .len()
        .saturating_sub(inner.height.saturating_sub(1) as usize) as u16;
    let widget = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .scroll((max_scroll, 0));
    f.render_widget(widget, inner);
}

pub fn apply_activity_event(state: &mut DashboardState, event: DashboardActivityEvent) {
    match event {
        DashboardActivityEvent::AppendCommittedCells { mut cells } => {
            state.activity_cells.append(&mut cells);
            state.activity_cells = coalesce_activity_cells(state.activity_cells.clone());
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
}

pub fn assistant_activity_cell(content: &str) -> Option<ActivityCell> {
    if content.trim().is_empty() {
        return None;
    }
    if content.starts_with("工具调用失败") || content.starts_with("tool loop 调用失败") {
        return Some(ActivityCell::Error(error_cell(
            first_line_or_fallback(content, "tool invocation error"),
            remaining_lines_with_limit(content, 24),
        )));
    }
    Some(ActivityCell::Assistant(assistant_cell(
        first_line_or_fallback(content, "assistant"),
        remaining_lines_with_limit(content, 8),
    )))
}

pub fn activity_cell_from_tool_ui_event(ui_event: ToolUiEvent) -> Option<ActivityCell> {
    match ui_event {
        ToolUiEvent::Exec(event) => Some(ActivityCell::ExecResult(event.into())),
        ToolUiEvent::Terminal(event) => Some(if matches!(event.action, TerminalUiAction::Poll) {
            ActivityCell::TerminalWait(terminal_wait_cell(event.title, event.body_lines))
        } else {
            ActivityCell::ExecResult(event.into())
        }),
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
        ToolUiEvent::Patch(event) => Some(ActivityCell::Patch(event.into())),
        ToolUiEvent::Telegram(event) => Some(ActivityCell::Telegram(event.into())),
        ToolUiEvent::Reply(event) => Some(ActivityCell::Reply(event.into())),
        ToolUiEvent::AppAttention(event) => match event.action {
            AppAttentionUiAction::Focus => Some(ActivityCell::AppAttention(event.into())),
            AppAttentionUiAction::PutAway => None,
        },
        ToolUiEvent::Plan(event) => Some(ActivityCell::PlanResult(event.into())),
        ToolUiEvent::CreateWorkflow(event) => {
            Some(ActivityCell::CreateWorkflowResult(event.into()))
        }
        ToolUiEvent::ActivateWorkflow(event) => {
            Some(ActivityCell::ActivateWorkflowResult(event.into()))
        }
        ToolUiEvent::DeepRecall(event) => Some(ActivityCell::DeepRecallResult(event.into())),
        ToolUiEvent::App(event) => Some(ActivityCell::GenericApp(event.into())),
        ToolUiEvent::Error(event) => Some(ActivityCell::Error(event.into())),
    }
}

fn render_activity_cell_lines(cell: &ActivityCell) -> Vec<Line<'static>> {
    match cell {
        ActivityCell::Assistant(cell) => cell.render_lines(),
        ActivityCell::User(cell) => cell.render_lines(),
        ActivityCell::AppAttention(cell) => cell.render_lines(),
        ActivityCell::Browser(cell) => cell.render_lines(),
        ActivityCell::LiveBrowser(cell) => cell.render_lines(),
        ActivityCell::GenericApp(cell) => cell.render_lines(),
        ActivityCell::PlanResult(cell) => cell.render_lines(),
        ActivityCell::CreateWorkflowResult(cell) => cell.render_lines(),
        ActivityCell::ActivateWorkflowResult(cell) => cell.render_lines(),
        ActivityCell::DeepRecallResult(cell) => cell.render_lines(),
        ActivityCell::ExecResult(cell) => cell.render_lines(),
        ActivityCell::LiveExec(cell) => cell.render_lines(),
        ActivityCell::Patch(cell) => cell.render_lines(),
        ActivityCell::Telegram(cell) => cell.render_lines(),
        ActivityCell::Reply(cell) => cell.render_lines(),
        ActivityCell::TerminalWait(cell) => cell.render_lines(),
        ActivityCell::Error(cell) => cell.render_lines(),
    }
}

fn activity_cells_from_prompt_message(message: HistoryMessage) -> Vec<ActivityCell> {
    match &message.message {
        AgentMessage::Assistant { content } => {
            let mut cells = Vec::new();
            let is_tool_protocol_placeholder =
                content.trim().starts_with("assistant tool-call protocol:");
            if !content.trim().is_empty() && !is_tool_protocol_placeholder {
                cells.push(ActivityCell::Assistant(assistant_cell(
                    first_line_or_fallback(content, "assistant"),
                    remaining_lines_with_limit(content, 8),
                )));
            }
            if content.starts_with("工具调用失败") || content.starts_with("tool loop 调用失败")
            {
                return vec![ActivityCell::Error(error_cell(
                    first_line_or_fallback(content, "tool invocation error"),
                    remaining_lines_with_limit(content, 24),
                ))];
            }
            cells
        }
        AgentMessage::AssistantToolCallProtocol { .. } => Vec::new(),
        AgentMessage::Tool { .. } => message
            .tool_ui_event
            .and_then(activity_cell_from_tool_ui_event)
            .into_iter()
            .collect(),
        AgentMessage::User { content } => vec![ActivityCell::User(user_cell(
            first_line_or_fallback(content, "user"),
            remaining_lines_with_limit(content, 8),
        ))],
        AgentMessage::System { .. } => Vec::new(),
    }
}

fn first_line_or_fallback<'a>(content: &'a str, fallback: &'a str) -> &'a str {
    content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or(fallback)
}

fn remaining_lines_with_limit(content: &str, limit: usize) -> Vec<String> {
    let mut lines = content.lines();
    let _ = lines.next();
    lines
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(limit)
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
