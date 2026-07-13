use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{
    DashboardAction, DashboardPendingUserInput, DashboardPendingUserInputMoveDirection,
    DashboardState, DashboardWorkflowSummary, command_text::skill_status_description,
};
use crate::openskills::{OpenSkillDashboardError, OpenSkillDashboardSummary};

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
    OpenWorkflowForm {
        workflow: DashboardWorkflowSummary,
    },
    OpenSkillsToggle,
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
pub(super) struct WorkflowFormPanel {
    pub(super) workflow: DashboardWorkflowSummary,
    pub(super) values: Vec<String>,
    pub(super) selected: usize,
    pub(super) scroll: usize,
    pub(super) feedback: Option<CommandFeedback>,
}

#[derive(Clone, Debug)]
pub(super) struct WorkflowFormSubmission {
    pub(super) workflow_id: String,
    pub(super) input: serde_json::Value,
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
    WorkflowForm(WorkflowFormPanel),
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

pub(super) enum CommandPanelAction {
    None,
    Close,
    Replace(CommandPanel),
    OpenSkillsList,
    OpenSkillsToggle,
    SubmitWorkflow(WorkflowFormSubmission),
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
    pub(super) state: &'a DashboardState,
}

#[derive(Clone)]
pub(super) struct CommandSuggestion {
    pub(super) display: String,
    pub(super) completion: String,
    pub(super) description: String,
}

impl CommandPanel {
    pub(super) fn sync_state(&mut self, state: &DashboardState) {
        match self {
            CommandPanel::SkillsList(panel) => panel.sync_state(state),
            CommandPanel::SkillsToggle(panel) => panel.sync_state(state),
            CommandPanel::WorkflowForm(panel) => panel.sync_state(state),
            CommandPanel::PendingUserInputQueue(panel) => panel.sync_state(state),
            CommandPanel::Detail(_) | CommandPanel::Selection(_) => {}
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
            CommandPanel::WorkflowForm(_) => {
                "Enter edit/submit   ↑/↓ field   type value   Tab next   Esc close"
            }
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
            CommandPanel::WorkflowForm(panel) => {
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
            CommandPanel::WorkflowForm(panel) => panel.feedback = None,
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
impl WorkflowFormPanel {
    pub(super) fn from_workflow(workflow: DashboardWorkflowSummary) -> Self {
        let values = workflow
            .input_fields
            .iter()
            .map(|field| default_value_text(&field.schema))
            .collect();
        Self {
            workflow,
            values,
            selected: 0,
            scroll: 0,
            feedback: None,
        }
    }

    pub(super) fn sync_state(&mut self, state: &DashboardState) {
        let Some(workflow) = state
            .workflows
            .iter()
            .find(|workflow| workflow.id == self.workflow.id)
            .cloned()
        else {
            return;
        };
        let previous_values = std::mem::take(&mut self.values);
        self.values = workflow
            .input_fields
            .iter()
            .enumerate()
            .map(|(index, field)| {
                previous_values
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| default_value_text(&field.schema))
            })
            .collect();
        self.workflow = workflow;
        self.clamp_selection();
    }

    fn clamp_selection(&mut self) {
        self.selected = self
            .selected
            .min(self.workflow.input_fields.len().saturating_sub(1));
        self.scroll = adjusted_list_scroll(
            self.scroll,
            self.selected,
            self.workflow.input_fields.len(),
            6,
        );
    }

    fn submit(&self) -> Result<WorkflowFormSubmission, String> {
        let mut input = serde_json::Map::new();
        for (index, field) in self.workflow.input_fields.iter().enumerate() {
            let value = self
                .values
                .get(index)
                .map(String::as_str)
                .unwrap_or_default();
            let parsed = parse_workflow_field_value(&field.name, value, &field.schema)?;
            input.insert(field.name.clone(), parsed);
        }
        Ok(WorkflowFormSubmission {
            workflow_id: self.workflow.id.clone(),
            input: serde_json::Value::Object(input),
        })
    }
}

fn default_value_text(schema: &serde_json::Value) -> String {
    if let Some(default) = schema.get("default") {
        return default_value_to_text(default);
    }
    match schema_type(schema).as_deref() {
        Some("string") => String::new(),
        Some("integer") | Some("number") => "0".to_string(),
        Some("boolean") => "false".to_string(),
        Some("array") => "[]".to_string(),
        Some("object") => "{}".to_string(),
        _ => String::new(),
    }
}

fn default_value_to_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(value) => value.clone(),
        _ => value.to_string(),
    }
}

fn schema_type(schema: &serde_json::Value) -> Option<String> {
    schema
        .get("type")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            schema
                .get("type")
                .and_then(serde_json::Value::as_array)
                .and_then(|types| {
                    types
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .find(|kind| *kind != "null")
                        .map(str::to_string)
                })
        })
}

fn parse_workflow_field_value(
    field_name: &str,
    value: &str,
    schema: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let trimmed = value.trim();
    let nullable = schema
        .get("type")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|types| types.iter().any(|kind| kind.as_str() == Some("null")));
    if nullable && (trimmed.is_empty() || trimmed.eq_ignore_ascii_case("null")) {
        return Ok(serde_json::Value::Null);
    }
    match schema_type(schema).as_deref() {
        Some("string") => Ok(serde_json::Value::String(value.to_string())),
        Some("integer") => trimmed
            .parse::<i64>()
            .map(serde_json::Value::from)
            .map_err(|_| format!("{field_name} must be an integer")),
        Some("number") => trimmed
            .parse::<f64>()
            .map(serde_json::Value::from)
            .map_err(|_| format!("{field_name} must be a number")),
        Some("boolean") => trimmed
            .parse::<bool>()
            .map(serde_json::Value::from)
            .map_err(|_| format!("{field_name} must be true or false")),
        Some("array") | Some("object") => {
            serde_json::from_str(trimmed).map_err(|_| format!("{field_name} must be valid JSON"))
        }
        _ => Ok(serde_json::Value::String(value.to_string())),
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
        CommandPanel::WorkflowForm(form) => handle_workflow_form_panel_key(form, key),
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
                CommandSelectionAction::OpenWorkflowForm { workflow } => {
                    CommandPanelAction::Replace(CommandPanel::WorkflowForm(
                        WorkflowFormPanel::from_workflow(workflow.clone()),
                    ))
                }
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
            }
        }
        _ => CommandPanelAction::None,
    }
}
fn handle_workflow_form_panel_key(
    panel: &mut WorkflowFormPanel,
    key: KeyEvent,
) -> CommandPanelAction {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => CommandPanelAction::Close,
        KeyCode::Up | KeyCode::Char('k') => {
            panel.selected = panel.selected.saturating_sub(1);
            panel.clamp_selection();
            CommandPanelAction::None
        }
        KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
            panel.selected =
                (panel.selected + 1).min(panel.workflow.input_fields.len().saturating_sub(1));
            panel.clamp_selection();
            CommandPanelAction::None
        }
        KeyCode::PageUp => {
            panel.selected = panel.selected.saturating_sub(6);
            panel.clamp_selection();
            CommandPanelAction::None
        }
        KeyCode::PageDown => {
            panel.selected =
                (panel.selected + 6).min(panel.workflow.input_fields.len().saturating_sub(1));
            panel.clamp_selection();
            CommandPanelAction::None
        }
        KeyCode::Home => {
            panel.selected = 0;
            panel.clamp_selection();
            CommandPanelAction::None
        }
        KeyCode::End => {
            panel.selected = panel.workflow.input_fields.len().saturating_sub(1);
            panel.clamp_selection();
            CommandPanelAction::None
        }
        KeyCode::Backspace => {
            if let Some(value) = panel.values.get_mut(panel.selected) {
                value.pop();
            }
            panel.feedback = None;
            CommandPanelAction::None
        }
        KeyCode::Enter => match panel.submit() {
            Ok(submission) => CommandPanelAction::SubmitWorkflow(submission),
            Err(message) => {
                panel.feedback = Some(CommandFeedback {
                    title: format!("WORKFLOW {}", panel.workflow.id),
                    message,
                    detail: None,
                    level: CommandFeedbackLevel::Error,
                });
                CommandPanelAction::None
            }
        },
        KeyCode::Char(value)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            if let Some(target) = panel.values.get_mut(panel.selected) {
                target.push(value);
            }
            panel.feedback = None;
            CommandPanelAction::None
        }
        _ => CommandPanelAction::None,
    }
}

fn handle_pending_user_input_queue_panel_key(
    panel: &mut PendingUserInputQueuePanel,
    key: KeyEvent,
) -> CommandPanelAction {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => CommandPanelAction::Close,
        KeyCode::Char('r') => {
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
