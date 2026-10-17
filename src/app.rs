use std::{collections::HashMap, fmt::Display, path::Path, path::PathBuf};

use async_trait::async_trait;
use daat_locus_macros::model_schema;
use miette::{Result, miette};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    actors::ToolPermissionRequest,
    model_protocol::{AgentContentPart, AgentToolCall, AgentToolInputSpec, AgentToolSpec},
    sandbox::RuntimeSandboxPolicy,
    schema_utils::{validate_model_facing_schema, validate_value_against_schema},
};

#[model_schema(transparent)]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct AppId(String);

impl AppId {
    pub const TOOL_NAME_SEPARATOR: &str = "__";

    pub fn new(name: impl Into<String>) -> Result<Self> {
        let name = name.into();
        if !Self::is_valid_name(&name) {
            return Err(miette!("invalid App identifier"));
        }
        Ok(Self(name))
    }

    pub fn browser() -> Self {
        Self("browser".to_string())
    }

    pub fn terminal() -> Self {
        Self("terminal".to_string())
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
}

impl Display for AppId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone)]
pub struct AppDocs {
    pub lines: Vec<String>,
    pub body_markdown: Option<String>,
}

impl AppDocs {
    pub fn from_markdown(markdown: &str) -> Self {
        Self {
            lines: markdown.lines().map(str::to_string).collect(),
            body_markdown: Some(markdown.to_string()),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AppToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EmptyAppArgs {}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AppToolExecutionResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    pub summary: String,
    pub payload: Value,
    pub model_content: Option<String>,
    /// Set by app tools whose output the model can obtain again from its
    /// source, so an over-budget rendering points at the source instead of a
    /// redundant temp copy.
    pub overflow_continuation: Option<String>,
    #[serde(default)]
    pub model_images: Vec<AgentContentPart>,
}

impl AppToolExecutionResult {
    pub fn new(summary: impl Into<String>, payload: Value, model_content: Option<String>) -> Self {
        Self {
            failure: None,
            summary: summary.into(),
            payload,
            model_content,
            overflow_continuation: None,
            model_images: Vec::new(),
        }
    }
}

#[derive(Clone)]
pub struct ActorCallContext {
    pub actor_id: String,
    pub message_id: String,
    /// Native processes must never inherit access to service storage trees.
    pub private_roots: Vec<PathBuf>,
    /// Server-created policy for a device-backed shared-core App invocation.
    pub device_policy: Option<crate::actors::ActorConfig>,
}

#[derive(Clone)]
pub struct AppToolExecutionContext {
    pub execution_cwd: PathBuf,
    pub sandbox_policy: RuntimeSandboxPolicy,
    pub tool_output_max_tokens: usize,
    pub turn_epoch: u64,
    /// Request provenance is supplied by the Actor execution mechanism.
    pub actor_call: Option<ActorCallContext>,
}

impl AppToolExecutionContext {
    pub fn resolve_tool_path(&self, path: &Path, base: Option<&Path>) -> PathBuf {
        RuntimeSandboxPolicy::resolve_path(path, base.or(Some(&self.execution_cwd)))
    }
}

#[async_trait]
pub trait App: Send + Sync {
    fn review_exempt(&self, _call: &AgentToolCall) -> bool {
        false
    }
    fn managed_revision(&self) -> Option<u64> {
        None
    }
    async fn prepare_turn(
        &mut self,
        _context: &AppToolExecutionContext,
        _refresh: bool,
    ) -> Result<()> {
        Ok(())
    }
    /// True only when dropping and reconstructing this instance loses no domain
    /// state or live resources. Unknown Apps conservatively keep their worker.
    fn can_passivate(&self) -> bool {
        false
    }
    /// The App owns authorization for this call: origin-bound conversation
    /// delivery or operation review on the target device.
    fn delegates_approval(&self, _call: &AgentToolCall) -> bool {
        false
    }
    fn id(&self) -> AppId;

    fn docs(&self) -> AppDocs;

    fn tool_specs(&self) -> Vec<AppToolSpec> {
        Vec::new()
    }

    fn tool_specs_for_context(&self, _context: &AppToolExecutionContext) -> Vec<AppToolSpec> {
        self.tool_specs()
    }

    fn permission_request(
        &self,
        _call: &AgentToolCall,
        _context: &AppToolExecutionContext,
    ) -> Result<ToolPermissionRequest> {
        Ok(ToolPermissionRequest::default())
    }

    fn before_tool_call(
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

    async fn shutdown(&mut self) -> Result<()> {
        Ok(())
    }
}

pub struct AppManager {
    order: Vec<AppId>,
    apps: HashMap<AppId, Box<dyn App>>,
}

impl AppManager {
    pub fn managed_versions(&self) -> std::collections::BTreeMap<String, u64> {
        self.apps
            .iter()
            .filter_map(|(id, app)| {
                app.managed_revision()
                    .map(|revision| (id.as_str().to_owned(), revision))
            })
            .collect()
    }
    pub fn review_exempt(&self, call: &AgentToolCall) -> Result<bool> {
        let (id, call) = self.dispatch(call)?;
        Ok(self
            .apps
            .get(&id)
            .is_some_and(|app| app.review_exempt(&call)))
    }
    pub async fn prepare_managed(
        &mut self,
        context: &AppToolExecutionContext,
        refresh: bool,
    ) -> Result<()> {
        for app in self.apps.values_mut() {
            if app.managed_revision().is_some() {
                app.prepare_turn(context, refresh).await?;
            }
        }
        Ok(())
    }
    /// Validate additions before replacing anything; unchanged instances keep their resources.
    pub async fn reconcile_managed(&mut self, replacements: Vec<Box<dyn App>>) -> Result<()> {
        let mut candidate = Self::new(replacements)?;
        let remove = self
            .order
            .iter()
            .filter(|id| {
                self.apps
                    .get(*id)
                    .is_some_and(|app| app.managed_revision().is_some())
                    && self.apps.get(*id).and_then(|app| app.managed_revision())
                        != candidate
                            .apps
                            .get(*id)
                            .and_then(|app| app.managed_revision())
            })
            .cloned()
            .collect::<Vec<_>>();
        for id in remove {
            if let Some(mut app) = self.apps.remove(&id) {
                app.shutdown().await?;
            }
            self.order.retain(|current| current != &id);
        }
        for id in candidate.order {
            if !self.apps.contains_key(&id) {
                self.order.push(id.clone());
                self.apps
                    .insert(id.clone(), candidate.apps.remove(&id).unwrap());
            }
        }
        Ok(())
    }
    pub fn can_passivate(&self) -> bool {
        self.apps.values().all(|app| app.can_passivate())
    }
    pub fn delegates_approval(
        &self,
        call: &AgentToolCall,
        _context: &AppToolExecutionContext,
    ) -> Result<bool> {
        let (id, call) = self.dispatch(call)?;
        Ok(self
            .apps
            .get(&id)
            .is_some_and(|app| app.delegates_approval(&call)))
    }
    pub fn new(apps: Vec<Box<dyn App>>) -> Result<Self> {
        let mut order = Vec::with_capacity(apps.len());
        let mut table = HashMap::with_capacity(apps.len());

        for app in apps {
            let id = app.id();
            if !AppId::is_valid_name(id.as_str()) {
                return Err(miette!("invalid App identifier"));
            }
            let mut names = std::collections::HashSet::new();
            for tool in app.tool_specs() {
                if !AppId::is_valid_name(&tool.name) || !names.insert(tool.name.clone()) {
                    return Err(miette!("invalid or duplicated tool name in App `{id}`"));
                }
                validate_model_facing_schema(&tool.input_schema)?;
            }
            if table.insert(id.clone(), app).is_some() {
                return Err(miette!("duplicated app id: {id}"));
            }
            order.push(id);
        }

        Ok(Self { order, apps: table })
    }

    pub fn docs(&self, id: &AppId) -> Option<AppDocs> {
        self.apps.get(id).map(|app| app.docs())
    }

    pub fn app_ids(&self) -> Vec<AppId> {
        self.order.clone()
    }

    pub fn all_tool_specs(&self) -> Vec<(AppId, Vec<AppToolSpec>)> {
        self.order
            .iter()
            .filter_map(|id| self.apps.get(id).map(|app| (id.clone(), app.tool_specs())))
            .collect()
    }

    pub fn model_tool_specs(&self) -> Result<Vec<AgentToolSpec>> {
        self.make_model_tool_specs(self.all_tool_specs())
    }

    pub fn model_tool_specs_for_context(
        &self,
        context: &AppToolExecutionContext,
    ) -> Result<Vec<AgentToolSpec>> {
        self.make_model_tool_specs(
            self.order
                .iter()
                .filter_map(|id| {
                    self.apps
                        .get(id)
                        .map(|app| (id.clone(), app.tool_specs_for_context(context)))
                })
                .collect(),
        )
    }

    fn make_model_tool_specs(
        &self,
        tools_by_app: Vec<(AppId, Vec<AppToolSpec>)>,
    ) -> Result<Vec<AgentToolSpec>> {
        let mut specs = Vec::new();
        for (id, tools) in tools_by_app {
            for tool in tools {
                validate_model_facing_schema(&tool.input_schema)?;
                specs.push(AgentToolSpec {
                    name: id.mangle_tool_name(&tool.name),
                    description: tool.description,
                    input_spec: AgentToolInputSpec::JsonSchema {
                        schema: tool.input_schema,
                    },
                });
            }
        }
        Ok(specs)
    }

    fn dispatch(&self, call: &AgentToolCall) -> Result<(AppId, AgentToolCall)> {
        let (owner, tool) = call
            .name
            .split_once(AppId::TOOL_NAME_SEPARATOR)
            .ok_or_else(|| miette!("tool names must include their App identifier"))?;
        let id = AppId::new(owner)?;
        if !AppId::is_valid_name(tool) || !self.apps.contains_key(&id) {
            return Err(miette!("unknown App tool `{}`", call.name));
        }
        Ok((id, call.with_name(tool)))
    }

    pub async fn execute_tool(
        &mut self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<AppToolExecutionResult> {
        let (id, app_call) = self.validated_dispatch(call, context)?;
        self.apps
            .get_mut(&id)
            .ok_or_else(|| miette!("App is missing"))?
            .execute_tool(&app_call, context)
            .await
    }

    pub fn validate_tool_call(
        &self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<()> {
        self.validated_dispatch(call, context).map(|_| ())
    }

    pub fn validate_tool_call_for_approval(
        &self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<()> {
        self.schema_dispatch(call, context).map(|_| ())
    }

    pub fn permission_request(
        &self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<ToolPermissionRequest> {
        let (id, app_call) = self.schema_dispatch(call, context)?;
        self.apps
            .get(&id)
            .ok_or_else(|| miette!("App is missing"))?
            .permission_request(&app_call, context)
    }

    fn validated_dispatch(
        &self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<(AppId, AgentToolCall)> {
        let (id, app_call) = self.schema_dispatch(call, context)?;
        self.apps
            .get(&id)
            .ok_or_else(|| miette!("App is missing"))?
            .before_tool_call(&app_call, context)?;
        Ok((id, app_call))
    }

    fn schema_dispatch(
        &self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<(AppId, AgentToolCall)> {
        let (id, app_call) = self.dispatch(call)?;
        let owner = self
            .apps
            .get(&id)
            .ok_or_else(|| miette!("App is missing"))?;
        let spec = owner
            .tool_specs_for_context(context)
            .into_iter()
            .find(|tool| tool.name == app_call.name)
            .ok_or_else(|| miette!("App does not own or allow this tool"))?;
        validate_model_facing_schema(&spec.input_schema)?;
        validate_value_against_schema(&call.arguments, &spec.input_schema, &call.name)?;
        Ok((id, app_call))
    }

    pub fn before_tool_call(
        &self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<()> {
        self.validate_tool_call(call, context)
    }

    pub async fn execute_tool_for_app(
        &mut self,
        app_id: &AppId,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<AppToolExecutionResult> {
        app_id
            .demangle_tool_name(&call.name)
            .ok_or_else(|| miette!("tool is not addressed to App `{app_id}`"))?;
        let (_, app_call) = self.validated_dispatch(call, context)?;
        let app = self
            .apps
            .get_mut(app_id)
            .ok_or_else(|| miette!("app missing for tool `{}`: {app_id}", call.name))?;
        app.execute_tool(&app_call, context).await
    }

    pub async fn shutdown(mut self) -> Result<()> {
        let mut first_error = None;
        for id in self.order {
            if let Some(app) = self.apps.get_mut(&id)
                && let Err(error) = app.shutdown().await
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_app_identifiers_are_validated() {
        assert!(AppId::new("actor_files").is_ok());
        for name in ["", "Upper", "files__read", "_files", "files_", "../files"] {
            assert!(AppId::new(name).is_err());
        }
    }

    #[test]
    fn empty_tool_input_schema_is_closed_and_portable() {
        let schema = crate::schema_utils::model_schema_for::<EmptyAppArgs>();
        assert_eq!(schema["properties"], serde_json::json!({}));
        assert_eq!(schema["required"], serde_json::json!([]));
        assert_eq!(schema["additionalProperties"], false);
    }

    struct TestApp;
    #[async_trait]
    impl App for TestApp {
        fn id(&self) -> AppId {
            AppId::new("test").unwrap()
        }
        fn docs(&self) -> AppDocs {
            AppDocs::from_markdown("test")
        }
        fn tool_specs(&self) -> Vec<AppToolSpec> {
            vec![AppToolSpec {
                name: "read".into(),
                description: "test".into(),
                input_schema: crate::schema_utils::model_schema_for::<EmptyAppArgs>(),
            }]
        }
        async fn execute_tool(
            &mut self,
            _: &AgentToolCall,
            _: &AppToolExecutionContext,
        ) -> Result<AppToolExecutionResult> {
            Ok(AppToolExecutionResult::new(
                "read",
                serde_json::json!({}),
                None,
            ))
        }
    }

    #[tokio::test]
    async fn dispatch_requires_owned_name_and_valid_arguments() {
        let mut manager = AppManager::new(vec![Box::new(TestApp)]).unwrap();
        assert!(
            !manager.can_passivate(),
            "An App must opt in to idle reconstruction"
        );
        let context = AppToolExecutionContext {
            execution_cwd: std::env::temp_dir(),
            sandbox_policy: RuntimeSandboxPolicy::disabled(),
            tool_output_max_tokens: 1000,
            turn_epoch: 0,
            actor_call: None,
        };
        let call = AgentToolCall {
            id: "c".into(),
            name: "read".into(),
            arguments: serde_json::json!({}),
        };
        assert!(manager.execute_tool(&call, &context).await.is_err());
        assert!(
            manager
                .execute_tool(&call.with_name("unknown__read"), &context)
                .await
                .is_err()
        );
        assert!(
            manager
                .execute_tool(&call.with_name("test__read"), &context)
                .await
                .is_ok()
        );
        let mut invalid = call.with_name("test__read");
        invalid.arguments = serde_json::json!({"extra":true});
        assert!(manager.execute_tool(&invalid, &context).await.is_err());
    }

    struct CleanupApp {
        name: &'static str,
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        fail: bool,
    }

    #[async_trait]
    impl App for CleanupApp {
        fn id(&self) -> AppId {
            AppId::new(self.name).unwrap()
        }
        fn docs(&self) -> AppDocs {
            AppDocs::from_markdown("cleanup")
        }
        async fn shutdown(&mut self) -> Result<()> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.fail {
                Err(miette!("cleanup failed"))
            } else {
                Ok(())
            }
        }
    }

    #[tokio::test]
    async fn shutdown_attempts_every_app_after_failure() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let manager = AppManager::new(vec![
            Box::new(CleanupApp {
                name: "first",
                calls: calls.clone(),
                fail: true,
            }),
            Box::new(CleanupApp {
                name: "second",
                calls: calls.clone(),
                fail: false,
            }),
        ])
        .unwrap();
        assert!(manager.shutdown().await.is_err());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
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
    }
}
