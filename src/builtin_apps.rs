//! Independent App instances and execution-time capability boundaries.
use std::collections::HashSet;

use async_trait::async_trait;
use miette::{Result, miette};

#[cfg(not(target_os = "android"))]
use crate::browser_app::BrowserApp;

use crate::{
    actor_app::ActorApp,
    actors::{ActorRecord, ActorSystem, ToolPermissionRequest},
    app::{
        App, AppDocs, AppId, AppManager, AppToolExecutionContext, AppToolExecutionResult,
        AppToolSpec,
    },
    artifacts_app::ArtifactsApp,
    browser_app::ReadOnlyBrowserApp,
    config::Config,
    files_app::FilesApp,
    model_catalog::catalog_model_capacity,
    model_protocol::AgentToolCall,
    openskills::load_openskills_with_policy,
    terminal_app::TerminalApp,
};

pub const DEVICE_APPS: &[&str] = &[
    "actors",
    "files",
    #[cfg(not(target_os = "android"))]
    "terminal",
    "browser",
    "artifacts",
];
pub(crate) const ALL_APPS: &[&str] = &["actors", "files", "terminal", "browser", "artifacts"];

pub(crate) fn history_app(actor: &ActorRecord, system: ActorSystem) -> Box<dyn App> {
    Box::new(RestrictedApp::new(
        Box::new(ActorApp::history_only(actor.id.clone(), system)),
        actor,
    ))
}

pub fn validate_app_names(names: &[String], _read_only: bool) -> Result<()> {
    let mut seen = HashSet::new();
    for name in names {
        if !ALL_APPS.contains(&name.as_str()) {
            return Err(miette!("unknown App `{name}`"));
        }
        if !seen.insert(name) {
            return Err(miette!("duplicated App `{name}`"));
        }
    }
    Ok(())
}

pub fn create_apps(
    actor: &ActorRecord,
    system: ActorSystem,
    config: &Config,
) -> Result<AppManager> {
    validate_app_names(&actor.config.apps, actor.config.permissions.read_only)?;
    let model_key = actor.config.model.as_deref().unwrap_or(&config.main_model);
    let model = config
        .models
        .get(model_key)
        .ok_or_else(|| miette!("Actor model is not configured"))?;
    let supports_vision = model.supports_vision.unwrap_or_else(|| {
        catalog_model_capacity(&model.model_id).is_some_and(|capacity| capacity.supports_vision)
    });
    let state_dir = system.actor_state_dir(&actor.id);
    let mut apps: Vec<Box<dyn App>> = Vec::new();
    for name in &actor.config.apps {
        let inner: Box<dyn App> = match name.as_str() {
            "actors" => Box::new(ActorApp::new(actor.id.clone(), system.clone())),
            "files" => Box::new(FilesApp::new(state_dir.join("images"), supports_vision)),
            "terminal" if !cfg!(target_os = "android") => Box::new(TerminalApp::new()),
            "browser" if cfg!(target_os = "android") || actor.config.permissions.read_only => {
                Box::new(ReadOnlyBrowserApp::new())
            }
            #[cfg(not(target_os = "android"))]
            "browser" => Box::new(BrowserApp::new()),
            "artifacts" => Box::new(ArtifactsApp::new(actor.id.clone(), system.clone())),
            _ => return Err(miette!("unknown App `{name}`")),
        };
        apps.push(Box::new(RestrictedApp::new(inner, actor)));
    }
    // Every Actor can recover its own execution archive. This adds no Actor
    // management tools and preserves explicit tool allowlists and identity checks.
    if !actor.config.apps.iter().any(|app| app == "actors") {
        apps.push(history_app(actor, system));
    }
    AppManager::new(apps)
}

pub fn prompt_resources(
    actor: &ActorRecord,
    context: &AppToolExecutionContext,
) -> Result<Vec<String>> {
    if !crate::actors::permissions::direct_apps(&actor.config)
        .iter()
        .any(|app| app == "files")
    {
        return Ok(Vec::new());
    }
    if let Some(tools) = &actor.config.permissions.tools
        && !tools.iter().any(|tool| tool == "files__read_file")
    {
        return Ok(Vec::new());
    }
    Ok(
        load_openskills_with_policy(&context.execution_cwd, &context.sandbox_policy)
            .render_prompt_block()
            .into_iter()
            .collect(),
    )
}

pub(crate) struct RestrictedApp {
    inner: Box<dyn App>,
    actor_id: String,
    read_only: bool,
    tools: Option<HashSet<String>>,
    app_allowed: bool,
}

impl RestrictedApp {
    pub(crate) fn new(inner: Box<dyn App>, actor: &ActorRecord) -> Self {
        let app_allowed = actor
            .config
            .permissions
            .app_access
            .as_ref()
            .is_none_or(|apps| apps.iter().any(|app| app == inner.id().as_str()));
        Self {
            inner,
            actor_id: actor.id.clone(),
            read_only: actor.config.permissions.read_only,
            app_allowed,
            tools: actor
                .config
                .permissions
                .tools
                .as_ref()
                .map(|tools| tools.iter().cloned().collect()),
        }
    }

    fn allowed(&self, name: &str) -> bool {
        let id = self.inner.id();
        let granted = self
            .tools
            .as_ref()
            .is_none_or(|tools| tools.contains(&id.mangle_tool_name(name)));
        let own_history =
            id.as_str() == "actors" && matches!(name, "read_history" | "read_resource" | "report");
        (self.app_allowed || own_history)
            && granted
            && (!self.read_only || read_only_tool(id.as_str(), name))
    }

    fn check(&self, call: &AgentToolCall, context: &AppToolExecutionContext) -> Result<()> {
        let caller = context
            .actor_call
            .as_ref()
            .ok_or_else(|| miette!("Actor call context is required"))?;
        if caller.actor_id != self.actor_id {
            return Err(miette!("Actor identity mismatch"));
        }
        if !self.allowed(&call.name) {
            return Err(miette!("tool is not granted to this Actor"));
        }
        Ok(())
    }
}

pub(crate) fn read_only_tool(app: &str, tool: &str) -> bool {
    match app {
        "files" => matches!(
            tool,
            "read_file" | "view_image" | "list_skills" | "list_directory" | "search_text"
        ),
        "browser" => tool == "read_url",
        "actors" => matches!(
            tool,
            "devices"
                | "list"
                | "create"
                | "send"
                | "status"
                | "cancel"
                | "deliver"
                | "report"
                | "read_history"
                | "read_resource"
        ),
        "artifacts" => tool == "present",
        _ => false,
    }
}

#[async_trait]
impl App for RestrictedApp {
    fn can_passivate(&self) -> bool {
        self.inner.can_passivate()
    }
    fn delegates_approval(&self, call: &AgentToolCall) -> bool {
        self.inner.delegates_approval(call)
    }
    fn id(&self) -> AppId {
        self.inner.id()
    }
    fn docs(&self) -> AppDocs {
        self.inner.docs()
    }
    fn tool_specs(&self) -> Vec<AppToolSpec> {
        self.inner
            .tool_specs()
            .into_iter()
            .filter(|tool| self.allowed(&tool.name))
            .collect()
    }
    fn before_tool_call(
        &self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<()> {
        self.check(call, context)?;
        self.inner.before_tool_call(call, context)
    }
    fn tool_specs_for_context(&self, context: &AppToolExecutionContext) -> Vec<AppToolSpec> {
        self.inner
            .tool_specs_for_context(context)
            .into_iter()
            .filter(|tool| self.allowed(&tool.name))
            .collect()
    }
    fn permission_request(
        &self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<ToolPermissionRequest> {
        self.check(call, context)?;
        self.inner.permission_request(call, context)
    }
    async fn execute_tool(
        &mut self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<AppToolExecutionResult> {
        self.check(call, context)?;
        self.inner.execute_tool(call, context).await
    }
    async fn shutdown(&mut self) -> Result<()> {
        self.inner.shutdown().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        actors::{ActorConfig, ActorPermissions, ActorStatus},
        app::ActorCallContext,
        sandbox::RuntimeSandboxPolicy,
    };
    use serde_json::json;

    fn actor(read_only: bool, tools: Option<Vec<String>>) -> ActorRecord {
        let mut permissions = ActorPermissions::workspace(std::env::temp_dir(), read_only);
        permissions.tools = tools;
        ActorRecord {
            context: Default::default(),
            id: "a".into(),
            parent_id: Some("root".into()),
            depth: 1,
            config: ActorConfig {
                name: "a".into(),
                workspace: std::env::temp_dir(),
                apps: vec!["files".into()],
                model: None,
                instructions: String::new(),
                permissions,
            },
            status: ActorStatus::Idle,
            created_at_ms: 1,
            updated_at_ms: 1,
        }
    }

    #[tokio::test]
    async fn whitelist_and_read_only_are_execution_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = RestrictedApp::new(
            Box::new(FilesApp::new(dir.path().join("images"), true)),
            &actor(
                true,
                Some(vec!["files__read_file".into(), "files__edit_file".into()]),
            ),
        );
        assert_eq!(
            app.tool_specs()
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["read_file"]
        );
        let context = AppToolExecutionContext {
            execution_cwd: dir.path().into(),
            sandbox_policy: RuntimeSandboxPolicy::disabled(),
            tool_output_max_tokens: 1000,
            turn_epoch: 1,
            actor_call: Some(ActorCallContext {
                actor_id: "a".into(),
                message_id: "m".into(),
                private_roots: Vec::new(),
                device_policy: None,
            }),
        };
        let call = AgentToolCall {
            id: "call".into(),
            name: "edit_file".into(),
            arguments: json!({"edits":[]}),
        };
        assert!(app.execute_tool(&call, &context).await.is_err());
        let call = call.with_name("view_image");
        assert!(app.execute_tool(&call, &context).await.is_err());
    }

    #[test]
    fn unknown_and_duplicate_app_configs_fail() {
        assert!(validate_app_names(&["unknown".into()], false).is_err());
        assert!(validate_app_names(&["files".into(), "files".into()], false).is_err());
    }
}
