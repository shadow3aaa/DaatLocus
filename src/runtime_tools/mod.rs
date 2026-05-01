use std::{collections::HashSet, future::Future, pin::Pin};

use async_trait::async_trait;
use miette::{Result, miette};
use schemars::schema_for;
use serde_json::{Value, json};

use crate::{
    app::{AppId, AppToolExecutionContext, AppToolScope},
    context::Context,
    context_budget::truncate_text_to_token_budget_with_notice,
    live_progress::TelegramLiveStatus,
    reasoning::{
        episode::EpisodeActionRecord,
        runtime::{AgentToolCall, AgentToolInputSpec, AgentToolSpec},
    },
    schema_utils::normalize_openai_json_schema,
    tool_ui::{AppAttentionUiAction, ToolCallUiEvent, ToolUiEvent, glyph},
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
        r#"Use `apply_patch` to edit files with apply_patch envelope format.

Patch requirements:
- The patch must start with `*** Begin Patch` and end with `*** End Patch`
- Each file operation starts with `*** Add File: <path>`, `*** Delete File: <path>`, or `*** Update File: <path>`
- New file content lines must start with `+`
- Update hunks must start with `@@`; hunk lines must start with a space, `+`, or `-`
- Paths may be workspace-relative or absolute paths allowed by the sandbox
- Rename patches are not currently supported; express them as delete plus add

Example:
*** Begin Patch
*** Update File: src/app.py
@@
-print("Hi")
+print("Hello, world!")

*** Add File: hello.txt
+Hello world
*** End Patch

Notes:
- Unified diff input is still accepted for compatibility, but apply_patch envelope format is preferred
- Do not output explanation text; output only the complete patch"#
    }

    fn input_spec(&self) -> AgentToolInputSpec {
        AgentToolInputSpec::FreeformGrammar {
            syntax: "lark".to_string(),
            definition: r#"start: begin_patch file_op+ end_patch
begin_patch: "*** Begin Patch" LF
end_patch: "*** End Patch" LF?
file_op: add_file | delete_file | update_file
add_file: "*** Add File: " filename LF add_line+
delete_file: "*** Delete File: " filename LF
update_file: "*** Update File: " filename LF change
filename: /(.+)/
add_line: "+" /(.*)/ LF
change: (change_context | change_line)+ eof_line?
change_context: ("@@" | "@@ " /(.*)/) LF
change_line: ("+" | "-" | " ") /(.*)/ LF
eof_line: "*** End of File" LF
LF: /\n/"#
                .to_string(),
            fallback_schema: freeform_string_fallback_schema("The entire contents of the patch"),
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
    app_id: AppId,
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
        let result = context
            .apps
            .execute_tool_for_app(&self.app_id, call, &app_context)
            .await?;
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
    for (app_id, tool) in context.apps.all_tool_specs() {
        if !is_valid_dynamic_tool_name(&tool.name) {
            tracing::warn!(
                "skipping app tool `{}` from app `{}` because its name must match [A-Za-z0-9_-]+",
                tool.name,
                app_id
            );
            continue;
        }
        if !seen_names.insert(tool.name.clone()) {
            tracing::warn!(
                "skipping app tool `{}` from app `{}` because its name conflicts with another runtime tool",
                tool.name,
                app_id
            );
            continue;
        }
        tools.push(Box::new(AppRuntimeTool {
            app_id,
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
        "app runtime tools require app-owned summarize/call-ui dispatch"
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
    tools.into_iter().map(|tool| tool.spec()).collect()
}

fn is_workflow_binding_tool(name: &str) -> bool {
    matches!(name, "activate_workflow" | "create_workflow")
}

fn tool_allowed_for_workflow_phase(bound_workflow_id: Option<&str>, tool_name: &str) -> bool {
    if bound_workflow_id.is_none() {
        is_workflow_binding_tool(tool_name)
    } else {
        !is_workflow_binding_tool(tool_name)
    }
}

fn workflow_phase_denial(
    bound_workflow_id: Option<&str>,
    tool_name: &str,
) -> Option<(String, String)> {
    if tool_allowed_for_workflow_phase(bound_workflow_id, tool_name) {
        return None;
    }

    if bound_workflow_id.is_none() {
        return Some((
            "No workflow is bound. Runtime policy requires binding a workflow before using non-workflow tools.".to_string(),
            "Call activate_workflow with the best routed workflow_id, or call create_workflow if no routed workflow fits.".to_string(),
        ));
    }

    Some((
        format!(
            "Workflow `{}` is already bound. Workflow binding tools are only available before a workflow is bound.",
            bound_workflow_id.unwrap_or("<unknown>")
        ),
        "Continue under the currently bound workflow instead of calling activate_workflow/create_workflow again.".to_string(),
    ))
}

fn runtime_availability_denial(
    context: &Context,
    tool: &dyn RuntimeTool,
) -> Option<(String, String)> {
    if tool.is_available(context) {
        return None;
    }

    match tool.name() {
        "apply_patch" => Some((
            "`apply_patch` is scoped to the Terminal app, but Terminal is not the focused app."
                .to_string(),
            "Call focus_app with app=\"Terminal\" before editing files with apply_patch."
                .to_string(),
        )),
        name => Some((
            format!("`{name}` is disabled by the current runtime availability policy."),
            "Use a currently allowed tool, or satisfy the tool's required runtime state before retrying."
                .to_string(),
        )),
    }
}

fn unavailable_tool_result(
    call: &AgentToolCall,
    reason: String,
    allowed_next_action: String,
) -> ToolExecutionResult {
    let model_content = format!(
        "Tool unavailable: `{}`\nReason: {reason}\nAllowed next action: {allowed_next_action}",
        call.name
    );
    ToolExecutionResult::new(
        format!("{} unavailable", call.name),
        json!({
            "available": false,
            "tool": call.name,
            "reason": reason,
            "allowed_next_action": allowed_next_action,
        }),
        ToolUiEvent::error(
            format!("{} unavailable", call.name),
            vec![reason, allowed_next_action],
        ),
    )
    .with_model_content(model_content)
}

pub fn summarize_action_from_tool_call(
    context: &Context,
    call: &AgentToolCall,
) -> Result<EpisodeActionRecord> {
    let tools = build_runtime_tools(context);
    let tool = find_runtime_tool(&tools, &call.name)?;
    match tool.summarize_action(call) {
        Ok(summary) => Ok(summary),
        Err(_) => context.apps.summarize_tool_call(call),
    }
}

pub fn render_tool_call_ui_event(
    context: &Context,
    call: &AgentToolCall,
) -> Result<ToolCallUiEvent> {
    let tools = build_runtime_tools(context);
    let tool = find_runtime_tool(&tools, &call.name)?;
    match tool.call_ui_event(call) {
        Ok(event) => Ok(event),
        Err(_) => context.apps.render_tool_call_ui(call),
    }
}

pub fn render_telegram_tool_result_status(
    call: &AgentToolCall,
    result: &ToolExecutionResult,
) -> Option<TelegramLiveStatus> {
    if telegram_status_ignored_tool(&call.name) {
        return None;
    }
    if matches!(result.ui_event, ToolUiEvent::Error(_)) {
        return telegram_tool_failure_status(&call.name);
    }

    match call.name.as_str() {
        "update_plan" => Some(telegram_status(glyph::PLAN, "Plan Updated")),
        "deep_recall" => match &result.ui_event {
            ToolUiEvent::DeepRecall(event) => Some(telegram_status(
                glyph::MEMORY,
                format!(
                    "Recalled {} {}",
                    event.memory_count,
                    plural_noun(event.memory_count, "Memory", "Memories")
                ),
            )),
            _ => Some(telegram_status(glyph::MEMORY, "Recalled Memories")),
        },
        "apply_patch" => match &result.ui_event {
            ToolUiEvent::Patch(event) => Some(telegram_status(
                glyph::PATCH,
                format!(
                    "Edited {} {}",
                    event.files.len(),
                    plural_noun(event.files.len(), "File", "Files")
                ),
            )),
            _ => Some(telegram_status(glyph::PATCH, "Edited Files")),
        },
        "terminal_exec" => {
            if result
                .payload
                .get("running")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                Some(telegram_status(glyph::EXEC, "Command Running"))
            } else {
                Some(telegram_status(glyph::EXEC, "Command Ran"))
            }
        }
        "terminal_write_stdin" => Some(telegram_status(glyph::EXEC, "Terminal Continued")),
        "terminal_terminate" => Some(telegram_status(glyph::EXEC, "Terminal Stopped")),
        "browser_open_page" => Some(telegram_status(glyph::BROWSER, "Browser Opened")),
        "browser_snapshot" => Some(telegram_status(glyph::BROWSER, "Browser Read")),
        "browser_wait" => Some(telegram_status(glyph::BROWSER, "Browser Waited")),
        "browser_click" | "browser_fill" => Some(telegram_status(glyph::BROWSER, "Browser Acted")),
        "browser_back" | "browser_forward" => {
            Some(telegram_status(glyph::BROWSER, "Browser Navigated"))
        }
        "browser_reload" => Some(telegram_status(glyph::BROWSER, "Browser Reloaded")),
        "browser_close_page" => Some(telegram_status(glyph::BROWSER, "Browser Closed")),
        "create_workflow" => Some(telegram_status(
            glyph::WORKFLOW,
            format!(
                "Workflow Created: {}",
                compact_telegram_status_detail(
                    workflow_id_from_result(&result.ui_event)
                        .or_else(|| call_arg_string(call, "id"))
                        .unwrap_or_else(|| "unknown".to_string()),
                )
            ),
        )),
        "activate_workflow" => Some(telegram_status(
            glyph::WORKFLOW,
            format!(
                "Workflow Active: {}",
                compact_telegram_status_detail(
                    workflow_id_from_result(&result.ui_event)
                        .or_else(|| call_arg_string(call, "workflow_id"))
                        .unwrap_or_else(|| "unknown".to_string()),
                )
            ),
        )),
        "read_workflow" => Some(telegram_status(
            glyph::WORKFLOW,
            format!(
                "Workflow Read: {}",
                compact_telegram_status_detail(
                    call_arg_string(call, "workflow_id").unwrap_or_else(|| "unknown".to_string())
                )
            ),
        )),
        "update_workflow" => Some(telegram_status(
            glyph::WORKFLOW,
            format!(
                "Workflow Updated: {}",
                compact_telegram_status_detail(
                    call_arg_string(call, "workflow_id").unwrap_or_else(|| "unknown".to_string())
                )
            ),
        )),
        "focus_app" => Some(telegram_status(
            glyph::APP_ATTENTION,
            format!(
                "App Focused: {}",
                compact_telegram_status_detail(
                    focused_app_from_result(&result.ui_event)
                        .or_else(|| call_arg_string(call, "app"))
                        .unwrap_or_else(|| "unknown".to_string()),
                )
            ),
        )),
        _ => Some(telegram_status(glyph::EXEC, "App Updated")),
    }
}

fn telegram_status_ignored_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "finish_and_send" | "notice_resolved" | "put_away_app"
    )
}

fn telegram_tool_failure_status(tool_name: &str) -> Option<TelegramLiveStatus> {
    match tool_name {
        "finish_and_send" | "notice_resolved" | "put_away_app" => None,
        "update_plan" => Some(telegram_status(glyph::ERROR, "Plan Update Failed")),
        "deep_recall" => Some(telegram_status(glyph::ERROR, "Memory Recall Failed")),
        "apply_patch" => Some(telegram_status(glyph::ERROR, "File Edit Failed")),
        "terminal_exec" => Some(telegram_status(glyph::ERROR, "Command Failed")),
        "terminal_write_stdin" => Some(telegram_status(glyph::ERROR, "Terminal Write Failed")),
        "terminal_terminate" => Some(telegram_status(glyph::ERROR, "Terminal Stop Failed")),
        "browser_open_page" | "browser_snapshot" | "browser_wait" | "browser_click"
        | "browser_fill" | "browser_back" | "browser_forward" | "browser_reload"
        | "browser_close_page" => Some(telegram_status(glyph::ERROR, "Browser Action Failed")),
        "create_workflow" => Some(telegram_status(glyph::ERROR, "Workflow Creation Failed")),
        "activate_workflow" => Some(telegram_status(glyph::ERROR, "Workflow Activation Failed")),
        "read_workflow" => Some(telegram_status(glyph::ERROR, "Workflow Read Failed")),
        "update_workflow" => Some(telegram_status(glyph::ERROR, "Workflow Update Failed")),
        "focus_app" => Some(telegram_status(glyph::ERROR, "App Focus Failed")),
        _ => Some(telegram_status(glyph::ERROR, "App Failed")),
    }
}

fn telegram_status(icon: impl Into<String>, text: impl Into<String>) -> TelegramLiveStatus {
    TelegramLiveStatus {
        icon: icon.into(),
        text: text.into(),
    }
}

fn call_arg_string(call: &AgentToolCall, name: &str) -> Option<String> {
    call.arguments.get(name).and_then(|value| match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(_) | Value::Bool(_) => Some(value.to_string()),
        _ => None,
    })
}

fn workflow_id_from_result(event: &ToolUiEvent) -> Option<String> {
    match event {
        ToolUiEvent::CreateWorkflow(event) => Some(event.workflow_id.clone()),
        ToolUiEvent::ActivateWorkflow(event) => Some(event.workflow_id.clone()),
        _ => None,
    }
}

fn focused_app_from_result(event: &ToolUiEvent) -> Option<String> {
    match event {
        ToolUiEvent::AppAttention(event)
            if matches!(&event.action, AppAttentionUiAction::Focus) =>
        {
            event.app.clone()
        }
        _ => None,
    }
}

fn compact_telegram_status_detail(detail: String) -> String {
    const MAX_CHARS: usize = 40;

    let compact = detail.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = compact.chars();
    let mut truncated = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        truncated.push_str("...");
    }
    if truncated.is_empty() {
        "unknown".to_string()
    } else {
        truncated
    }
}

fn plural_noun(count: usize, singular: &'static str, plural: &'static str) -> &'static str {
    if count == 1 { singular } else { plural }
}

pub async fn execute_agent_tool_call(
    context: &mut Context,
    call: &AgentToolCall,
) -> Result<ToolExecutionResult> {
    let tools = build_runtime_tools(context);
    let tool = find_runtime_tool(&tools, &call.name)?;
    if let Some((reason, allowed_next_action)) =
        workflow_phase_denial(context.bound_workflow_id.as_deref(), tool.name())
    {
        return Ok(unavailable_tool_result(call, reason, allowed_next_action));
    }
    if let Some((reason, allowed_next_action)) = runtime_availability_denial(context, tool) {
        return Ok(unavailable_tool_result(call, reason, allowed_next_action));
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
    fn apply_patch_tool_uses_lark_envelope_grammar() {
        let spec = ApplyPatchRuntimeTool.spec();

        match spec.input_spec {
            AgentToolInputSpec::FreeformGrammar {
                syntax, definition, ..
            } => {
                assert_eq!(syntax, "lark");
                assert!(definition.contains("start: begin_patch file_op+ end_patch"));
                assert!(definition.contains("*** Begin Patch"));
                assert!(definition.contains("*** Update File: "));
            }
            AgentToolInputSpec::JsonSchema { .. } => panic!("expected freeform grammar"),
        }
    }

    #[test]
    fn workflow_selection_phase_allows_only_binding_tools() {
        assert!(tool_allowed_for_workflow_phase(None, "activate_workflow"));
        assert!(tool_allowed_for_workflow_phase(None, "create_workflow"));
        assert!(!tool_allowed_for_workflow_phase(None, "finish_and_send"));
        assert!(!tool_allowed_for_workflow_phase(None, "terminal_exec"));
    }

    #[test]
    fn bound_workflow_phase_rejects_binding_tools() {
        assert!(!tool_allowed_for_workflow_phase(
            Some("repo-analysis-summary"),
            "activate_workflow"
        ));
        assert!(!tool_allowed_for_workflow_phase(
            Some("repo-analysis-summary"),
            "create_workflow"
        ));
        assert!(tool_allowed_for_workflow_phase(
            Some("repo-analysis-summary"),
            "finish_and_send"
        ));
        assert!(tool_allowed_for_workflow_phase(
            Some("repo-analysis-summary"),
            "terminal_exec"
        ));
    }

    fn tool_result(tool_name: &str, payload: Value, ui_event: ToolUiEvent) -> ToolExecutionResult {
        ToolExecutionResult::new(format!("{tool_name} summary"), payload, ui_event)
    }

    #[test]
    fn telegram_tool_status_renders_plan_update_without_steps() {
        let call = AgentToolCall {
            id: "call_1".to_string(),
            name: "update_plan".to_string(),
            arguments: serde_json::json!({}),
        };
        let result = tool_result(
            "update_plan",
            serde_json::json!({}),
            ToolUiEvent::plan(vec![]),
        );

        let status = render_telegram_tool_result_status(&call, &result).unwrap();

        assert_eq!(status.icon, glyph::PLAN);
        assert_eq!(status.text, "Plan Updated");
    }

    #[test]
    fn telegram_tool_status_renders_deep_recall_count() {
        let call = AgentToolCall {
            id: "call_1".to_string(),
            name: "deep_recall".to_string(),
            arguments: serde_json::json!({}),
        };
        let result = tool_result(
            "deep_recall",
            serde_json::json!({}),
            ToolUiEvent::deep_recall(4),
        );

        let status = render_telegram_tool_result_status(&call, &result).unwrap();

        assert_eq!(status.icon, glyph::MEMORY);
        assert_eq!(status.text, "Recalled 4 Memories");
    }

    #[test]
    fn telegram_tool_status_hides_final_reply_tool() {
        let call = AgentToolCall {
            id: "call_1".to_string(),
            name: "finish_and_send".to_string(),
            arguments: serde_json::json!({
                "disposition": "resolved",
                "reply_message": "done",
            }),
        };
        let result = tool_result(
            "finish_and_send",
            serde_json::json!({}),
            ToolUiEvent::reply(crate::tool_ui::ReplyDisposition::Resolved, Vec::new()),
        );

        assert!(render_telegram_tool_result_status(&call, &result).is_none());
    }

    #[test]
    fn telegram_tool_status_renders_terminal_running_and_finished() {
        let call = AgentToolCall {
            id: "call_1".to_string(),
            name: "terminal_exec".to_string(),
            arguments: serde_json::json!({}),
        };
        let running = tool_result(
            "terminal_exec",
            serde_json::json!({ "running": true }),
            ToolUiEvent::terminal(
                crate::tool_ui::TerminalUiAction::Execute,
                "cargo test",
                Vec::new(),
            ),
        );
        let finished = tool_result(
            "terminal_exec",
            serde_json::json!({ "running": false }),
            ToolUiEvent::terminal(
                crate::tool_ui::TerminalUiAction::Continue,
                "cargo test",
                Vec::new(),
            ),
        );

        assert_eq!(
            render_telegram_tool_result_status(&call, &running)
                .unwrap()
                .text,
            "Command Running"
        );
        assert_eq!(
            render_telegram_tool_result_status(&call, &finished)
                .unwrap()
                .text,
            "Command Ran"
        );
    }

    #[test]
    fn telegram_tool_status_renders_workflow_activation_failure() {
        let call = AgentToolCall {
            id: "call_1".to_string(),
            name: "activate_workflow".to_string(),
            arguments: serde_json::json!({ "workflow_id": "repo-analysis-summary" }),
        };
        let result = tool_result(
            "activate_workflow",
            serde_json::json!({ "error": "unknown workflow" }),
            ToolUiEvent::error("activate_workflow failed", Vec::new()),
        );

        let status = render_telegram_tool_result_status(&call, &result).unwrap();

        assert_eq!(status.icon, glyph::ERROR);
        assert_eq!(status.text, "Workflow Activation Failed");
    }
}
