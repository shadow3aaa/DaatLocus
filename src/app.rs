use std::{collections::HashMap, fmt::Display, path::Path, path::PathBuf, time::Duration};

use async_trait::async_trait;
use miette::{Result, miette};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    dashboard::DashboardState,
    reasoning::{episode::EpisodeActionRecord, runtime::AgentToolCall},
    sandbox::RuntimeSandboxPolicy,
    tool_ui::{ToolCallUiEvent, ToolUiData, ToolUiEvent},
};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct AppId(String);

impl AppId {
    pub const DEFAULT_WORKSPACE_ENTRY: &str = "runtime/app.lua";

    pub fn browser() -> Self {
        Self("Browser".to_string())
    }

    pub fn terminal() -> Self {
        Self("Terminal".to_string())
    }

    pub fn coding() -> Self {
        Self("Coding".to_string())
    }

    pub fn from_workspace_folder(name: impl Into<String>) -> Result<Self> {
        let name = name.into();
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(miette!("workspace app folder name cannot be empty"));
        }
        if trimmed.contains(std::path::MAIN_SEPARATOR) || trimmed.contains('/') || trimmed == "." {
            return Err(miette!("invalid workspace app folder name `{trimmed}`"));
        }
        if !trimmed
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
        {
            return Err(miette!(
                "workspace app folder name `{trimmed}` must use only ASCII letters, numbers, `_`, or `-`"
            ));
        }
        if trimmed == Self::browser().as_str()
            || trimmed == Self::terminal().as_str()
            || trimmed == Self::coding().as_str()
        {
            return Err(miette!("workspace app id `{trimmed}` is reserved"));
        }
        Ok(Self(trimmed.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_terminal(&self) -> bool {
        self.as_str() == Self::terminal().as_str()
    }
}

impl Display for AppId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppToolScope {
    Browser,
    Terminal,
    Coding,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AppStateRender {
    pub title: String,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct AppUsage {
    pub description: String,
    pub when_to_focus: Vec<String>,
    pub body_markdown: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AppHowToUse {
    pub lines: Vec<String>,
    pub body_markdown: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AppDynamicToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AppDynamicToolResult {
    pub summary: String,
    pub payload: Value,
    pub model_content: Option<String>,
    pub ui_lines: Vec<String>,
    pub turn_boundary_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AppToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone)]
pub struct AppToolExecutionResult {
    pub summary: String,
    pub payload: Value,
    pub model_content: Option<String>,
    pub ui_event: ToolUiEvent,
    pub turn_boundary_reason: Option<String>,
}

#[derive(Clone)]
pub struct AppToolExecutionContext {
    pub execution_cwd: PathBuf,
    pub sandbox_policy: RuntimeSandboxPolicy,
    pub dashboard_tx: Option<tokio::sync::watch::Sender<DashboardState>>,
    pub tool_output_max_tokens: usize,
}

impl AppToolExecutionContext {
    pub fn resolve_tool_path(&self, path: &Path, base: Option<&Path>) -> PathBuf {
        self.sandbox_policy
            .resolve_path(path, base.or(Some(&self.execution_cwd)))
    }
}

fn summarize_app_inline_text(text: &str) -> String {
    const MAX_CHARS: usize = 120;
    let compact = text.replace('\n', "\\n");
    let mut chars = compact.chars();
    let summary = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{summary}...")
    } else {
        summary
    }
}

fn compact_app_tool_ui_lines(arguments: &Value) -> Vec<String> {
    match arguments {
        Value::Object(map) if map.is_empty() => Vec::new(),
        Value::Object(map) => map
            .iter()
            .map(|(key, value)| format!("{key}={}", summarize_app_inline_text(&value.to_string())))
            .take(8)
            .collect(),
        other => vec![summarize_app_inline_text(&other.to_string())],
    }
}

#[async_trait]
pub trait App: Send + Sync {
    fn id(&self) -> AppId;

    fn render_state(&self) -> AppStateRender;

    fn usage(&self) -> AppUsage;

    fn how_to_use(&self) -> AppHowToUse;

    fn focused_tool_scopes(&self) -> &'static [AppToolScope] {
        &[]
    }

    fn dynamic_tools(&self) -> Result<Vec<AppDynamicToolSpec>> {
        Ok(Vec::new())
    }

    fn tool_specs(&self) -> Result<Vec<AppToolSpec>> {
        Ok(self
            .dynamic_tools()?
            .into_iter()
            .map(|tool| AppToolSpec {
                name: tool.name,
                description: tool.description,
                input_schema: tool.input_schema,
            })
            .collect())
    }

    fn summarize_tool_call(&self, call: &AgentToolCall) -> Result<EpisodeActionRecord> {
        Ok(EpisodeActionRecord {
            kind: call.name.clone(),
            summary: summarize_app_inline_text(&call.arguments.to_string()),
        })
    }

    fn render_tool_call_ui(&self, call: &AgentToolCall) -> Result<ToolCallUiEvent> {
        Ok(ToolCallUiEvent::App(ToolUiData {
            title: call.name.clone(),
            body_lines: compact_app_tool_ui_lines(&call.arguments),
        }))
    }

    async fn execute_tool(
        &mut self,
        call: &AgentToolCall,
        _context: &AppToolExecutionContext,
    ) -> Result<AppToolExecutionResult> {
        let result = self
            .execute_dynamic_tool(&call.name, call.arguments.clone())
            .await?;
        Ok(AppToolExecutionResult {
            summary: result.summary,
            payload: result.payload,
            model_content: result.model_content,
            ui_event: ToolUiEvent::app(call.name.clone(), result.ui_lines),
            turn_boundary_reason: result.turn_boundary_reason,
        })
    }

    async fn execute_dynamic_tool(
        &mut self,
        _name: &str,
        _arguments: Value,
    ) -> Result<AppDynamicToolResult> {
        Err(miette!("unknown app tool"))
    }

    async fn on_focus(&mut self) -> Result<()> {
        Ok(())
    }

    async fn on_blur(&mut self) -> Result<()> {
        Ok(())
    }

    async fn refresh_notice(&mut self) -> Result<()> {
        Ok(())
    }

    fn notice_reason(&self) -> Option<String> {
        None
    }

    async fn shutdown(&mut self) -> Result<()> {
        Ok(())
    }

    async fn wait_until_settled(&self, _: Duration, _: Duration) -> bool {
        true
    }
}

pub struct AppManager {
    focused: Option<AppId>,
    order: Vec<AppId>,
    apps: HashMap<AppId, Box<dyn App>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppInstallDisposition {
    Added,
    Replaced,
}

impl AppManager {
    pub async fn new(focused: Option<AppId>, apps: Vec<Box<dyn App>>) -> Result<Self> {
        let mut order = Vec::with_capacity(apps.len());
        let mut table = HashMap::with_capacity(apps.len());

        for app in apps {
            let id = app.id();
            if table.insert(id.clone(), app).is_some() {
                return Err(miette!("duplicated app id: {id}"));
            }
            order.push(id);
        }

        let mut manager = Self {
            focused: None,
            order,
            apps: table,
        };
        if let Some(id) = focused {
            manager.focus(id).await?;
        }
        Ok(manager)
    }

    pub fn focused(&self) -> Option<AppId> {
        self.focused.clone()
    }

    pub fn state_renders(&self) -> Vec<(AppId, AppStateRender)> {
        self.order
            .iter()
            .filter_map(|id| {
                self.apps
                    .get(id)
                    .map(|app| (id.clone(), app.render_state()))
            })
            .collect()
    }

    pub fn usage(&self, id: &AppId) -> Option<AppUsage> {
        self.apps.get(id).map(|app| app.usage())
    }

    pub fn how_to_use(&self, id: &AppId) -> Option<AppHowToUse> {
        self.apps.get(id).map(|app| app.how_to_use())
    }

    pub fn focused_tool_scopes(&self) -> &'static [AppToolScope] {
        let Some(focused) = self.focused.clone() else {
            return &[];
        };
        self.apps
            .get(&focused)
            .map(|app| app.focused_tool_scopes())
            .unwrap_or(&[])
    }

    pub fn app_ids(&self) -> Vec<AppId> {
        self.order.clone()
    }

    pub fn notice_reason(&self, id: &AppId) -> Option<String> {
        self.apps.get(id).and_then(|app| app.notice_reason())
    }

    pub async fn refresh_all_notices(&mut self) -> Result<()> {
        for id in self.order.clone() {
            if let Some(app) = self.apps.get_mut(&id) {
                app.refresh_notice().await?;
            }
        }
        Ok(())
    }

    pub async fn refresh_notice_for(&mut self, id: &AppId) -> Result<()> {
        if let Some(app) = self.apps.get_mut(id) {
            app.refresh_notice().await?;
        }
        Ok(())
    }

    pub fn all_tool_specs(&self) -> Vec<(AppId, AppToolSpec)> {
        let mut tools = Vec::new();
        for id in &self.order {
            let Some(app) = self.apps.get(id) else {
                continue;
            };
            match app.tool_specs() {
                Ok(app_tools) => {
                    tools.extend(app_tools.into_iter().map(|tool| (id.clone(), tool)));
                }
                Err(err) => {
                    tracing::warn!("failed to list tools for app `{id}`: {err:?}");
                }
            }
        }
        tools
    }

    pub fn summarize_tool_call(&self, call: &AgentToolCall) -> Result<EpisodeActionRecord> {
        let app_id = self.app_id_for_tool_name(&call.name)?;
        let app = self
            .apps
            .get(&app_id)
            .ok_or_else(|| miette!("app missing for tool `{}`: {app_id}", call.name))?;
        app.summarize_tool_call(call)
    }

    pub fn render_tool_call_ui(&self, call: &AgentToolCall) -> Result<ToolCallUiEvent> {
        let app_id = self.app_id_for_tool_name(&call.name)?;
        let app = self
            .apps
            .get(&app_id)
            .ok_or_else(|| miette!("app missing for tool `{}`: {app_id}", call.name))?;
        app.render_tool_call_ui(call)
    }

    pub async fn execute_tool_for_app(
        &mut self,
        app_id: &AppId,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<AppToolExecutionResult> {
        if self.focused.as_ref() != Some(app_id) {
            return Ok(app_tool_unavailable_result(
                app_id,
                self.focused.as_ref(),
                call,
            ));
        }
        let app = self
            .apps
            .get_mut(app_id)
            .ok_or_else(|| miette!("app missing for tool `{}`: {app_id}", call.name))?;
        app.execute_tool(call, context).await
    }

    pub async fn focus(&mut self, id: AppId) -> Result<()> {
        if self.focused.as_ref() == Some(&id) {
            return Ok(());
        }

        if !self.apps.contains_key(&id) {
            return Err(miette!("unknown app: {id}"));
        }

        if let Some(current) = self.focused.clone()
            && let Some(app) = self.apps.get_mut(&current)
        {
            app.on_blur().await?;
        }

        self.focused = Some(id.clone());
        if let Some(app) = self.apps.get_mut(&id) {
            app.on_focus().await?;
        }
        Ok(())
    }

    pub async fn put_away(&mut self) -> Result<()> {
        let Some(current) = self.focused.take() else {
            return Ok(());
        };
        if let Some(app) = self.apps.get_mut(&current) {
            app.on_blur().await?;
        }
        Ok(())
    }

    pub async fn install_or_replace(
        &mut self,
        mut app: Box<dyn App>,
    ) -> Result<AppInstallDisposition> {
        let id = app.id();
        let was_focused = self.focused.as_ref() == Some(&id);
        let disposition = if let Some(mut previous) = self.apps.remove(&id) {
            previous.shutdown().await?;
            AppInstallDisposition::Replaced
        } else {
            self.order.push(id.clone());
            AppInstallDisposition::Added
        };
        if was_focused {
            app.on_focus().await?;
        }
        self.apps.insert(id, app);
        Ok(disposition)
    }

    pub async fn remove(&mut self, id: &AppId) -> Result<bool> {
        let was_focused = self.focused.as_ref() == Some(id);
        if was_focused {
            self.focused = None;
        }
        let Some(mut app) = self.apps.remove(id) else {
            return Ok(false);
        };
        self.order.retain(|existing| existing != id);
        if was_focused {
            app.on_blur().await?;
        }
        app.shutdown().await?;
        Ok(true)
    }

    pub async fn wait_until_settled(&self, silence_duration: Duration, timeout: Duration) -> bool {
        let Some(focused) = self.focused.clone() else {
            return true;
        };
        let Some(app) = self.apps.get(&focused) else {
            return true;
        };
        app.wait_until_settled(silence_duration, timeout).await
    }

    #[cfg(test)]
    fn focused_app_mut(&mut self) -> Result<&mut Box<dyn App>> {
        let Some(focused) = self.focused.clone() else {
            return Err(miette!("no focused app"));
        };
        self.apps
            .get_mut(&focused)
            .ok_or_else(|| miette!("focused app missing: {focused}"))
    }

    fn app_id_for_tool_name(&self, tool_name: &str) -> Result<AppId> {
        for id in &self.order {
            let Some(app) = self.apps.get(id) else {
                continue;
            };
            match app.tool_specs() {
                Ok(app_tools) => {
                    if app_tools.iter().any(|tool| tool.name == tool_name) {
                        return Ok(id.clone());
                    }
                }
                Err(err) => {
                    tracing::warn!("failed to list tools for app `{id}`: {err:?}");
                }
            }
        }
        Err(miette!("unknown app tool `{tool_name}`"))
    }

    #[cfg(test)]
    pub async fn execute_dynamic_tool(
        &mut self,
        name: &str,
        arguments: Value,
    ) -> Result<AppDynamicToolResult> {
        let app = self.focused_app_mut()?;
        app.execute_dynamic_tool(name, arguments).await
    }

    pub async fn shutdown(mut self) -> Result<()> {
        for id in self.order {
            if let Some(app) = self.apps.get_mut(&id) {
                app.shutdown().await?;
            }
        }
        Ok(())
    }
}

fn app_tool_unavailable_result(
    app_id: &AppId,
    focused: Option<&AppId>,
    call: &AgentToolCall,
) -> AppToolExecutionResult {
    let focused_text = focused
        .map(|id| id.to_string())
        .unwrap_or_else(|| "<none>".to_string());
    let reason = format!(
        "`{}` is scoped to app `{app_id}`, but the focused app is `{focused_text}`.",
        call.name
    );
    let allowed_next_action = format!(
        "Call focus_app with app=\"{app_id}\" before using `{}` if the task requires this app.",
        call.name
    );
    let model_content = format!(
        "Tool unavailable: `{}`\nReason: {reason}\nAllowed next action: {allowed_next_action}",
        call.name
    );
    AppToolExecutionResult {
        summary: format!("{} unavailable", call.name),
        payload: json!({
            "available": false,
            "tool": call.name,
            "app": app_id.to_string(),
            "focused_app": focused_text,
            "reason": reason,
            "allowed_next_action": allowed_next_action,
        }),
        model_content: Some(model_content),
        ui_event: ToolUiEvent::error(
            format!("{} unavailable", call.name),
            vec![reason, allowed_next_action],
        ),
        turn_boundary_reason: None,
    }
}
