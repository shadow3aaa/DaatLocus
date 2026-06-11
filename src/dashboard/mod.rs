//! Dashboard: activity feed + command console.

pub mod cells;
pub mod frame_rate_limiter;
pub mod frame_requester;
pub mod history;
pub mod render;
pub mod renderable;
pub mod tui_event;

pub use cells::{
    ActivityCell, CachedActivityLines, DashboardActivityEvent, LiveActivityCell,
    LiveWebActivityItem, ReducedMotion, WebActivityItem, activity_cell_from_tool_ui_event,
    activity_cells_from_history_items, apply_activity_event, assistant_activity_cell,
    default_web_activity_version, render_activity_feed_cached, render_activity_from_messages,
    sync_web_activity_state, thinking_activity_cell, user_activity_cell_from_event,
    web_activity_item_from_cell,
};
pub use history::{
    DashboardActivityHistoryPage, DashboardActivityHistoryStore, DashboardActivityHistoryWindow,
};

use std::{collections::HashSet, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use crossterm::cursor::SetCursorStyle;
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use futures_util::StreamExt;
use ratatui::{
    prelude::*,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Paragraph, Wrap},
};
use std::time::Duration;

use crate::{
    app::AppId,
    core::TokenUsageInfo,
    openskills::{OpenSkillDashboardError, OpenSkillDashboardSummary},
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DashboardSessionTitle {
    pub title: String,
    pub generated: bool,
    pub updated_at_ms: i64,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct DashboardTokenUsageSnapshot {
    pub main: Option<TokenUsageInfo>,
    #[serde(default)]
    pub main_model: Option<String>,
    pub judge: Option<TokenUsageInfo>,
    #[serde(default)]
    pub judge_model: Option<String>,
    #[serde(default)]
    pub efficient_model: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct DashboardPrimitiveOptimizationSnapshot {
    pub running: bool,
    pub current_trigger: Option<String>,
    pub last_result: Option<String>,
    pub last_completed_at_ms: Option<i64>,
    pub primitive_evidence_records: usize,
    pub total_primitive_evidence_run_records: usize,
    pub total_primitive_reflections: usize,
    pub total_primitive_patch_candidates: usize,
    pub total_primitive_merge_candidates: usize,
    pub total_primitive_candidate_evaluations: usize,
    pub total_primitive_frontier_entries: usize,
    pub latest_primitive_frontier_root_entries: usize,
    pub latest_primitive_frontier_branched_entries: usize,
    pub latest_primitive_frontier_max_generation: usize,
    pub total_primitive_patch_applied: usize,
    pub total_primitive_merge_applied: usize,
    pub total_primitive_update_rollbacks: usize,
    pub total_primitive_optimization_rounds: usize,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct DashboardRuntimeOptimizationSnapshot {
    pub running: bool,
    pub current_trigger: Option<String>,
    pub last_result: Option<String>,
    pub last_completed_at_ms: Option<i64>,
    pub unread_runtime_error_backlog: usize,
    pub total_runtime_error_cases_consumed: usize,
    pub total_runtime_error_cases: usize,
    pub total_runtime_error_reflections: usize,
    pub total_runtime_contract_candidates: usize,
    pub total_runtime_contract_candidate_evaluations: usize,
    pub total_runtime_contract_system_additions: usize,
    pub total_runtime_contract_updates: usize,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct DashboardContextCompositionSegment {
    pub name: String,
    pub label: String,
    pub source: String,
    pub tokens: usize,
    pub bytes: usize,
    pub percent: f64,
    pub hash: String,
    pub cache_role: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct DashboardContextCompositionPrefixUnit {
    pub hash: String,
    pub tokens: usize,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct DashboardContextCompositionSnapshot {
    pub captured_at_ms: Option<i64>,
    pub model: Option<String>,
    pub total_estimated_tokens: usize,
    pub total_bytes: usize,
    pub message_count: usize,
    pub tool_count: usize,
    pub tools_schema_tokens: usize,
    pub stable_prefix_tokens: usize,
    pub new_suffix_tokens: usize,
    pub changed_prefix_tokens: usize,
    pub previous_common_prefix_tokens: usize,
    pub previous_request_hash: Option<String>,
    pub current_request_hash: Option<String>,
    pub segments: Vec<DashboardContextCompositionSegment>,
    #[serde(default)]
    pub prefix_units: Vec<DashboardContextCompositionPrefixUnit>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DashboardRuntimeActivityStatus {
    #[default]
    Idle,
    Thinking,
    Running,
    Tooling,
    Waiting,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DashboardRuntimeStatusLevel {
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardRuntimeActivity {
    pub status: DashboardRuntimeActivityStatus,
    pub label: String,
    #[serde(default)]
    pub detail: Option<String>,
    pub active_runtime_turn: bool,
    #[serde(default)]
    pub active_runtime_phase: Option<String>,
}

impl DashboardRuntimeActivity {
    pub fn new(
        status: DashboardRuntimeActivityStatus,
        label: impl Into<String>,
        detail: Option<String>,
    ) -> Self {
        Self {
            status,
            label: label.into(),
            detail,
            active_runtime_turn: false,
            active_runtime_phase: None,
        }
    }

    pub fn with_runtime_turn(mut self, active_runtime_phase: Option<String>) -> Self {
        self.active_runtime_turn = true;
        self.active_runtime_phase = active_runtime_phase;
        self
    }
}

impl Default for DashboardRuntimeActivity {
    fn default() -> Self {
        Self::new(DashboardRuntimeActivityStatus::Idle, "Idle", None)
    }
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct DashboardState {
    #[serde(default = "default_agent_name")]
    pub agent_name: String,
    #[serde(default)]
    pub session_title: Option<DashboardSessionTitle>,
    pub focused_app: Option<AppId>,
    pub status_output: String,
    pub sleep_status_output: String,
    pub inspect_telegram_output: String,
    pub system_prompt_output: String,
    pub preturn_context_output: String,
    pub app_status_outputs: Vec<(String, String)>,
    #[serde(default)]
    pub skills: Vec<OpenSkillDashboardSummary>,
    #[serde(default)]
    pub skill_errors: Vec<OpenSkillDashboardError>,
    #[serde(default)]
    pub pending_access_requests: Vec<PendingAccessRequest>,
    pub activity_cells: Vec<ActivityCell>,
    pub live_activity_cells: Vec<LiveActivityCell>,
    #[serde(default = "default_web_activity_version")]
    pub web_activity_version: u8,
    #[serde(default)]
    pub web_activity_items: Vec<WebActivityItem>,
    #[serde(default)]
    pub live_web_activity_items: Vec<LiveWebActivityItem>,
    #[serde(default)]
    pub activity_history: DashboardActivityHistoryWindow,
    pub last_cycle_elapsed_ms: Option<u64>,
    pub runtime_status: Option<String>,
    #[serde(default)]
    pub runtime_status_level: Option<DashboardRuntimeStatusLevel>,
    #[serde(default)]
    pub runtime_activity: DashboardRuntimeActivity,
    #[serde(default)]
    pub current_plan_step: Option<DashboardPlanStep>,
    #[serde(default)]
    pub token_usage: DashboardTokenUsageSnapshot,
    #[serde(default)]
    pub primitive_optimization: DashboardPrimitiveOptimizationSnapshot,
    #[serde(default)]
    pub runtime_optimization: DashboardRuntimeOptimizationSnapshot,
    #[serde(default)]
    pub context_composition: Option<DashboardContextCompositionSnapshot>,
    #[serde(default)]
    pub reduced_motion: ReducedMotion,
    pub footer_context: String,
    pub footer_estimated_input_tokens: Option<usize>,
}

fn default_agent_name() -> String {
    dashboard_agent_name()
}

pub fn dashboard_agent_name() -> String {
    let name = load_prompt_persona_spec_sync().name.trim().to_string();
    if name.is_empty() {
        "Agent".to_string()
    } else {
        name
    }
}

#[derive(Clone, Debug)]
pub enum DashboardControlCommand {
    RunSleep,
    ClearConversation,
    RestartDaemon,
    ReloadSkills,
    SetSkillAutoUse { path: PathBuf, enabled: bool },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DashboardAction {
    RunSleep,
    ClearConversation,
    RestartDaemon,
    ReloadSkills,
    SetSkillAutoUse { path: PathBuf, enabled: bool },
    ApproveTelegramAccess { chat_id: i64 },
    RejectTelegramAccess { chat_id: i64 },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DashboardActionResult {
    pub success: bool,
    pub message: String,
    #[serde(default)]
    pub detail: Option<String>,
}

#[async_trait]
pub trait DashboardCommandRunner: Send + Sync {
    async fn run_command(&self, command: &str, state: &DashboardState) -> String;
    async fn run_action(
        &self,
        action: DashboardAction,
        state: &DashboardState,
    ) -> DashboardActionResult;
}

#[async_trait]
pub trait DashboardHistoryLoader: Send + Sync {
    async fn load_history_before(
        &self,
        before: Option<i64>,
        limit: usize,
    ) -> Result<DashboardActivityHistoryPage, String>;
}

#[derive(Clone)]
pub struct DashboardIncomingAttachment {
    pub media_type: String,
    pub local_path: String,
    pub description: Option<String>,
}

/// Editable input string with cursor tracking for in-place editing.
#[derive(Debug)]
struct InputState {
    text: String,
    /// Byte offset of the cursor within `text`.
    cursor_pos: usize,
}

impl InputState {
    fn new() -> Self {
        Self {
            text: String::new(),
            cursor_pos: 0,
        }
    }

    fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    fn as_str(&self) -> &str {
        &self.text
    }

    /// Insert a character at cursor and advance cursor past it.
    fn insert_char(&mut self, c: char) {
        self.text.insert(self.cursor_pos, c);
        self.cursor_pos += c.len_utf8();
    }

    /// Delete the character before the cursor (Backspace).
    fn delete_before_cursor(&mut self) {
        if self.cursor_pos > 0 {
            let mut prev = self.cursor_pos - 1;
            while prev > 0 && !self.text.is_char_boundary(prev) {
                prev -= 1;
            }
            self.text.remove(prev);
            self.cursor_pos = prev;
        }
    }

    fn move_left(&mut self) {
        if self.cursor_pos > 0 {
            let mut pos = self.cursor_pos - 1;
            while pos > 0 && !self.text.is_char_boundary(pos) {
                pos -= 1;
            }
            self.cursor_pos = pos;
        }
    }

    fn move_right(&mut self) {
        if self.cursor_pos < self.text.len() {
            let mut pos = self.cursor_pos + 1;
            while pos < self.text.len() && !self.text.is_char_boundary(pos) {
                pos += 1;
            }
            self.cursor_pos = pos;
        }
    }

    fn move_home(&mut self) {
        self.cursor_pos = 0;
    }

    fn move_end(&mut self) {
        self.cursor_pos = self.text.len();
    }

    fn clear(&mut self) {
        self.text.clear();
        self.cursor_pos = 0;
    }

    /// Replace text and move cursor to end.
    fn set_text(&mut self, text: String) {
        self.text = text;
        self.cursor_pos = self.text.len();
    }
}

struct CommandDetailPanel {
    title: String,
    text: String,
    scroll: u16,
}

struct CommandSelectionPanel {
    title: String,
    subtitle: Option<String>,
    items: Vec<CommandSelectionItem>,
    selected: usize,
    scroll: usize,
}

struct CommandSelectionItem {
    name: String,
    description: String,
    action: CommandSelectionAction,
    disabled: bool,
}

enum CommandSelectionAction {
    ShowDetail {
        title: String,
        text: String,
    },
    OpenSkillsList,
    OpenSkillsToggle,
    OpenTelegramAccess(TelegramAccessAction),
    RunAction {
        title: String,
        action: DashboardAction,
        keep_panel: bool,
    },
}

struct SkillsListPanel {
    items: Vec<SkillsListPanelItem>,
    errors: Vec<OpenSkillDashboardError>,
    selected: usize,
    scroll: usize,
    search: String,
}

#[derive(Clone)]
struct SkillsListPanelItem {
    name: String,
    description: String,
    path: String,
    scope: String,
    status: String,
}

struct SkillsTogglePanel {
    items: Vec<SkillsTogglePanelItem>,
    selected: usize,
    scroll: usize,
    search: String,
    feedback: Option<CommandFeedback>,
}

#[derive(Clone)]
struct SkillsTogglePanelItem {
    name: String,
    description: String,
    path: String,
    scope: String,
    allow_implicit_invocation: bool,
    user_disabled: bool,
    auto_use_enabled: bool,
}

enum CommandPanel {
    Detail(CommandDetailPanel),
    Selection(CommandSelectionPanel),
    SkillsList(SkillsListPanel),
    SkillsToggle(SkillsTogglePanel),
    TelegramAccess(TelegramAccessPicker),
}

#[derive(Clone, Debug)]
struct CommandFeedback {
    title: String,
    message: String,
    detail: Option<String>,
    level: CommandFeedbackLevel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommandFeedbackLevel {
    Info,
    Warning,
    Error,
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

enum CommandPanelAction {
    None,
    Close,
    Replace(CommandPanel),
    OpenSkillsList,
    OpenSkillsToggle,
    OpenTelegramAccess(TelegramAccessAction),
    RunAction {
        title: String,
        action: DashboardAction,
        keep_panel: bool,
    },
}

struct DashboardActionInvocation {
    title: String,
    action: DashboardAction,
    quiet_success: bool,
}

impl CommandPanel {
    fn sync_state(&mut self, state: &DashboardState) {
        match self {
            CommandPanel::SkillsList(panel) => panel.sync_state(state),
            CommandPanel::SkillsToggle(panel) => panel.sync_state(state),
            CommandPanel::Detail(_)
            | CommandPanel::Selection(_)
            | CommandPanel::TelegramAccess(_) => {}
        }
    }

    fn desired_height(&self) -> u16 {
        match self {
            CommandPanel::Detail(panel) => {
                let line_count = render_panel_text_lines(&panel.text).len() as u16;
                line_count.saturating_add(3).clamp(5, 16)
            }
            CommandPanel::Selection(panel) => {
                let header = 1 + u16::from(panel.subtitle.is_some());
                header
                    .saturating_add(panel.items.len().min(8) as u16)
                    .saturating_add(2)
                    .clamp(5, 14)
            }
            CommandPanel::SkillsList(panel) => {
                let rows = panel.visible_indices().len().min(8) as u16;
                let error_rows = panel.errors.len().min(2) as u16;
                4u16.saturating_add(rows)
                    .saturating_add(error_rows)
                    .clamp(6, 16)
            }
            CommandPanel::SkillsToggle(panel) => {
                let rows = panel.visible_indices().len().min(8) as u16;
                let feedback_rows = command_feedback_row_count(panel.feedback.as_ref());
                4u16.saturating_add(rows)
                    .saturating_add(feedback_rows)
                    .clamp(6, 16)
            }
            CommandPanel::TelegramAccess(picker) => 4u16
                .saturating_add(
                    picker
                        .requests
                        .len()
                        .min(TELEGRAM_ACCESS_PICKER_VISIBLE_ROWS) as u16,
                )
                .clamp(6, 15),
        }
    }

    fn footer_hint(&self) -> &'static str {
        match self {
            CommandPanel::Detail(_) => "Esc close   ↑/↓ scroll   PgUp/PgDn page",
            CommandPanel::Selection(_) => "Enter select   ↑/↓ move   PgUp/PgDn page   Esc close",
            CommandPanel::SkillsList(_) => {
                "Enter details   type search   Backspace edit   ↑/↓ move   Esc close"
            }
            CommandPanel::SkillsToggle(_) => {
                "Space/Enter toggle auto-use   type search   Backspace edit   Esc close"
            }
            CommandPanel::TelegramAccess(picker) => match picker.action {
                TelegramAccessAction::Approve => {
                    "Enter approve selected   ↑/↓ move   PgUp/PgDn page   Esc cancel"
                }
                TelegramAccessAction::Reject => {
                    "Enter reject selected   ↑/↓ move   PgUp/PgDn page   Esc cancel"
                }
            },
        }
    }

    fn set_error_feedback(&mut self, feedback: CommandFeedback) {
        if let CommandPanel::SkillsToggle(panel) = self {
            panel.feedback =
                matches!(feedback.level, CommandFeedbackLevel::Error).then_some(feedback);
        }
    }

    fn clear_feedback(&mut self) {
        if let CommandPanel::SkillsToggle(panel) = self {
            panel.feedback = None;
        }
    }
}

impl SkillsListPanel {
    fn from_state(state: &DashboardState) -> Self {
        Self {
            items: state
                .skills
                .iter()
                .map(SkillsListPanelItem::from_summary)
                .collect(),
            errors: state.skill_errors.clone(),
            selected: 0,
            scroll: 0,
            search: String::new(),
        }
    }

    fn sync_state(&mut self, state: &DashboardState) {
        let selected_path = self
            .selected_actual_index()
            .and_then(|idx| self.items.get(idx))
            .map(|item| item.path.clone());
        self.items = state
            .skills
            .iter()
            .map(SkillsListPanelItem::from_summary)
            .collect();
        self.errors = state.skill_errors.clone();
        if let Some(selected_path) = selected_path
            && let Some(actual_idx) = self
                .items
                .iter()
                .position(|item| item.path == selected_path)
            && let Some(visible_idx) = self
                .visible_indices()
                .iter()
                .position(|idx| *idx == actual_idx)
        {
            self.selected = visible_idx;
        }
        self.clamp_after_filter_change();
    }

    fn visible_indices(&self) -> Vec<usize> {
        let query = self.search.trim().to_ascii_lowercase();
        self.items
            .iter()
            .enumerate()
            .filter_map(|(idx, item)| {
                if query.is_empty()
                    || item.name.to_ascii_lowercase().contains(&query)
                    || item.description.to_ascii_lowercase().contains(&query)
                    || item.path.to_ascii_lowercase().contains(&query)
                    || item.scope.to_ascii_lowercase().contains(&query)
                {
                    Some(idx)
                } else {
                    None
                }
            })
            .collect()
    }

    fn selected_actual_index(&self) -> Option<usize> {
        self.visible_indices().get(self.selected).copied()
    }

    fn selected_detail_panel(&self) -> Option<CommandPanel> {
        let idx = self.selected_actual_index()?;
        let item = self.items.get(idx)?;
        Some(detail_panel(
            format!("SKILL {}", item.name),
            [
                format!("Name: {}", item.name),
                format!("Status: {}", item.status),
                format!("Scope: {}", item.scope),
                format!("Path: {}", item.path),
                format!("Description: {}", item.description),
            ]
            .join("\n"),
        ))
    }

    fn clamp_after_filter_change(&mut self) {
        let visible_len = self.visible_indices().len();
        self.selected = self.selected.min(visible_len.saturating_sub(1));
        self.scroll = adjusted_list_scroll(self.scroll, self.selected, visible_len, 8);
    }
}

impl SkillsListPanelItem {
    fn from_summary(skill: &OpenSkillDashboardSummary) -> Self {
        Self {
            name: skill.name.clone(),
            description: skill.description.clone(),
            path: skill.path.clone(),
            scope: skill.scope.clone(),
            status: skill_status_description(skill),
        }
    }
}

impl CommandSelectionPanel {
    fn adjusted_scroll(&self) -> usize {
        adjusted_list_scroll(self.scroll, self.selected, self.items.len(), 8)
    }
}

impl SkillsTogglePanel {
    fn from_state(state: &DashboardState) -> Self {
        Self {
            items: state
                .skills
                .iter()
                .map(SkillsTogglePanelItem::from_summary)
                .collect(),
            selected: 0,
            scroll: 0,
            search: String::new(),
            feedback: None,
        }
    }

    fn sync_state(&mut self, state: &DashboardState) {
        let selected_path = self
            .selected_actual_index()
            .and_then(|idx| self.items.get(idx))
            .map(|item| item.path.clone());
        self.items = state
            .skills
            .iter()
            .map(SkillsTogglePanelItem::from_summary)
            .collect();
        if let Some(selected_path) = selected_path
            && let Some(actual_idx) = self
                .items
                .iter()
                .position(|item| item.path == selected_path)
            && let Some(visible_idx) = self
                .visible_indices()
                .iter()
                .position(|idx| *idx == actual_idx)
        {
            self.selected = visible_idx;
        }
        self.clamp_after_filter_change();
    }

    fn visible_indices(&self) -> Vec<usize> {
        let query = self.search.trim().to_ascii_lowercase();
        self.items
            .iter()
            .enumerate()
            .filter_map(|(idx, item)| {
                if query.is_empty()
                    || item.name.to_ascii_lowercase().contains(&query)
                    || item.description.to_ascii_lowercase().contains(&query)
                    || item.path.to_ascii_lowercase().contains(&query)
                {
                    Some(idx)
                } else {
                    None
                }
            })
            .collect()
    }

    fn selected_actual_index(&self) -> Option<usize> {
        self.visible_indices().get(self.selected).copied()
    }

    fn clamp_after_filter_change(&mut self) {
        let visible_len = self.visible_indices().len();
        self.selected = self.selected.min(visible_len.saturating_sub(1));
        self.scroll = adjusted_list_scroll(self.scroll, self.selected, visible_len, 8);
    }
}

impl SkillsTogglePanelItem {
    fn from_summary(skill: &OpenSkillDashboardSummary) -> Self {
        Self {
            name: skill.name.clone(),
            description: skill.description.clone(),
            path: skill.path.clone(),
            scope: skill.scope.clone(),
            allow_implicit_invocation: skill.allow_implicit_invocation,
            user_disabled: skill.user_disabled,
            auto_use_enabled: skill.auto_use_enabled,
        }
    }

    fn status_description(&self) -> String {
        if self.auto_use_enabled {
            "auto-use enabled".to_string()
        } else if self.user_disabled {
            "manual-only: disabled by /skills".to_string()
        } else if !self.allow_implicit_invocation {
            "manual-only: policy disallows implicit invocation".to_string()
        } else {
            "manual-only".to_string()
        }
    }
}

struct DashboardCommandContext<'a> {
    requests: &'a [PendingAccessRequest],
    state: &'a DashboardState,
}

#[derive(Clone)]
struct CommandSuggestion {
    display: String,
    completion: String,
    description: String,
}

#[derive(Clone, Copy)]
struct DashboardCommandSpec {
    primary_verb: &'static str,
    description: &'static str,
    aliases: &'static [&'static str],
    remote_command: Option<&'static str>,
    remote_description: Option<&'static str>,
}

impl DashboardCommandSpec {
    fn accepts(self, verb: &str) -> bool {
        self.primary_verb == verb || self.aliases.contains(&verb)
    }

    fn remote_description(self) -> &'static str {
        self.remote_description.unwrap_or(self.description)
    }
}

const NO_ALIASES: &[&str] = &[];
const QUIT_ALIASES: &[&str] = &["q", "exit"];
const APP_STATUS_ALIASES: &[&str] = &["app_status"];

static DASHBOARD_COMMANDS: [DashboardCommandSpec; 9] = [
    DashboardCommandSpec {
        primary_verb: "quit",
        description: "exit the dashboard",
        aliases: QUIT_ALIASES,
        remote_command: None,
        remote_description: None,
    },
    DashboardCommandSpec {
        primary_verb: "clear",
        description: "clear runtime conversation history, current plan, and all events",
        aliases: NO_ALIASES,
        remote_command: Some("clear"),
        remote_description: None,
    },
    DashboardCommandSpec {
        primary_verb: "debug",
        description: "debug outputs and internal runtime views",
        aliases: NO_ALIASES,
        remote_command: Some("debug"),
        remote_description: None,
    },
    DashboardCommandSpec {
        primary_verb: "app-status",
        description: "show current structured app state and llm-facing note",
        aliases: APP_STATUS_ALIASES,
        remote_command: Some("app_status"),
        remote_description: None,
    },
    DashboardCommandSpec {
        primary_verb: "status",
        description: "show overall status",
        aliases: NO_ALIASES,
        remote_command: Some("status"),
        remote_description: None,
    },
    DashboardCommandSpec {
        primary_verb: "restart",
        description: "restart the daemon",
        aliases: NO_ALIASES,
        remote_command: Some("restart"),
        remote_description: None,
    },
    DashboardCommandSpec {
        primary_verb: "sleep",
        description: "sleep controls and status",
        aliases: NO_ALIASES,
        remote_command: Some("sleep"),
        remote_description: None,
    },
    DashboardCommandSpec {
        primary_verb: "skills",
        description: "list and manage OpenSkills automatic use",
        aliases: NO_ALIASES,
        remote_command: Some("skills"),
        remote_description: None,
    },
    DashboardCommandSpec {
        primary_verb: "telegram",
        description: "telegram status and access controls",
        aliases: NO_ALIASES,
        remote_command: Some("telegram"),
        remote_description: None,
    },
];

fn dashboard_commands() -> &'static [DashboardCommandSpec] {
    &DASHBOARD_COMMANDS
}

fn dashboard_command_spec(primary_verb: &str) -> Option<DashboardCommandSpec> {
    dashboard_commands()
        .iter()
        .copied()
        .find(|command| command.primary_verb == primary_verb)
}

fn dashboard_command_accepts(primary_verb: &str, verb: &str) -> bool {
    dashboard_command_spec(primary_verb).is_some_and(|command| command.accepts(verb))
}

fn quit_command_accepts(verb: &str) -> bool {
    dashboard_command_accepts("quit", verb)
}

fn clear_command_accepts(verb: &str) -> bool {
    dashboard_command_accepts("clear", verb)
}

fn status_command_accepts(verb: &str) -> bool {
    dashboard_command_accepts("status", verb)
}

fn restart_command_accepts(verb: &str) -> bool {
    dashboard_command_accepts("restart", verb)
}

fn debug_command_accepts(verb: &str) -> bool {
    dashboard_command_accepts("debug", verb)
}

fn app_status_command_accepts(verb: &str) -> bool {
    dashboard_command_accepts("app-status", verb)
}

fn sleep_command_accepts(verb: &str) -> bool {
    dashboard_command_accepts("sleep", verb)
}

fn skills_command_accepts(verb: &str) -> bool {
    dashboard_command_accepts("skills", verb)
}

fn telegram_command_accepts(verb: &str) -> bool {
    dashboard_command_accepts("telegram", verb)
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
                .remote_command
                .map(|remote_command| RemoteDashboardCommand {
                    command: remote_command,
                    description: command.remote_description(),
                })
        })
        .collect()
}

fn render_skills_list(state: &DashboardState) -> String {
    if state.skills.is_empty() {
        let mut lines = vec![
            "No OpenSkills loaded.".to_string(),
            "Scanned fixed roots: project .agents/skills, ~/.daat-locus/skills, ~/.agents/skills."
                .to_string(),
        ];
        if !state.skill_errors.is_empty() {
            lines.push(String::new());
            lines.push("Load errors:".to_string());
            lines.extend(state.skill_errors.iter().map(|error| {
                format!(
                    "  {} | {}",
                    error.path,
                    truncate_command_text(&error.message, 160)
                )
            }));
        }
        return lines.join("\n");
    }

    let auto_count = state
        .skills
        .iter()
        .filter(|skill| skill.auto_use_enabled)
        .count();
    let manual_count = state.skills.len().saturating_sub(auto_count);
    let mut lines = vec![
        format!(
            "OpenSkills loaded: {} (auto: {auto_count}, manual-only: {manual_count})",
            state.skills.len()
        ),
        "Use /skills in the dashboard to browse, inspect, or toggle skills.".to_string(),
        String::new(),
        "Skills:".to_string(),
    ];
    lines.extend(state.skills.iter().map(|skill| {
        format!(
            "  {:<22} {:<12} {} - {}",
            skill.name,
            skill_status_label(skill),
            skill.scope,
            truncate_command_text(&skill.description, 140)
        )
    }));
    if !state.skill_errors.is_empty() {
        lines.push(String::new());
        lines.push("Load errors:".to_string());
        lines.extend(state.skill_errors.iter().map(|error| {
            format!(
                "  {} | {}",
                error.path,
                truncate_command_text(&error.message, 160)
            )
        }));
    }
    lines.join("\n")
}

fn render_skill_detail(state: &DashboardState, target: &str) -> String {
    let skill = match resolve_skill_target(state, target) {
        Ok(skill) => skill,
        Err(message) => return message,
    };
    [
        format!("Name: {}", skill.name),
        format!("Status: {}", skill_status_description(skill)),
        format!("Scope: {}", skill.scope),
        format!("Path: {}", skill.path),
        format!("Description: {}", skill.description),
    ]
    .join("\n")
}

fn resolve_skill_target<'a>(
    state: &'a DashboardState,
    target: &str,
) -> Result<&'a OpenSkillDashboardSummary, String> {
    let target = target.trim();
    if target.is_empty() {
        return Err("skill name or path is required".to_string());
    }

    let path_matches = state
        .skills
        .iter()
        .filter(|skill| skill.path == target)
        .collect::<Vec<_>>();
    if path_matches.len() == 1 {
        return Ok(path_matches[0]);
    }

    let name_matches = state
        .skills
        .iter()
        .filter(|skill| skill.name == target)
        .collect::<Vec<_>>();
    match name_matches.len() {
        1 => Ok(name_matches[0]),
        0 => Err(format!("unknown skill: {target}")),
        _ => {
            let paths = name_matches
                .iter()
                .map(|skill| format!("  {}", skill.path))
                .collect::<Vec<_>>()
                .join("\n");
            Err(format!(
                "ambiguous skill name: {target}\nuse full path:\n{paths}"
            ))
        }
    }
}

fn skill_status_label(skill: &OpenSkillDashboardSummary) -> &'static str {
    if skill.auto_use_enabled {
        "auto"
    } else {
        "manual-only"
    }
}

fn skill_status_description(skill: &OpenSkillDashboardSummary) -> String {
    if skill.auto_use_enabled {
        "auto-use enabled".to_string()
    } else if skill.user_disabled {
        "manual-only: disabled by /skills".to_string()
    } else if !skill.allow_implicit_invocation {
        "manual-only: policy disallows implicit invocation".to_string()
    } else {
        "manual-only".to_string()
    }
}

fn truncate_command_text(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out = text
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    out.push_str("...");
    out
}

pub(crate) fn execute_dashboard_action(
    action: DashboardAction,
    telegram_acl: &TelegramAclHandle,
    control_tx: &tokio::sync::mpsc::UnboundedSender<DashboardControlCommand>,
) -> DashboardActionResult {
    match action {
        DashboardAction::RunSleep => match control_tx.send(DashboardControlCommand::RunSleep) {
            Ok(()) => DashboardActionResult::ok("queued sleep run"),
            Err(err) => DashboardActionResult::error(format!("failed to queue sleep run: {err}")),
        },
        DashboardAction::ClearConversation => {
            match control_tx.send(DashboardControlCommand::ClearConversation) {
                Ok(()) => DashboardActionResult::ok("queued runtime clear"),
                Err(err) => DashboardActionResult::error(format!("failed to queue clear: {err}")),
            }
        }
        DashboardAction::RestartDaemon => {
            match control_tx.send(DashboardControlCommand::RestartDaemon) {
                Ok(()) => DashboardActionResult::ok("queued daemon restart"),
                Err(err) => {
                    DashboardActionResult::error(format!("failed to queue daemon restart: {err}"))
                }
            }
        }
        DashboardAction::ReloadSkills => {
            match control_tx.send(DashboardControlCommand::ReloadSkills) {
                Ok(()) => DashboardActionResult::ok("queued skills reload"),
                Err(err) => {
                    DashboardActionResult::error(format!("failed to queue skills reload: {err}"))
                }
            }
        }
        DashboardAction::SetSkillAutoUse { path, enabled } => {
            match control_tx.send(DashboardControlCommand::SetSkillAutoUse { path, enabled }) {
                Ok(()) => {
                    let action = if enabled { "enable" } else { "disable" };
                    DashboardActionResult::ok(format!("queued skills auto-use {action}"))
                }
                Err(err) => {
                    DashboardActionResult::error(format!("failed to queue skills auto-use: {err}"))
                }
            }
        }
        DashboardAction::ApproveTelegramAccess { chat_id } => match telegram_acl.approve(chat_id) {
            Ok(()) => DashboardActionResult::ok(format!("approved {chat_id}")),
            Err(err) => {
                DashboardActionResult::error(format!("approve failed for {chat_id}: {err}"))
            }
        },
        DashboardAction::RejectTelegramAccess { chat_id } => match telegram_acl.reject(chat_id) {
            Ok(()) => DashboardActionResult::ok(format!("rejected {chat_id}")),
            Err(err) => DashboardActionResult::error(format!("reject failed for {chat_id}: {err}")),
        },
    }
}

impl DashboardActionResult {
    fn ok(message: impl Into<String>) -> Self {
        Self {
            success: true,
            message: message.into(),
            detail: None,
        }
    }

    fn error(message: impl Into<String>) -> Self {
        Self {
            success: false,
            message: message.into(),
            detail: None,
        }
    }
}

fn command_feedback_from_action_result(
    title: String,
    result: DashboardActionResult,
) -> CommandFeedback {
    CommandFeedback {
        title,
        message: result.message,
        detail: result.detail,
        level: if result.success {
            CommandFeedbackLevel::Info
        } else {
            CommandFeedbackLevel::Error
        },
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

fn command_panel_for_input(
    input: &str,
    context: &DashboardCommandContext<'_>,
) -> Option<CommandPanel> {
    let parts = dashboard_command_parts(input)?;
    match parts.as_slice() {
        ["status"] => Some(detail_panel(
            "STATUS",
            fallback_output(&context.state.status_output),
        )),
        ["debug"] => Some(debug_command_panel(context.state)),
        ["debug", "persona"] => Some(debug_persona_panel()),
        ["debug", "system-prompt"] | ["debug", "system_prompt"] => {
            Some(debug_system_prompt_panel(context.state))
        }
        ["debug", "context"] | ["debug", "preturn-context"] | ["debug", "preturn_context"] => {
            Some(debug_context_panel(context.state))
        }
        ["sleep"] => Some(sleep_command_panel(context.state)),
        ["sleep", "status"] => Some(sleep_status_panel(context.state)),
        ["telegram"] => Some(telegram_command_panel(context.state, context.requests)),
        ["telegram", "status"] => Some(telegram_status_panel(context.state)),
        ["telegram", "approve"] => Some(
            telegram_access_picker_for_input(input, context.requests)
                .map(CommandPanel::TelegramAccess)
                .unwrap_or_else(|| {
                    telegram_access_command_panel(TelegramAccessAction::Approve, context.requests)
                }),
        ),
        ["telegram", "reject"] => Some(
            telegram_access_picker_for_input(input, context.requests)
                .map(CommandPanel::TelegramAccess)
                .unwrap_or_else(|| {
                    telegram_access_command_panel(TelegramAccessAction::Reject, context.requests)
                }),
        ),
        [verb] if app_status_command_accepts(verb) => {
            Some(app_status_selection_panel(context.state))
        }
        [verb, target] if app_status_command_accepts(verb) => {
            Some(app_status_detail_panel(context.state, target))
        }
        ["skills"] => Some(skills_command_panel(context.state)),
        ["skills", "list"] | ["skills", "show"] => Some(CommandPanel::SkillsList(
            SkillsListPanel::from_state(context.state),
        )),
        ["skills", "show", target] => skill_detail_panel(context.state, target),
        _ => None,
    }
}

fn dashboard_action_for_input(
    input: &str,
    context: &DashboardCommandContext<'_>,
) -> Result<Option<DashboardActionInvocation>, CommandFeedback> {
    let Some(parts) = dashboard_command_parts(input) else {
        return Ok(None);
    };
    let invocation = match parts.as_slice() {
        ["clear"] => DashboardActionInvocation {
            title: "CLEAR".to_string(),
            action: DashboardAction::ClearConversation,
            quiet_success: true,
        },
        ["restart"] => DashboardActionInvocation {
            title: "RESTART".to_string(),
            action: DashboardAction::RestartDaemon,
            quiet_success: false,
        },
        ["sleep", "run"] => DashboardActionInvocation {
            title: "SLEEP".to_string(),
            action: DashboardAction::RunSleep,
            quiet_success: false,
        },
        ["skills", "reload"] => DashboardActionInvocation {
            title: "SKILLS".to_string(),
            action: DashboardAction::ReloadSkills,
            quiet_success: false,
        },
        ["skills", "enable", target] | ["skills", "disable", target] => {
            let enabled = parts[1] == "enable";
            let skill =
                resolve_skill_target(context.state, target).map_err(|message| CommandFeedback {
                    title: "SKILLS".to_string(),
                    message,
                    detail: None,
                    level: CommandFeedbackLevel::Error,
                })?;
            DashboardActionInvocation {
                title: "SKILLS".to_string(),
                action: DashboardAction::SetSkillAutoUse {
                    path: PathBuf::from(&skill.path),
                    enabled,
                },
                quiet_success: false,
            }
        }
        ["telegram", "approve", chat_id] | ["telegram", "reject", chat_id] => {
            let chat_id = chat_id.parse::<i64>().map_err(|_| CommandFeedback {
                title: "TELEGRAM".to_string(),
                message: format!("invalid chat_id: {chat_id}"),
                detail: None,
                level: CommandFeedbackLevel::Error,
            })?;
            let action = if parts[1] == "approve" {
                DashboardAction::ApproveTelegramAccess { chat_id }
            } else {
                DashboardAction::RejectTelegramAccess { chat_id }
            };
            DashboardActionInvocation {
                title: "TELEGRAM".to_string(),
                action,
                quiet_success: false,
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(invocation))
}

fn debug_command_panel(state: &DashboardState) -> CommandPanel {
    CommandPanel::Selection(CommandSelectionPanel {
        title: "Debug".to_string(),
        subtitle: Some("Inspect internal runtime views.".to_string()),
        items: vec![
            CommandSelectionItem {
                name: "Prompt persona".to_string(),
                description: "show current prompt persona config".to_string(),
                action: CommandSelectionAction::ShowDetail {
                    title: "DEBUG PERSONA".to_string(),
                    text: debug_persona_text(),
                },
                disabled: false,
            },
            CommandSelectionItem {
                name: "System prompt".to_string(),
                description: "show current runtime system prompt".to_string(),
                action: CommandSelectionAction::ShowDetail {
                    title: "DEBUG SYSTEM PROMPT".to_string(),
                    text: fallback_output(&state.system_prompt_output),
                },
                disabled: false,
            },
            CommandSelectionItem {
                name: "Runtime context".to_string(),
                description: "show latest pre-turn runtime context".to_string(),
                action: CommandSelectionAction::ShowDetail {
                    title: "DEBUG CONTEXT".to_string(),
                    text: fallback_output(&state.preturn_context_output),
                },
                disabled: false,
            },
        ],
        selected: 0,
        scroll: 0,
    })
}

fn debug_persona_text() -> String {
    let path = prompt_persona_path_sync();
    match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(_) => render_prompt_persona_markdown(&load_prompt_persona_spec_sync()),
    }
}

fn debug_persona_panel() -> CommandPanel {
    detail_panel("DEBUG PERSONA", debug_persona_text())
}

fn debug_system_prompt_panel(state: &DashboardState) -> CommandPanel {
    detail_panel(
        "DEBUG SYSTEM PROMPT",
        fallback_output(&state.system_prompt_output),
    )
}

fn debug_context_panel(state: &DashboardState) -> CommandPanel {
    detail_panel(
        "DEBUG CONTEXT",
        fallback_output(&state.preturn_context_output),
    )
}

fn sleep_command_panel(state: &DashboardState) -> CommandPanel {
    CommandPanel::Selection(CommandSelectionPanel {
        title: "Sleep".to_string(),
        subtitle: Some("Inspect sleep state or start a background sleep run.".to_string()),
        items: vec![
            CommandSelectionItem {
                name: "Status".to_string(),
                description: "show sleep status".to_string(),
                action: CommandSelectionAction::ShowDetail {
                    title: "SLEEP STATUS".to_string(),
                    text: fallback_output(&state.sleep_status_output),
                },
                disabled: false,
            },
            CommandSelectionItem {
                name: "Start sleep run".to_string(),
                description: "start a background sleep run".to_string(),
                action: CommandSelectionAction::RunAction {
                    title: "SLEEP".to_string(),
                    action: DashboardAction::RunSleep,
                    keep_panel: false,
                },
                disabled: false,
            },
        ],
        selected: 0,
        scroll: 0,
    })
}

fn sleep_status_panel(state: &DashboardState) -> CommandPanel {
    detail_panel("SLEEP STATUS", fallback_output(&state.sleep_status_output))
}

fn app_status_selection_panel(state: &DashboardState) -> CommandPanel {
    let items = state
        .app_status_outputs
        .iter()
        .map(|(name, output)| CommandSelectionItem {
            name: name.clone(),
            description: truncate_command_text(
                output
                    .lines()
                    .find(|line| !line.trim().is_empty())
                    .unwrap_or("app state"),
                120,
            ),
            action: CommandSelectionAction::ShowDetail {
                title: format!("APP STATUS {}", name.to_uppercase()),
                text: output.clone(),
            },
            disabled: false,
        })
        .collect::<Vec<_>>();
    if items.is_empty() {
        return detail_panel("APP STATUS", "No app state is currently available.");
    }
    CommandPanel::Selection(CommandSelectionPanel {
        title: "App Status".to_string(),
        subtitle: Some("Choose an app to inspect.".to_string()),
        items,
        selected: 0,
        scroll: 0,
    })
}

fn app_status_detail_panel(state: &DashboardState, target: &str) -> CommandPanel {
    let output = render_app_status_text(state, target);
    let target = target.trim().to_ascii_lowercase();
    detail_panel(format!("APP STATUS {}", target.to_uppercase()), output)
}

fn render_available_app_statuses(state: &DashboardState) -> String {
    let apps = state
        .app_status_outputs
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    if apps.is_empty() {
        "available apps: none".to_string()
    } else {
        format!("available apps: {}", apps.join(", "))
    }
}

fn render_app_status_text(state: &DashboardState, target: &str) -> String {
    let target = target.trim().to_ascii_lowercase();
    state
        .app_status_outputs
        .iter()
        .find(|(name, _)| name == &target)
        .map(|(_, output)| output.clone())
        .unwrap_or_else(|| {
            let apps = render_available_app_statuses(state);
            format!("unknown app: {target}\n{apps}")
        })
}

fn telegram_command_panel(
    state: &DashboardState,
    requests: &[PendingAccessRequest],
) -> CommandPanel {
    CommandPanel::Selection(CommandSelectionPanel {
        title: "Telegram".to_string(),
        subtitle: Some("Inspect transport state or handle access requests.".to_string()),
        items: vec![
            CommandSelectionItem {
                name: "Status".to_string(),
                description: "show Telegram transport details".to_string(),
                action: CommandSelectionAction::ShowDetail {
                    title: "TELEGRAM STATUS".to_string(),
                    text: fallback_output(&state.inspect_telegram_output),
                },
                disabled: false,
            },
            CommandSelectionItem {
                name: "Approve access request".to_string(),
                description: format!("approve one of {} pending requests", requests.len()),
                action: CommandSelectionAction::OpenTelegramAccess(TelegramAccessAction::Approve),
                disabled: requests.is_empty(),
            },
            CommandSelectionItem {
                name: "Reject access request".to_string(),
                description: format!("reject one of {} pending requests", requests.len()),
                action: CommandSelectionAction::OpenTelegramAccess(TelegramAccessAction::Reject),
                disabled: requests.is_empty(),
            },
        ],
        selected: 0,
        scroll: 0,
    })
}

fn telegram_status_panel(state: &DashboardState) -> CommandPanel {
    detail_panel(
        "TELEGRAM STATUS",
        fallback_output(&state.inspect_telegram_output),
    )
}

fn telegram_access_command_panel(
    action: TelegramAccessAction,
    requests: &[PendingAccessRequest],
) -> CommandPanel {
    if requests.is_empty() {
        return detail_panel(action.title(), "No pending Telegram access requests.");
    }
    CommandPanel::TelegramAccess(TelegramAccessPicker {
        action,
        requests: requests.to_vec(),
        selected: 0,
        scroll: 0,
    })
}

fn skills_command_panel(state: &DashboardState) -> CommandPanel {
    let auto_count = state
        .skills
        .iter()
        .filter(|skill| skill.auto_use_enabled)
        .count();
    let manual_count = state.skills.len().saturating_sub(auto_count);
    CommandPanel::Selection(CommandSelectionPanel {
        title: "Skills".to_string(),
        subtitle: Some(format!(
            "{} loaded, {auto_count} auto-use, {manual_count} manual-only",
            state.skills.len()
        )),
        items: vec![
            CommandSelectionItem {
                name: "List skills".to_string(),
                description: "show loaded skills and load errors".to_string(),
                action: CommandSelectionAction::OpenSkillsList,
                disabled: false,
            },
            CommandSelectionItem {
                name: "Enable/Disable Skills".to_string(),
                description: "toggle whether skills may be selected automatically".to_string(),
                action: CommandSelectionAction::OpenSkillsToggle,
                disabled: state.skills.is_empty(),
            },
        ],
        selected: 0,
        scroll: 0,
    })
}

fn skill_detail_panel(state: &DashboardState, target: &str) -> Option<CommandPanel> {
    let skill = resolve_skill_target(state, target).ok()?;
    Some(detail_panel(
        format!("SKILL {}", skill.name),
        [
            format!("Name: {}", skill.name),
            format!("Status: {}", skill_status_description(skill)),
            format!("Scope: {}", skill.scope),
            format!("Path: {}", skill.path),
            format!("Description: {}", skill.description),
        ]
        .join("\n"),
    ))
}

fn detail_panel(title: impl Into<String>, text: impl Into<String>) -> CommandPanel {
    CommandPanel::Detail(CommandDetailPanel {
        title: title.into(),
        text: text.into(),
        scroll: 0,
    })
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

fn handle_command_panel_key(panel: &mut CommandPanel, key: KeyEvent) -> CommandPanelAction {
    match panel {
        CommandPanel::Detail(detail) => handle_detail_panel_key(detail, key),
        CommandPanel::Selection(selection) => handle_selection_panel_key(selection, key),
        CommandPanel::SkillsList(skills) => handle_skills_list_panel_key(skills, key),
        CommandPanel::SkillsToggle(skills) => handle_skills_toggle_panel_key(skills, key),
        CommandPanel::TelegramAccess(picker) => handle_telegram_access_panel_key(picker, key),
    }
}

fn handle_detail_panel_key(panel: &mut CommandDetailPanel, key: KeyEvent) -> CommandPanelAction {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => CommandPanelAction::Close,
        KeyCode::Up | KeyCode::Char('k') => {
            panel.scroll = panel.scroll.saturating_sub(1);
            CommandPanelAction::None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            panel.scroll = panel.scroll.saturating_add(1);
            CommandPanelAction::None
        }
        KeyCode::PageUp => {
            panel.scroll = panel.scroll.saturating_sub(10);
            CommandPanelAction::None
        }
        KeyCode::PageDown => {
            panel.scroll = panel.scroll.saturating_add(10);
            CommandPanelAction::None
        }
        KeyCode::Home => {
            panel.scroll = 0;
            CommandPanelAction::None
        }
        KeyCode::End => {
            panel.scroll = u16::MAX;
            CommandPanelAction::None
        }
        _ => CommandPanelAction::None,
    }
}

fn handle_selection_panel_key(
    panel: &mut CommandSelectionPanel,
    key: KeyEvent,
) -> CommandPanelAction {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => CommandPanelAction::Close,
        KeyCode::Up | KeyCode::Char('k') => {
            panel.selected = panel
                .selected
                .saturating_sub(1)
                .min(panel.items.len().saturating_sub(1));
            panel.scroll = panel.adjusted_scroll();
            CommandPanelAction::None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            panel.selected = (panel.selected + 1).min(panel.items.len().saturating_sub(1));
            panel.scroll = panel.adjusted_scroll();
            CommandPanelAction::None
        }
        KeyCode::PageUp => {
            panel.selected = panel.selected.saturating_sub(8);
            panel.scroll = panel.adjusted_scroll();
            CommandPanelAction::None
        }
        KeyCode::PageDown => {
            panel.selected = (panel.selected + 8).min(panel.items.len().saturating_sub(1));
            panel.scroll = panel.adjusted_scroll();
            CommandPanelAction::None
        }
        KeyCode::Home => {
            panel.selected = 0;
            panel.scroll = 0;
            CommandPanelAction::None
        }
        KeyCode::End => {
            panel.selected = panel.items.len().saturating_sub(1);
            panel.scroll = panel.adjusted_scroll();
            CommandPanelAction::None
        }
        KeyCode::Enter => {
            let Some(item) = panel.items.get(panel.selected) else {
                return CommandPanelAction::None;
            };
            if item.disabled {
                return CommandPanelAction::None;
            }
            match &item.action {
                CommandSelectionAction::ShowDetail { title, text } => {
                    CommandPanelAction::Replace(detail_panel(title.clone(), text.clone()))
                }
                CommandSelectionAction::OpenSkillsList => CommandPanelAction::OpenSkillsList,
                CommandSelectionAction::RunAction {
                    title,
                    action,
                    keep_panel,
                } => CommandPanelAction::RunAction {
                    title: title.clone(),
                    action: action.clone(),
                    keep_panel: *keep_panel,
                },
                CommandSelectionAction::OpenSkillsToggle => CommandPanelAction::OpenSkillsToggle,
                CommandSelectionAction::OpenTelegramAccess(action) => {
                    CommandPanelAction::OpenTelegramAccess(*action)
                }
            }
        }
        _ => CommandPanelAction::None,
    }
}

fn handle_skills_list_panel_key(panel: &mut SkillsListPanel, key: KeyEvent) -> CommandPanelAction {
    match key.code {
        KeyCode::Esc => CommandPanelAction::Close,
        KeyCode::Up => {
            panel.selected = panel.selected.saturating_sub(1);
            panel.clamp_after_filter_change();
            CommandPanelAction::None
        }
        KeyCode::Down => {
            let len = panel.visible_indices().len();
            panel.selected = (panel.selected + 1).min(len.saturating_sub(1));
            panel.clamp_after_filter_change();
            CommandPanelAction::None
        }
        KeyCode::PageUp => {
            panel.selected = panel.selected.saturating_sub(8);
            panel.clamp_after_filter_change();
            CommandPanelAction::None
        }
        KeyCode::PageDown => {
            let len = panel.visible_indices().len();
            panel.selected = (panel.selected + 8).min(len.saturating_sub(1));
            panel.clamp_after_filter_change();
            CommandPanelAction::None
        }
        KeyCode::Home => {
            panel.selected = 0;
            panel.clamp_after_filter_change();
            CommandPanelAction::None
        }
        KeyCode::End => {
            panel.selected = panel.visible_indices().len().saturating_sub(1);
            panel.clamp_after_filter_change();
            CommandPanelAction::None
        }
        KeyCode::Backspace => {
            panel.search.pop();
            panel.clamp_after_filter_change();
            CommandPanelAction::None
        }
        KeyCode::Enter => panel
            .selected_detail_panel()
            .map(CommandPanelAction::Replace)
            .unwrap_or(CommandPanelAction::None),
        KeyCode::Char(c)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            panel.search.push(c);
            panel.clamp_after_filter_change();
            CommandPanelAction::None
        }
        _ => CommandPanelAction::None,
    }
}

fn handle_skills_toggle_panel_key(
    panel: &mut SkillsTogglePanel,
    key: KeyEvent,
) -> CommandPanelAction {
    match key.code {
        KeyCode::Esc => CommandPanelAction::Close,
        KeyCode::Up => {
            panel.selected = panel.selected.saturating_sub(1);
            panel.clamp_after_filter_change();
            CommandPanelAction::None
        }
        KeyCode::Down => {
            let len = panel.visible_indices().len();
            panel.selected = (panel.selected + 1).min(len.saturating_sub(1));
            panel.clamp_after_filter_change();
            CommandPanelAction::None
        }
        KeyCode::PageUp => {
            panel.selected = panel.selected.saturating_sub(8);
            panel.clamp_after_filter_change();
            CommandPanelAction::None
        }
        KeyCode::PageDown => {
            let len = panel.visible_indices().len();
            panel.selected = (panel.selected + 8).min(len.saturating_sub(1));
            panel.clamp_after_filter_change();
            CommandPanelAction::None
        }
        KeyCode::Home => {
            panel.selected = 0;
            panel.clamp_after_filter_change();
            CommandPanelAction::None
        }
        KeyCode::End => {
            panel.selected = panel.visible_indices().len().saturating_sub(1);
            panel.clamp_after_filter_change();
            CommandPanelAction::None
        }
        KeyCode::Backspace => {
            panel.search.pop();
            panel.clamp_after_filter_change();
            CommandPanelAction::None
        }
        KeyCode::Char(' ') | KeyCode::Enter => {
            let Some(idx) = panel.selected_actual_index() else {
                return CommandPanelAction::None;
            };
            let Some(item) = panel.items.get(idx) else {
                return CommandPanelAction::None;
            };
            let next_enabled = !item.auto_use_enabled;
            let item_path = PathBuf::from(&item.path);
            panel.feedback = None;
            CommandPanelAction::RunAction {
                title: "SKILLS".to_string(),
                action: DashboardAction::SetSkillAutoUse {
                    path: item_path,
                    enabled: next_enabled,
                },
                keep_panel: true,
            }
        }
        KeyCode::Char(c)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            panel.search.push(c);
            panel.clamp_after_filter_change();
            CommandPanelAction::None
        }
        _ => CommandPanelAction::None,
    }
}

fn handle_telegram_access_panel_key(
    picker: &mut TelegramAccessPicker,
    key: KeyEvent,
) -> CommandPanelAction {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => CommandPanelAction::Close,
        KeyCode::Up | KeyCode::Char('k') => {
            picker.selected = picker
                .selected
                .saturating_sub(1)
                .min(picker.requests.len().saturating_sub(1));
            picker.scroll =
                adjusted_picker_scroll(picker.scroll, picker.selected, picker.requests.len());
            CommandPanelAction::None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            picker.selected = (picker.selected + 1).min(picker.requests.len().saturating_sub(1));
            picker.scroll =
                adjusted_picker_scroll(picker.scroll, picker.selected, picker.requests.len());
            CommandPanelAction::None
        }
        KeyCode::PageUp => {
            picker.selected = picker.selected.saturating_sub(8);
            picker.scroll =
                adjusted_picker_scroll(picker.scroll, picker.selected, picker.requests.len());
            CommandPanelAction::None
        }
        KeyCode::PageDown => {
            picker.selected = (picker.selected + 8).min(picker.requests.len().saturating_sub(1));
            picker.scroll =
                adjusted_picker_scroll(picker.scroll, picker.selected, picker.requests.len());
            CommandPanelAction::None
        }
        KeyCode::Home => {
            picker.selected = 0;
            picker.scroll = 0;
            CommandPanelAction::None
        }
        KeyCode::End => {
            picker.selected = picker.requests.len().saturating_sub(1);
            picker.scroll =
                adjusted_picker_scroll(picker.scroll, picker.selected, picker.requests.len());
            CommandPanelAction::None
        }
        KeyCode::Enter => {
            let Some(request) = picker.requests.get(picker.selected) else {
                return CommandPanelAction::None;
            };
            let action = match picker.action {
                TelegramAccessAction::Approve => DashboardAction::ApproveTelegramAccess {
                    chat_id: request.chat_id,
                },
                TelegramAccessAction::Reject => DashboardAction::RejectTelegramAccess {
                    chat_id: request.chat_id,
                },
            };
            CommandPanelAction::RunAction {
                title: picker.action.title().to_string(),
                action,
                keep_panel: false,
            }
        }
        _ => CommandPanelAction::None,
    }
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

fn adjusted_list_scroll(
    current_scroll: usize,
    selected_index: usize,
    total: usize,
    visible_rows: usize,
) -> usize {
    if total <= visible_rows {
        return 0;
    }
    let max_scroll = total.saturating_sub(visible_rows);
    if selected_index < current_scroll {
        selected_index
    } else if selected_index >= current_scroll + visible_rows {
        (selected_index + 1)
            .saturating_sub(visible_rows)
            .min(max_scroll)
    } else {
        current_scroll.min(max_scroll)
    }
}

pub async fn run_tui_dashboard(
    rx: &mut tokio::sync::watch::Receiver<DashboardState>,
    command_runner: &dyn DashboardCommandRunner,
    history_loader: Option<Arc<dyn DashboardHistoryLoader>>,
) -> Result<(), std::io::Error> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen,)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    crossterm::execute!(terminal.backend_mut(), SetCursorStyle::SteadyBar,)?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::event::EnableBracketedPaste
    )?;
    let keyboard_enhancement_enabled = crossterm::execute!(
        terminal.backend_mut(),
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
    )
    .is_ok();
    let mut command_input = InputState::new();
    // Large/multi-line pastes stored as (placeholder, full_text) pairs.
    let mut pending_pastes: Vec<(String, String)> = Vec::new();
    let mut command_popup_selection: usize = 0;
    let mut command_popup_scroll: usize = 0;
    let mut command_panel: Option<CommandPanel> = None;
    let mut command_feedback: Option<CommandFeedback> = None;
    // Scroll and lazy-load state
    let mut scroll_offset: u16 = 0;
    let mut auto_scroll: bool = true;
    let mut max_scroll_storage: u16 = 0;
    let mut page_height: u16 = 20; // updated each frame from area height
    let mut last_cursor_pos: Option<(u16, u16)> = None;
    let mut extra_history_cells: Vec<ActivityCell> = Vec::new();
    let mut oldest_cursor: Option<i64> = None;
    let mut has_more_before: bool = false;
    let mut loading_history: bool = false;
    let mut load_cooldown: u8 = 0;
    let mut history_load_rx: Option<
        tokio::sync::oneshot::Receiver<Result<DashboardActivityHistoryPage, String>>,
    > = None;
    let mut cached_activity_lines = CachedActivityLines::new();
    let mut expanded_thinking: HashSet<usize> = HashSet::new();
    let mut visible_activity_cleared = false;

    // Async event loop: TuiEvent from crossterm stream, watch channel, frame requester.
    let mut needs_render = true;
    let mut event_stream = crossterm::event::EventStream::new();
    let (draw_tx, mut draw_rx) = tokio::sync::broadcast::channel::<()>(16);
    // FrameRequester spawns FrameScheduler; keep alive so the task isn't cancelled.
    let _frame_requester = frame_requester::FrameRequester::new(draw_tx.clone());

    // Periodic tick for animations: breathing light, working indicator (~60fps).
    let mut animation_tick = tokio::time::interval(Duration::from_millis(16));

    loop {
        tokio::select! {
                    event = event_stream.next() => {
                        let event = match event {
                            Some(Ok(e)) => e,
                            _ => continue,
                        };
                        let Some(tui_event) = tui_event::TuiEvent::from_crossterm(event) else {
                            continue;
                        };
                        match tui_event {
                            tui_event::TuiEvent::Key(key) => {
                                needs_render = true;
                                if key.kind != KeyEventKind::Press {
                                    continue;
                                }
                                let pending_requests = rx.borrow_and_update().pending_access_requests.clone();
                    if command_panel.is_some() {
                        let action = command_panel
                            .as_mut()
                            .map(|panel| handle_command_panel_key(panel, key))
                            .unwrap_or(CommandPanelAction::None);
                        match action {
                            CommandPanelAction::None => {}
                            CommandPanelAction::Close => {
                                command_panel = None;
                            }
                            CommandPanelAction::Replace(panel) => {
                                command_panel = Some(panel);
                                command_feedback = None;
                            }
                            CommandPanelAction::OpenSkillsList => {
                                let state = rx.borrow();
                                command_panel = Some(CommandPanel::SkillsList(
                                    SkillsListPanel::from_state(&state),
                                ));
                            }
                            CommandPanelAction::OpenSkillsToggle => {
                                let state = rx.borrow();
                                command_panel = Some(CommandPanel::SkillsToggle(
                                    SkillsTogglePanel::from_state(&state),
                                ));
                            }
                            CommandPanelAction::OpenTelegramAccess(action) => {
                                command_panel = Some(telegram_access_command_panel(
                                    action,
                                    &pending_requests,
                                ));
                            }
                            CommandPanelAction::RunAction {
                                title,
                                action,
                                keep_panel,
                            } => {
                                if !keep_panel {
                                    command_panel = None;
                                }
                                let state = rx.borrow().clone();
                                let result = command_runner.run_action(action, &state).await;
                                let feedback = command_feedback_from_action_result(title, result);
                                if keep_panel {
                                    if let Some(panel) = command_panel.as_mut() {
                                        if matches!(feedback.level, CommandFeedbackLevel::Error) {
                                            panel.set_error_feedback(feedback);
                                        } else {
                                            panel.clear_feedback();
                                        }
                                    }
                                } else {
                                    command_feedback = Some(feedback);
                                }
                            }
                        }
                        continue;
                    }
                    // Activity feed scroll keys (normal mode)
                    // Only active when command input is empty – popup nav works otherwise
                    if command_input.is_empty() {
                        {
                            let mut scrolled = true;
                            // Ctrl+T toggles all thinking cell expansion
                            if key.code == KeyCode::Char('t') && key.modifiers.contains(KeyModifiers::CONTROL) {
                                let state = rx.borrow();
                                let mut any_thinking = false;
                                let offset = extra_history_cells.len();
                                for (i, cell) in state.activity_cells.iter().enumerate() {
                                    if matches!(cell, ActivityCell::Thinking(_)) {
                                        let idx = offset + i;
                                        if expanded_thinking.contains(&idx) {
                                            expanded_thinking.remove(&idx);
                                        } else {
                                            expanded_thinking.insert(idx);
                                        }
                                        any_thinking = true;
                                    }
                                }
                                if any_thinking {
                                    cached_activity_lines = CachedActivityLines::new();
                                }
                                continue;
                            }
                            match key.code {
                                KeyCode::Up => {
                                    if auto_scroll {
                                        auto_scroll = false;
                                        scroll_offset = max_scroll_storage.saturating_sub(1);
                                    } else {
                                        scroll_offset = scroll_offset.saturating_sub(1);
                                    }
                                }
                                KeyCode::Down => {
                                    scroll_offset = scroll_offset.saturating_add(1);
                                    if scroll_offset >= max_scroll_storage {
                                        auto_scroll = true;
                                    }
                                }
                                KeyCode::PageUp => {
                                    if auto_scroll {
                                        auto_scroll = false;
                                        scroll_offset = max_scroll_storage.saturating_sub(page_height);
                                    } else {
                                        scroll_offset = scroll_offset.saturating_sub(page_height);
                                    }
                                }
                                KeyCode::PageDown => {
                                    scroll_offset = scroll_offset.saturating_add(page_height);
                                    if scroll_offset >= max_scroll_storage {
                                        auto_scroll = true;
                                    }
                                }
                                KeyCode::Home => {
                                    auto_scroll = false;
                                    scroll_offset = 0;
                                }
                                KeyCode::End => {
                                    auto_scroll = true;
                                    scroll_offset = 0;
                                }
                                _ => {
                                    scrolled = false;
                                }
                            }
                            if scrolled {
                                // Reset End→MAX; keep cursor at bottom for new activity
                                if scroll_offset >= max_scroll_storage {
                                    auto_scroll = true;
                                }
                                continue;
                            }
                        }
                    }
                    match key.code {
                        KeyCode::Char(c) => {
                            command_input.insert_char(c);
                            command_feedback = None;
                            command_popup_selection = 0;
                            command_popup_scroll = 0;
                        }
                        KeyCode::Tab => {
                            let state = rx.borrow();
                            let command_context = DashboardCommandContext {
                                requests: &pending_requests,
                                state: &state,
                            };
                            if let Some(completion) = selected_command_completion(
                                command_input.as_str(),
                                command_popup_selection,
                                &command_context,
                            ) {
                                command_input.set_text(completion);
                                command_feedback = None;
                                command_popup_selection = 0;
                                command_popup_scroll = 0;
                            }
                        }
                        KeyCode::Backspace => {
                            command_input.delete_before_cursor();
                            command_feedback = None;
                            command_popup_selection = 0;
                            command_popup_scroll = 0;
                        }
                        KeyCode::Up => {
                            let state = rx.borrow();
                            let command_context = DashboardCommandContext {
                                requests: &pending_requests,
                                state: &state,
                            };
                            let matches = matching_commands(command_input.as_str(), &command_context);
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
                            };
                            let matches = matching_commands(command_input.as_str(), &command_context);
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
                            command_feedback = None;
                            command_popup_selection = 0;
                            command_popup_scroll = 0;
                        }
                        KeyCode::Enter => {
                            if should_insert_newline_on_enter(key.modifiers) {
                                command_input.insert_char('\n');
                                command_popup_selection = 0;
                                command_popup_scroll = 0;
                                continue;
                            }
                            // Expand pending paste placeholders before submission.
                            if !pending_pastes.is_empty() {
                                command_input.set_text(expand_paste_placeholders(
                                    command_input.as_str(),
                                    &pending_pastes,
                                ));
                                pending_pastes.clear();
                            }
                            let state = rx.borrow().clone();
                            let command_context = DashboardCommandContext {
                                requests: &pending_requests,
                                state: &state,
                            };
                            let input = command_input.as_str().trim().to_string();
                            if !input.is_empty() {
                                if matches!(dashboard_command_body(&input), Some("quit" | "q" | "exit")) {
                                    break;
                                }
                                if let Some(panel) = command_panel_for_input(&input, &command_context) {
                                    command_panel = Some(panel);
                                    command_feedback = None;
                                    command_input.clear();
                                    command_popup_selection = 0;
                                    command_popup_scroll = 0;
                                    continue;
                                }
                                match dashboard_action_for_input(&input, &command_context) {
                                    Ok(Some(invocation)) => {
                                        let result = command_runner
                                            .run_action(invocation.action, &state)
                                            .await;
                                        if is_clear_command_input(&input) && result.success {
                                            extra_history_cells.clear();
                                            oldest_cursor = None;
                                            has_more_before = false;
                                            loading_history = false;
                                            history_load_rx = None;
                                            cached_activity_lines = CachedActivityLines::new();
                                            expanded_thinking.clear();
                                            auto_scroll = true;
                                            scroll_offset = 0;
                                            visible_activity_cleared = true;
                                        }
                                        command_feedback =
                                            if invocation.quiet_success && result.success {
                                                None
                                            } else {
                                                Some(command_feedback_from_action_result(
                                                    invocation.title,
                                                    result,
                                                ))
                                            };
                                        command_input.clear();
                                        command_popup_selection = 0;
                                        command_popup_scroll = 0;
                                        continue;
                                    }
                                    Ok(None) => {}
                                    Err(feedback) => {
                                        command_panel = None;
                                        command_feedback = Some(feedback);
                                        command_popup_selection = 0;
                                        command_popup_scroll = 0;
                                        continue;
                                    }
                                }
                            }
                            if let Some(completion) = selected_command_completion(
                                command_input.as_str(),
                                command_popup_selection,
                                &command_context,
                            ) && completion != command_input.as_str()
                            {
                                command_input.set_text(completion);
                                command_feedback = None;
                                command_popup_selection = 0;
                                command_popup_scroll = 0;
                                continue;
                            }
                            if !input.is_empty() {
                                if is_dashboard_command_input(&input) {
                                    command_panel = None;
                                    command_feedback = Some(
                                        command_blocks_submission(&input, &command_context)
                                            .unwrap_or_else(|| {
                                                unsupported_dashboard_command_feedback(&input)
                                            }),
                                    );
                                    command_popup_selection = 0;
                                    command_popup_scroll = 0;
                                    continue;
                                }
                                let _ = command_runner.run_command(&input, &state).await;
                                command_panel = None;
                                command_feedback = None;
                            }
                            command_input.clear();
                            command_popup_selection = 0;
                            command_popup_scroll = 0;
                        }
                        KeyCode::Left => {
                            command_input.move_left();
                            command_popup_selection = 0;
                            command_popup_scroll = 0;
                        }
                        KeyCode::Right => {
                            command_input.move_right();
                            command_popup_selection = 0;
                            command_popup_scroll = 0;
                        }
                        KeyCode::Home => {
                            command_input.move_home();
                            command_popup_selection = 0;
                            command_popup_scroll = 0;
                        }
                        KeyCode::End => {
                            command_input.move_end();
                            command_popup_selection = 0;
                            command_popup_scroll = 0;
                        }
                        _ => {}
                    }
                    }
                    tui_event::TuiEvent::Resize => {
                        needs_render = true;
                    }
                    tui_event::TuiEvent::Paste(text) => {
                        handle_paste_placeholder(
                            &text,
                            &mut command_input.text,
                            &mut pending_pastes,
                        );
                        command_input.move_end();
                        command_feedback = None;
                        needs_render = true;
                    }
                    tui_event::TuiEvent::Draw => {
                        needs_render = true;
                    }
                }
            }
            result = rx.changed() => {
                if result.is_ok() {
                    needs_render = true;
                }
            }
            result = draw_rx.recv() => {
                if result.is_ok() {
                    needs_render = true;
                }
            }
            _ = animation_tick.tick() => {
                needs_render = true;
            }
        }

        // If nothing changed and no input arrived, keep sleeping.
        if !needs_render {
            continue;
        }
        needs_render = false;

        let state = rx.borrow_and_update();
        let pending_requests = state.pending_access_requests.clone();
        if visible_activity_cleared
            && state.activity_history.items.is_empty()
            && state.activity_cells.is_empty()
            && state.live_activity_cells.is_empty()
        {
            visible_activity_cleared = false;
        }
        if let Some(panel) = command_panel.as_mut() {
            panel.sync_state(&state);
        }
        let panel_open = command_panel.is_some();
        let live_command_feedback = if !panel_open {
            let command_context = DashboardCommandContext {
                requests: &pending_requests,
                state: &state,
            };
            command_live_feedback(command_input.as_str(), &command_context)
        } else {
            None
        };
        let active_command_feedback = if !panel_open {
            live_command_feedback.as_ref().or(command_feedback.as_ref())
        } else {
            None
        };
        let feedback_rows = command_feedback_row_count(active_command_feedback);
        let popup_rows = if !panel_open {
            let command_context = DashboardCommandContext {
                requests: &pending_requests,
                state: &state,
            };
            command_popup_row_count(command_input.as_str(), &command_context)
        } else {
            0
        };
        let term_size = terminal.size().ok();
        let term_width = term_size.map(|size| size.width).unwrap_or(80);
        let term_height = term_size.map(|size| size.height).unwrap_or(24);
        let input_lines = command_input_display_height(
            wrapped_input_height(command_input.as_str(), term_width),
            term_height,
            popup_rows.saturating_add(feedback_rows),
        );
        let panel_rows = command_panel_row_count(
            command_panel.as_ref(),
            term_height,
            input_lines,
            popup_rows,
            feedback_rows,
        );

        // Decrement load cooldown each tick
        load_cooldown = load_cooldown.saturating_sub(1);

        // Lazy-load more history when scrolled near the top
        let effective_scroll = if auto_scroll {
            max_scroll_storage
        } else {
            scroll_offset
        };
        if history_loader.is_some()
            && !loading_history
            && load_cooldown == 0
            && has_more_before
            && effective_scroll <= 3
        {
            loading_history = true;
            if let Some(loader) = history_loader.clone() {
                let cursor = oldest_cursor;
                let (load_tx, load_rx) = tokio::sync::oneshot::channel();
                history_load_rx = Some(load_rx);
                tokio::spawn(async move {
                    let result = loader.load_history_before(cursor, 40).await;
                    let _ = load_tx.send(result);
                });
            }
        }

        // Check if a lazy load completed and merge results
        if let Some(mut rx) = history_load_rx.take() {
            match rx.try_recv() {
                Ok(Ok(page)) => {
                    let new_cells = activity_cells_from_history_items(&page.items);
                    let mut merged = new_cells;
                    merged.extend(extra_history_cells.clone());
                    extra_history_cells = merged;
                    // Reset to top to show newly loaded content
                    auto_scroll = false;
                    scroll_offset = 0;
                    oldest_cursor = page.oldest_cursor;
                    has_more_before = page.has_more_before;
                    loading_history = false;
                    // Prevent immediate cascade: require user to scroll again
                    load_cooldown = 10;
                }
                Ok(Err(err)) => {
                    tracing::warn!("TUI history lazy load failed: {err}");
                    loading_history = false;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    // Still loading
                    history_load_rx = Some(rx);
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    loading_history = false;
                }
            }
        }

        // On first iteration, sync cursor from state
        if oldest_cursor.is_none() && !state.activity_history.items.is_empty() {
            oldest_cursor = state.activity_history.oldest_cursor;
            has_more_before = state.activity_history.has_more_before;
        }

        // Build combined cells: extra history + current state
        let mut combined_cells: Vec<ActivityCell> = if visible_activity_cleared {
            Vec::new()
        } else {
            let mut cells = extra_history_cells.clone();
            cells.extend(state.activity_cells.clone());
            cells
        };
        let empty_live_activity_cells: Vec<LiveActivityCell> = Vec::new();
        let live_activity_cells = if visible_activity_cleared {
            empty_live_activity_cells.as_slice()
        } else {
            state.live_activity_cells.as_slice()
        };

        // Sync expanded state onto thinking cells
        for (i, cell) in combined_cells.iter_mut().enumerate() {
            if let ActivityCell::Thinking(t) = cell {
                t.expanded = expanded_thinking.contains(&i);
            }
        }

        // Resolve auto-scroll before rendering
        let display_scroll = if auto_scroll { u16::MAX } else { scroll_offset };

        terminal.draw(|f| {
            let root = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(6),
                    Constraint::Length(2 + input_lines + panel_rows + popup_rows + feedback_rows),
                ])
                .split(f.area());
            // max_scroll now returned directly from render (no double traversal)
            max_scroll_storage = render_activity_feed_cached(
                f.buffer_mut(),
                root[0],
                &combined_cells,
                live_activity_cells,
                display_scroll,
                &mut cached_activity_lines,
                expanded_thinking.len(),
            );
            // Update page height for PageUp/PageDown
            page_height = root[0].height.saturating_sub(1);

            render_command_bar(
                f,
                root[1],
                CommandBarRenderState {
                    input: command_input.as_str(),
                    cursor_pos: command_input.cursor_pos,
                    context: &DashboardCommandContext {
                        requests: &pending_requests,
                        state: &state,
                    },
                    runtime_status: state.runtime_status.as_deref(),
                    footer_context: &state.footer_context,
                    panel: command_panel.as_ref(),
                    panel_rows,
                    popup_selection: command_popup_selection,
                    popup_scroll: command_popup_scroll,
                    last_cursor_pos: &mut last_cursor_pos,
                    input_lines,
                    feedback: active_command_feedback,
                },
            );
        })?;
    }

    crossterm::terminal::disable_raw_mode()?;
    if keyboard_enhancement_enabled {
        let _ = crossterm::execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags,);
    }
    crossterm::execute!(
        terminal.backend_mut(),
        SetCursorStyle::DefaultUserShape,
        crossterm::terminal::LeaveAlternateScreen,
    )?;
    Ok(())
}

pub(crate) fn execute_control_command(
    command: &str,
    telegram_acl: &TelegramAclHandle,
    state: &DashboardState,
    control_tx: &tokio::sync::mpsc::UnboundedSender<DashboardControlCommand>,
) -> String {
    let command = command.trim().trim_start_matches('/').trim();
    if command.is_empty() {
        return "empty command".to_string();
    }
    let input = format!("/{command}");
    let requests = telegram_acl.pending_requests();
    let context = DashboardCommandContext {
        requests: &requests,
        state,
    };
    let Some(parts) = dashboard_command_parts(&input) else {
        return "empty command".to_string();
    };

    if matches!(parts.as_slice(), ["quit"] | ["q"] | ["exit"]) {
        return "quit command is only available in the local dashboard".to_string();
    }

    match dashboard_action_for_input(&input, &context) {
        Ok(Some(invocation)) => {
            let result = execute_dashboard_action(invocation.action, telegram_acl, control_tx);
            return result.message;
        }
        Ok(None) => {}
        Err(feedback) => return feedback.message,
    }

    if let Some(feedback) = command_extra_argument_feedback(&parts) {
        return feedback.message;
    }

    match parts.as_slice() {
        ["status"] => fallback_output(&state.status_output),
        ["debug"] => "available views: persona, system-prompt, context".to_string(),
        ["debug", "persona"] => debug_persona_text(),
        ["debug", "system-prompt"] | ["debug", "system_prompt"] => {
            fallback_output(&state.system_prompt_output)
        }
        ["debug", "context"] | ["debug", "preturn-context"] | ["debug", "preturn_context"] => {
            fallback_output(&state.preturn_context_output)
        }
        [verb] if app_status_command_accepts(verb) => render_available_app_statuses(state),
        [verb, target] if app_status_command_accepts(verb) => render_app_status_text(state, target),
        ["sleep"] => "available actions: status, run".to_string(),
        ["sleep", "status"] => fallback_output(&state.sleep_status_output),
        ["skills"] | ["skills", "list"] | ["skills", "show"] => render_skills_list(state),
        ["skills", "show", target] => render_skill_detail(state, target),
        ["telegram"] => "available actions: status, approve, reject".to_string(),
        ["telegram", "status"] => fallback_output(&state.inspect_telegram_output),
        ["telegram", "approve"] => render_pending_access_requests("approve", &requests),
        ["telegram", "reject"] => render_pending_access_requests("reject", &requests),
        [verb, ..] if dashboard_command_is_known(verb) => {
            format!("unsupported command shape: /{}", parts.join(" "))
        }
        [verb, ..] => format!("unknown command: {verb}"),
        [] => "empty command".to_string(),
    }
}

fn render_panel_text_lines(text: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut previous_blank = true;

    for raw_line in text.lines() {
        let line = raw_line.trim_end();
        if line.trim().is_empty() {
            lines.push(Line::from(""));
            previous_blank = true;
            continue;
        }

        if is_panel_section_header(line, previous_blank) {
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
            lines.push(render_panel_bullet_line(content));
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

fn is_panel_section_header(line: &str, previous_blank: bool) -> bool {
    previous_blank
        && !line.contains(':')
        && !line.starts_with('[')
        && !line.starts_with("• ")
        && line.chars().count() <= 32
}

fn render_panel_bullet_line(content: &str) -> Line<'static> {
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
                Style::default().fg(Color::Gray)
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

fn render_command_feedback(f: &mut Frame, area: Rect, feedback: &CommandFeedback) {
    let (marker, marker_style, text_style) = match feedback.level {
        CommandFeedbackLevel::Info => (
            "ok",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
            Style::default().fg(Color::Gray),
        ),
        CommandFeedbackLevel::Warning => (
            "!",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            Style::default().fg(Color::Gray),
        ),
        CommandFeedbackLevel::Error => (
            "x",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            Style::default().fg(Color::Gray),
        ),
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(marker, marker_style),
        Span::raw("  "),
        Span::styled(feedback.title.clone(), Style::default().fg(Color::White)),
        Span::raw("  "),
        Span::styled(feedback.message.clone(), text_style),
    ])];
    if let Some(detail) = feedback
        .detail
        .as_ref()
        .filter(|detail| !detail.trim().is_empty())
    {
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(detail.clone(), Style::default().fg(Color::DarkGray)),
        ]));
    }
    f.render_widget(
        Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
        area,
    );
}

struct CommandBarRenderState<'a> {
    input: &'a str,
    cursor_pos: usize,
    context: &'a DashboardCommandContext<'a>,
    feedback: Option<&'a CommandFeedback>,
    runtime_status: Option<&'a str>,
    footer_context: &'a str,
    panel: Option<&'a CommandPanel>,
    panel_rows: u16,
    popup_selection: usize,
    popup_scroll: usize,
    last_cursor_pos: &'a mut Option<(u16, u16)>,
    input_lines: u16,
}

fn should_insert_newline_on_enter(modifiers: KeyModifiers) -> bool {
    modifiers.contains(KeyModifiers::SHIFT) || modifiers.contains(KeyModifiers::ALT)
}
fn command_input_display_height(input_height: u16, terminal_height: u16, popup_rows: u16) -> u16 {
    let reserved_rows = 2u16.saturating_add(popup_rows);
    let max_rows = terminal_height
        .saturating_sub(8)
        .saturating_sub(reserved_rows)
        .max(1);
    input_height.max(1).min(max_rows)
}

fn command_panel_row_count(
    panel: Option<&CommandPanel>,
    terminal_height: u16,
    input_lines: u16,
    popup_rows: u16,
    feedback_rows: u16,
) -> u16 {
    let Some(panel) = panel else {
        return 0;
    };
    let base_rows = 2u16
        .saturating_add(input_lines)
        .saturating_add(popup_rows)
        .saturating_add(feedback_rows);
    let available = terminal_height.saturating_sub(base_rows).saturating_sub(6);
    if available < 5 {
        return 0;
    }
    panel.desired_height().min(available)
}

fn wrapped_input_height(text: &str, term_width: u16) -> u16 {
    let available = term_width.saturating_sub(2).max(1) as usize;
    if text.is_empty() {
        return 1;
    }
    let mut total: u16 = 0;
    for line in text.split('\n') {
        if line.is_empty() {
            total += 1;
            continue;
        }
        let display_width: usize = line.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum();
        let lines = display_width.div_ceil(available).max(1);
        total += lines as u16;
    }
    total.max(1)
}

/// Threshold above which pasted text gets a placeholder block instead of being inserted inline.
const LARGE_PASTE_CHAR_THRESHOLD: usize = 500;

/// Decide whether a paste should be collapsed into a placeholder block.
///
/// Rules (matching codex behaviour):
/// - Pastes exceeding `LARGE_PASTE_CHAR_THRESHOLD` chars → placeholder
/// - Pastes containing newlines and > 10 chars → placeholder
/// - Otherwise → insert inline as normal text
fn handle_paste_placeholder(text: &str, input: &mut String, pending: &mut Vec<(String, String)>) {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let char_count = normalized.chars().count();
    let is_multi_line = normalized.contains('\n');

    if char_count > LARGE_PASTE_CHAR_THRESHOLD || (char_count > 10 && is_multi_line) {
        let base = format!("[Pasted Content {char_count} chars]");
        let prefix = format!("{base} #");
        let mut max_suffix: usize = 0;
        for (ph, _) in pending.iter() {
            if ph == &base {
                max_suffix = max_suffix.max(1);
            } else if let Some(suffix) = ph.strip_prefix(&prefix)
                && let Ok(n) = suffix.parse::<usize>()
            {
                max_suffix = max_suffix.max(n);
            }
        }
        let placeholder = if max_suffix == 0 {
            base
        } else {
            format!("{base} #{max}", max = max_suffix + 1)
        };
        input.push_str(&placeholder);
        pending.push((placeholder, normalized));
    } else {
        input.push_str(&normalized);
    }
}

/// Replace all `[Pasted Content N chars]` placeholders in `text` with their stored full text.
fn expand_paste_placeholders(text: &str, pending: &[(String, String)]) -> String {
    let mut result = text.to_string();
    for (placeholder, full_text) in pending {
        result = result.replace(placeholder, full_text);
    }
    result
}

/// Compute (x, y) display position for a byte cursor position within the input text.
/// Accounts for multi-line input and terminal wrapping at `available_width`.
/// `prompt_width` is the display width of the leading prompt (e.g. "› " = 2).
fn cursor_display_row(text: &str, byte_pos: usize, available_width: usize) -> u16 {
    let byte_pos = byte_pos.min(text.len());
    let before = &text[..byte_pos];
    let mut total_rows: u16 = 0;
    let mut lines = before.split('\n').peekable();
    while let Some(line) = lines.next() {
        let dw: usize = line.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum();
        if lines.peek().is_some() {
            if dw == 0 {
                total_rows += 1;
            } else {
                total_rows += dw.div_ceil(available_width) as u16;
            }
        } else {
            total_rows += (dw / available_width) as u16;
        }
    }
    total_rows
}

fn cursor_display_xy(
    text: &str,
    byte_pos: usize,
    available_width: usize,
    prompt_width: u16,
    area: Rect,
    scroll: u16,
) -> (u16, u16) {
    let byte_pos = byte_pos.min(text.len());
    let before = &text[..byte_pos];
    let mut total_rows: u16 = 0;
    let mut col: u16 = 0;
    let mut lines = before.split('\n').peekable();
    while let Some(line) = lines.next() {
        let dw: usize = line.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum();
        if lines.peek().is_some() {
            // completed logical line
            if dw == 0 {
                total_rows += 1;
            } else {
                total_rows += dw.div_ceil(available_width) as u16;
            }
        } else {
            // current (last) line
            total_rows += (dw / available_width) as u16;
            col = (dw % available_width) as u16;
        }
    }
    let x = area.x + prompt_width + col;
    let y = area.y + total_rows.saturating_sub(scroll);
    (x, y)
}

fn render_command_panel(f: &mut Frame, area: Rect, panel: &CommandPanel) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let inner = inset_rect(area, 1, 2);
    match panel {
        CommandPanel::Detail(detail) => render_detail_panel(f, inner, detail),
        CommandPanel::Selection(selection) => render_selection_panel(f, inner, selection),
        CommandPanel::SkillsList(skills) => render_skills_list_panel(f, inner, skills),
        CommandPanel::SkillsToggle(skills) => render_skills_toggle_panel(f, inner, skills),
        CommandPanel::TelegramAccess(picker) => render_telegram_access_panel(f, inner, picker),
    }
}

fn inset_rect(area: Rect, vertical: u16, horizontal: u16) -> Rect {
    Rect {
        x: area.x.saturating_add(horizontal),
        y: area.y.saturating_add(vertical),
        width: area.width.saturating_sub(horizontal.saturating_mul(2)),
        height: area.height.saturating_sub(vertical.saturating_mul(2)),
    }
}

fn render_panel_title(f: &mut Frame, area: Rect, title: &str, subtitle: Option<&str>) -> Rect {
    if area.height == 0 {
        return area;
    }
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            title.to_string(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )])),
        Rect { height: 1, ..area },
    );
    let mut rest = Rect {
        y: area.y.saturating_add(1),
        height: area.height.saturating_sub(1),
        ..area
    };
    if let Some(subtitle) = subtitle
        && rest.height > 0
    {
        f.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                subtitle.to_string(),
                Style::default().fg(Color::DarkGray),
            )])),
            Rect { height: 1, ..rest },
        );
        rest.y = rest.y.saturating_add(1);
        rest.height = rest.height.saturating_sub(1);
    }
    rest
}

fn render_detail_panel(f: &mut Frame, area: Rect, panel: &CommandDetailPanel) {
    let body = render_panel_title(f, area, &panel.title, None);
    if body.height == 0 {
        return;
    }
    let lines = render_panel_text_lines(&panel.text);
    let max_scroll = lines.len().saturating_sub(body.height as usize) as u16;
    let scroll = panel.scroll.min(max_scroll);
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .scroll((scroll, 0))
            .wrap(Wrap { trim: false }),
        body,
    );
}

fn render_selection_panel(f: &mut Frame, area: Rect, panel: &CommandSelectionPanel) {
    let list_area = render_panel_title(f, area, &panel.title, panel.subtitle.as_deref());
    if list_area.height == 0 {
        return;
    }
    let lines = panel
        .items
        .iter()
        .skip(panel.scroll)
        .take(list_area.height as usize)
        .enumerate()
        .map(|(visible_idx, item)| {
            let idx = panel.scroll + visible_idx;
            let selected = idx == panel.selected;
            let marker = if selected { "›" } else { " " };
            let name_style = if item.disabled {
                Style::default().fg(Color::DarkGray)
            } else if selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            let description_style = if item.disabled {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::Gray)
            };
            Line::from(vec![
                Span::styled(marker, Style::default().fg(Color::Cyan)),
                Span::raw(" "),
                Span::styled(item.name.clone(), name_style),
                Span::raw("  "),
                Span::styled(item.description.clone(), description_style),
            ])
        })
        .collect::<Vec<_>>();
    f.render_widget(
        Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
        list_area,
    );
}

fn render_skills_list_panel(f: &mut Frame, area: Rect, panel: &SkillsListPanel) {
    let subtitle = if panel.items.is_empty() {
        "No skills loaded.".to_string()
    } else {
        format!("{} loaded. Choose a skill to inspect.", panel.items.len())
    };
    let mut rest = render_panel_title(f, area, "Skills", Some(&subtitle));
    if rest.height == 0 {
        return;
    }
    let search_line = if panel.search.is_empty() {
        Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "Type to search skills",
                Style::default().fg(Color::DarkGray),
            ),
        ])
    } else {
        Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::DarkGray)),
            Span::styled(panel.search.clone(), Style::default().fg(Color::White)),
        ])
    };
    f.render_widget(Paragraph::new(search_line), Rect { height: 1, ..rest });
    rest.y = rest.y.saturating_add(1);
    rest.height = rest.height.saturating_sub(1);

    if !panel.errors.is_empty() && rest.height > 0 {
        let error_lines = panel
            .errors
            .iter()
            .take(rest.height.min(2) as usize)
            .map(|error| {
                Line::from(vec![
                    Span::styled("!", Style::default().fg(Color::Yellow)),
                    Span::raw(" "),
                    Span::styled(
                        truncate_command_text(&error.path, 42),
                        Style::default().fg(Color::Gray),
                    ),
                    Span::raw("  "),
                    Span::styled(
                        truncate_command_text(&error.message, 120),
                        Style::default().fg(Color::DarkGray),
                    ),
                ])
            })
            .collect::<Vec<_>>();
        let rows = error_lines.len() as u16;
        f.render_widget(
            Paragraph::new(Text::from(error_lines)).wrap(Wrap { trim: false }),
            Rect {
                height: rows,
                ..rest
            },
        );
        rest.y = rest.y.saturating_add(rows);
        rest.height = rest.height.saturating_sub(rows);
    }

    if rest.height == 0 {
        return;
    }
    let visible_indices = panel.visible_indices();
    let lines = visible_indices
        .iter()
        .skip(panel.scroll)
        .take(rest.height as usize)
        .enumerate()
        .filter_map(|(visible_idx, actual_idx)| {
            let item = panel.items.get(*actual_idx)?;
            let idx = panel.scroll + visible_idx;
            let selected = idx == panel.selected;
            let marker = if selected { "›" } else { " " };
            let name_style = if selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            Some(Line::from(vec![
                Span::styled(marker, Style::default().fg(Color::Cyan)),
                Span::raw(" "),
                Span::styled(item.name.clone(), name_style),
                Span::raw("  "),
                Span::styled(item.status.clone(), Style::default().fg(Color::Gray)),
                Span::raw("  "),
                Span::styled(
                    truncate_command_text(&item.description, 100),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect::<Vec<_>>();
    let lines = if lines.is_empty() {
        vec![Line::from(vec![Span::styled(
            "no matches",
            Style::default().fg(Color::DarkGray),
        )])]
    } else {
        lines
    };
    f.render_widget(
        Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
        rest,
    );
}

fn render_skills_toggle_panel(f: &mut Frame, area: Rect, panel: &SkillsTogglePanel) {
    let subtitle = if panel.items.is_empty() {
        "No skills loaded.".to_string()
    } else {
        "Toggle automatic use for loaded skills.".to_string()
    };
    let mut rest = render_panel_title(f, area, "Skills", Some(&subtitle));
    if rest.height == 0 {
        return;
    }
    let search_line = if panel.search.is_empty() {
        Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "Type to search skills",
                Style::default().fg(Color::DarkGray),
            ),
        ])
    } else {
        Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::DarkGray)),
            Span::styled(panel.search.clone(), Style::default().fg(Color::White)),
        ])
    };
    f.render_widget(Paragraph::new(search_line), Rect { height: 1, ..rest });
    rest.y = rest.y.saturating_add(1);
    rest.height = rest.height.saturating_sub(1);

    if let Some(feedback) = panel.feedback.as_ref() {
        let rows = command_feedback_row_count(Some(feedback)).min(rest.height);
        if rows > 0 {
            render_command_feedback(
                f,
                Rect {
                    height: rows,
                    ..rest
                },
                feedback,
            );
            rest.y = rest.y.saturating_add(rows);
            rest.height = rest.height.saturating_sub(rows);
        }
    }

    if rest.height == 0 {
        return;
    }
    let visible_indices = panel.visible_indices();
    let lines = visible_indices
        .iter()
        .skip(panel.scroll)
        .take(rest.height as usize)
        .enumerate()
        .filter_map(|(visible_idx, actual_idx)| {
            let item = panel.items.get(*actual_idx)?;
            let idx = panel.scroll + visible_idx;
            let selected = idx == panel.selected;
            let marker = if selected { "›" } else { " " };
            let checkbox = if item.auto_use_enabled { "[x]" } else { "[ ]" };
            let name_style = if selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            Some(Line::from(vec![
                Span::styled(marker, Style::default().fg(Color::Cyan)),
                Span::raw(" "),
                Span::styled(checkbox, Style::default().fg(Color::Gray)),
                Span::raw(" "),
                Span::styled(item.name.clone(), name_style),
                Span::raw("  "),
                Span::styled(
                    format!("{} - {}", item.scope, item.status_description()),
                    Style::default().fg(Color::Gray),
                ),
            ]))
        })
        .collect::<Vec<_>>();
    let lines = if lines.is_empty() {
        vec![Line::from(vec![Span::styled(
            "no matches",
            Style::default().fg(Color::DarkGray),
        )])]
    } else {
        lines
    };
    f.render_widget(
        Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
        rest,
    );
}

fn render_telegram_access_panel(f: &mut Frame, area: Rect, picker: &TelegramAccessPicker) {
    let rest = render_panel_title(
        f,
        area,
        picker.action.title(),
        Some(&format!(
            "Select a pending request to {}.",
            picker.action.verb()
        )),
    );
    if rest.height == 0 {
        return;
    }
    let lines = picker
        .requests
        .iter()
        .skip(picker.scroll)
        .take(rest.height as usize)
        .enumerate()
        .map(|(visible_idx, request)| {
            let idx = picker.scroll + visible_idx;
            let selected = idx == picker.selected;
            let marker = if selected { "›" } else { " " };
            let style = if selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            Line::from(vec![
                Span::styled(marker, Style::default().fg(Color::Cyan)),
                Span::raw(" "),
                Span::styled(
                    format!(
                        "{}  {}  {}  {}",
                        request.chat_id,
                        request.title,
                        request.sender,
                        truncate_command_text(&request.last_message_preview, 100)
                    ),
                    style,
                ),
            ])
        })
        .collect::<Vec<_>>();
    f.render_widget(
        Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
        rest,
    );
}

fn render_command_bar(f: &mut Frame, area: Rect, state: CommandBarRenderState<'_>) {
    let CommandBarRenderState {
        input,
        cursor_pos,
        input_lines,
        context,
        feedback,
        runtime_status,
        footer_context,
        panel,
        panel_rows,
        popup_selection,
        popup_scroll,
        last_cursor_pos,
    } = state;

    let completion = if panel.is_none() {
        selected_command_completion(input, 0, context)
    } else {
        None
    };
    let hint = command_hint(input, context);
    let popup_rows = if panel.is_some() {
        0
    } else {
        command_popup_row_count(input, context)
    };
    let feedback_rows = if panel.is_some() {
        0
    } else {
        command_feedback_row_count(feedback)
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints({
            let mut c = Vec::new();
            if panel_rows > 0 {
                c.push(Constraint::Length(panel_rows));
            }
            c.push(Constraint::Length(1));
            if feedback_rows > 0 {
                c.push(Constraint::Length(feedback_rows));
            }
            c.push(Constraint::Length(input_lines));
            if popup_rows > 0 {
                c.push(Constraint::Length(popup_rows));
            }
            c.push(Constraint::Length(1));
            c
        })
        .split(area);
    let mut row_index = 0usize;
    if let Some(panel) = panel
        && panel_rows > 0
    {
        render_command_panel(f, rows[row_index], panel);
        row_index += 1;
    }
    let status_line = match runtime_status {
        Some("Working") => render_working_status_line(),
        Some(status) if !status.trim().is_empty() => Line::from(vec![Span::styled(
            status.to_string(),
            Style::default().fg(Color::DarkGray),
        )]),
        _ => Line::from(""),
    };
    f.render_widget(Paragraph::new(status_line), rows[row_index]);
    row_index += 1;
    if let Some(feedback) = feedback
        && feedback_rows > 0
    {
        render_command_feedback(f, rows[row_index], feedback);
        row_index += 1;
    }
    let input_row_index = row_index;
    let available_width = area.width.saturating_sub(2).max(1) as usize;
    // Build input text with prompt prefix, render as wrapping Paragraph.
    let cursor_total_row = cursor_display_row(input, cursor_pos, available_width);
    let input_scroll =
        cursor_total_row.saturating_sub(rows[input_row_index].height.saturating_sub(1));
    let input_para = Paragraph::new(command_input_display_text(input, completion.as_deref()))
        .wrap(ratatui::widgets::Wrap { trim: false })
        .scroll((input_scroll, 0));
    f.render_widget(input_para, rows[input_row_index]);

    // Compute cursor position from tracked cursor_pos, accounting for wrapping
    let (cursor_x, cursor_y) = cursor_display_xy(
        input,
        cursor_pos,
        available_width,
        2, // prompt "› " width
        rows[input_row_index],
        input_scroll,
    );
    f.set_cursor_position(Position {
        x: cursor_x,
        y: cursor_y,
    });
    *last_cursor_pos = Some((cursor_x, cursor_y));
    let popup_row_index = input_row_index + 1;
    let footer_row = if popup_rows > 0 {
        render_command_popup(
            f,
            rows[popup_row_index],
            input,
            context,
            popup_selection,
            popup_scroll,
        );
        rows[popup_row_index + 1]
    } else {
        rows[popup_row_index]
    };
    let footer_line = if let Some(panel) = panel {
        Line::from(vec![
            Span::styled("panel", Style::default().fg(Color::DarkGray)),
            Span::raw("  "),
            Span::styled(panel.footer_hint(), Style::default().fg(Color::DarkGray)),
        ])
    } else if !footer_context.trim().is_empty() {
        Line::from(vec![Span::styled(
            footer_context.to_string(),
            Style::default().fg(Color::DarkGray),
        )])
    } else {
        Line::from(vec![
            Span::styled("hint", Style::default().fg(Color::DarkGray)),
            Span::raw("  "),
            Span::styled(hint, Style::default().fg(Color::DarkGray)),
        ])
    };
    let footer = Paragraph::new(footer_line);
    f.render_widget(footer, footer_row);
}

fn push_command_input_display_text(output: &mut String, input: &str) {
    for (line_index, line) in input.split('\n').enumerate() {
        if line_index > 0 {
            output.push('\n');
            output.push_str("  ");
        }
        output.push_str(line);
    }
}

fn command_input_display_text(input: &str, completion: Option<&str>) -> Text<'static> {
    if input.is_empty() {
        return Text::from(Line::from(vec![Span::styled(
            "› type a message, or /command",
            Style::default().fg(Color::DarkGray),
        )]));
    }

    let mut display = String::with_capacity(input.len() + 2 + input.matches('\n').count() * 2);
    display.push_str("› ");
    push_command_input_display_text(&mut display, input);
    let completion_suffix = completion
        .filter(|completion| *completion != input)
        .and_then(|completion| completion.strip_prefix(input))
        .unwrap_or_default();
    let raw_lines = display.split('\n').collect::<Vec<_>>();
    let last_index = raw_lines.len().saturating_sub(1);
    let lines = raw_lines
        .into_iter()
        .enumerate()
        .map(|(idx, line)| {
            let mut spans = vec![Span::styled(
                line.to_string(),
                Style::default().fg(Color::White),
            )];
            if idx == last_index && !completion_suffix.is_empty() {
                spans.push(Span::styled(
                    completion_suffix.to_string(),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            Line::from(spans)
        })
        .collect::<Vec<_>>();
    Text::from(lines)
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

fn fallback_output(output: &str) -> String {
    if output.trim().is_empty() {
        "no data".to_string()
    } else {
        output.to_string()
    }
}

fn is_clear_command_input(input: &str) -> bool {
    matches!(
        dashboard_command_parts(input).as_deref(),
        Some(["clear", ..])
    )
}

fn debug_subcommand_is_read_only(subcommand: &str) -> bool {
    matches!(
        subcommand,
        "persona"
            | "system-prompt"
            | "system_prompt"
            | "context"
            | "preturn-context"
            | "preturn_context"
    )
}

fn dashboard_command_parts(input: &str) -> Option<Vec<&str>> {
    let body = dashboard_command_body(input)?;
    let parts = body.split_whitespace().collect::<Vec<_>>();
    (!parts.is_empty()).then_some(parts)
}

fn command_live_feedback(
    input: &str,
    context: &DashboardCommandContext<'_>,
) -> Option<CommandFeedback> {
    let command_input = command_completion_body(input)?;
    let trimmed = command_input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let parts = trimmed.split_whitespace().collect::<Vec<_>>();
    let verb = parts.first().copied().unwrap_or_default();
    let command = dashboard_commands()
        .iter()
        .copied()
        .find(|command| command.accepts(verb));
    let Some(_command) = command else {
        if matching_commands(input, context).is_empty() {
            return Some(CommandFeedback {
                title: "UNKNOWN COMMAND".to_string(),
                message: format!("No dashboard command named '{verb}'."),
                detail: Some("Type / to browse available commands.".to_string()),
                level: CommandFeedbackLevel::Error,
            });
        }
        return None;
    };

    if parts.len() == 1 {
        return None;
    }

    if let Some(feedback) = command_extra_argument_feedback(&parts) {
        return Some(feedback);
    }

    if debug_command_accepts(verb) {
        let subcommand = parts[1];
        if !debug_subcommand_is_read_only(subcommand) {
            return Some(unknown_command_part_feedback(
                "DEBUG",
                format!("Unknown debug view '{subcommand}'."),
                "Use /debug to choose a view.",
            ));
        }
    } else if sleep_command_accepts(verb) {
        match parts.as_slice() {
            ["sleep", "run"] | ["sleep", "status"] => {}
            ["sleep", subcommand, ..] => {
                return Some(unknown_command_part_feedback(
                    "SLEEP",
                    format!("Unknown sleep action '{subcommand}'."),
                    "Use /sleep to choose an action.",
                ));
            }
            _ => {}
        }
    } else if skills_command_accepts(verb) {
        match parts.as_slice() {
            ["skills", "list"] | ["skills", "reload"] => {}
            ["skills", "show" | "enable" | "disable", target] => {
                if let Err(message) = resolve_skill_target(context.state, target) {
                    return Some(CommandFeedback {
                        title: "SKILLS".to_string(),
                        message,
                        detail: Some("Use /skills to browse loaded skills.".to_string()),
                        level: CommandFeedbackLevel::Error,
                    });
                }
            }
            ["skills", subcommand, ..] => {
                return Some(unknown_command_part_feedback(
                    "SKILLS",
                    format!("Unknown skills action '{subcommand}'."),
                    "Use /skills to choose an action.",
                ));
            }
            _ => {}
        }
    } else if telegram_command_accepts(verb) {
        match parts.as_slice() {
            ["telegram", "status"] => {}
            ["telegram", "approve" | "reject"] => {
                let subcommand = parts[1];
                if context.requests.is_empty() {
                    return Some(CommandFeedback {
                        title: "TELEGRAM".to_string(),
                        message: format!("No pending Telegram requests to {subcommand}."),
                        detail: Some("Use /telegram to inspect Telegram state.".to_string()),
                        level: CommandFeedbackLevel::Info,
                    });
                }
                return Some(CommandFeedback {
                    title: "TELEGRAM".to_string(),
                    message: format!("Press Enter to choose a request to {subcommand}."),
                    detail: Some(format_pending_request_choices(context.requests)),
                    level: CommandFeedbackLevel::Info,
                });
            }
            ["telegram", "approve" | "reject", chat_id] if chat_id.parse::<i64>().is_err() => {
                return Some(CommandFeedback {
                    title: "TELEGRAM".to_string(),
                    message: format!("Invalid chat_id '{chat_id}'."),
                    detail: None,
                    level: CommandFeedbackLevel::Error,
                });
            }
            ["telegram", "approve" | "reject", _] => {}
            ["telegram", subcommand, ..] => {
                return Some(unknown_command_part_feedback(
                    "TELEGRAM",
                    format!("Unknown Telegram action '{subcommand}'."),
                    "Use /telegram to choose an action.",
                ));
            }
            _ => {}
        }
    } else if app_status_command_accepts(verb) {
        let apps = context
            .state
            .app_status_outputs
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>();
        let target = parts[1].to_ascii_lowercase();
        let known = apps.iter().any(|name| *name == target);
        let possible = apps.iter().any(|name| name.starts_with(&target));
        if !known && !possible {
            return Some(CommandFeedback {
                title: "APP STATUS".to_string(),
                message: format!("Unknown app '{target}'."),
                detail: Some(if apps.is_empty() {
                    "No app state is currently available.".to_string()
                } else {
                    format!("available: {}", apps.join(", "))
                }),
                level: CommandFeedbackLevel::Error,
            });
        }
    }

    None
}

fn unknown_command_part_feedback(
    title: &str,
    message: impl Into<String>,
    detail: impl Into<String>,
) -> CommandFeedback {
    CommandFeedback {
        title: title.to_string(),
        message: message.into(),
        detail: Some(detail.into()),
        level: CommandFeedbackLevel::Error,
    }
}

fn command_extra_argument_feedback(parts: &[&str]) -> Option<CommandFeedback> {
    let verb = parts.first().copied().unwrap_or_default();
    let extra_for_root = |usage: &str| CommandFeedback {
        title: verb.to_uppercase(),
        message: format!("{verb} does not take extra arguments."),
        detail: Some(format!("usage: /{usage}")),
        level: CommandFeedbackLevel::Error,
    };
    if (quit_command_accepts(verb)
        || clear_command_accepts(verb)
        || status_command_accepts(verb)
        || restart_command_accepts(verb))
        && parts.len() > 1
    {
        let usage = if quit_command_accepts(verb) {
            "quit"
        } else if clear_command_accepts(verb) {
            "clear"
        } else if status_command_accepts(verb) {
            "status"
        } else {
            "restart"
        };
        return Some(extra_for_root(usage));
    }

    match parts {
        ["debug", subcommand, ..]
            if parts.len() > 2 && debug_subcommand_is_read_only(subcommand) =>
        {
            Some(CommandFeedback {
                title: "DEBUG".to_string(),
                message: format!("debug {subcommand} does not take extra arguments."),
                detail: Some(format!("usage: /debug {subcommand}")),
                level: CommandFeedbackLevel::Error,
            })
        }
        ["sleep", "run" | "status", ..] if parts.len() > 2 => Some(CommandFeedback {
            title: "SLEEP".to_string(),
            message: format!("sleep {} does not take extra arguments.", parts[1]),
            detail: Some(format!("usage: /sleep {}", parts[1])),
            level: CommandFeedbackLevel::Error,
        }),
        ["skills", "list" | "reload", ..] if parts.len() > 2 => Some(CommandFeedback {
            title: "SKILLS".to_string(),
            message: format!("skills {} does not take extra arguments.", parts[1]),
            detail: Some(format!("usage: /skills {}", parts[1])),
            level: CommandFeedbackLevel::Error,
        }),
        ["skills", "show" | "enable" | "disable"] => Some(CommandFeedback {
            title: "SKILLS".to_string(),
            message: format!("skills {} needs a skill name.", parts[1]),
            detail: Some(format!("usage: /skills {} <skill>", parts[1])),
            level: CommandFeedbackLevel::Warning,
        }),
        ["skills", "show" | "enable" | "disable", ..] if parts.len() > 3 => Some(CommandFeedback {
            title: "SKILLS".to_string(),
            message: format!("skills {} accepts exactly one skill name.", parts[1]),
            detail: Some(format!("usage: /skills {} <skill>", parts[1])),
            level: CommandFeedbackLevel::Error,
        }),
        ["telegram", "status", ..] if parts.len() > 2 => Some(CommandFeedback {
            title: "TELEGRAM".to_string(),
            message: "telegram status does not take extra arguments.".to_string(),
            detail: Some("usage: /telegram status".to_string()),
            level: CommandFeedbackLevel::Error,
        }),
        ["telegram", "approve" | "reject", ..] if parts.len() > 3 => Some(CommandFeedback {
            title: "TELEGRAM".to_string(),
            message: format!("telegram {} accepts at most one chat_id.", parts[1]),
            detail: Some(format!("usage: /telegram {} [chat_id]", parts[1])),
            level: CommandFeedbackLevel::Error,
        }),
        [verb, ..] if app_status_command_accepts(verb) && parts.len() > 2 => {
            Some(CommandFeedback {
                title: "APP STATUS".to_string(),
                message: "app-status accepts exactly one app name.".to_string(),
                detail: Some("usage: /app-status <app>".to_string()),
                level: CommandFeedbackLevel::Error,
            })
        }
        _ => None,
    }
}

fn command_blocks_submission(
    input: &str,
    context: &DashboardCommandContext<'_>,
) -> Option<CommandFeedback> {
    let feedback = command_live_feedback(input, context)?;
    match feedback.level {
        CommandFeedbackLevel::Warning | CommandFeedbackLevel::Error => Some(feedback),
        CommandFeedbackLevel::Info => {
            let parts = dashboard_command_parts(input)?;
            if matches!(
                parts.as_slice(),
                ["telegram", "approve"] | ["telegram", "reject"]
            ) {
                Some(feedback)
            } else {
                None
            }
        }
    }
}

fn unsupported_dashboard_command_feedback(input: &str) -> CommandFeedback {
    let command = dashboard_command_body(input)
        .and_then(|body| body.split_whitespace().next())
        .unwrap_or_default();
    CommandFeedback {
        title: "COMMAND".to_string(),
        message: if command.is_empty() {
            "Incomplete dashboard command.".to_string()
        } else {
            format!("Dashboard command '/{command}' is incomplete or unsupported here.")
        },
        detail: Some("Use / to choose a top-level command, then press Enter.".to_string()),
        level: CommandFeedbackLevel::Error,
    }
}

fn format_pending_request_choices(requests: &[PendingAccessRequest]) -> String {
    requests
        .iter()
        .take(4)
        .map(|request| format!("{} {}", request.chat_id, request.sender))
        .collect::<Vec<_>>()
        .join(" | ")
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

fn matching_commands(
    input: &str,
    _context: &DashboardCommandContext<'_>,
) -> Vec<CommandSuggestion> {
    let Some(command_input) = command_completion_body(input) else {
        return Vec::new();
    };
    let trimmed = command_input.trim();
    if trimmed.is_empty() {
        return dashboard_commands()
            .iter()
            .map(|command| CommandSuggestion {
                display: command.primary_verb.to_string(),
                completion: format!("/{}", command.primary_verb),
                description: command.description.to_string(),
            })
            .collect::<Vec<_>>();
    }
    let parts = trimmed.split_whitespace().collect::<Vec<_>>();
    if parts.len() > 1 || command_input.ends_with(' ') {
        return Vec::new();
    }
    dashboard_commands()
        .iter()
        .copied()
        .filter(|command| command.primary_verb.starts_with(parts[0]))
        .map(|command| CommandSuggestion {
            display: command.primary_verb.to_string(),
            completion: format!("/{}", command.primary_verb),
            description: command.description.to_string(),
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

fn command_feedback_row_count(feedback: Option<&CommandFeedback>) -> u16 {
    match feedback {
        Some(feedback)
            if feedback
                .detail
                .as_ref()
                .is_some_and(|detail| !detail.trim().is_empty()) =>
        {
            2
        }
        Some(_) => 1,
        None => 0,
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
            return "Enter send. Shift+Enter newline. Prefix / for commands. Esc clear."
                .to_string();
        }
        return "Enter send. Shift+Enter newline. Prefix / for commands.".to_string();
    }
    let matches = matching_commands(input, context);
    if command_completion_body(input)
        .map(str::trim)
        .unwrap_or_default()
        .is_empty()
    {
        return "Up/Down select. Tab accept. Enter run. Shift+Enter newline. Esc clear."
            .to_string();
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
    if let Some(parts) = dashboard_command_parts(input) {
        if dashboard_parts_open_panel(&parts) {
            return "Enter open panel. Shift+Enter newline. Esc clear.".to_string();
        }
        if dashboard_parts_run_action(&parts) {
            return "Enter run action. Shift+Enter newline. Esc clear.".to_string();
        }
        if dashboard_command_is_known(parts[0]) {
            return "Enter run command. Shift+Enter newline. Esc clear.".to_string();
        }
    }
    "unknown command".to_string()
}

fn dashboard_parts_open_panel(parts: &[&str]) -> bool {
    matches!(
        parts,
        ["status"]
            | ["debug"]
            | ["debug", "persona"]
            | ["debug", "system-prompt"]
            | ["debug", "system_prompt"]
            | ["debug", "context"]
            | ["debug", "preturn-context"]
            | ["debug", "preturn_context"]
            | ["sleep"]
            | ["sleep", "status"]
            | ["telegram"]
            | ["telegram", "status"]
            | ["telegram", "approve"]
            | ["telegram", "reject"]
            | ["skills"]
            | ["skills", "list"]
            | ["skills", "show"]
            | ["skills", "show", _]
    ) || matches!(parts, [verb] if app_status_command_accepts(verb))
        || matches!(parts, [verb, _] if app_status_command_accepts(verb))
}

fn dashboard_parts_run_action(parts: &[&str]) -> bool {
    matches!(
        parts,
        ["clear"]
            | ["restart"]
            | ["sleep", "run"]
            | ["skills", "reload"]
            | ["skills", "enable", _]
            | ["skills", "disable", _]
            | ["telegram", "approve", _]
            | ["telegram", "reject", _]
    )
}

fn dashboard_command_is_known(verb: &str) -> bool {
    dashboard_commands()
        .iter()
        .copied()
        .any(|command| command.accepts(verb))
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
    fn exact_entry_command_does_not_complete_to_subcommands() {
        let context = test_command_context();
        let matches = matching_commands("/sleep", &context);

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].completion, "/sleep");
        assert!(command_live_feedback("/sleep", &context).is_none());
    }

    #[test]
    fn completions_stay_at_top_level_entries() {
        let requests = vec![pending_request(42)];
        let state = DashboardState {
            app_status_outputs: vec![("browser".to_string(), "state".to_string())],
            skills: vec![OpenSkillDashboardSummary {
                name: "writer".to_string(),
                description: "Write release notes".to_string(),
                path: "/tmp/skills/writer/SKILL.md".to_string(),
                scope: "user".to_string(),
                allow_implicit_invocation: true,
                user_disabled: false,
                auto_use_enabled: true,
            }],
            ..DashboardState::default()
        };
        let context = DashboardCommandContext {
            requests: &requests,
            state: &state,
        };

        let app_match = matching_commands("/app", &context)
            .into_iter()
            .next()
            .expect("app completion");
        assert_eq!(app_match.completion, "/app-status");

        assert!(matching_commands("/app-status ", &context).is_empty());
        assert!(matching_commands("/telegram approve ", &context).is_empty());
        assert!(matching_commands("/skills disable ", &context).is_empty());
    }

    #[test]
    fn skills_command_lists_without_requiring_subcommand() {
        let state = DashboardState {
            skills: vec![OpenSkillDashboardSummary {
                name: "writer".to_string(),
                description: "Write release notes".to_string(),
                path: "/tmp/skills/writer/SKILL.md".to_string(),
                scope: "user".to_string(),
                allow_implicit_invocation: true,
                user_disabled: false,
                auto_use_enabled: true,
            }],
            ..DashboardState::default()
        };
        let context = DashboardCommandContext {
            requests: &[],
            state: &state,
        };

        assert!(command_blocks_submission("/skills", &context).is_none());
        let text = render_skills_list(&state);
        assert!(text.contains("OpenSkills loaded: 1"));
        assert!(text.contains("writer"));
    }

    #[test]
    fn root_commands_open_interactive_bottom_panels() {
        let requests = vec![pending_request(42)];
        let state = DashboardState {
            app_status_outputs: vec![("browser".to_string(), "ready".to_string())],
            skills: vec![OpenSkillDashboardSummary {
                name: "writer".to_string(),
                description: "Write release notes".to_string(),
                path: "/tmp/skills/writer/SKILL.md".to_string(),
                scope: "user".to_string(),
                allow_implicit_invocation: true,
                user_disabled: false,
                auto_use_enabled: true,
            }],
            ..DashboardState::default()
        };
        let context = DashboardCommandContext {
            requests: &requests,
            state: &state,
        };

        match command_panel_for_input("/debug", &context).expect("debug panel") {
            CommandPanel::Selection(panel) => {
                assert!(panel.items.iter().any(|item| item.name.contains("persona")));
            }
            _ => panic!("debug should open a selection panel"),
        }
        match command_panel_for_input("/app-status", &context).expect("app panel") {
            CommandPanel::Selection(panel) => {
                assert_eq!(panel.items[0].name, "browser");
            }
            _ => panic!("app-status should open an app selection panel"),
        }
        match command_panel_for_input("/skills", &context).expect("skills panel") {
            CommandPanel::Selection(panel) => {
                assert_eq!(
                    panel
                        .items
                        .iter()
                        .map(|item| item.name.as_str())
                        .collect::<Vec<_>>(),
                    vec!["List skills", "Enable/Disable Skills"]
                );
            }
            _ => panic!("skills should open a management panel"),
        }
        match command_panel_for_input("/skills list", &context).expect("skills list panel") {
            CommandPanel::SkillsList(panel) => {
                assert_eq!(panel.items[0].name, "writer");
            }
            _ => panic!("skills list should open a skill list panel"),
        }
        match command_panel_for_input("/telegram approve", &context).expect("telegram panel") {
            CommandPanel::TelegramAccess(panel) => {
                assert_eq!(panel.requests.len(), 1);
            }
            _ => panic!("telegram approve should open an access picker"),
        }
    }

    #[test]
    fn illegal_extra_arguments_block_submission() {
        let context = test_command_context();
        let feedback =
            command_blocks_submission("/sleep run now", &context).expect("extra arg feedback");

        assert!(matches!(feedback.level, CommandFeedbackLevel::Error));
        assert!(feedback.message.contains("does not take extra arguments"));
    }

    #[test]
    fn hidden_action_commands_parse_to_dashboard_actions() {
        let requests = vec![pending_request(42)];
        let state = DashboardState {
            skills: vec![OpenSkillDashboardSummary {
                name: "writer".to_string(),
                description: "Write release notes".to_string(),
                path: "/tmp/skills/writer/SKILL.md".to_string(),
                scope: "user".to_string(),
                allow_implicit_invocation: true,
                user_disabled: false,
                auto_use_enabled: true,
            }],
            ..DashboardState::default()
        };
        let context = DashboardCommandContext {
            requests: &requests,
            state: &state,
        };

        let invocation = dashboard_action_for_input("/skills disable writer", &context)
            .expect("valid action")
            .expect("skills action");
        assert_eq!(
            invocation.action,
            DashboardAction::SetSkillAutoUse {
                path: PathBuf::from("/tmp/skills/writer/SKILL.md"),
                enabled: false,
            }
        );

        let invocation = dashboard_action_for_input("/telegram approve 42", &context)
            .expect("valid action")
            .expect("telegram action");
        assert_eq!(
            invocation.action,
            DashboardAction::ApproveTelegramAccess { chat_id: 42 }
        );
    }

    #[test]
    fn skills_toggle_panel_shows_only_error_feedback() {
        let mut panel = CommandPanel::SkillsToggle(SkillsTogglePanel {
            items: Vec::new(),
            selected: 0,
            scroll: 0,
            search: String::new(),
            feedback: None,
        });

        panel.set_error_feedback(CommandFeedback {
            title: "SKILLS".to_string(),
            message: "queued skills auto-use enable".to_string(),
            detail: None,
            level: CommandFeedbackLevel::Info,
        });
        match &panel {
            CommandPanel::SkillsToggle(panel) => assert!(panel.feedback.is_none()),
            _ => panic!("expected skills toggle panel"),
        }

        panel.set_error_feedback(CommandFeedback {
            title: "SKILLS".to_string(),
            message: "failed to queue skills auto-use".to_string(),
            detail: None,
            level: CommandFeedbackLevel::Error,
        });
        match &panel {
            CommandPanel::SkillsToggle(panel) => {
                assert_eq!(
                    panel
                        .feedback
                        .as_ref()
                        .map(|feedback| feedback.message.as_str()),
                    Some("failed to queue skills auto-use")
                );
            }
            _ => panic!("expected skills toggle panel"),
        }
    }

    #[test]
    fn clear_command_maps_to_quiet_dashboard_action() {
        let context = test_command_context();
        let invocation = dashboard_action_for_input("/clear", &context)
            .expect("valid action")
            .expect("clear action");

        assert!(is_clear_command_input("/clear"));
        assert!(invocation.quiet_success);
        assert_eq!(invocation.action, DashboardAction::ClearConversation);
    }

    #[test]
    fn shift_or_alt_enter_insert_newline() {
        assert!(should_insert_newline_on_enter(KeyModifiers::SHIFT));
        assert!(should_insert_newline_on_enter(KeyModifiers::ALT));
        assert!(should_insert_newline_on_enter(
            KeyModifiers::SHIFT | KeyModifiers::ALT
        ));
        assert!(!should_insert_newline_on_enter(KeyModifiers::NONE));
        assert!(!should_insert_newline_on_enter(KeyModifiers::CONTROL));
    }

    #[test]
    fn wrapped_input_height_counts_trailing_newline() {
        assert_eq!(wrapped_input_height("hello", 80), 1);
        assert_eq!(wrapped_input_height("hello\n", 80), 2);
        assert_eq!(wrapped_input_height("hello\nworld", 80), 2);
    }

    #[test]
    fn command_input_display_height_expands_past_ten_lines() {
        assert_eq!(command_input_display_height(18, 40, 0), 18);
    }

    #[test]
    fn cursor_display_xy_moves_after_trailing_newline() {
        let area = Rect::new(0, 0, 80, 10);
        assert_eq!(
            cursor_display_xy("hello\n", "hello\n".len(), 78, 2, area, 0),
            (2, 1)
        );
    }

    #[test]
    fn command_input_display_text_indents_multiline_continuations() {
        let mut display = String::from("› ");
        push_command_input_display_text(&mut display, "hello\nworld\n");

        assert_eq!(display, "› hello\n  world\n  ");
    }

    #[test]
    fn command_input_display_text_renders_completion_suffix_dim() {
        let text = command_input_display_text("/app", Some("/app-status"));

        assert_eq!(text.lines.len(), 1);
        assert_eq!(text.lines[0].spans.len(), 2);
        assert_eq!(text.lines[0].spans[0].content.as_ref(), "› /app");
        assert_eq!(text.lines[0].spans[0].style.fg, Some(Color::White));
        assert_eq!(text.lines[0].spans[1].content.as_ref(), "-status");
        assert_eq!(text.lines[0].spans[1].style.fg, Some(Color::DarkGray));
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
        assert!(commands.contains(&"skills"));
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
        };

        let output = render_pending_access_requests("approve", context.requests);

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

    #[test]
    fn dashboard_state_millisecond_fields_json_round_trip() {
        let state = DashboardState {
            last_cycle_elapsed_ms: Some(42),
            ..DashboardState::default()
        };

        let encoded = serde_json::to_string(&state).expect("serialize dashboard state");
        let decoded: DashboardState =
            serde_json::from_str(&encoded).expect("deserialize dashboard state");

        assert_eq!(decoded.last_cycle_elapsed_ms, Some(42));
    }
}
