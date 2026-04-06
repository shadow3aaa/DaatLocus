//! Dashboard: activity feed + command console.

use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEventKind};
use ratatui::{
    prelude::*,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Wrap},
};

use crate::{
    device::DeviceId,
    reasoning::runtime::{PromptMessage, PromptRole},
    spinova_paths::spinova_paths_sync,
    telegram_acl::TelegramAclHandle,
    tool_ui::{
        PatchFileUiData, PatchUiData, TelegramUiAction, TelegramUiData, TerminalUiAction,
        TerminalUiData, ToolCallUiEvent, ToolUiData, ToolUiEvent,
    },
};

pub struct DashboardState {
    pub focused_device: Option<DeviceId>,
    pub status_output: String,
    pub sleep_status_output: String,
    pub inspect_telegram_output: String,
    pub system_prompt_output: String,
    pub device_status_outputs: Vec<(String, String)>,
    pub activity_cells: Vec<ActivityCell>,
    pub live_activity_cells: Vec<LiveActivityCell>,
    pub last_cycle_elapsed_ms: Option<u128>,
    pub runtime_status: Option<String>,
    pub footer_context: String,
    pub footer_estimated_input_tokens: Option<usize>,
}

#[derive(Clone, Debug)]
pub enum DashboardControlCommand {
    RunSleep,
    ClearConversation,
}

#[derive(Clone, PartialEq, Eq)]
pub struct LiveActivityCell {
    pub key: String,
    pub cell: ActivityCell,
}

pub fn render_activity_from_messages(messages: Vec<PromptMessage>) -> Vec<ActivityCell> {
    let cells = messages
        .into_iter()
        .filter(|message| !matches!(message.role, PromptRole::System))
        .rev()
        .take(12)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .flat_map(activity_cells_from_prompt_message)
        .collect::<Vec<_>>();
    coalesce_activity_cells(cells)
}

#[derive(Clone, PartialEq, Eq)]
pub struct TextActivityCell {
    pub title: String,
    pub body_lines: Vec<String>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ExecActivityCell {
    pub title: String,
    pub call_lines: Vec<String>,
    pub meta: Option<String>,
    pub output_lines: Vec<String>,
    pub active: bool,
    pub started_at: Option<Instant>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct PatchActivityCell {
    pub title: String,
    pub summary_line: String,
    pub files: Vec<PatchFileUiData>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct TelegramActivityCell {
    pub title: String,
    pub detail_lines: Vec<String>,
    pub message_lines: Vec<String>,
}

#[derive(Clone, PartialEq, Eq)]
pub enum ActivityCell {
    Assistant(TextActivityCell),
    User(TextActivityCell),
    ToolCall(TextActivityCell),
    ToolResult(TextActivityCell),
    Exec(ExecActivityCell),
    Patch(PatchActivityCell),
    Telegram(TelegramActivityCell),
    TerminalWait(TextActivityCell),
    Error(TextActivityCell),
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
    ExecUpdate {
        key: String,
        meta: Option<String>,
        output_lines: Vec<String>,
    },
    ExecEnd {
        key: String,
    },
}

fn text_cell(title: impl Into<String>, body_lines: Vec<String>) -> TextActivityCell {
    TextActivityCell {
        title: title.into(),
        body_lines,
    }
}

fn tool_ui_text_cell(data: ToolUiData) -> TextActivityCell {
    text_cell(data.title, data.body_lines)
}

fn exec_cell_from_ui(data: ToolUiData) -> ExecActivityCell {
    let mut body_lines = data.body_lines;
    let meta = if body_lines.is_empty() {
        None
    } else {
        Some(body_lines.remove(0))
    };
    ExecActivityCell {
        title: data.title,
        call_lines: Vec::new(),
        meta,
        output_lines: body_lines,
        active: false,
        started_at: None,
    }
}

fn exec_call_cell_from_ui(data: ToolUiData) -> ExecActivityCell {
    ExecActivityCell {
        title: data.title,
        call_lines: data.body_lines,
        meta: None,
        output_lines: Vec::new(),
        active: true,
        started_at: Some(Instant::now()),
    }
}

fn patch_cell_from_ui(data: PatchUiData) -> PatchActivityCell {
    PatchActivityCell {
        title: data.title,
        summary_line: data.summary_line,
        files: data.files,
    }
}

fn telegram_cell_from_ui(data: TelegramUiData) -> TelegramActivityCell {
    let mut detail_lines = data.detail_lines;
    if detail_lines.is_empty() {
        detail_lines.push(match data.action {
            TelegramUiAction::ListChats => "list chats".to_string(),
            TelegramUiAction::ReadHistory => "read history".to_string(),
            TelegramUiAction::SelectChat => "select chat".to_string(),
            TelegramUiAction::SendMessage => "send message".to_string(),
            TelegramUiAction::ResolveChat => "resolve chat".to_string(),
        });
    }
    TelegramActivityCell {
        title: data.title,
        detail_lines,
        message_lines: data.message_lines,
    }
}

fn activity_cells_from_prompt_message(message: PromptMessage) -> Vec<ActivityCell> {
    match message.role {
        PromptRole::Assistant => {
            let mut cells = Vec::new();
            if !message.content.trim().is_empty() {
                cells.push(ActivityCell::Assistant(text_cell(
                    first_line_or_fallback(&message.content, "assistant"),
                    remaining_lines_with_limit(&message.content, 8),
                )));
            }
            if !message.tool_call_ui_events.is_empty() {
                cells.extend(
                    message
                        .tool_call_ui_events
                        .into_iter()
                        .flat_map(activity_cells_from_tool_call_ui_event),
                );
                return cells;
            }
            if message.content.starts_with("工具调用失败")
                || message.content.starts_with("tool loop 调用失败")
            {
                return vec![ActivityCell::Error(text_cell(
                    first_line_or_fallback(&message.content, "tool invocation error"),
                    remaining_lines_with_limit(&message.content, 24),
                ))];
            }
            cells
        }
        PromptRole::Tool => vec![activity_cell_from_tool_ui_event(
            message
                .tool_ui_event
                .expect("tool prompt messages must carry ToolUiEvent"),
        )],
        PromptRole::User => vec![ActivityCell::User(text_cell(
            first_line_or_fallback(&message.content, "user"),
            remaining_lines_with_limit(&message.content, 8),
        ))],
        PromptRole::System => Vec::new(),
    }
}

pub fn activity_cells_from_tool_call_ui_event(ui_event: ToolCallUiEvent) -> Vec<ActivityCell> {
    match ui_event {
        ToolCallUiEvent::Exec(event) => {
            vec![ActivityCell::Exec(exec_call_cell_from_ui(ToolUiData {
                title: event.title,
                body_lines: event.body_lines,
            }))]
        }
        ToolCallUiEvent::Terminal(event) => {
            if matches!(event.action, TerminalUiAction::Poll) {
                Vec::new()
            } else {
                vec![ActivityCell::Exec(exec_call_cell_from_terminal_ui(event))]
            }
        }
        ToolCallUiEvent::Patch(event) => vec![ActivityCell::Patch(patch_cell_from_ui(event))],
        ToolCallUiEvent::Telegram(event) => {
            vec![ActivityCell::Telegram(telegram_cell_from_ui(event))]
        }
        ToolCallUiEvent::Work(event) | ToolCallUiEvent::Device(event) => {
            vec![ActivityCell::ToolCall(tool_ui_text_cell(ToolUiData {
                title: event.title,
                body_lines: event.body_lines,
            }))]
        }
        ToolCallUiEvent::Error(event) => vec![ActivityCell::Error(tool_ui_text_cell(ToolUiData {
            title: event.title,
            body_lines: event.body_lines,
        }))],
    }
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
                cell: ActivityCell::Exec(ExecActivityCell {
                    title,
                    call_lines,
                    meta: None,
                    output_lines: Vec::new(),
                    active: true,
                    started_at: Some(Instant::now()),
                }),
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
                cell: ActivityCell::Exec(ExecActivityCell {
                    title: String::new(),
                    call_lines: Vec::new(),
                    meta,
                    output_lines,
                    active: true,
                    started_at: None,
                }),
            },
        ),
        DashboardActivityEvent::ExecEnd { key } => {
            state.live_activity_cells.retain(|cell| cell.key != key);
        }
    }
}

pub fn assistant_activity_cell(content: &str) -> Option<ActivityCell> {
    if content.trim().is_empty() {
        return None;
    }
    if content.starts_with("工具调用失败") || content.starts_with("tool loop 调用失败") {
        return Some(ActivityCell::Error(text_cell(
            first_line_or_fallback(content, "tool invocation error"),
            remaining_lines_with_limit(content, 24),
        )));
    }
    Some(ActivityCell::Assistant(text_cell(
        first_line_or_fallback(content, "assistant"),
        remaining_lines_with_limit(content, 8),
    )))
}

fn upsert_live_activity_cell(cells: &mut Vec<LiveActivityCell>, incoming: LiveActivityCell) {
    if let Some(existing) = cells.iter_mut().find(|cell| cell.key == incoming.key) {
        match (&mut existing.cell, incoming.cell) {
            (ActivityCell::Exec(existing_exec), ActivityCell::Exec(incoming_exec)) => {
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
                existing_exec.active = incoming_exec.active;
                if existing_exec.started_at.is_none() {
                    existing_exec.started_at = incoming_exec.started_at;
                }
            }
            (slot, cell) => *slot = cell,
        }
    } else {
        cells.push(incoming);
    }
}

pub fn activity_cell_from_tool_ui_event(ui_event: ToolUiEvent) -> ActivityCell {
    match ui_event {
        ToolUiEvent::Exec(event) => ActivityCell::Exec(exec_cell_from_ui(ToolUiData {
            title: event.title,
            body_lines: event.body_lines,
        })),
        ToolUiEvent::Terminal(event) => {
            if matches!(event.action, TerminalUiAction::Poll) {
                ActivityCell::TerminalWait(text_cell(event.title, event.body_lines))
            } else {
                ActivityCell::Exec(exec_cell_from_terminal_ui(event))
            }
        }
        ToolUiEvent::Patch(event) => ActivityCell::Patch(patch_cell_from_ui(event)),
        ToolUiEvent::Telegram(event) => ActivityCell::Telegram(telegram_cell_from_ui(event)),
        ToolUiEvent::Work(event) | ToolUiEvent::Device(event) => {
            ActivityCell::ToolResult(tool_ui_text_cell(ToolUiData {
                title: event.title,
                body_lines: event.body_lines,
            }))
        }
        ToolUiEvent::Error(event) => ActivityCell::Error(tool_ui_text_cell(ToolUiData {
            title: event.title,
            body_lines: event.body_lines,
        })),
    }
}

fn exec_call_cell_from_terminal_ui(data: TerminalUiData) -> ExecActivityCell {
    ExecActivityCell {
        title: data.title,
        call_lines: data.body_lines,
        meta: None,
        output_lines: Vec::new(),
        active: !matches!(data.action, TerminalUiAction::Terminate),
        started_at: if matches!(data.action, TerminalUiAction::Terminate) {
            None
        } else {
            Some(Instant::now())
        },
    }
}

fn exec_cell_from_terminal_ui(data: TerminalUiData) -> ExecActivityCell {
    let mut body_lines = data.body_lines;
    let meta = if body_lines.is_empty() {
        None
    } else {
        Some(body_lines.remove(0))
    };
    ExecActivityCell {
        title: data.title,
        call_lines: Vec::new(),
        meta,
        output_lines: body_lines,
        active: false,
        started_at: None,
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

fn coalesce_activity_cells(cells: Vec<ActivityCell>) -> Vec<ActivityCell> {
    let mut merged: Vec<ActivityCell> = Vec::new();
    for cell in cells {
        if let Some(last) = merged.last_mut() {
            let same_exact = *last == cell;
            let same_exec_pair = matches!(
                (&mut *last, &cell),
                (ActivityCell::Exec(last_exec), ActivityCell::Exec(new_exec))
                    if last_exec.title == new_exec.title
            );
            let same_error_family = matches!(
                (&*last, &cell),
                (ActivityCell::Error(last_error), ActivityCell::Error(new_error))
                    if strip_repeated_suffix(&last_error.title) == new_error.title
            );
            if same_exact || same_error_family || same_exec_pair {
                if same_exec_pair {
                    if let (ActivityCell::Exec(last_exec), ActivityCell::Exec(new_exec)) =
                        (&mut *last, cell)
                    {
                        if !new_exec.call_lines.is_empty() {
                            last_exec.call_lines.extend(new_exec.call_lines);
                        }
                        if new_exec.meta.is_some() {
                            last_exec.meta = new_exec.meta;
                        }
                        last_exec.output_lines = new_exec.output_lines;
                        last_exec.active = new_exec.active;
                        if new_exec.started_at.is_some() {
                            last_exec.started_at = new_exec.started_at;
                        } else if !last_exec.active {
                            last_exec.started_at = None;
                        }
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

struct CommandOverlay {
    title: String,
    text: String,
    scroll: u16,
}

struct DashboardCommandContext<'a> {
    requests: &'a [crate::telegram_acl::PendingAccessRequest],
    telegram_acl: &'a TelegramAclHandle,
    state: &'a DashboardState,
    control_tx: &'a tokio::sync::mpsc::UnboundedSender<DashboardControlCommand>,
}

#[derive(Clone)]
struct CommandSuggestion {
    display: String,
    completion: String,
    description: String,
}

enum DashboardCommandResult {
    ShowOverlay { title: String, text: String },
    Quit,
}

trait DashboardCommand: Sync {
    fn usage(&self) -> &'static str;
    fn description(&self) -> &'static str;

    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    fn primary_verb(&self) -> &'static str {
        self.usage().split_whitespace().next().unwrap_or_default()
    }

    fn accepts(&self, verb: &str) -> bool {
        self.primary_verb() == verb || self.aliases().contains(&verb)
    }

    fn overlay_title(&self, raw: &str) -> String {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            "RESULT".to_string()
        } else {
            trimmed.to_uppercase()
        }
    }

    fn execute(
        &self,
        parts: &[&str],
        raw: &str,
        context: &DashboardCommandContext<'_>,
    ) -> DashboardCommandResult;

    fn subcommands(&self) -> &'static [&'static dyn DashboardSubcommand] {
        &[]
    }
}

trait DashboardSubcommand: Sync {
    fn usage(&self) -> &'static str;
    fn description(&self) -> &'static str;

    fn name(&self) -> &'static str {
        self.usage().split_whitespace().next().unwrap_or_default()
    }

    fn execute(
        &self,
        parts: &[&str],
        raw: &str,
        context: &DashboardCommandContext<'_>,
    ) -> DashboardCommandResult;
}

struct QuitCommand;
struct ClearCommand;
struct PersonaCommand;
struct SystemPromptCommand;
struct DeviceStatusCommand;
struct StatusCommand;
struct SleepCommand;
struct SleepRunSubcommand;
struct SleepStatusSubcommand;
struct TelegramCommand;
struct TelegramStatusSubcommand;
struct TelegramApproveSubcommand;
struct TelegramRejectSubcommand;

static QUIT_COMMAND: QuitCommand = QuitCommand;
static CLEAR_COMMAND: ClearCommand = ClearCommand;
static PERSONA_COMMAND: PersonaCommand = PersonaCommand;
static SYSTEM_PROMPT_COMMAND: SystemPromptCommand = SystemPromptCommand;
static DEVICE_STATUS_COMMAND: DeviceStatusCommand = DeviceStatusCommand;
static STATUS_COMMAND: StatusCommand = StatusCommand;
static SLEEP_COMMAND: SleepCommand = SleepCommand;
static SLEEP_RUN_SUBCOMMAND: SleepRunSubcommand = SleepRunSubcommand;
static SLEEP_STATUS_SUBCOMMAND: SleepStatusSubcommand = SleepStatusSubcommand;
static TELEGRAM_COMMAND: TelegramCommand = TelegramCommand;
static TELEGRAM_STATUS_SUBCOMMAND: TelegramStatusSubcommand = TelegramStatusSubcommand;
static TELEGRAM_APPROVE_SUBCOMMAND: TelegramApproveSubcommand = TelegramApproveSubcommand;
static TELEGRAM_REJECT_SUBCOMMAND: TelegramRejectSubcommand = TelegramRejectSubcommand;
static SLEEP_SUBCOMMANDS: [&dyn DashboardSubcommand; 2] =
    [&SLEEP_RUN_SUBCOMMAND, &SLEEP_STATUS_SUBCOMMAND];
static TELEGRAM_SUBCOMMANDS: [&dyn DashboardSubcommand; 3] = [
    &TELEGRAM_STATUS_SUBCOMMAND,
    &TELEGRAM_APPROVE_SUBCOMMAND,
    &TELEGRAM_REJECT_SUBCOMMAND,
];

static DASHBOARD_COMMANDS: [&dyn DashboardCommand; 8] = [
    &QUIT_COMMAND,
    &CLEAR_COMMAND,
    &PERSONA_COMMAND,
    &SYSTEM_PROMPT_COMMAND,
    &DEVICE_STATUS_COMMAND,
    &STATUS_COMMAND,
    &SLEEP_COMMAND,
    &TELEGRAM_COMMAND,
];

impl DashboardCommand for QuitCommand {
    fn usage(&self) -> &'static str {
        "quit"
    }

    fn description(&self) -> &'static str {
        "exit the dashboard"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["q", "exit"]
    }

    fn execute(
        &self,
        _: &[&str],
        _: &str,
        _: &DashboardCommandContext<'_>,
    ) -> DashboardCommandResult {
        DashboardCommandResult::Quit
    }
}

impl DashboardCommand for StatusCommand {
    fn usage(&self) -> &'static str {
        "status"
    }

    fn description(&self) -> &'static str {
        "show overall status"
    }

    fn execute(
        &self,
        _: &[&str],
        raw: &str,
        context: &DashboardCommandContext<'_>,
    ) -> DashboardCommandResult {
        DashboardCommandResult::ShowOverlay {
            title: self.overlay_title(raw),
            text: fallback_output(&context.state.status_output),
        }
    }
}

impl DashboardCommand for ClearCommand {
    fn usage(&self) -> &'static str {
        "clear"
    }

    fn description(&self) -> &'static str {
        "clear runtime conversation history"
    }

    fn execute(
        &self,
        _: &[&str],
        raw: &str,
        context: &DashboardCommandContext<'_>,
    ) -> DashboardCommandResult {
        match context
            .control_tx
            .send(DashboardControlCommand::ClearConversation)
        {
            Ok(()) => DashboardCommandResult::ShowOverlay {
                title: raw.trim().to_uppercase(),
                text: "queued runtime conversation clear".to_string(),
            },
            Err(err) => DashboardCommandResult::ShowOverlay {
                title: raw.trim().to_uppercase(),
                text: format!("failed to queue clear command: {err}"),
            },
        }
    }
}

impl DashboardCommand for PersonaCommand {
    fn usage(&self) -> &'static str {
        "persona"
    }

    fn description(&self) -> &'static str {
        "show current prompt persona config"
    }

    fn execute(
        &self,
        _: &[&str],
        raw: &str,
        _: &DashboardCommandContext<'_>,
    ) -> DashboardCommandResult {
        let path = spinova_paths_sync().config_file("prompt_persona.toml");
        let text = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(err) => format!("failed to read {}: {err}", path.display()),
        };
        DashboardCommandResult::ShowOverlay {
            title: raw.trim().to_uppercase(),
            text,
        }
    }
}

impl DashboardCommand for SystemPromptCommand {
    fn usage(&self) -> &'static str {
        "system-prompt"
    }

    fn description(&self) -> &'static str {
        "show current runtime system prompt"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["system_prompt"]
    }

    fn execute(
        &self,
        _: &[&str],
        raw: &str,
        context: &DashboardCommandContext<'_>,
    ) -> DashboardCommandResult {
        DashboardCommandResult::ShowOverlay {
            title: raw.trim().to_uppercase(),
            text: fallback_output(&context.state.system_prompt_output),
        }
    }
}

impl DashboardCommand for DeviceStatusCommand {
    fn usage(&self) -> &'static str {
        "device-status <device>"
    }

    fn description(&self) -> &'static str {
        "show current structured device state and llm-facing note"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["device_status"]
    }

    fn execute(
        &self,
        parts: &[&str],
        raw: &str,
        context: &DashboardCommandContext<'_>,
    ) -> DashboardCommandResult {
        let Some(target) = parts.get(1).copied() else {
            let devices = context
                .state
                .device_status_outputs
                .iter()
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>();
            return DashboardCommandResult::ShowOverlay {
                title: self.overlay_title(raw),
                text: if devices.is_empty() {
                    "available devices: none".to_string()
                } else {
                    format!("available devices: {}", devices.join(", "))
                },
            };
        };
        let target = target.trim().to_ascii_lowercase();
        let output = context
            .state
            .device_status_outputs
            .iter()
            .find(|(name, _)| name == &target)
            .map(|(_, output)| output.clone());
        DashboardCommandResult::ShowOverlay {
            title: self.overlay_title(raw),
            text: output.unwrap_or_else(|| {
                let devices = context
                    .state
                    .device_status_outputs
                    .iter()
                    .map(|(name, _)| name.clone())
                    .collect::<Vec<_>>();
                if devices.is_empty() {
                    format!("unknown device: {target}")
                } else {
                    format!("unknown device: {target}\navailable devices: {}", devices.join(", "))
                }
            }),
        }
    }
}

impl DashboardCommand for SleepCommand {
    fn usage(&self) -> &'static str {
        "sleep"
    }

    fn description(&self) -> &'static str {
        "sleep controls and status"
    }

    fn subcommands(&self) -> &'static [&'static dyn DashboardSubcommand] {
        &SLEEP_SUBCOMMANDS
    }

    fn execute(
        &self,
        parts: &[&str],
        raw: &str,
        context: &DashboardCommandContext<'_>,
    ) -> DashboardCommandResult {
        let Some(subcommand_name) = parts.get(1).copied() else {
            return DashboardCommandResult::ShowOverlay {
                title: self.overlay_title(raw),
                text: "available:\n  sleep run\n  sleep status".to_string(),
            };
        };
        if let Some(subcommand) = self
            .subcommands()
            .iter()
            .copied()
            .find(|subcommand| subcommand.name() == subcommand_name)
        {
            subcommand.execute(parts, raw, context)
        } else {
            DashboardCommandResult::ShowOverlay {
                title: self.overlay_title(raw),
                text: format!("unknown sleep subcommand: {subcommand_name}"),
            }
        }
    }
}

impl DashboardSubcommand for SleepRunSubcommand {
    fn usage(&self) -> &'static str {
        "run"
    }

    fn description(&self) -> &'static str {
        "start a background sleep run"
    }

    fn execute(
        &self,
        _: &[&str],
        raw: &str,
        context: &DashboardCommandContext<'_>,
    ) -> DashboardCommandResult {
        match context.control_tx.send(DashboardControlCommand::RunSleep) {
            Ok(()) => DashboardCommandResult::ShowOverlay {
                title: raw.trim().to_uppercase(),
                text: "queued sleep run".to_string(),
            },
            Err(err) => DashboardCommandResult::ShowOverlay {
                title: raw.trim().to_uppercase(),
                text: format!("failed to queue sleep run: {err}"),
            },
        }
    }
}

impl DashboardSubcommand for SleepStatusSubcommand {
    fn usage(&self) -> &'static str {
        "status"
    }

    fn description(&self) -> &'static str {
        "show sleep status"
    }

    fn execute(
        &self,
        _parts: &[&str],
        raw: &str,
        context: &DashboardCommandContext<'_>,
    ) -> DashboardCommandResult {
        DashboardCommandResult::ShowOverlay {
            title: raw.trim().to_uppercase(),
            text: fallback_output(&context.state.sleep_status_output),
        }
    }
}

impl DashboardCommand for TelegramCommand {
    fn usage(&self) -> &'static str {
        "telegram"
    }

    fn description(&self) -> &'static str {
        "telegram status and access controls"
    }

    fn subcommands(&self) -> &'static [&'static dyn DashboardSubcommand] {
        &TELEGRAM_SUBCOMMANDS
    }

    fn execute(
        &self,
        parts: &[&str],
        raw: &str,
        context: &DashboardCommandContext<'_>,
    ) -> DashboardCommandResult {
        let Some(subcommand_name) = parts.get(1).copied() else {
            return DashboardCommandResult::ShowOverlay {
                title: self.overlay_title(raw),
                text: "available:\n  telegram status\n  telegram approve <index|chat_id>\n  telegram reject <index|chat_id>".to_string(),
            };
        };
        if let Some(subcommand) = self
            .subcommands()
            .iter()
            .copied()
            .find(|subcommand| subcommand.name() == subcommand_name)
        {
            subcommand.execute(parts, raw, context)
        } else {
            DashboardCommandResult::ShowOverlay {
                title: self.overlay_title(raw),
                text: format!("unknown telegram subcommand: {subcommand_name}"),
            }
        }
    }
}

impl DashboardSubcommand for TelegramStatusSubcommand {
    fn usage(&self) -> &'static str {
        "status"
    }

    fn description(&self) -> &'static str {
        "show telegram details"
    }

    fn execute(
        &self,
        parts: &[&str],
        raw: &str,
        context: &DashboardCommandContext<'_>,
    ) -> DashboardCommandResult {
        DashboardCommandResult::ShowOverlay {
            title: raw.trim().to_uppercase(),
            text: match parts.get(1).copied() {
                Some("status") => fallback_output(&context.state.inspect_telegram_output),
                _ => "unknown telegram subcommand: status".to_string(),
            },
        }
    }
}

impl DashboardSubcommand for TelegramApproveSubcommand {
    fn usage(&self) -> &'static str {
        "approve <index|chat_id>"
    }

    fn description(&self) -> &'static str {
        "approve a telegram access request"
    }

    fn execute(
        &self,
        parts: &[&str],
        raw: &str,
        context: &DashboardCommandContext<'_>,
    ) -> DashboardCommandResult {
        DashboardCommandResult::ShowOverlay {
            title: raw.trim().to_uppercase(),
            text: execute_access_request_command(true, parts, context),
        }
    }
}

impl DashboardSubcommand for TelegramRejectSubcommand {
    fn usage(&self) -> &'static str {
        "reject <index|chat_id>"
    }

    fn description(&self) -> &'static str {
        "reject a telegram access request"
    }

    fn execute(
        &self,
        parts: &[&str],
        raw: &str,
        context: &DashboardCommandContext<'_>,
    ) -> DashboardCommandResult {
        DashboardCommandResult::ShowOverlay {
            title: raw.trim().to_uppercase(),
            text: execute_access_request_command(false, parts, context),
        }
    }
}

fn dashboard_commands() -> &'static [&'static dyn DashboardCommand] {
    &DASHBOARD_COMMANDS
}

fn execute_access_request_command(
    approve: bool,
    parts: &[&str],
    context: &DashboardCommandContext<'_>,
) -> String {
    let Some(target) = parts.get(2).copied() else {
        return format!(
            "{} requires <index|chat_id>",
            if approve {
                "telegram approve"
            } else {
                "telegram reject"
            }
        );
    };

    let chat_id = if let Ok(index) = target.parse::<usize>() {
        context
            .requests
            .get(index.saturating_sub(1))
            .map(|request| request.chat_id)
    } else {
        target.parse::<i64>().ok()
    };

    let Some(chat_id) = chat_id else {
        return format!("no pending request at index {}", target);
    };

    let result = if approve {
        context.telegram_acl.approve(chat_id)
    } else {
        context.telegram_acl.reject(chat_id)
    };
    match result {
        Ok(()) => format!(
            "{} {}",
            if approve { "approved" } else { "rejected" },
            chat_id
        ),
        Err(err) => format!(
            "{} failed for {}: {err}",
            if approve { "approve" } else { "reject" },
            chat_id
        ),
    }
}

pub async fn run_tui_dashboard(
    rx: &mut tokio::sync::watch::Receiver<DashboardState>,
    telegram_acl: TelegramAclHandle,
    control_tx: tokio::sync::mpsc::UnboundedSender<DashboardControlCommand>,
) -> Result<(), std::io::Error> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut command_input = String::new();
    let mut command_popup_selection: usize = 0;
    let mut command_popup_scroll: usize = 0;
    let mut command_overlay: Option<CommandOverlay> = None;

    loop {
        let pending_requests = telegram_acl.pending_requests();

        if crossterm::event::poll(Duration::from_millis(16))?
            && let Event::Key(key) = crossterm::event::read()?
            && key.kind == KeyEventKind::Press
        {
            if let Some(overlay) = command_overlay.as_mut() {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        command_overlay = None;
                    }
                    KeyCode::Up => {
                        overlay.scroll = overlay.scroll.saturating_sub(1);
                    }
                    KeyCode::Down => {
                        overlay.scroll = overlay.scroll.saturating_add(1);
                    }
                    KeyCode::PageUp => {
                        overlay.scroll = overlay.scroll.saturating_sub(10);
                    }
                    KeyCode::PageDown => {
                        overlay.scroll = overlay.scroll.saturating_add(10);
                    }
                    KeyCode::Home => {
                        overlay.scroll = 0;
                    }
                    KeyCode::End => {
                        overlay.scroll = u16::MAX;
                    }
                    _ => {}
                }
                continue;
            }
            match key.code {
                KeyCode::Char(c) => {
                    command_input.push(c);
                    command_popup_selection = 0;
                    command_popup_scroll = 0;
                }
                KeyCode::Tab => {
                    if let Some(completion) =
                        selected_command_completion(&command_input, command_popup_selection)
                    {
                        command_input = completion;
                        command_popup_selection = 0;
                        command_popup_scroll = 0;
                    }
                }
                KeyCode::Backspace => {
                    command_input.pop();
                    command_popup_selection = 0;
                    command_popup_scroll = 0;
                }
                KeyCode::Up => {
                    let matches = matching_commands(&command_input);
                    if !matches.is_empty() {
                        command_popup_selection = command_popup_selection
                            .saturating_sub(1)
                            .min(matches.len() - 1);
                        command_popup_scroll = adjusted_popup_scroll(
                            command_popup_scroll,
                            command_popup_selection,
                            matches.len(),
                        );
                    }
                }
                KeyCode::Down => {
                    let matches = matching_commands(&command_input);
                    if !matches.is_empty() {
                        command_popup_selection =
                            (command_popup_selection + 1).min(matches.len() - 1);
                        command_popup_scroll = adjusted_popup_scroll(
                            command_popup_scroll,
                            command_popup_selection,
                            matches.len(),
                        );
                    }
                }
                KeyCode::Esc => {
                    command_input.clear();
                    command_popup_selection = 0;
                    command_popup_scroll = 0;
                }
                KeyCode::Enter => {
                    if let Some(completion) =
                        selected_command_completion(&command_input, command_popup_selection)
                        && completion != command_input
                    {
                        command_input = completion;
                        command_popup_selection = 0;
                        command_popup_scroll = 0;
                        continue;
                    }
                    let command = command_input.trim().to_string();
                    if !command.is_empty() {
                        let state = rx.borrow();
                        match execute_dashboard_command(
                            &command,
                            &pending_requests,
                            &telegram_acl,
                            &state,
                            &control_tx,
                        ) {
                            DashboardCommandResult::ShowOverlay { title, text } => {
                                command_overlay = Some(CommandOverlay {
                                    title,
                                    text,
                                    scroll: 0,
                                });
                            }
                            DashboardCommandResult::Quit => break,
                        }
                    }
                    command_input.clear();
                    command_popup_selection = 0;
                    command_popup_scroll = 0;
                }
                _ => {}
            }
        }

        let state = rx.borrow();
        let popup_rows = if command_overlay.is_none() {
            command_popup_row_count(&command_input)
        } else {
            0
        };

        terminal.draw(|f| {
            let root = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(18), Constraint::Length(4 + popup_rows)])
                .split(f.area());
            render_activity_feed(
                f,
                root[0],
                &state.activity_cells,
                &state.live_activity_cells,
            );
            if let Some(overlay) = command_overlay.as_ref() {
                render_command_overlay(f, root[0], overlay);
            }
            render_command_bar(
                f,
                root[1],
                &command_input,
                state.runtime_status.as_deref(),
                &state.footer_context,
                command_overlay.is_some(),
                command_popup_selection,
                command_popup_scroll,
            );
        })?;
    }

    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen
    )?;
    Ok(())
}

fn execute_dashboard_command(
    command: &str,
    requests: &[crate::telegram_acl::PendingAccessRequest],
    telegram_acl: &TelegramAclHandle,
    state: &DashboardState,
    control_tx: &tokio::sync::mpsc::UnboundedSender<DashboardControlCommand>,
) -> DashboardCommandResult {
    let parts = command.split_whitespace().collect::<Vec<_>>();
    if parts.is_empty() {
        return DashboardCommandResult::ShowOverlay {
            title: "EMPTY".to_string(),
            text: "empty command".to_string(),
        };
    }

    let context = DashboardCommandContext {
        requests,
        telegram_acl,
        state,
        control_tx,
    };

    if let Some(command_impl) = dashboard_commands()
        .iter()
        .copied()
        .find(|command_impl| command_impl.accepts(parts[0]))
    {
        command_impl.execute(&parts, command, &context)
    } else {
        DashboardCommandResult::ShowOverlay {
            title: "UNKNOWN COMMAND".to_string(),
            text: format!("unknown command: {}", parts[0]),
        }
    }
}

pub(crate) fn execute_remote_command(
    command: &str,
    telegram_acl: &TelegramAclHandle,
    state: &DashboardState,
    control_tx: &tokio::sync::mpsc::UnboundedSender<DashboardControlCommand>,
) -> String {
    let result = execute_dashboard_command(
        command,
        &telegram_acl.pending_requests(),
        telegram_acl,
        state,
        control_tx,
    );
    match result {
        DashboardCommandResult::ShowOverlay { text, .. } => text,
        DashboardCommandResult::Quit => {
            "quit command is only available in the local dashboard".to_string()
        }
    }
}

fn panel(title: impl Into<Line<'static>>) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(title.into())
        .title_style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .padding(Padding::new(1, 1, 0, 0))
}

fn render_activity_feed(
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

fn render_activity_cell_lines(cell: &ActivityCell) -> Vec<Line<'static>> {
    match cell {
        ActivityCell::Exec(cell) => render_exec_activity_cell(cell),
        ActivityCell::Patch(cell) => render_patch_activity_cell(cell),
        ActivityCell::ToolCall(cell) => render_tool_call_activity_cell(cell),
        ActivityCell::ToolResult(cell) => render_tool_result_activity_cell(cell),
        ActivityCell::Assistant(cell) => render_assistant_activity_cell(cell),
        ActivityCell::User(cell) => render_user_activity_cell(cell),
        ActivityCell::Telegram(cell) => render_telegram_activity_cell(cell),
        ActivityCell::TerminalWait(cell) => render_terminal_wait_activity_cell(cell),
        ActivityCell::Error(cell) => render_error_activity_cell(cell),
    }
}

fn render_assistant_activity_cell(cell: &TextActivityCell) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled(
            "›",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            cell.title.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ])];
    for line in cell.body_lines.iter().take(8) {
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(line.to_string(), Style::default().fg(Color::Gray)),
        ]));
    }
    lines
}

fn render_tool_call_activity_cell(cell: &TextActivityCell) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled(
            "→",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            cell.title.clone(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
    ])];
    for line in cell.body_lines.iter().take(6) {
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled("· ", Style::default().fg(Color::DarkGray)),
            Span::styled(line.to_string(), Style::default().fg(Color::Gray)),
        ]));
    }
    lines
}

fn render_exec_activity_cell(cell: &ExecActivityCell) -> Vec<Line<'static>> {
    let elapsed = cell.started_at.map(|started_at| started_at.elapsed());
    let exit_code = cell.meta.as_deref().and_then(parse_exit_code_from_meta);
    let (indicator, indicator_style) = if cell.active {
        (
            exec_spinner(elapsed),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
    } else if exit_code == Some(0) {
        (
            "•".to_string(),
            Style::default()
                .fg(Color::LightGreen)
                .add_modifier(Modifier::BOLD),
        )
    } else if exit_code.is_some() {
        (
            "•".to_string(),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )
    } else {
        (
            "•".to_string(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
    };
    let verb = if cell.active { "Running" } else { "Ran" };
    let mut lines = vec![Line::from(vec![
        Span::styled(indicator, indicator_style),
        Span::raw("  "),
        Span::styled(
            verb,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(cell.title.clone(), Style::default().fg(Color::White)),
    ])];
    if cell.active && cell.output_lines.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("  └ ", Style::default().fg(Color::DarkGray)),
            Span::styled("running...", Style::default().fg(Color::DarkGray)),
        ]));
    }
    let rendered_output = if cell.output_lines.is_empty() && !cell.active {
        vec!["(no output)".to_string()]
    } else {
        truncate_lines_middle(&cell.output_lines, 4, 4)
    };
    for (index, line) in rendered_output.into_iter().enumerate() {
        let style = if line.starts_with("… +") {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::Gray)
        };
        lines.push(Line::from(vec![
            Span::styled(
                if index == 0 { "  └ " } else { "    " },
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(line, style),
        ]));
    }
    lines
}

fn exec_spinner(elapsed: Option<Duration>) -> String {
    const FRAMES: &[&str] = &["•", "◦", "▪", "◦"];
    let index = elapsed
        .map(|duration| ((duration.as_millis() / 200) as usize) % FRAMES.len())
        .unwrap_or(0);
    FRAMES[index].to_string()
}

fn parse_exit_code_from_meta(meta: &str) -> Option<i32> {
    let exit = meta
        .split_whitespace()
        .find_map(|part| part.strip_prefix("exit="))?;
    if exit == "-" {
        None
    } else {
        exit.parse::<i32>().ok()
    }
}

fn render_patch_activity_cell(cell: &PatchActivityCell) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled(
            "Δ",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            cell.title.clone(),
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
    ])];
    lines.push(Line::from(vec![
        Span::raw("   "),
        Span::styled(
            cell.summary_line.clone(),
            Style::default().fg(Color::DarkGray),
        ),
    ]));
    for (index, file) in limit_patch_files(&cell.files, 8).into_iter().enumerate() {
        if index > 0 {
            lines.push(Line::from(""));
        }
        let (marker, label, style) = match file.operation.as_str() {
            "add" => ("+", "added", Style::default().fg(Color::LightGreen)),
            "delete" => ("-", "deleted", Style::default().fg(Color::LightRed)),
            "update" => ("~", "updated", Style::default().fg(Color::Yellow)),
            _ => ("·", "summary", Style::default().fg(Color::DarkGray)),
        };
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(format!("{marker} "), style.add_modifier(Modifier::BOLD)),
            Span::styled(file.path.clone(), style.add_modifier(Modifier::BOLD)),
        ]));
        if file.operation == "summary" {
            continue;
        }
        lines.push(Line::from(vec![
            Span::raw("     "),
            Span::styled(
                format!("{label}  +{} -{}", file.added_lines, file.removed_lines),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }
    lines
}

fn render_telegram_activity_cell(cell: &TelegramActivityCell) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled(
            "◦",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            cell.title.clone(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    ])];
    for line in cell.detail_lines.iter().take(6) {
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(line.to_string(), Style::default().fg(Color::Gray)),
        ]));
    }
    for (index, line) in cell.message_lines.iter().take(6).enumerate() {
        lines.push(Line::from(vec![
            Span::styled(
                if index == 0 { "  └ " } else { "    " },
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(line.to_string(), Style::default().fg(Color::White)),
        ]));
    }
    lines
}

fn render_terminal_wait_activity_cell(cell: &TextActivityCell) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled(
            "•",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            cell.title.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ])];
    for line in cell.body_lines.iter().take(6) {
        lines.push(Line::from(vec![
            Span::styled("  └ ", Style::default().fg(Color::DarkGray)),
            Span::styled(line.to_string(), Style::default().fg(Color::Gray)),
        ]));
    }
    lines
}

fn truncate_lines_middle(lines: &[String], head: usize, tail: usize) -> Vec<String> {
    if lines.len() <= head + tail + 1 {
        return lines.to_vec();
    }
    let mut out = Vec::with_capacity(head + tail + 1);
    out.extend(lines.iter().take(head).cloned());
    out.push(format!(
        "… +{} lines",
        lines.len().saturating_sub(head + tail)
    ));
    out.extend(lines.iter().skip(lines.len().saturating_sub(tail)).cloned());
    out
}

fn limit_patch_files(files: &[PatchFileUiData], keep: usize) -> Vec<PatchFileUiData> {
    if files.len() <= keep {
        return files.to_vec();
    }
    let mut out = files[..keep].to_vec();
    out.push(PatchFileUiData {
        path: format!("… +{} files", files.len() - keep),
        operation: "summary".to_string(),
        added_lines: 0,
        removed_lines: 0,
    });
    out
}

fn render_tool_result_activity_cell(cell: &TextActivityCell) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled(
            "←",
            Style::default()
                .fg(Color::LightGreen)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            cell.title.clone(),
            Style::default()
                .fg(Color::LightGreen)
                .add_modifier(Modifier::BOLD),
        ),
    ])];
    for line in cell.body_lines.iter().take(8) {
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(line.to_string(), Style::default().fg(Color::Gray)),
        ]));
    }
    lines
}

fn render_user_activity_cell(cell: &TextActivityCell) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled(
            "•",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            cell.title.clone(),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
    ])];
    for line in cell.body_lines.iter().take(6) {
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(line.to_string(), Style::default().fg(Color::Gray)),
        ]));
    }
    lines
}

fn render_error_activity_cell(cell: &TextActivityCell) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled(
            "!",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            cell.title.clone(),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
    ])];
    for line in cell.body_lines.iter().take(12) {
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(line.to_string(), Style::default().fg(Color::LightRed)),
        ]));
    }
    lines
}

fn render_command_overlay(f: &mut Frame, area: Rect, overlay: &CommandOverlay) {
    let outer = centered_rect(area, 92, 78);
    let block = panel(format!("  {}  ", overlay.title));
    let inner = block.inner(outer);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(1)])
        .split(inner);

    f.render_widget(Clear, outer);
    f.render_widget(block, outer);
    let lines = render_overlay_lines(&overlay.text);
    let max_scroll = lines
        .len()
        .saturating_sub(rows[0].height.saturating_sub(1) as usize) as u16;
    let scroll = overlay.scroll.min(max_scroll);
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .scroll((scroll, 0))
            .wrap(Wrap { trim: false }),
        rows[0],
    );
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            "↑/↓ scroll   PgUp/PgDn page   Home/End jump   Esc close",
            Style::default().fg(Color::DarkGray),
        )])),
        rows[1],
    );
}

fn render_overlay_lines(text: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut previous_blank = true;

    for raw_line in text.lines() {
        let line = raw_line.trim_end();
        if line.trim().is_empty() {
            lines.push(Line::from(""));
            previous_blank = true;
            continue;
        }

        if is_overlay_section_header(line, previous_blank) {
            lines.push(Line::from(vec![Span::styled(
                line.to_string(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )]));
            previous_blank = false;
            continue;
        }

        if let Some(content) = line.strip_prefix("• ") {
            lines.push(render_overlay_bullet_line(content));
            previous_blank = false;
            continue;
        }

        if let Some((label, value)) = line.split_once(':') {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{label}:"),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(value.trim().to_string(), Style::default().fg(Color::Gray)),
            ]));
            previous_blank = false;
            continue;
        }

        lines.push(Line::from(vec![Span::styled(
            line.to_string(),
            Style::default().fg(Color::Gray),
        )]));
        previous_blank = false;
    }

    if lines.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "No data",
            Style::default().fg(Color::DarkGray),
        )]));
    }

    lines
}

fn is_overlay_section_header(line: &str, previous_blank: bool) -> bool {
    previous_blank
        && !line.contains(':')
        && !line.starts_with('[')
        && !line.starts_with("• ")
        && line.chars().count() <= 32
}

fn render_overlay_bullet_line(content: &str) -> Line<'static> {
    if let Some((label, value)) = content.split_once(':') {
        Line::from(vec![
            Span::styled("•", Style::default().fg(Color::Cyan)),
            Span::raw(" "),
            Span::styled(format!("{label}:"), Style::default().fg(Color::White)),
            Span::raw(" "),
            Span::styled(value.trim().to_string(), Style::default().fg(Color::Gray)),
        ])
    } else {
        Line::from(vec![
            Span::styled("•", Style::default().fg(Color::Cyan)),
            Span::raw(" "),
            Span::styled(content.to_string(), Style::default().fg(Color::White)),
        ])
    }
}

fn render_command_popup(
    f: &mut Frame,
    area: Rect,
    input: &str,
    selected_index: usize,
    scroll: usize,
) {
    let matches = matching_commands(input);
    if matches.is_empty() {
        return;
    }

    let lines = matches
        .into_iter()
        .skip(scroll)
        .take(6)
        .enumerate()
        .map(|(visible_idx, suggestion)| {
            let idx = scroll + visible_idx;
            let selected = idx == selected_index;
            let style = if selected {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::White)
            };
            let desc_style = if selected {
                Style::default().fg(Color::Gray)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            Line::from(vec![
                Span::raw("  "),
                Span::styled(suggestion.display, style),
                Span::raw("  "),
                Span::styled(suggestion.description, desc_style),
            ])
        })
        .collect::<Vec<_>>();

    f.render_widget(
        Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
        area,
    );
}

fn render_command_bar(
    f: &mut Frame,
    area: Rect,
    input: &str,
    runtime_status: Option<&str>,
    footer_context: &str,
    overlay_open: bool,
    popup_selection: usize,
    popup_scroll: usize,
) {
    let completion = selected_command_completion(input, 0);
    let hint = command_hint(input);
    let popup_rows = if overlay_open {
        0
    } else {
        command_popup_row_count(input)
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(if popup_rows > 0 {
            vec![
                Constraint::Length(1),
                Constraint::Length(2),
                Constraint::Length(popup_rows),
                Constraint::Length(1),
            ]
        } else {
            vec![
                Constraint::Length(1),
                Constraint::Length(2),
                Constraint::Length(1),
            ]
        })
        .split(area);
    let status_line = match runtime_status {
        Some("Working") => render_working_status_line(),
        Some(status) if !status.trim().is_empty() => Line::from(vec![Span::styled(
            status.to_string(),
            Style::default().fg(Color::DarkGray),
        )]),
        _ => Line::from(""),
    };
    f.render_widget(Paragraph::new(status_line), rows[0]);
    let prompt = Line::from(vec![
        Span::styled("›", Style::default().fg(Color::Cyan)),
        Span::raw(" "),
        Span::styled(
            if input.is_empty() {
                "type a command".to_string()
            } else {
                input.to_string()
            },
            if input.is_empty() {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::White)
            },
        ),
        if input.is_empty() {
            Span::raw("")
        } else if let Some(completion) = completion
            && completion != input
        {
            Span::styled(
                completion
                    .strip_prefix(input)
                    .unwrap_or_default()
                    .to_string(),
                Style::default().fg(Color::DarkGray),
            )
        } else {
            Span::raw("")
        },
    ]);
    f.render_widget(Paragraph::new(prompt), rows[1]);
    let footer_row = if popup_rows > 0 {
        render_command_popup(f, rows[2], input, popup_selection, popup_scroll);
        rows[3]
    } else {
        rows[2]
    };
    let footer = Paragraph::new(match runtime_status {
        _ if overlay_open => Line::from(vec![
            Span::styled("overlay", Style::default().fg(Color::DarkGray)),
            Span::raw("  "),
            Span::styled(
                "Esc/q close, Up/Down scroll, PgUp/PgDn page",
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        _ if !footer_context.trim().is_empty() => Line::from(vec![Span::styled(
            footer_context.to_string(),
            Style::default().fg(Color::DarkGray),
        )]),
        _ => Line::from(vec![
            Span::styled("hint", Style::default().fg(Color::DarkGray)),
            Span::raw("  "),
            Span::styled(hint, Style::default().fg(Color::DarkGray)),
        ]),
    });
    f.render_widget(footer, footer_row);
}

fn render_working_status_line() -> Line<'static> {
    let frame = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        / 200) as usize
        % 4;
    let glyph = ["•", "◦", "▪", "◦"][frame];
    Line::from(vec![
        Span::styled(
            glyph,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            "Working",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

fn centered_rect(area: Rect, width_percent: u16, height_percent: u16) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - height_percent) / 2),
            Constraint::Percentage(height_percent),
            Constraint::Percentage(100 - height_percent - (100 - height_percent) / 2),
        ])
        .split(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width_percent) / 2),
            Constraint::Percentage(width_percent),
            Constraint::Percentage(100 - width_percent - (100 - width_percent) / 2),
        ])
        .split(vertical[1]);
    horizontal[1]
}

fn fallback_output(output: &str) -> String {
    if output.trim().is_empty() {
        "no data".to_string()
    } else {
        output.to_string()
    }
}

fn selected_command_completion(input: &str, selected_index: usize) -> Option<String> {
    let matches = matching_commands(input);
    if matches.is_empty() {
        return None;
    }
    let index = selected_index.min(matches.len().saturating_sub(1));
    Some(matches[index].completion.clone())
}

fn matching_commands(input: &str) -> Vec<CommandSuggestion> {
    let trimmed = input.trim();
    let trailing_space = input.ends_with(' ');
    if trimmed.is_empty() {
        return dashboard_commands()
            .iter()
            .map(|command| CommandSuggestion {
                display: command.usage().to_string(),
                completion: command.primary_verb().to_string(),
                description: command.description().to_string(),
            })
            .collect::<Vec<_>>();
    }
    let parts = trimmed.split_whitespace().collect::<Vec<_>>();
    if let Some(command) = dashboard_commands()
        .iter()
        .copied()
        .find(|command| command.accepts(parts[0]))
        && !command.subcommands().is_empty()
        && (parts.len() > 1 || trailing_space)
    {
        let prefix = if trailing_space {
            ""
        } else {
            parts.get(1).copied().unwrap_or_default()
        };
        let direct = command
            .subcommands()
            .iter()
            .copied()
            .filter(|subcommand| subcommand.name().starts_with(prefix))
            .map(|subcommand| CommandSuggestion {
                display: subcommand.usage().to_string(),
                completion: format!("{} {}", command.primary_verb(), subcommand.name()),
                description: subcommand.description().to_string(),
            })
            .collect::<Vec<_>>();
        if !direct.is_empty() {
            return direct;
        }
    }
    dashboard_commands()
        .iter()
        .copied()
        .filter(|command| command.primary_verb().starts_with(parts[0]))
        .map(|command| CommandSuggestion {
            display: command.usage().to_string(),
            completion: command.primary_verb().to_string(),
            description: command.description().to_string(),
        })
        .collect::<Vec<_>>()
}

fn command_popup_row_count(input: &str) -> u16 {
    let matches = matching_commands(input);
    if matches.is_empty() {
        0
    } else {
        matches.len().min(6) as u16
    }
}

fn adjusted_popup_scroll(current_scroll: usize, selected_index: usize, total: usize) -> usize {
    if total <= 6 {
        return 0;
    }
    let max_scroll = total.saturating_sub(6);
    if selected_index < current_scroll {
        selected_index
    } else if selected_index >= current_scroll + 6 {
        (selected_index + 1).saturating_sub(6).min(max_scroll)
    } else {
        current_scroll.min(max_scroll)
    }
}

fn command_hint(input: &str) -> String {
    let matches = matching_commands(input);
    if input.trim().is_empty() {
        return "Up/Down select. Tab accept. Enter run. Esc clear.".to_string();
    }
    if matches.len() == 1 {
        let suggestion = &matches[0];
        return format!("{} — {}", suggestion.display, suggestion.description);
    }
    if matches.len() > 1 {
        return matches
            .iter()
            .take(4)
            .map(|suggestion| suggestion.display.clone())
            .collect::<Vec<_>>()
            .join(" | ");
    }
    "unknown command".to_string()
}
