use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{
    DashboardAction, DashboardPendingUserInput, DashboardPendingUserInputMoveDirection,
    DashboardState, command_text::skill_status_description,
};
use crate::{
    openskills::{OpenSkillDashboardError, OpenSkillDashboardSummary},
    telegram_acl::PendingAccessRequest,
};

pub(super) const TELEGRAM_ACCESS_PICKER_VISIBLE_ROWS: usize = 8;

pub(super) struct CommandDetailPanel {
    pub(super) title: String,
    pub(super) text: String,
    pub(super) scroll: u16,
}

pub(super) struct CommandSelectionPanel {
    pub(super) title: String,
    pub(super) subtitle: Option<String>,
    pub(super) items: Vec<CommandSelectionItem>,
    pub(super) selected: usize,
    pub(super) scroll: usize,
}

pub(super) struct CommandSelectionItem {
    pub(super) name: String,
    pub(super) description: String,
    pub(super) action: CommandSelectionAction,
    pub(super) disabled: bool,
}

pub(super) enum CommandSelectionAction {
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

pub(super) struct SkillsListPanel {
    pub(super) items: Vec<SkillsListPanelItem>,
    pub(super) errors: Vec<OpenSkillDashboardError>,
    pub(super) selected: usize,
    pub(super) scroll: usize,
    pub(super) search: String,
}

#[derive(Clone)]
pub(super) struct SkillsListPanelItem {
    pub(super) name: String,
    pub(super) description: String,
    pub(super) path: String,
    pub(super) scope: String,
    pub(super) status: String,
}

pub(super) struct SkillsTogglePanel {
    pub(super) items: Vec<SkillsTogglePanelItem>,
    pub(super) selected: usize,
    pub(super) scroll: usize,
    pub(super) search: String,
    pub(super) feedback: Option<CommandFeedback>,
}

#[derive(Clone)]
pub(super) struct SkillsTogglePanelItem {
    pub(super) name: String,
    pub(super) description: String,
    pub(super) path: String,
    pub(super) scope: String,
    pub(super) allow_implicit_invocation: bool,
    pub(super) user_disabled: bool,
    pub(super) auto_use_enabled: bool,
}
pub(super) struct PendingUserInputQueuePanel {
    pub(super) inputs: Vec<DashboardPendingUserInput>,
    pub(super) selected: usize,
    pub(super) scroll: usize,
    pub(super) feedback: Option<CommandFeedback>,
}

pub(super) enum CommandPanel {
    Detail(CommandDetailPanel),
    Selection(CommandSelectionPanel),
    SkillsList(SkillsListPanel),
    SkillsToggle(SkillsTogglePanel),
    TelegramAccess(TelegramAccessPicker),
    PendingUserInputQueue(PendingUserInputQueuePanel),
}

#[derive(Clone, Debug)]
pub(super) struct CommandFeedback {
    pub(super) title: String,
    pub(super) message: String,
    pub(super) detail: Option<String>,
    pub(super) level: CommandFeedbackLevel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CommandFeedbackLevel {
    Info,
    Warning,
    Error,
}

pub(super) struct TelegramAccessPicker {
    pub(super) action: TelegramAccessAction,
    pub(super) requests: Vec<PendingAccessRequest>,
    pub(super) selected: usize,
    pub(super) scroll: usize,
}

#[derive(Clone, Copy)]
pub(super) enum TelegramAccessAction {
    Approve,
    Reject,
}

pub(super) enum CommandPanelAction {
    None,
    Close,
    Replace(CommandPanel),
    OpenSkillsList,
    OpenSkillsToggle,
    OpenTelegramAccess(TelegramAccessAction),
    EditPendingUserInput {
        event_id: String,
        incoming_text: String,
    },
    RunAction {
        title: String,
        action: DashboardAction,
        keep_panel: bool,
    },
}

pub(super) struct DashboardActionInvocation {
    pub(super) title: String,
    pub(super) action: DashboardAction,
    pub(super) quiet_success: bool,
}

pub(super) struct DashboardCommandContext<'a> {
    pub(super) requests: &'a [PendingAccessRequest],
    pub(super) state: &'a DashboardState,
}

#[derive(Clone)]
pub(super) struct CommandSuggestion {
    pub(super) display: String,
    pub(super) completion: String,
    pub(super) description: String,
}

impl TelegramAccessAction {
    pub(super) fn verb(self) -> &'static str {
        match self {
            TelegramAccessAction::Approve => "approve",
            TelegramAccessAction::Reject => "reject",
        }
    }

    pub(super) fn title(self) -> &'static str {
        match self {
            TelegramAccessAction::Approve => "TELEGRAM APPROVE",
            TelegramAccessAction::Reject => "TELEGRAM REJECT",
        }
    }
}

impl CommandPanel {
    pub(super) fn sync_state(&mut self, state: &DashboardState) {
        match self {
            CommandPanel::SkillsList(panel) => panel.sync_state(state),
            CommandPanel::SkillsToggle(panel) => panel.sync_state(state),
            CommandPanel::PendingUserInputQueue(panel) => panel.sync_state(state),
            CommandPanel::Detail(_)
            | CommandPanel::Selection(_)
            | CommandPanel::TelegramAccess(_) => {}
        }
    }

    pub(super) fn footer_hint(&self) -> &'static str {
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
            CommandPanel::PendingUserInputQueue(_) => {
                "Enter edit   d discard   Shift+↑/↓ reorder   c clear   Esc close"
            }
        }
    }

    pub(super) fn set_error_feedback(&mut self, feedback: CommandFeedback) {
        match self {
            CommandPanel::SkillsToggle(panel) => {
                panel.feedback =
                    matches!(feedback.level, CommandFeedbackLevel::Error).then_some(feedback);
            }
            CommandPanel::PendingUserInputQueue(panel) => {
                panel.feedback =
                    matches!(feedback.level, CommandFeedbackLevel::Error).then_some(feedback);
            }
            _ => {}
        }
    }

    pub(super) fn clear_feedback(&mut self) {
        match self {
            CommandPanel::SkillsToggle(panel) => panel.feedback = None,
            CommandPanel::PendingUserInputQueue(panel) => panel.feedback = None,
            _ => {}
        }
    }
}

impl SkillsListPanel {
    pub(super) fn from_state(state: &DashboardState) -> Self {
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

    pub(super) fn sync_state(&mut self, state: &DashboardState) {
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

    pub(super) fn visible_indices(&self) -> Vec<usize> {
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
impl PendingUserInputQueuePanel {
    pub(super) fn from_state(state: &DashboardState) -> Option<Self> {
        if state.pending_user_inputs.is_empty() {
            return None;
        }
        Some(Self {
            inputs: state.pending_user_inputs.clone(),
            selected: 0,
            scroll: 0,
            feedback: None,
        })
    }

    pub(super) fn sync_state(&mut self, state: &DashboardState) {
        let selected_event_id = self
            .inputs
            .get(self.selected)
            .map(|input| input.event_id.clone());
        self.inputs = state.pending_user_inputs.clone();
        if let Some(selected_event_id) = selected_event_id
            && let Some(index) = self
                .inputs
                .iter()
                .position(|input| input.event_id == selected_event_id)
        {
            self.selected = index;
        }
        self.clamp_selection();
    }

    fn selected_input(&self) -> Option<&DashboardPendingUserInput> {
        self.inputs.get(self.selected)
    }

    fn clamp_selection(&mut self) {
        self.selected = self.selected.min(self.inputs.len().saturating_sub(1));
        self.scroll = adjusted_list_scroll(self.scroll, self.selected, self.inputs.len(), 8);
    }
}

impl CommandSelectionPanel {
    fn adjusted_scroll(&self) -> usize {
        adjusted_list_scroll(self.scroll, self.selected, self.items.len(), 8)
    }
}

impl SkillsTogglePanel {
    pub(super) fn from_state(state: &DashboardState) -> Self {
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

    pub(super) fn sync_state(&mut self, state: &DashboardState) {
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

    pub(super) fn visible_indices(&self) -> Vec<usize> {
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

    pub(super) fn status_description(&self) -> String {
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

pub(super) fn detail_panel(title: impl Into<String>, text: impl Into<String>) -> CommandPanel {
    CommandPanel::Detail(CommandDetailPanel {
        title: title.into(),
        text: text.into(),
        scroll: 0,
    })
}

pub(super) fn handle_command_panel_key(
    panel: &mut CommandPanel,
    key: KeyEvent,
) -> CommandPanelAction {
    match panel {
        CommandPanel::Detail(detail) => handle_detail_panel_key(detail, key),
        CommandPanel::Selection(selection) => handle_selection_panel_key(selection, key),
        CommandPanel::SkillsList(skills) => handle_skills_list_panel_key(skills, key),
        CommandPanel::SkillsToggle(skills) => handle_skills_toggle_panel_key(skills, key),
        CommandPanel::TelegramAccess(picker) => handle_telegram_access_panel_key(picker, key),
        CommandPanel::PendingUserInputQueue(queue) => {
            handle_pending_user_input_queue_panel_key(queue, key)
        }
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
fn handle_pending_user_input_queue_panel_key(
    panel: &mut PendingUserInputQueuePanel,
    key: KeyEvent,
) -> CommandPanelAction {
    match key.code {
        KeyCode::Char('q') => CommandPanelAction::Close,
        KeyCode::Esc => {
            let Some(input) = panel.selected_input() else {
                return CommandPanelAction::None;
            };
            let Ok(event_id) = input.event_id.parse() else {
                return CommandPanelAction::None;
            };
            CommandPanelAction::RunAction {
                title: "Run queued input now".to_string(),
                action: DashboardAction::PreemptPendingUserInput { event_id },
                keep_panel: false,
            }
        }
        KeyCode::Up | KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::SHIFT) => {
            let Some(input) = panel.selected_input() else {
                return CommandPanelAction::None;
            };
            let Ok(event_id) = input.event_id.parse() else {
                return CommandPanelAction::None;
            };
            CommandPanelAction::RunAction {
                title: "Move queued input".to_string(),
                action: DashboardAction::MovePendingUserInput {
                    event_id,
                    direction: DashboardPendingUserInputMoveDirection::Up,
                },
                keep_panel: true,
            }
        }
        KeyCode::Down | KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::SHIFT) => {
            let Some(input) = panel.selected_input() else {
                return CommandPanelAction::None;
            };
            let Ok(event_id) = input.event_id.parse() else {
                return CommandPanelAction::None;
            };
            CommandPanelAction::RunAction {
                title: "Move queued input".to_string(),
                action: DashboardAction::MovePendingUserInput {
                    event_id,
                    direction: DashboardPendingUserInputMoveDirection::Down,
                },
                keep_panel: true,
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            panel.selected = panel.selected.saturating_sub(1);
            panel.clamp_selection();
            CommandPanelAction::None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            panel.selected = (panel.selected + 1).min(panel.inputs.len().saturating_sub(1));
            panel.clamp_selection();
            CommandPanelAction::None
        }
        KeyCode::PageUp => {
            panel.selected = panel.selected.saturating_sub(8);
            panel.clamp_selection();
            CommandPanelAction::None
        }
        KeyCode::PageDown => {
            panel.selected = (panel.selected + 8).min(panel.inputs.len().saturating_sub(1));
            panel.clamp_selection();
            CommandPanelAction::None
        }
        KeyCode::Home => {
            panel.selected = 0;
            panel.clamp_selection();
            CommandPanelAction::None
        }
        KeyCode::End => {
            panel.selected = panel.inputs.len().saturating_sub(1);
            panel.clamp_selection();
            CommandPanelAction::None
        }
        KeyCode::Enter | KeyCode::Char('e') => {
            let Some(input) = panel.selected_input() else {
                return CommandPanelAction::None;
            };
            CommandPanelAction::EditPendingUserInput {
                event_id: input.event_id.clone(),
                incoming_text: input.incoming_text.clone(),
            }
        }
        KeyCode::Char('d') | KeyCode::Delete | KeyCode::Backspace => {
            let Some(input) = panel.selected_input() else {
                return CommandPanelAction::None;
            };
            let Ok(event_id) = input.event_id.parse() else {
                return CommandPanelAction::None;
            };
            CommandPanelAction::RunAction {
                title: "Discard queued input".to_string(),
                action: DashboardAction::DismissPendingUserInput { event_id },
                keep_panel: true,
            }
        }
        KeyCode::Char('c') => CommandPanelAction::RunAction {
            title: "Clear queued inputs".to_string(),
            action: DashboardAction::ClearPendingUserInputs,
            keep_panel: true,
        },
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
