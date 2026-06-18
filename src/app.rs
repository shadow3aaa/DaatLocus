use std::{collections::HashMap, fmt::Display, path::Path, path::PathBuf, time::Duration};

use async_trait::async_trait;
use daat_locus_macros::model_schema;
use miette::{Result, miette};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    dashboard::DashboardState,
    reasoning::{episode::EpisodeActionRecord, runtime::AgentToolCall},
    sandbox::RuntimeSandboxPolicy,
    tool_ui::{ToolCallUiEvent, ToolUiData, ToolUiEvent},
};

#[model_schema(transparent)]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct AppId(String);

impl AppId {
    pub const DEFAULT_WORKSPACE_ENTRY: &str = "runtime/app.lua";
    pub const TOOL_NAME_SEPARATOR: &str = "__";

    pub fn browser() -> Self {
        Self("browser".to_string())
    }

    pub fn terminal() -> Self {
        Self("terminal".to_string())
    }

    pub fn coding() -> Self {
        Self("coding".to_string())
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
        if !Self::is_valid_name(trimmed) {
            return Err(miette!(
                "workspace app folder name `{trimmed}` must be snake_case: start with a lowercase ASCII letter and use only lowercase letters, numbers, and single `_` separators"
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

    pub fn is_valid_name(name: &str) -> bool {
        let Some(first) = name.chars().next() else {
            return false;
        };
        if !first.is_ascii_lowercase() {
            return false;
        }

        let mut previous_underscore = false;
        for ch in name.chars().skip(1) {
            if ch == '_' {
                if previous_underscore {
                    return false;
                }
                previous_underscore = true;
            } else if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
                previous_underscore = false;
            } else {
                return false;
            }
        }

        !previous_underscore
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn mangle_tool_name(&self, tool_name: &str) -> String {
        format!(
            "{}{separator}{tool_name}",
            self.as_str(),
            separator = Self::TOOL_NAME_SEPARATOR
        )
    }

    pub fn demangle_tool_name<'a>(&self, tool_name: &'a str) -> Option<&'a str> {
        tool_name
            .strip_prefix(self.as_str())?
            .strip_prefix(Self::TOOL_NAME_SEPARATOR)
    }

    pub fn render_exposed_tool_name(tool_name: &str) -> String {
        let Some((app_id, app_tool_name)) = tool_name.split_once(Self::TOOL_NAME_SEPARATOR) else {
            return tool_name.to_string();
        };
        if !Self::is_valid_name(app_id) || app_tool_name.trim().is_empty() {
            return tool_name.to_string();
        }
        format!("{app_id}::{app_tool_name}")
    }
}

impl Display for AppId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AppStateRender {
    pub title: String,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct AppUsage {
    pub description: String,
    pub when_to_use: Vec<String>,
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
}

#[derive(Clone)]
pub struct AppToolExecutionContext {
    pub execution_cwd: PathBuf,
    pub sandbox_policy: RuntimeSandboxPolicy,
    pub dashboard_tx: Option<tokio::sync::watch::Sender<DashboardState>>,
    pub tool_output_max_tokens: usize,
    pub turn_epoch: u64,
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

    fn before_runtime_tool_call(
        &self,
        _call: &AgentToolCall,
        _context: &AppToolExecutionContext,
    ) -> Result<()> {
        Ok(())
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
        })
    }

    async fn execute_dynamic_tool(
        &mut self,
        _name: &str,
        _arguments: Value,
    ) -> Result<AppDynamicToolResult> {
        Err(miette!("unknown app tool"))
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
    order: Vec<AppId>,
    apps: HashMap<AppId, Box<dyn App>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppInstallDisposition {
    Added,
    Replaced,
}

impl AppManager {
    pub async fn new(_initial_app: Option<AppId>, apps: Vec<Box<dyn App>>) -> Result<Self> {
        let mut order = Vec::with_capacity(apps.len());
        let mut table = HashMap::with_capacity(apps.len());

        for app in apps {
            let id = app.id();
            if table.insert(id.clone(), app).is_some() {
                return Err(miette!("duplicated app id: {id}"));
            }
            order.push(id);
        }

        Ok(Self { order, apps: table })
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

    pub fn state_render_for(&self, id: &AppId) -> Option<AppStateRender> {
        self.apps.get(id).map(|app| app.render_state())
    }

    pub fn usage(&self, id: &AppId) -> Option<AppUsage> {
        self.apps.get(id).map(|app| app.usage())
    }

    pub fn how_to_use(&self, id: &AppId) -> Option<AppHowToUse> {
        self.apps.get(id).map(|app| app.how_to_use())
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

    pub fn all_tool_specs(&self) -> Vec<(AppId, Vec<AppToolSpec>)> {
        let mut app_tools = Vec::new();
        for id in &self.order {
            let Some(app) = self.apps.get(id) else {
                continue;
            };
            match app.tool_specs() {
                Ok(tools) => {
                    app_tools.push((id.clone(), tools));
                }
                Err(err) => {
                    tracing::warn!("failed to list tools for app `{id}`: {err:?}");
                }
            }
        }
        app_tools
    }

    pub fn before_runtime_tool_call(
        &self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<()> {
        for id in &self.order {
            let Some(app) = self.apps.get(id) else {
                continue;
            };
            let app_call = self.demangle_call_for_app(id, call);
            app.before_runtime_tool_call(&app_call, context)?;
        }
        Ok(())
    }

    pub fn summarize_tool_call(&self, call: &AgentToolCall) -> Result<EpisodeActionRecord> {
        let (app_id, app_tool_name) = self.app_tool_name_from_exposed(&call.name)?;
        let app = self
            .apps
            .get(&app_id)
            .ok_or_else(|| miette!("app missing for tool `{}`: {app_id}", call.name))?;
        let app_call = call.with_name(app_tool_name);
        app.summarize_tool_call(&app_call)
    }

    pub fn render_tool_call_ui(&self, call: &AgentToolCall) -> Result<ToolCallUiEvent> {
        let (app_id, app_tool_name) = self.app_tool_name_from_exposed(&call.name)?;
        let app = self
            .apps
            .get(&app_id)
            .ok_or_else(|| miette!("app missing for tool `{}`: {app_id}", call.name))?;
        let app_call = call.with_name(app_tool_name);
        app.render_tool_call_ui(&app_call)
    }

    pub async fn execute_tool_for_app(
        &mut self,
        app_id: &AppId,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<AppToolExecutionResult> {
        let app_tool_name = app_id
            .demangle_tool_name(&call.name)
            .unwrap_or(&call.name)
            .to_string();
        let app_call = call.with_name(app_tool_name.clone());
        let owner = self
            .apps
            .get(app_id)
            .ok_or_else(|| miette!("app missing for tool `{}`: {app_id}", call.name))?;
        let tool_spec = owner
            .tool_specs()?
            .into_iter()
            .find(|tool| tool.name == app_tool_name)
            .ok_or_else(|| miette!("app `{app_id}` does not own tool `{}`", call.name))?;
        let _ = tool_spec;
        let app = self
            .apps
            .get_mut(app_id)
            .ok_or_else(|| miette!("app missing for tool `{}`: {app_id}", call.name))?;
        app.execute_tool(&app_call, context).await
    }

    pub async fn install_or_replace(&mut self, app: Box<dyn App>) -> Result<AppInstallDisposition> {
        let id = app.id();
        let disposition = if let Some(mut previous) = self.apps.remove(&id) {
            previous.shutdown().await?;
            AppInstallDisposition::Replaced
        } else {
            self.order.push(id.clone());
            AppInstallDisposition::Added
        };
        self.apps.insert(id, app);
        Ok(disposition)
    }

    pub async fn remove(&mut self, id: &AppId) -> Result<bool> {
        let Some(mut app) = self.apps.remove(id) else {
            return Ok(false);
        };
        self.order.retain(|existing| existing != id);
        app.shutdown().await?;
        Ok(true)
    }

    pub async fn wait_until_settled(&self, silence_duration: Duration, timeout: Duration) -> bool {
        for id in &self.order {
            let Some(app) = self.apps.get(id) else {
                continue;
            };
            if !app.wait_until_settled(silence_duration, timeout).await {
                return false;
            }
        }
        true
    }

    fn app_tool_name_from_exposed(&self, exposed_tool_name: &str) -> Result<(AppId, String)> {
        for id in &self.order {
            let Some(app) = self.apps.get(id) else {
                continue;
            };
            let Some(app_tool_name) = id.demangle_tool_name(exposed_tool_name) else {
                continue;
            };
            match app.tool_specs() {
                Ok(app_tools) => {
                    if app_tools.iter().any(|tool| tool.name == app_tool_name) {
                        return Ok((id.clone(), app_tool_name.to_string()));
                    }
                }
                Err(err) => {
                    tracing::warn!("failed to list tools for app `{id}`: {err:?}");
                }
            }
        }
        Err(miette!("unknown app tool `{exposed_tool_name}`"))
    }

    fn demangle_call_for_app(&self, app_id: &AppId, call: &AgentToolCall) -> AgentToolCall {
        app_id
            .demangle_tool_name(&call.name)
            .map(|name| call.with_name(name))
            .unwrap_or_else(|| call.clone())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_app_id_rejects_separator_and_non_ascii_names() {
        assert!(AppId::from_workspace_folder("notes").is_ok());
        assert!(AppId::from_workspace_folder("my_app").is_ok());
        assert!(AppId::from_workspace_folder("my app").is_err());
        assert!(AppId::from_workspace_folder("应用").is_err());
        assert!(AppId::from_workspace_folder("MyApp").is_err());
        assert!(AppId::from_workspace_folder("my-app").is_err());
        assert!(AppId::from_workspace_folder("my__app").is_err());
        assert!(AppId::from_workspace_folder("my_app_").is_err());
        assert!(AppId::from_workspace_folder("2app").is_err());
    }

    #[test]
    fn app_tool_names_use_openai_safe_separator() {
        let exposed = AppId::terminal().mangle_tool_name("terminal_exec");

        assert_eq!(exposed, "terminal__terminal_exec");
        assert!(
            exposed
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
        );
        assert_eq!(
            AppId::terminal().demangle_tool_name(&exposed),
            Some("terminal_exec")
        );
        assert_eq!(
            AppId::render_exposed_tool_name("terminal__terminal_exec"),
            "terminal::terminal_exec"
        );
        assert_eq!(
            AppId::render_exposed_tool_name("terminal_exec"),
            "terminal_exec"
        );
    }
}
