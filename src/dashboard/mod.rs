//! Dashboard: activity feed + command console.

pub mod cells;
pub mod render;

pub use cells::{
    ActivityCell, DashboardActivityEvent, LiveActivityCell, activity_cell_from_tool_ui_event,
    apply_activity_event, assistant_activity_cell, render_activity_feed,
    render_activity_from_messages,
};

use std::time::Duration;

use async_trait::async_trait;
use crossterm::event::{Event, KeyCode, KeyEventKind};
use ratatui::{
    prelude::*,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Wrap},
};

use crate::{
    app::AppId,
    core::TokenUsageInfo,
    events::{EventStore, TerminalIncomingEvent},
    pending_work::{PendingWork, PendingWorkQueue},
    reasoning::turn_compile::{
        load_prompt_persona_spec_sync, prompt_persona_path_sync, render_prompt_persona_markdown,
    },
    telegram_acl::{PendingAccessRequest, TelegramAclHandle},
};
use serde::{Deserialize, Serialize};

const TELEGRAM_ACCESS_PICKER_VISIBLE_ROWS: usize = 8;

#[derive(Clone, Serialize, Deserialize)]
pub struct DashboardPlanStep {
    pub status: String,
    pub step: String,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct DashboardTokenUsageSnapshot {
    pub main: Option<TokenUsageInfo>,
    #[serde(default)]
    pub main_model: Option<String>,
    pub judge: Option<TokenUsageInfo>,
    #[serde(default)]
    pub judge_model: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct DashboardWorkflowOptimizationSnapshot {
    pub running: bool,
    pub current_trigger: Option<String>,
    pub last_result: Option<String>,
    pub last_completed_at_ms: Option<i64>,
    pub workflow_evidence_records: usize,
    pub total_workflow_evidence_run_records: usize,
    pub total_workflow_reflections: usize,
    pub total_workflow_patch_candidates: usize,
    pub total_workflow_merge_candidates: usize,
    pub total_workflow_candidate_evaluations: usize,
    pub total_workflow_frontier_entries: usize,
    pub latest_workflow_frontier_root_entries: usize,
    pub latest_workflow_frontier_branched_entries: usize,
    pub latest_workflow_frontier_max_generation: usize,
    pub total_workflow_patch_applied: usize,
    pub total_workflow_merge_applied: usize,
    pub total_workflow_update_rollbacks: usize,
    pub total_workflow_optimization_rounds: usize,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct DashboardState {
    pub focused_app: Option<AppId>,
    pub status_output: String,
    pub sleep_status_output: String,
    pub inspect_telegram_output: String,
    pub system_prompt_output: String,
    pub preturn_context_output: String,
    pub app_status_outputs: Vec<(String, String)>,
    #[serde(default)]
    pub pending_access_requests: Vec<PendingAccessRequest>,
    pub activity_cells: Vec<ActivityCell>,
    pub live_activity_cells: Vec<LiveActivityCell>,
    pub last_cycle_elapsed_ms: Option<u128>,
    pub runtime_status: Option<String>,
    #[serde(default)]
    pub current_plan_step: Option<DashboardPlanStep>,
    #[serde(default)]
    pub token_usage: DashboardTokenUsageSnapshot,
    #[serde(default)]
    pub workflow_optimization: DashboardWorkflowOptimizationSnapshot,
    pub footer_context: String,
    pub footer_estimated_input_tokens: Option<usize>,
}

#[derive(Clone, Debug)]
pub enum DashboardControlCommand {
    RunSleep,
    ClearConversation,
    RestartDaemon,
}

#[async_trait]
pub trait DashboardCommandRunner: Send + Sync {
    async fn run_command(&self, command: &str, state: &DashboardState) -> String;
}

struct CommandOverlay {
    title: String,
    text: String,
    scroll: u16,
}

struct TelegramAccessPicker {
    action: TelegramAccessAction,
    requests: Vec<PendingAccessRequest>,
    selected: usize,
    scroll: usize,
}

#[derive(Clone, Copy)]
enum TelegramAccessAction {
    Approve,
    Reject,
}

impl TelegramAccessAction {
    fn verb(self) -> &'static str {
        match self {
            TelegramAccessAction::Approve => "approve",
            TelegramAccessAction::Reject => "reject",
        }
    }

    fn title(self) -> &'static str {
        match self {
            TelegramAccessAction::Approve => "TELEGRAM APPROVE",
            TelegramAccessAction::Reject => "TELEGRAM REJECT",
        }
    }
}

struct DashboardCommandContext<'a> {
    requests: &'a [PendingAccessRequest],
    state: &'a DashboardState,
    executor: Option<DashboardCommandExecutor<'a>>,
}

#[derive(Clone, Copy)]
struct DashboardCommandExecutor<'a> {
    telegram_acl: &'a TelegramAclHandle,
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

    fn remote_command(&self) -> Option<&'static str> {
        Some(self.primary_verb())
    }

    fn remote_description(&self) -> &'static str {
        self.description()
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

    fn complete_arguments(
        &self,
        _parts: &[&str],
        _context: &DashboardCommandContext<'_>,
    ) -> Vec<CommandSuggestion> {
        Vec::new()
    }
}

trait DashboardSubcommand: Sync {
    fn usage(&self) -> &'static str;
    fn description(&self) -> &'static str;

    fn name(&self) -> &'static str {
        self.usage().split_whitespace().next().unwrap_or_default()
    }

    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    fn accepts(&self, verb: &str) -> bool {
        self.name() == verb || self.aliases().contains(&verb)
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
struct DebugCommand;
struct DebugPersonaSubcommand;
struct DebugSystemPromptSubcommand;
struct DebugContextSubcommand;
struct AppStatusCommand;
struct StatusCommand;
struct SleepCommand;
struct RestartCommand;
struct SleepRunSubcommand;
struct SleepStatusSubcommand;
struct TelegramCommand;
struct TelegramStatusSubcommand;
struct TelegramApproveSubcommand;
struct TelegramRejectSubcommand;

static QUIT_COMMAND: QuitCommand = QuitCommand;
static CLEAR_COMMAND: ClearCommand = ClearCommand;
static DEBUG_COMMAND: DebugCommand = DebugCommand;
static DEBUG_PERSONA_SUBCOMMAND: DebugPersonaSubcommand = DebugPersonaSubcommand;
static DEBUG_SYSTEM_PROMPT_SUBCOMMAND: DebugSystemPromptSubcommand = DebugSystemPromptSubcommand;
static DEBUG_CONTEXT_SUBCOMMAND: DebugContextSubcommand = DebugContextSubcommand;
static APP_STATUS_COMMAND: AppStatusCommand = AppStatusCommand;
static STATUS_COMMAND: StatusCommand = StatusCommand;
static SLEEP_COMMAND: SleepCommand = SleepCommand;
static RESTART_COMMAND: RestartCommand = RestartCommand;
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
static DEBUG_SUBCOMMANDS: [&dyn DashboardSubcommand; 3] = [
    &DEBUG_PERSONA_SUBCOMMAND,
    &DEBUG_SYSTEM_PROMPT_SUBCOMMAND,
    &DEBUG_CONTEXT_SUBCOMMAND,
];

static DASHBOARD_COMMANDS: [&dyn DashboardCommand; 8] = [
    &QUIT_COMMAND,
    &CLEAR_COMMAND,
    &DEBUG_COMMAND,
    &APP_STATUS_COMMAND,
    &STATUS_COMMAND,
    &RESTART_COMMAND,
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

    fn remote_command(&self) -> Option<&'static str> {
        None
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
        "clear runtime conversation history, current plan, and all events"
    }

    fn execute(
        &self,
        _: &[&str],
        raw: &str,
        context: &DashboardCommandContext<'_>,
    ) -> DashboardCommandResult {
        let Some(executor) = context.executor else {
            return DashboardCommandResult::ShowOverlay {
                title: raw.trim().to_uppercase(),
                text: "clear is unavailable in completion-only mode".to_string(),
            };
        };
        match executor
            .control_tx
            .send(DashboardControlCommand::ClearConversation)
        {
            Ok(()) => DashboardCommandResult::ShowOverlay {
                title: raw.trim().to_uppercase(),
                text: "queued runtime conversation + plan + event clear".to_string(),
            },
            Err(err) => DashboardCommandResult::ShowOverlay {
                title: raw.trim().to_uppercase(),
                text: format!("failed to queue clear command: {err}"),
            },
        }
    }
}

impl DashboardCommand for DebugCommand {
    fn usage(&self) -> &'static str {
        "debug"
    }

    fn description(&self) -> &'static str {
        "debug outputs and internal runtime views"
    }

    fn subcommands(&self) -> &'static [&'static dyn DashboardSubcommand] {
        &DEBUG_SUBCOMMANDS
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
                text: "available:\n  debug persona\n  debug system-prompt\n  debug context"
                    .to_string(),
            };
        };
        if let Some(subcommand) = self
            .subcommands()
            .iter()
            .copied()
            .find(|subcommand| subcommand.accepts(subcommand_name))
        {
            subcommand.execute(parts, raw, context)
        } else {
            DashboardCommandResult::ShowOverlay {
                title: self.overlay_title(raw),
                text: format!("unknown debug subcommand: {subcommand_name}"),
            }
        }
    }
}

impl DashboardSubcommand for DebugPersonaSubcommand {
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
        let path = prompt_persona_path_sync();
        let text = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(_) => render_prompt_persona_markdown(&load_prompt_persona_spec_sync()),
        };
        DashboardCommandResult::ShowOverlay {
            title: raw.trim().to_uppercase(),
            text,
        }
    }
}

impl DashboardSubcommand for DebugSystemPromptSubcommand {
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

impl DashboardSubcommand for DebugContextSubcommand {
    fn usage(&self) -> &'static str {
        "context"
    }

    fn description(&self) -> &'static str {
        "show latest pre-turn runtime context"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["preturn-context", "preturn_context"]
    }

    fn execute(
        &self,
        _: &[&str],
        raw: &str,
        context: &DashboardCommandContext<'_>,
    ) -> DashboardCommandResult {
        DashboardCommandResult::ShowOverlay {
            title: raw.trim().to_uppercase(),
            text: fallback_output(&context.state.preturn_context_output),
        }
    }
}

impl DashboardCommand for AppStatusCommand {
    fn usage(&self) -> &'static str {
        "app-status <app>"
    }

    fn description(&self) -> &'static str {
        "show current structured app state and llm-facing note"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["app_status"]
    }

    fn remote_command(&self) -> Option<&'static str> {
        Some("app_status")
    }

    fn complete_arguments(
        &self,
        parts: &[&str],
        context: &DashboardCommandContext<'_>,
    ) -> Vec<CommandSuggestion> {
        let prefix = parts
            .get(1)
            .copied()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        context
            .state
            .app_status_outputs
            .iter()
            .map(|(name, _)| name.as_str())
            .filter(|candidate| candidate.starts_with(&prefix))
            .map(|candidate| CommandSuggestion {
                display: candidate.to_string(),
                completion: format!("{} {}", self.primary_verb(), candidate),
                description: self.description().to_string(),
            })
            .collect()
    }

    fn execute(
        &self,
        parts: &[&str],
        raw: &str,
        context: &DashboardCommandContext<'_>,
    ) -> DashboardCommandResult {
        let Some(target) = parts.get(1).copied() else {
            let apps = context
                .state
                .app_status_outputs
                .iter()
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>();
            return DashboardCommandResult::ShowOverlay {
                title: self.overlay_title(raw),
                text: if apps.is_empty() {
                    "available apps: none".to_string()
                } else {
                    format!("available apps: {}", apps.join(", "))
                },
            };
        };
        let target = target.trim().to_ascii_lowercase();
        let output = context
            .state
            .app_status_outputs
            .iter()
            .find(|(name, _)| name == &target)
            .map(|(_, output)| output.clone());
        DashboardCommandResult::ShowOverlay {
            title: self.overlay_title(raw),
            text: output.unwrap_or_else(|| {
                let apps = context
                    .state
                    .app_status_outputs
                    .iter()
                    .map(|(name, _)| name.clone())
                    .collect::<Vec<_>>();
                if apps.is_empty() {
                    format!("unknown app: {target}")
                } else {
                    format!("unknown app: {target}\navailable apps: {}", apps.join(", "))
                }
            }),
        }
    }
}

impl DashboardCommand for RestartCommand {
    fn usage(&self) -> &'static str {
        "restart"
    }

    fn description(&self) -> &'static str {
        "restart the daemon"
    }

    fn execute(
        &self,
        _: &[&str],
        raw: &str,
        context: &DashboardCommandContext<'_>,
    ) -> DashboardCommandResult {
        let Some(executor) = context.executor else {
            return DashboardCommandResult::ShowOverlay {
                title: self.overlay_title(raw),
                text: "restart is unavailable in completion-only mode".to_string(),
            };
        };
        match executor
            .control_tx
            .send(DashboardControlCommand::RestartDaemon)
        {
            Ok(()) => DashboardCommandResult::ShowOverlay {
                title: self.overlay_title(raw),
                text: "queued daemon restart".to_string(),
            },
            Err(err) => DashboardCommandResult::ShowOverlay {
                title: self.overlay_title(raw),
                text: format!("failed to queue daemon restart: {err}"),
            },
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
            .find(|subcommand| subcommand.accepts(subcommand_name))
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
        let Some(executor) = context.executor else {
            return DashboardCommandResult::ShowOverlay {
                title: raw.trim().to_uppercase(),
                text: "sleep run is unavailable in completion-only mode".to_string(),
            };
        };
        match executor.control_tx.send(DashboardControlCommand::RunSleep) {
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

    fn complete_arguments(
        &self,
        parts: &[&str],
        context: &DashboardCommandContext<'_>,
    ) -> Vec<CommandSuggestion> {
        let subcommand = parts.get(1).copied().unwrap_or_default();
        if subcommand != "approve" && subcommand != "reject" {
            return Vec::new();
        }
        let prefix = parts.get(2).copied().unwrap_or_default();
        context
            .requests
            .iter()
            .filter(|r| r.chat_id.to_string().starts_with(prefix))
            .map(|r| CommandSuggestion {
                display: format!("{} ({})", r.chat_id, r.sender),
                completion: format!("telegram {} {}", subcommand, r.chat_id),
                description: format!("{} — {}", r.title, r.sender),
            })
            .collect()
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
                text: "available:\n  telegram status\n  telegram approve [chat_id]\n  telegram reject [chat_id]".to_string(),
            };
        };
        if let Some(subcommand) = self
            .subcommands()
            .iter()
            .copied()
            .find(|subcommand| subcommand.accepts(subcommand_name))
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
        "approve [chat_id]"
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
        "reject [chat_id]"
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

#[derive(Clone, Copy, Serialize)]
pub(crate) struct RemoteDashboardCommand {
    pub command: &'static str,
    pub description: &'static str,
}

pub(crate) fn remote_dashboard_commands() -> Vec<RemoteDashboardCommand> {
    dashboard_commands()
        .iter()
        .filter_map(|command| {
            command
                .remote_command()
                .map(|remote_command| RemoteDashboardCommand {
                    command: remote_command,
                    description: command.remote_description(),
                })
        })
        .collect()
}

fn execute_access_request_command(
    approve: bool,
    parts: &[&str],
    context: &DashboardCommandContext<'_>,
) -> String {
    let action = if approve { "approve" } else { "reject" };

    let chat_id = if let Some(target) = parts.get(2).copied() {
        match target.parse::<i64>() {
            Ok(id) => id,
            Err(_) => return format!("invalid chat_id: {target}"),
        }
    } else {
        return render_pending_access_requests(action, context.requests);
    };

    let result = if approve {
        let Some(executor) = context.executor else {
            return format!("{action} is unavailable in completion-only mode");
        };
        executor.telegram_acl.approve(chat_id)
    } else {
        let Some(executor) = context.executor else {
            return format!("{action} is unavailable in completion-only mode");
        };
        executor.telegram_acl.reject(chat_id)
    };
    match result {
        Ok(()) => format!("{} {}", action, chat_id),
        Err(err) => format!("{action} failed for {chat_id}: {err}"),
    }
}

fn render_pending_access_requests(action: &str, requests: &[PendingAccessRequest]) -> String {
    if requests.is_empty() {
        return "no pending requests".to_string();
    }

    let mut lines = vec![format!(
        "pending requests - send '/telegram {action} <chat_id>' to proceed:"
    )];
    lines.extend(requests.iter().map(|request| {
        format!(
            "  {} | {} | {} | {}",
            request.chat_id, request.title, request.sender, request.last_message_preview
        )
    }));
    lines.join("\n")
}

fn telegram_access_picker_for_input(
    input: &str,
    requests: &[PendingAccessRequest],
) -> Option<TelegramAccessPicker> {
    if requests.is_empty() {
        return None;
    }

    let command = dashboard_command_body(input)?;
    let parts = command.split_whitespace().collect::<Vec<_>>();
    let action = match parts.as_slice() {
        ["telegram", "approve"] => TelegramAccessAction::Approve,
        ["telegram", "reject"] => TelegramAccessAction::Reject,
        _ => return None,
    };

    Some(TelegramAccessPicker {
        action,
        requests: requests.to_vec(),
        selected: 0,
        scroll: 0,
    })
}

fn adjusted_picker_scroll(current_scroll: usize, selected_index: usize, total: usize) -> usize {
    if total <= TELEGRAM_ACCESS_PICKER_VISIBLE_ROWS {
        return 0;
    }
    let max_scroll = total.saturating_sub(TELEGRAM_ACCESS_PICKER_VISIBLE_ROWS);
    if selected_index < current_scroll {
        selected_index
    } else if selected_index >= current_scroll + TELEGRAM_ACCESS_PICKER_VISIBLE_ROWS {
        (selected_index + 1)
            .saturating_sub(TELEGRAM_ACCESS_PICKER_VISIBLE_ROWS)
            .min(max_scroll)
    } else {
        current_scroll.min(max_scroll)
    }
}

pub async fn run_tui_dashboard(
    rx: &mut tokio::sync::watch::Receiver<DashboardState>,
    command_runner: &dyn DashboardCommandRunner,
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
    let mut telegram_access_picker: Option<TelegramAccessPicker> = None;

    loop {
        let pending_requests = rx.borrow().pending_access_requests.clone();

        if crossterm::event::poll(Duration::from_millis(16))?
            && let Event::Key(key) = crossterm::event::read()?
            && key.kind == KeyEventKind::Press
        {
            if telegram_access_picker.is_some() {
                let mut command_to_run: Option<String> = None;
                let mut close_picker = false;
                if let Some(picker) = telegram_access_picker.as_mut() {
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('q') => {
                            close_picker = true;
                        }
                        KeyCode::Up => {
                            picker.selected = picker
                                .selected
                                .saturating_sub(1)
                                .min(picker.requests.len().saturating_sub(1));
                            picker.scroll = adjusted_picker_scroll(
                                picker.scroll,
                                picker.selected,
                                picker.requests.len(),
                            );
                        }
                        KeyCode::Down => {
                            picker.selected =
                                (picker.selected + 1).min(picker.requests.len().saturating_sub(1));
                            picker.scroll = adjusted_picker_scroll(
                                picker.scroll,
                                picker.selected,
                                picker.requests.len(),
                            );
                        }
                        KeyCode::PageUp => {
                            picker.selected = picker.selected.saturating_sub(8);
                            picker.scroll = adjusted_picker_scroll(
                                picker.scroll,
                                picker.selected,
                                picker.requests.len(),
                            );
                        }
                        KeyCode::PageDown => {
                            picker.selected =
                                (picker.selected + 8).min(picker.requests.len().saturating_sub(1));
                            picker.scroll = adjusted_picker_scroll(
                                picker.scroll,
                                picker.selected,
                                picker.requests.len(),
                            );
                        }
                        KeyCode::Home => {
                            picker.selected = 0;
                            picker.scroll = 0;
                        }
                        KeyCode::End => {
                            picker.selected = picker.requests.len().saturating_sub(1);
                            picker.scroll = adjusted_picker_scroll(
                                picker.scroll,
                                picker.selected,
                                picker.requests.len(),
                            );
                        }
                        KeyCode::Enter => {
                            if let Some(request) = picker.requests.get(picker.selected) {
                                command_to_run = Some(format!(
                                    "/telegram {} {}",
                                    picker.action.verb(),
                                    request.chat_id
                                ));
                            }
                        }
                        _ => {}
                    }
                }
                if close_picker {
                    telegram_access_picker = None;
                }
                if let Some(command) = command_to_run {
                    telegram_access_picker = None;
                    let state = rx.borrow().clone();
                    let response = command_runner.run_command(&command, &state).await;
                    command_overlay = Some(CommandOverlay {
                        title: command.to_uppercase(),
                        text: response,
                        scroll: 0,
                    });
                }
                continue;
            }
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
                    let state = rx.borrow();
                    let command_context = DashboardCommandContext {
                        requests: &pending_requests,
                        state: &state,
                        executor: None,
                    };
                    if let Some(completion) = selected_command_completion(
                        &command_input,
                        command_popup_selection,
                        &command_context,
                    ) {
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
                    let state = rx.borrow();
                    let command_context = DashboardCommandContext {
                        requests: &pending_requests,
                        state: &state,
                        executor: None,
                    };
                    let matches = matching_commands(&command_input, &command_context);
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
                    let state = rx.borrow();
                    let command_context = DashboardCommandContext {
                        requests: &pending_requests,
                        state: &state,
                        executor: None,
                    };
                    let matches = matching_commands(&command_input, &command_context);
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
                    let state = rx.borrow().clone();
                    let command_context = DashboardCommandContext {
                        requests: &pending_requests,
                        state: &state,
                        executor: None,
                    };
                    if let Some(completion) = selected_command_completion(
                        &command_input,
                        command_popup_selection,
                        &command_context,
                    ) && completion != command_input
                    {
                        command_input = completion;
                        command_popup_selection = 0;
                        command_popup_scroll = 0;
                        continue;
                    }
                    let input = command_input.trim().to_string();
                    if !input.is_empty() {
                        if matches!(dashboard_command_body(&input), Some("quit" | "q" | "exit")) {
                            break;
                        }
                        if let Some(picker) =
                            telegram_access_picker_for_input(&input, &pending_requests)
                        {
                            telegram_access_picker = Some(picker);
                            command_input.clear();
                            command_popup_selection = 0;
                            command_popup_scroll = 0;
                            continue;
                        }
                        let response = command_runner.run_command(&input, &state).await;
                        if is_dashboard_command_input(&input) {
                            command_overlay = Some(CommandOverlay {
                                title: input.to_uppercase(),
                                text: response,
                                scroll: 0,
                            });
                        } else {
                            command_overlay = None;
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
        let popup_rows = if command_overlay.is_none() && telegram_access_picker.is_none() {
            let command_context = DashboardCommandContext {
                requests: &pending_requests,
                state: &state,
                executor: None,
            };
            command_popup_row_count(&command_input, &command_context)
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
            if let Some(picker) = telegram_access_picker.as_ref() {
                render_telegram_access_picker(f, root[0], picker);
            }
            render_command_bar(
                f,
                root[1],
                CommandBarRenderState {
                    input: &command_input,
                    context: &DashboardCommandContext {
                        requests: &pending_requests,
                        state: &state,
                        executor: None,
                    },
                    runtime_status: state.runtime_status.as_deref(),
                    footer_context: &state.footer_context,
                    overlay_open: command_overlay.is_some() || telegram_access_picker.is_some(),
                    popup_selection: command_popup_selection,
                    popup_scroll: command_popup_scroll,
                },
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
        state,
        executor: Some(DashboardCommandExecutor {
            telegram_acl,
            control_tx,
        }),
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

pub(crate) fn execute_control_command(
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
        DashboardCommandResult::ShowOverlay { title, text } => {
            if text.trim().is_empty() {
                title
            } else {
                text
            }
        }
        DashboardCommandResult::Quit => {
            "quit command is only available in the local dashboard".to_string()
        }
    }
}

pub(crate) fn execute_remote_command(
    input: &str,
    telegram_acl: &TelegramAclHandle,
    events: &EventStore,
    pending_work: &PendingWorkQueue,
    state: &DashboardState,
    control_tx: &tokio::sync::mpsc::UnboundedSender<DashboardControlCommand>,
) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return "empty input".to_string();
    }
    let Some(command) = dashboard_command_body(trimmed) else {
        return enqueue_terminal_message(events, pending_work, trimmed);
    };
    execute_control_command(command, telegram_acl, state, control_tx)
}

fn enqueue_terminal_message(
    events: &EventStore,
    pending_work: &PendingWorkQueue,
    input: &str,
) -> String {
    match events.register_terminal_incoming(TerminalIncomingEvent {
        origin: "dashboard".to_string(),
        incoming_text: input.to_string(),
    }) {
        Ok(event_id) => match pending_work.enqueue(PendingWork::Event { event_id }) {
            Ok(_) => format!("queued terminal message as event {event_id}"),
            Err(err) => format!("failed to queue terminal message {event_id}: {err}"),
        },
        Err(err) => format!("failed to register terminal message: {err}"),
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

fn render_telegram_access_picker(f: &mut Frame, area: Rect, picker: &TelegramAccessPicker) {
    let outer = centered_rect(area, 92, 70);
    let block = panel(format!("  {}  ", picker.action.title()));
    let inner = block.inner(outer);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(TELEGRAM_ACCESS_PICKER_VISIBLE_ROWS as u16),
            Constraint::Length(1),
        ])
        .split(inner);

    f.render_widget(Clear, outer);
    f.render_widget(block, outer);
    f.render_widget(
        Paragraph::new(Text::from(vec![
            Line::from(vec![Span::styled(
                format!("Select a pending request to {}.", picker.action.verb()),
                Style::default().fg(Color::Gray),
            )]),
            Line::from(vec![Span::styled(
                "chat_id  title  sender  last message",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )]),
        ])),
        rows[0],
    );

    let lines = picker
        .requests
        .iter()
        .skip(picker.scroll)
        .take(TELEGRAM_ACCESS_PICKER_VISIBLE_ROWS)
        .enumerate()
        .map(|(visible_idx, request)| {
            let idx = picker.scroll + visible_idx;
            let selected = idx == picker.selected;
            let marker = if selected { ">" } else { " " };
            let style = if selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            Line::from(vec![Span::styled(
                format!(
                    "{marker} {}  {}  {}  {}",
                    request.chat_id, request.title, request.sender, request.last_message_preview
                ),
                style,
            )])
        })
        .collect::<Vec<_>>();
    f.render_widget(
        Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
        rows[1],
    );
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            format!(
                "Enter {} selected   Up/Down move   PgUp/PgDn page   Esc cancel",
                picker.action.verb()
            ),
            Style::default().fg(Color::DarkGray),
        )])),
        rows[2],
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
    context: &DashboardCommandContext<'_>,
    selected_index: usize,
    scroll: usize,
) {
    let matches = matching_commands(input, context);
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

struct CommandBarRenderState<'a> {
    input: &'a str,
    context: &'a DashboardCommandContext<'a>,
    runtime_status: Option<&'a str>,
    footer_context: &'a str,
    overlay_open: bool,
    popup_selection: usize,
    popup_scroll: usize,
}

fn render_command_bar(f: &mut Frame, area: Rect, state: CommandBarRenderState<'_>) {
    let CommandBarRenderState {
        input,
        context,
        runtime_status,
        footer_context,
        overlay_open,
        popup_selection,
        popup_scroll,
    } = state;

    let completion = selected_command_completion(input, 0, context);
    let hint = command_hint(input, context);
    let popup_rows = if overlay_open {
        0
    } else {
        command_popup_row_count(input, context)
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
                "type a message, or /command".to_string()
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
        render_command_popup(f, rows[2], input, context, popup_selection, popup_scroll);
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

fn selected_command_completion(
    input: &str,
    selected_index: usize,
    context: &DashboardCommandContext<'_>,
) -> Option<String> {
    let matches = matching_commands(input, context);
    if matches.is_empty() {
        return None;
    }
    let index = selected_index.min(matches.len().saturating_sub(1));
    Some(matches[index].completion.clone())
}

fn dashboard_command_body(input: &str) -> Option<&str> {
    let stripped = input.trim_start().strip_prefix('/')?.trim();
    (!stripped.is_empty()).then_some(stripped)
}

fn command_completion_body(input: &str) -> Option<&str> {
    input.trim_start().strip_prefix('/')
}

fn is_dashboard_command_input(input: &str) -> bool {
    dashboard_command_body(input).is_some()
}

fn matching_commands(input: &str, context: &DashboardCommandContext<'_>) -> Vec<CommandSuggestion> {
    let Some(command_input) = command_completion_body(input) else {
        return Vec::new();
    };
    let trimmed = command_input.trim();
    let trailing_space = command_input.ends_with(' ');
    if trimmed.is_empty() {
        return dashboard_commands()
            .iter()
            .map(|command| CommandSuggestion {
                display: command.usage().to_string(),
                completion: format!("/{}", command.primary_verb()),
                description: command.description().to_string(),
            })
            .collect::<Vec<_>>();
    }
    let parts = trimmed.split_whitespace().collect::<Vec<_>>();
    if let Some(command) = dashboard_commands()
        .iter()
        .copied()
        .find(|command| command.accepts(parts[0]))
    {
        if !command.subcommands().is_empty() && (parts.len() > 1 || trailing_space) {
            // Complete subcommands only while the cursor is still within the subcommand word:
            //   "telegram "      → trailing_space=true,  parts.len()==1  ✓
            //   "telegram app"   → trailing_space=false, parts.len()==2  ✓
            // Once the user has typed a subcommand plus a space or argument, stop completing subcommands:
            //   "telegram approve "   → trailing_space=true,  parts.len()==2  ✗
            //   "telegram approve 1"  → trailing_space=false, parts.len()==3  ✗
            let in_subcommand_word =
                (trailing_space && parts.len() == 1) || (!trailing_space && parts.len() == 2);
            if in_subcommand_word {
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
                        completion: format!("/{} {}", command.primary_verb(), subcommand.name()),
                        description: subcommand.description().to_string(),
                    })
                    .collect::<Vec<_>>();
                if !direct.is_empty() {
                    return direct;
                }
            } else {
                // Argument phase: let the command provide argument completions.
                let args = command.complete_arguments(&parts, context);
                if !args.is_empty() {
                    return args;
                }
            }
            return Vec::new();
        } else if parts.len() > 1 || trailing_space {
            let args = command.complete_arguments(&parts, context);
            if !args.is_empty() {
                return args;
            }
            return Vec::new();
        }
    }
    dashboard_commands()
        .iter()
        .copied()
        .filter(|command| command.primary_verb().starts_with(parts[0]))
        .map(|command| CommandSuggestion {
            display: command.usage().to_string(),
            completion: format!("/{}", command.primary_verb()),
            description: command.description().to_string(),
        })
        .collect::<Vec<_>>()
}

fn command_popup_row_count(input: &str, context: &DashboardCommandContext<'_>) -> u16 {
    let matches = matching_commands(input, context);
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

fn command_hint(input: &str, context: &DashboardCommandContext<'_>) -> String {
    if !is_dashboard_command_input(input) {
        if input.trim().is_empty() {
            return "Enter send. Prefix / for commands. Esc clear.".to_string();
        }
        return "Enter send. Prefix / for commands.".to_string();
    }
    let matches = matching_commands(input, context);
    if command_completion_body(input)
        .map(str::trim)
        .unwrap_or_default()
        .is_empty()
    {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn pending_request(chat_id: i64) -> PendingAccessRequest {
        PendingAccessRequest {
            chat_id,
            title: format!("chat-{chat_id}"),
            sender: format!("sender-{chat_id}"),
            last_message_preview: format!("message-{chat_id}"),
            first_seen_at_ms: chat_id,
            last_seen_at_ms: chat_id,
        }
    }

    fn test_command_context<'a>() -> DashboardCommandContext<'a> {
        DashboardCommandContext {
            requests: &[],
            state: Box::leak(Box::new(DashboardState::default())),
            executor: None,
        }
    }

    #[test]
    fn dashboard_command_body_requires_slash_prefix() {
        assert_eq!(dashboard_command_body("status"), None);
        assert_eq!(dashboard_command_body("/status"), Some("status"));
        assert_eq!(dashboard_command_body("  /status  "), Some("status"));
    }

    #[test]
    fn matching_commands_only_triggers_for_slash_inputs() {
        let context = test_command_context();
        assert!(matching_commands("status", &context).is_empty());
        let matches = matching_commands("/sta", &context);
        assert!(!matches.is_empty());
        assert!(
            matches
                .iter()
                .all(|suggestion| suggestion.completion.starts_with('/'))
        );
    }

    #[test]
    fn remote_dashboard_commands_are_derived_from_command_registry() {
        let commands = remote_dashboard_commands()
            .into_iter()
            .map(|command| command.command)
            .collect::<Vec<_>>();

        assert!(commands.contains(&"debug"));
        assert!(commands.contains(&"app_status"));
        assert!(commands.contains(&"restart"));
        assert!(!commands.contains(&"snapshot"));
        assert!(!commands.contains(&"system_prompt"));
        assert!(!commands.contains(&"quit"));
    }

    #[test]
    fn telegram_access_without_chat_id_lists_pending_requests() {
        let requests = vec![pending_request(42)];
        let state = DashboardState::default();
        let context = DashboardCommandContext {
            requests: &requests,
            state: &state,
            executor: None,
        };

        let output = execute_access_request_command(true, &["telegram", "approve"], &context);

        assert!(output.contains("pending requests"));
        assert!(output.contains("/telegram approve <chat_id>"));
        assert!(output.contains("42"));
    }

    #[test]
    fn local_telegram_access_picker_matches_no_arg_commands() {
        let requests = vec![pending_request(42), pending_request(7)];

        let approve = telegram_access_picker_for_input("/telegram approve", &requests)
            .expect("approve command should open picker");
        assert_eq!(approve.action.verb(), "approve");
        assert_eq!(approve.requests.len(), 2);

        let reject = telegram_access_picker_for_input("/telegram reject ", &requests)
            .expect("reject command should open picker");
        assert_eq!(reject.action.verb(), "reject");

        assert!(telegram_access_picker_for_input("/telegram approve 42", &requests).is_none());
        assert!(telegram_access_picker_for_input("/status", &requests).is_none());
    }
}
