use std::{collections::HashMap, fmt::Display, path::Path, path::PathBuf, time::Duration};

use async_trait::async_trait;
use daat_locus_macros::model_schema;
use miette::{Result, miette};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    activity_event::{TextActivityDescriptor, ToolCallActivityEvent},
    dashboard::{DashboardState, SessionActivityEvent},
    reasoning::{episode::EpisodeActionRecord, runtime::AgentToolCall},
    sandbox::RuntimeSandboxPolicy,
};

#[model_schema(transparent)]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct AppId(String);

impl AppId {
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

    pub fn study() -> Self {
        Self("study".to_string())
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
pub struct AppDocs {
    pub lines: Vec<String>,
    pub body_markdown: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AppToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AppToolExecutionResult {
    pub summary: String,
    pub payload: Value,
    pub model_content: Option<String>,
    pub activity_event: Option<SessionActivityEvent>,
    /// Set by app tools whose output the model can obtain again from its
    /// source, so an over-budget rendering points at the source instead of a
    /// redundant temp copy.
    pub overflow_continuation: Option<String>,
}

impl AppToolExecutionResult {
    pub fn from_activity_event(
        summary: impl Into<String>,
        payload: Value,
        model_content: Option<String>,
        activity_event: Option<SessionActivityEvent>,
    ) -> Self {
        Self {
            summary: summary.into(),
            payload,
            model_content,
            activity_event,
            overflow_continuation: None,
        }
    }
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
        RuntimeSandboxPolicy::resolve_path(path, base.or(Some(&self.execution_cwd)))
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

fn compact_app_activity_event_lines(arguments: &Value) -> Vec<String> {
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

    fn docs(&self) -> AppDocs;

    /// Monotonic revision of this app's structured state, when clients need to
    /// detect app-owned data changes during a turn.
    fn state_revision(&self) -> Option<i64> {
        None
    }

    fn tool_specs(&self) -> Vec<AppToolSpec> {
        Vec::new()
    }

    fn summarize_tool_call(&self, call: &AgentToolCall) -> Result<EpisodeActionRecord> {
        Ok(EpisodeActionRecord {
            kind: call.name.clone(),
            summary: summarize_app_inline_text(&call.arguments.to_string()),
        })
    }

    fn tool_call_activity_event(
        &self,
        call: &AgentToolCall,
    ) -> Result<Option<ToolCallActivityEvent>> {
        Ok(Some(ToolCallActivityEvent::App(TextActivityDescriptor {
            title: call.name.clone(),
            body_lines: compact_app_activity_event_lines(&call.arguments),
        })))
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
        Err(miette!("unknown app tool `{}`", call.name))
    }

    fn cached_root_project_instructions(
        &self,
    ) -> Option<&[crate::coding_app::ProjectInstructionDocument]> {
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

impl AppManager {
    pub fn new(apps: Vec<Box<dyn App>>) -> Result<Self> {
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

    pub fn docs(&self, id: &AppId) -> Option<AppDocs> {
        self.apps.get(id).map(|app| app.docs())
    }

    pub fn app_state_revisions(&self) -> Vec<(AppId, i64)> {
        self.order
            .iter()
            .filter_map(|id| {
                let app = self.apps.get(id)?;
                let revision = app.state_revision()?;
                Some((id.clone(), revision))
            })
            .collect()
    }

    pub fn app_ids(&self) -> Vec<AppId> {
        self.order.clone()
    }

    pub fn cached_root_project_instructions(
        &self,
    ) -> &[crate::coding_app::ProjectInstructionDocument] {
        for id in &self.order {
            if let Some(app) = self.apps.get(id)
                && let Some(instructions) = app.cached_root_project_instructions()
            {
                return instructions;
            }
        }
        &[]
    }

    pub fn all_tool_specs(&self) -> Vec<(AppId, Vec<AppToolSpec>)> {
        self.order
            .iter()
            .filter_map(|id| self.apps.get(id).map(|app| (id.clone(), app.tool_specs())))
            .collect()
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
            let app_call = Self::demangle_call_for_app(id, call);
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

    pub fn tool_call_activity_event(
        &self,
        call: &AgentToolCall,
    ) -> Result<Option<ToolCallActivityEvent>> {
        let (app_id, app_tool_name) = self.app_tool_name_from_exposed(&call.name)?;
        let app = self
            .apps
            .get(&app_id)
            .ok_or_else(|| miette!("app missing for tool `{}`: {app_id}", call.name))?;
        let app_call = call.with_name(app_tool_name);
        app.tool_call_activity_event(&app_call)
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
        if !owner
            .tool_specs()
            .iter()
            .any(|tool| tool.name == app_tool_name)
        {
            return Err(miette!("app `{app_id}` does not own tool `{}`", call.name));
        }
        let app = self
            .apps
            .get_mut(app_id)
            .ok_or_else(|| miette!("app missing for tool `{}`: {app_id}", call.name))?;
        app.execute_tool(&app_call, context).await
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
            if app
                .tool_specs()
                .iter()
                .any(|tool| tool.name == app_tool_name)
            {
                return Ok((id.clone(), app_tool_name.to_string()));
            }
        }
        Err(miette!("unknown app tool `{exposed_tool_name}`"))
    }

    fn demangle_call_for_app(app_id: &AppId, call: &AgentToolCall) -> AgentToolCall {
        app_id
            .demangle_tool_name(&call.name)
            .map_or_else(|| call.clone(), |name| call.with_name(name))
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
