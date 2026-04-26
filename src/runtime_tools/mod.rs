use std::{collections::HashSet, future::Future, pin::Pin};

use async_trait::async_trait;
use miette::{Result, miette};
use schemars::schema_for;
use serde_json::Value;

use crate::{
    app::{AppToolExecutionContext, AppToolScope},
    context::Context,
    context_budget::truncate_text_to_token_budget_with_notice,
    reasoning::{
        episode::EpisodeActionRecord,
        runtime::{AgentToolCall, AgentToolInputSpec, AgentToolSpec},
    },
    schema_utils::normalize_openai_json_schema,
    tool_ui::{ToolCallUiEvent, ToolUiEvent},
};

mod work;

pub(super) type ToolFuture<'a> =
    Pin<Box<dyn Future<Output = miette::Result<ToolExecutionResult>> + Send + 'a>>;
type ToolExecutor = for<'a> fn(&'a mut Context, &'a AgentToolCall) -> ToolFuture<'a>;
type ToolSummarizer = fn(&AgentToolCall) -> miette::Result<EpisodeActionRecord>;
type ToolCallUiBuilder = fn(&AgentToolCall) -> miette::Result<ToolCallUiEvent>;
type ToolAvailability = fn(&Context) -> bool;

pub(super) fn parse_tool_args<T: for<'de> serde::Deserialize<'de>>(
    call: &AgentToolCall,
) -> miette::Result<T> {
    serde_json::from_value(call.arguments.clone()).map_err(|err| {
        miette!(
            "invalid arguments for tool `{}`: {}; arguments={}",
            call.name,
            err,
            call.arguments
        )
    })
}

pub(super) fn summarize_inline_text(text: &str) -> String {
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

fn freeform_string_fallback_schema(description: &'static str) -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "input": {
                "type": "string",
                "description": description,
            }
        },
        "required": ["input"],
        "additionalProperties": false,
    })
}

fn normalize_tool_input_schema(mut schema: serde_json::Value) -> serde_json::Value {
    schema = normalize_openai_json_schema(schema);
    schema
}

#[derive(Clone, Debug)]
pub struct ToolExecutionResult {
    pub summary: String,
    pub payload: Value,
    pub model_content_override: Option<String>,
    pub ui_event: ToolUiEvent,
    pub turn_boundary_reason: Option<String>,
}

impl ToolExecutionResult {
    pub fn new(summary: impl Into<String>, payload: Value, ui_event: ToolUiEvent) -> Self {
        Self {
            summary: summary.into(),
            payload,
            model_content_override: None,
            ui_event,
            turn_boundary_reason: None,
        }
    }

    pub fn with_model_content(mut self, model_content: impl Into<String>) -> Self {
        self.model_content_override = Some(model_content.into());
        self
    }

    pub fn with_turn_boundary(mut self, reason: impl Into<String>) -> Self {
        self.turn_boundary_reason = Some(reason.into());
        self
    }

    pub fn model_content(&self) -> String {
        if let Some(model_content) = &self.model_content_override {
            return model_content.clone();
        }
        self.default_content_for_payload(&self.payload)
    }

    pub fn history_content(&self, tool_call_id: &str, tool_name: &str) -> String {
        format!(
            "tool_call_id={tool_call_id}\nname={tool_name}\n{}",
            self.default_content_for_payload(&self.payload)
        )
    }

    pub fn history_content_with_budget(
        &self,
        tool_call_id: &str,
        tool_name: &str,
        max_tokens: usize,
    ) -> String {
        truncate_text_to_token_budget_with_notice(
            &self.history_content(tool_call_id, tool_name),
            max_tokens.max(1),
            "... [tool output too long; history content truncated]",
        )
    }

    fn default_content_for_payload(&self, payload: &Value) -> String {
        if payload.is_null() {
            format!("summary={}", self.summary)
        } else {
            format!(
                "summary={}\npayload=\n{}",
                self.summary,
                serde_json::to_string_pretty(payload).unwrap_or_else(|_| payload.to_string())
            )
        }
    }

    fn ensure_model_content_with_budget(mut self, max_tokens: usize) -> Self {
        if self.model_content_override.is_none() {
            self.model_content_override = Some(truncate_text_to_token_budget_with_notice(
                &self.default_content_for_payload(&self.payload),
                max_tokens,
                "... [tool output too long; model content truncated]",
            ));
        }
        self
    }
}

#[async_trait]
pub trait RuntimeTool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_spec(&self) -> AgentToolInputSpec;

    fn is_available(&self, _: &Context) -> bool {
        true
    }

    fn spec(&self) -> AgentToolSpec {
        AgentToolSpec {
            name: self.name().to_string(),
            description: self.description().to_string(),
            input_spec: self.input_spec(),
        }
    }

    fn summarize_action(&self, call: &AgentToolCall) -> miette::Result<EpisodeActionRecord>;
    fn call_ui_event(&self, call: &AgentToolCall) -> miette::Result<ToolCallUiEvent>;
    async fn execute(
        &self,
        context: &mut Context,
        call: &AgentToolCall,
    ) -> miette::Result<ToolExecutionResult>;
}

struct StaticRuntimeTool {
    name: &'static str,
    description: &'static str,
    input_spec: AgentToolInputSpec,
    scope: Option<AppToolScope>,
    availability: Option<ToolAvailability>,
    summarize: ToolSummarizer,
    call_ui: ToolCallUiBuilder,
    execute: ToolExecutor,
}

impl StaticRuntimeTool {
    fn new<T: schemars::JsonSchema>(
        name: &'static str,
        description: &'static str,
        scope: Option<AppToolScope>,
        summarize: ToolSummarizer,
        call_ui: ToolCallUiBuilder,
        execute: ToolExecutor,
    ) -> Self {
        Self {
            name,
            description,
            input_spec: AgentToolInputSpec::JsonSchema {
                schema: normalize_tool_input_schema(serde_json::to_value(schema_for!(T)).unwrap()),
            },
            scope,
            availability: None,
            summarize,
            call_ui,
            execute,
        }
    }
}

#[async_trait]
impl RuntimeTool for StaticRuntimeTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        self.description
    }

    fn input_spec(&self) -> AgentToolInputSpec {
        self.input_spec.clone()
    }

    fn is_available(&self, context: &Context) -> bool {
        let scope_available = match self.scope {
            None => true,
            Some(scope) => context.apps.focused_tool_scopes().contains(&scope),
        };
        scope_available
            && self
                .availability
                .map(|availability| availability(context))
                .unwrap_or(true)
    }

    fn summarize_action(&self, call: &AgentToolCall) -> miette::Result<EpisodeActionRecord> {
        (self.summarize)(call)
    }

    fn call_ui_event(&self, call: &AgentToolCall) -> miette::Result<ToolCallUiEvent> {
        (self.call_ui)(call)
    }

    async fn execute(
        &self,
        context: &mut Context,
        call: &AgentToolCall,
    ) -> miette::Result<ToolExecutionResult> {
        (self.execute)(context, call).await
    }
}

struct ApplyPatchRuntimeTool;

#[async_trait]
impl RuntimeTool for ApplyPatchRuntimeTool {
    fn name(&self) -> &str {
        "apply_patch"
    }

    fn description(&self) -> &str {
        r#"Use `apply_patch` to edit files with unified diff format.

Patch requirements:
- Use standard unified diff file headers: `--- <old_path>` / `+++ <new_path>`
- Every change block must include an `@@ ... @@` hunk header
- Every hunk line must start with a space, `+`, or `-`
- New files use `--- /dev/null` and `+++ <path>`
- Deleted files use `--- <path>` and `+++ /dev/null`

Example:
--- a/src/app.py
+++ b/src/app.py
@@ -1,1 +1,1 @@
-print("Hi")
+print("Hello, world!")

--- /dev/null
+++ b/hello.txt
@@ -0,0 +1 @@
+Hello world

Notes:
- Patches must use paths relative to the workspace
- Rename patches are not currently supported; express them as delete plus add
- Do not output explanation text; output only the complete unified diff"#
    }

    fn input_spec(&self) -> AgentToolInputSpec {
        AgentToolInputSpec::FreeformGrammar {
            syntax: "unified_diff".to_string(),
            definition: r#"file_patch := file_header hunk+
file_header := "--- " old_path LF "+++ " new_path LF
hunk := "@@ " hunk_range " @@" [header] LF hunk_line+
hunk_line := (" " | "+" | "-") text LF
new_file := old_path == "/dev/null"
deleted_file := new_path == "/dev/null""#
                .to_string(),
            fallback_schema: freeform_string_fallback_schema(
                "The entire contents of the unified diff",
            ),
        }
    }

    fn is_available(&self, context: &Context) -> bool {
        context
            .apps
            .focused_tool_scopes()
            .contains(&AppToolScope::Terminal)
    }

    fn summarize_action(&self, call: &AgentToolCall) -> miette::Result<EpisodeActionRecord> {
        work::summarize_apply_patch_tool(call)
    }

    fn call_ui_event(&self, call: &AgentToolCall) -> miette::Result<ToolCallUiEvent> {
        work::render_apply_patch_call_ui(call)
    }

    async fn execute(
        &self,
        context: &mut Context,
        call: &AgentToolCall,
    ) -> miette::Result<ToolExecutionResult> {
        work::execute_apply_patch_runtime_tool(context, call).await
    }
}

struct AppRuntimeTool {
    name: String,
    description: String,
    input_spec: AgentToolInputSpec,
}

#[async_trait]
impl RuntimeTool for AppRuntimeTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_spec(&self) -> AgentToolInputSpec {
        self.input_spec.clone()
    }

    fn summarize_action(&self, _call: &AgentToolCall) -> miette::Result<EpisodeActionRecord> {
        context_free_error()?;
        unreachable!()
    }

    fn call_ui_event(&self, _call: &AgentToolCall) -> miette::Result<ToolCallUiEvent> {
        context_free_error()?;
        unreachable!()
    }

    async fn execute(
        &self,
        context: &mut Context,
        call: &AgentToolCall,
    ) -> miette::Result<ToolExecutionResult> {
        let app_context = AppToolExecutionContext {
            execution_cwd: context.execution_cwd.clone(),
            sandbox_policy: context.sandbox_policy.clone(),
            dashboard_tx: context.dashboard_tx.clone(),
            tool_output_max_tokens: context
                .config
                .main_model_config()
                .tool_output_max_tokens
                .max(1),
        };
        let result = context.apps.execute_tool(call, &app_context).await?;
        let mut output =
            ToolExecutionResult::new(result.summary.clone(), result.payload, result.ui_event);
        if let Some(model_content) = result.model_content {
            output = output.with_model_content(model_content);
        }
        if let Some(reason) = result.turn_boundary_reason {
            output = output.with_turn_boundary(reason);
        }
        Ok(output)
    }
}

fn build_static_runtime_tools() -> Vec<Box<dyn RuntimeTool>> {
    let mut tools: Vec<Box<dyn RuntimeTool>> = vec![Box::new(ApplyPatchRuntimeTool)];
    tools.extend(work::register_tools());
    tools
}

fn build_app_runtime_tools(
    context: &Context,
    reserved_names: &HashSet<String>,
) -> Vec<Box<dyn RuntimeTool>> {
    let mut tools: Vec<Box<dyn RuntimeTool>> = Vec::new();
    let mut seen_names = reserved_names.clone();
    let app_tools = match context.apps.tool_specs() {
        Ok(app_tools) => app_tools,
        Err(err) => {
            tracing::warn!("failed to list focused app tools: {err:?}");
            return tools;
        }
    };
    for tool in app_tools {
        if !is_valid_dynamic_tool_name(&tool.name) {
            tracing::warn!(
                "skipping focused app tool `{}` because its name must match [A-Za-z0-9_-]+",
                tool.name
            );
            continue;
        }
        if !seen_names.insert(tool.name.clone()) {
            tracing::warn!(
                "skipping focused app tool `{}` because its name conflicts with another runtime tool",
                tool.name
            );
            continue;
        }
        tools.push(Box::new(AppRuntimeTool {
            name: tool.name,
            description: tool.description,
            input_spec: AgentToolInputSpec::JsonSchema {
                schema: normalize_tool_input_schema(tool.input_schema),
            },
        }));
    }
    tools
}

pub fn build_runtime_tools(context: &Context) -> Vec<Box<dyn RuntimeTool>> {
    let mut tools = build_static_runtime_tools();
    let reserved_names = tools
        .iter()
        .map(|tool| tool.name().to_string())
        .collect::<HashSet<_>>();
    tools.extend(build_app_runtime_tools(context, &reserved_names));
    tools
}

fn is_valid_dynamic_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
}

fn context_free_error<T>() -> miette::Result<T> {
    Err(miette!(
        "focused app runtime tools require app-owned summarize/call-ui dispatch"
    ))
}

fn find_runtime_tool<'a>(
    tools: &'a [Box<dyn RuntimeTool>],
    name: &str,
) -> miette::Result<&'a dyn RuntimeTool> {
    tools
        .iter()
        .find(|tool| tool.name() == name)
        .map(|tool| tool.as_ref())
        .ok_or_else(|| miette!("unknown runtime tool: {name}"))
}

pub fn build_runtime_tool_specs(context: &Context) -> Vec<AgentToolSpec> {
    let tools = build_runtime_tools(context);
    tools
        .into_iter()
        .filter(|tool| tool.is_available(context))
        .filter(|tool| {
            tool_visible_for_workflow_phase(context.bound_workflow_id.as_deref(), tool.name())
        })
        .map(|tool| tool.spec())
        .collect()
}

fn is_workflow_binding_tool(name: &str) -> bool {
    matches!(name, "activate_workflow" | "create_workflow")
}

fn tool_visible_for_workflow_phase(bound_workflow_id: Option<&str>, tool_name: &str) -> bool {
    if bound_workflow_id.is_none() {
        is_workflow_binding_tool(tool_name)
    } else {
        !is_workflow_binding_tool(tool_name)
    }
}

pub fn summarize_action_from_tool_call(
    context: &Context,
    call: &AgentToolCall,
) -> Result<EpisodeActionRecord> {
    if let Ok(summary) = context.apps.summarize_tool_call(call) {
        return Ok(summary);
    }
    let tools = build_runtime_tools(context);
    find_runtime_tool(&tools, &call.name)?.summarize_action(call)
}

pub fn render_tool_call_ui_event(
    context: &Context,
    call: &AgentToolCall,
) -> Result<ToolCallUiEvent> {
    if let Ok(event) = context.apps.render_tool_call_ui(call) {
        return Ok(event);
    }
    let tools = build_runtime_tools(context);
    find_runtime_tool(&tools, &call.name)?.call_ui_event(call)
}

pub async fn execute_agent_tool_call(
    context: &mut Context,
    call: &AgentToolCall,
) -> Result<ToolExecutionResult> {
    let tools = build_runtime_tools(context);
    let tool = find_runtime_tool(&tools, &call.name)?;
    if !tool.is_available(context) {
        return Err(miette!("tool `{}` is not currently available", call.name));
    }
    let result = tool.execute(context, call).await?;
    Ok(result.ensure_model_content_with_budget(
        context
            .config
            .main_model_config()
            .tool_output_max_tokens
            .max(1),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_selection_phase_exposes_only_binding_tools() {
        assert!(tool_visible_for_workflow_phase(None, "activate_workflow"));
        assert!(tool_visible_for_workflow_phase(None, "create_workflow"));
        assert!(!tool_visible_for_workflow_phase(None, "finish_and_send"));
        assert!(!tool_visible_for_workflow_phase(None, "terminal_exec"));
    }

    #[test]
    fn bound_workflow_phase_hides_binding_tools() {
        assert!(!tool_visible_for_workflow_phase(
            Some("repo-analysis-summary"),
            "activate_workflow"
        ));
        assert!(!tool_visible_for_workflow_phase(
            Some("repo-analysis-summary"),
            "create_workflow"
        ));
        assert!(tool_visible_for_workflow_phase(
            Some("repo-analysis-summary"),
            "finish_and_send"
        ));
        assert!(tool_visible_for_workflow_phase(
            Some("repo-analysis-summary"),
            "terminal_exec"
        ));
    }
}
