use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{
    DashboardAction, DashboardPendingUserInput, DashboardPendingUserInputMoveDirection,
    DashboardState, DashboardWorkflowSummary,
    command_text::{format_skill_detail, skill_status_description},
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

pub(super) struct SkillSearchKey {
    name: String,
    description: String,
    path: String,
    scope: String,
}

impl SkillSearchKey {
    fn from_list(item: &SkillsListPanelItem) -> Self {
        Self {
            name: item.name.to_ascii_lowercase(),
            description: item.description.to_ascii_lowercase(),
            path: item.path.to_ascii_lowercase(),
            scope: item.scope.to_ascii_lowercase(),
        }
    }

    fn from_toggle(item: &SkillsTogglePanelItem) -> Self {
        Self {
            name: item.name.to_ascii_lowercase(),
            description: item.description.to_ascii_lowercase(),
            path: item.path.to_ascii_lowercase(),
            scope: String::new(),
        }
    }

    fn matches(&self, query: &str) -> bool {
        self.name.contains(query)
            || self.description.contains(query)
            || self.path.contains(query)
            || (!self.scope.is_empty() && self.scope.contains(query))
    }
}

pub(super) struct SkillsListPanel {
    pub(super) items: Vec<SkillsListPanelItem>,
    pub(super) errors: Vec<OpenSkillDashboardError>,
    pub(super) selected: usize,
    pub(super) scroll: usize,
    pub(super) search: String,
    pub(super) visible: Vec<usize>,
    pub(super) search_keys: Vec<SkillSearchKey>,
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
    pub(super) visible: Vec<usize>,
    pub(super) search_keys: Vec<SkillSearchKey>,
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
            Self::SkillsList(panel) => panel.sync_state(state),
            Self::SkillsToggle(panel) => panel.sync_state(state),
            Self::WorkflowForm(panel) => panel.sync_state(state),
            Self::PendingUserInputQueue(panel) => panel.sync_state(state),
            Self::Detail(_) | Self::Selection(_) => {}
        }
    }

    pub(super) const fn footer_hint(&self) -> &'static str {
        match self {
            Self::Detail(_) => "Esc close   ↑/↓ scroll   PgUp/PgDn page",
            Self::Selection(_) => "Enter select   ↑/↓ move   PgUp/PgDn page   Esc close",
            Self::SkillsList(_) => {
                "Enter details   type search   Backspace edit   ↑/↓ move   Esc close"
            }
            Self::SkillsToggle(_) => {
                "Space/Enter toggle auto-use   type search   Backspace edit   Esc close"
            }
            Self::WorkflowForm(_) => {
                "Enter edit/submit   ↑/↓ field   type value   Tab next   Esc close"
            }
            Self::PendingUserInputQueue(_) => {
                "Enter edit   d discard   Shift+↑/↓ reorder   c clear   Esc close"
            }
        }
    }

    pub(super) fn set_error_feedback(&mut self, feedback: CommandFeedback) {
        match self {
            Self::SkillsToggle(panel) => {
                panel.feedback =
                    matches!(feedback.level, CommandFeedbackLevel::Error).then_some(feedback);
            }
            Self::WorkflowForm(panel) => {
                panel.feedback =
                    matches!(feedback.level, CommandFeedbackLevel::Error).then_some(feedback);
            }
            Self::PendingUserInputQueue(panel) => {
                panel.feedback =
                    matches!(feedback.level, CommandFeedbackLevel::Error).then_some(feedback);
            }
            _ => {}
        }
    }

    pub(super) fn clear_feedback(&mut self) {
        match self {
            Self::SkillsToggle(panel) => panel.feedback = None,
            Self::WorkflowForm(panel) => panel.feedback = None,
            Self::PendingUserInputQueue(panel) => panel.feedback = None,
            _ => {}
        }
    }
}

impl SkillsListPanel {
    pub(super) fn from_state(state: &DashboardState) -> Self {
        let mut panel = Self {
            items: state
                .skills
                .iter()
                .map(SkillsListPanelItem::from_summary)
                .collect(),
            errors: state.skill_errors.clone(),
            selected: 0,
            scroll: 0,
            search: String::new(),
            visible: Vec::new(),
            search_keys: Vec::new(),
        };
        panel.rebuild_search_keys();
        panel.rebuild_visible();
        panel
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
        self.rebuild_search_keys();
        self.rebuild_visible();
        self.errors.clone_from(&state.skill_errors);
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

    pub(super) fn visible_indices(&self) -> &[usize] {
        &self.visible
    }

    fn rebuild_search_keys(&mut self) {
        self.search_keys = self.items.iter().map(SkillSearchKey::from_list).collect();
    }

    fn rebuild_visible(&mut self) {
        let query = self.search.trim().to_ascii_lowercase();
        self.visible.clear();
        if query.is_empty() {
            self.visible.extend(0..self.items.len());
            return;
        }
        for (idx, key) in self.search_keys.iter().enumerate() {
            if key.matches(&query) {
                self.visible.push(idx);
            }
        }
    }

    fn selected_actual_index(&self) -> Option<usize> {
        self.visible_indices().get(self.selected).copied()
    }

    fn selected_detail_panel(&self) -> Option<CommandPanel> {
        let idx = self.selected_actual_index()?;
        let item = self.items.get(idx)?;
        Some(detail_panel(
            format!("SKILL {}", item.name),
            format_skill_detail(
                &item.name,
                &item.status,
                &item.scope,
                &item.path,
                &item.description,
            ),
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
    match primary_schema_type(schema).as_deref() {
        Some("integer" | "number") => "0".to_string(),
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

fn schema_type_names(schema: &serde_json::Value) -> Vec<String> {
    match schema.get("type") {
        Some(serde_json::Value::String(kind)) => vec![kind.clone()],
        Some(serde_json::Value::Array(types)) => types
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn primary_schema_type(schema: &serde_json::Value) -> Option<String> {
    schema_type_names(schema)
        .into_iter()
        .find(|kind| kind != "null")
}

fn schema_enum_values(schema: &serde_json::Value) -> Option<Vec<serde_json::Value>> {
    schema
        .get("enum")
        .and_then(serde_json::Value::as_array)
        .cloned()
}

fn parse_workflow_field_value(
    field_name: &str,
    value: &str,
    schema: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let trimmed = value.trim();
    let type_names = schema_type_names(schema);
    let nullable = type_names.iter().any(|kind| kind == "null");
    if nullable && (trimmed.is_empty() || trimmed.eq_ignore_ascii_case("null")) {
        return Ok(serde_json::Value::Null);
    }

    let parsed = coerce_schema_text(field_name, value, trimmed, schema, &type_names)?;
    if let Some(enum_values) = schema_enum_values(schema)
        && !enum_values.iter().any(|candidate| candidate == &parsed)
    {
        return Err(format!("{field_name} must be one of the allowed values"));
    }
    Ok(parsed)
}

fn coerce_schema_text(
    field_name: &str,
    raw: &str,
    trimmed: &str,
    schema: &serde_json::Value,
    type_names: &[String],
) -> Result<serde_json::Value, String> {
    let concrete: Vec<&str> = type_names
        .iter()
        .map(String::as_str)
        .filter(|kind| *kind != "null")
        .collect();
    if concrete.is_empty() {
        if schema_enum_values(schema).is_some() {
            return Ok(coerce_untyped_enum_text(trimmed));
        }
        return Ok(serde_json::Value::String(raw.to_string()));
    }

    let mut errors = Vec::new();
    for kind in concrete {
        match coerce_typed_text(field_name, raw, trimmed, kind) {
            Ok(value) => return Ok(value),
            Err(error) => errors.push(error),
        }
    }
    Err(errors
        .into_iter()
        .next()
        .unwrap_or_else(|| format!("{field_name} does not match the schema")))
}

fn coerce_typed_text(
    field_name: &str,
    raw: &str,
    trimmed: &str,
    kind: &str,
) -> Result<serde_json::Value, String> {
    match kind {
        "string" => Ok(serde_json::Value::String(raw.to_string())),
        "integer" => trimmed
            .parse::<i64>()
            .map(serde_json::Value::from)
            .map_err(|_| format!("{field_name} must be an integer")),
        "number" => parse_schema_number(field_name, trimmed),
        "boolean" => trimmed
            .parse::<bool>()
            .map(serde_json::Value::from)
            .map_err(|_| format!("{field_name} must be true or false")),
        "array" => parse_schema_json_kind(field_name, trimmed, serde_json::Value::is_array, "array"),
        "object" => {
            parse_schema_json_kind(field_name, trimmed, serde_json::Value::is_object, "object")
        }
        "null" => {
            if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("null") {
                Ok(serde_json::Value::Null)
            } else {
                Err(format!("{field_name} must be null"))
            }
        }
        _ => Err(format!("{field_name} has unsupported schema type {kind}")),
    }
}

fn parse_schema_number(field_name: &str, trimmed: &str) -> Result<serde_json::Value, String> {
    if let Ok(integer) = trimmed.parse::<i64>() {
        return Ok(serde_json::Value::from(integer));
    }
    let number = trimmed
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
        .and_then(serde_json::Number::from_f64)
        .ok_or_else(|| format!("{field_name} must be a number"))?;
    Ok(serde_json::Value::Number(number))
}

fn parse_schema_json_kind(
    field_name: &str,
    trimmed: &str,
    matches_kind: fn(&serde_json::Value) -> bool,
    label: &str,
) -> Result<serde_json::Value, String> {
    let parsed = serde_json::from_str::<serde_json::Value>(trimmed)
        .map_err(|_| format!("{field_name} must be valid JSON"))?;
    if matches_kind(&parsed) {
        Ok(parsed)
    } else {
        Err(format!("{field_name} must be a {label}"))
    }
}

fn coerce_untyped_enum_text(trimmed: &str) -> serde_json::Value {
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return parsed;
    }
    serde_json::Value::String(trimmed.to_string())
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
        self.inputs.clone_from(&state.pending_user_inputs);
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
        let mut panel = Self {
            items: state
                .skills
                .iter()
                .map(SkillsTogglePanelItem::from_summary)
                .collect(),
            selected: 0,
            scroll: 0,
            search: String::new(),
            visible: Vec::new(),
            search_keys: Vec::new(),
            feedback: None,
        };
        panel.rebuild_search_keys();
        panel.rebuild_visible();
        panel
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
        self.rebuild_search_keys();
        self.rebuild_visible();
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

    pub(super) fn visible_indices(&self) -> &[usize] {
        &self.visible
    }

    fn rebuild_search_keys(&mut self) {
        self.search_keys = self.items.iter().map(SkillSearchKey::from_toggle).collect();
    }

    fn rebuild_visible(&mut self) {
        let query = self.search.trim().to_ascii_lowercase();
        self.visible.clear();
        if query.is_empty() {
            self.visible.extend(0..self.items.len());
            return;
        }
        for (idx, key) in self.search_keys.iter().enumerate() {
            if key.matches(&query) {
                self.visible.push(idx);
            }
        }
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

const fn handle_detail_panel_key(
    panel: &mut CommandDetailPanel,
    key: KeyEvent,
) -> CommandPanelAction {
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
            panel.rebuild_visible();
            panel.clamp_after_filter_change();
            CommandPanelAction::None
        }
        KeyCode::Enter => panel
            .selected_detail_panel()
            .map_or(CommandPanelAction::None, CommandPanelAction::Replace),
        KeyCode::Char(c)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            panel.search.push(c);
            panel.rebuild_visible();
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
            panel.rebuild_visible();
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
            panel.rebuild_visible();
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

#[cfg(test)]
mod workflow_form_tests {
    use super::{default_value_text, parse_workflow_field_value};

    #[test]
    fn string_schema_keeps_defaults_and_rejects_non_enum_values() {
        let string_schema = serde_json::json!({"type": "string", "default": "keep"});
        assert_eq!(default_value_text(&string_schema), "keep");
        assert_eq!(
            parse_workflow_field_value("name", "42", &string_schema).unwrap(),
            serde_json::json!("42")
        );
        let enumerated = serde_json::json!({"type": "string", "enum": ["alpha", "beta"]});
        assert!(parse_workflow_field_value("name", "42", &enumerated).is_err());
        assert_eq!(
            parse_workflow_field_value("name", "alpha", &enumerated).unwrap(),
            serde_json::json!("alpha")
        );
    }

    #[test]
    fn type_arrays_honor_numbers_enums_and_null() {
        let nullable_number = serde_json::json!({"type": ["number", "null"], "default": 3});
        assert_eq!(default_value_text(&nullable_number), "3");
        assert_eq!(
            parse_workflow_field_value("count", "null", &nullable_number).unwrap(),
            serde_json::Value::Null
        );
        assert_eq!(
            parse_workflow_field_value("count", "1.5", &nullable_number).unwrap(),
            serde_json::json!(1.5)
        );
        assert!(parse_workflow_field_value("count", "nope", &nullable_number).is_err());

        let mode = serde_json::json!({"type": ["string", "null"], "enum": ["fast", "slow", null]});
        assert_eq!(
            parse_workflow_field_value("mode", "fast", &mode).unwrap(),
            serde_json::json!("fast")
        );
        assert!(parse_workflow_field_value("mode", "other", &mode).is_err());
        assert_eq!(
            parse_workflow_field_value("mode", "", &mode).unwrap(),
            serde_json::Value::Null
        );

        let items = serde_json::json!({"type": "array"});
        assert!(parse_workflow_field_value("items", "{}", &items).is_err());
        assert_eq!(
            parse_workflow_field_value("items", "[1]", &items).unwrap(),
            serde_json::json!([1])
        );
    }
}
