use std::{collections::HashSet, future::Future, pin::Pin};

use async_trait::async_trait;
use daat_locus_macros::model_schema;
use miette::{Result, miette};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    app::{AppHowToUse, AppId, AppStateRender, AppToolExecutionContext, AppUsage},
    context::Context,
    context_budget::truncate_text_to_token_budget_with_notice,
    live_progress::TelegramLiveStatus,
    reasoning::{
        episode::EpisodeActionRecord,
        runtime::{AgentToolCall, AgentToolInputSpec, AgentToolSpec},
    },
    schema_utils::{model_schema, model_schema_for, validate_model_facing_schema},
    tool_ui::{ToolCallUiEvent, ToolUiEvent, glyph},
};

mod files;
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

    fn app_tool_name(&self) -> Option<&str> {
        None
    }

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
    availability: Option<ToolAvailability>,
    summarize: ToolSummarizer,
    call_ui: ToolCallUiBuilder,
    execute: ToolExecutor,
}

impl StaticRuntimeTool {
    fn new_with_schema(
        name: &'static str,
        description: &'static str,
        schema: serde_json::Value,
        summarize: ToolSummarizer,
        call_ui: ToolCallUiBuilder,
        execute: ToolExecutor,
    ) -> Self {
        Self {
            name,
            description,
            input_spec: AgentToolInputSpec::JsonSchema {
                schema: model_schema(schema),
            },
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
        self.availability
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

struct AppRuntimeTool {
    owner_app_id: AppId,
    exposed_name: String,
    app_tool_name: String,
    description: String,
    input_spec: AgentToolInputSpec,
}

const APP_GET_STATE_TOOL_NAME: &str = "get_state";

#[model_schema]
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum AppStateDetail {
    #[default]
    Summary,
    Full,
}

#[model_schema]
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
struct AppGetStateArgs {
    /// State detail to return. Use full when exact visible facts are needed.
    detail: Option<AppStateDetail>,
    /// Include the app usage and operation manual alongside current state.
    include_manual: Option<bool>,
}

struct AppGetStateRuntimeTool {
    owner_app_id: AppId,
    exposed_name: String,
    input_spec: AgentToolInputSpec,
}

impl AppGetStateRuntimeTool {
    fn new(owner_app_id: AppId, exposed_name: String) -> Self {
        Self {
            owner_app_id,
            exposed_name,
            input_spec: AgentToolInputSpec::JsonSchema {
                schema: model_schema_for::<AppGetStateArgs>(),
            },
        }
    }
}

fn app_state_payload(
    app_id: &AppId,
    state: &AppStateRender,
    detail: AppStateDetail,
    usage: Option<&AppUsage>,
    how_to_use: Option<&AppHowToUse>,
) -> Value {
    let mut payload = json!({
        "app": app_id.to_string(),
        "detail": match detail {
            AppStateDetail::Summary => "summary",
            AppStateDetail::Full => "full",
        },
        "state": state,
    });

    if let Value::Object(map) = &mut payload {
        if let Some(usage) = usage {
            map.insert(
                "usage".to_string(),
                json!({
                    "description": &usage.description,
                    "when_to_use": &usage.when_to_use,
                    "body_markdown": &usage.body_markdown,
                }),
            );
        }
        if let Some(how_to_use) = how_to_use {
            map.insert(
                "how_to_use".to_string(),
                json!({
                    "lines": &how_to_use.lines,
                    "body_markdown": &how_to_use.body_markdown,
                }),
            );
        }
    }
    payload
}

fn render_app_get_state_model_content(
    app_id: &AppId,
    state: &AppStateRender,
    usage: Option<&AppUsage>,
    how_to_use: Option<&AppHowToUse>,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("app={app_id}\n"));
    out.push_str(&format!("state_title={}\n", state.title));
    out.push_str("state:\n");
    if state.lines.is_empty() {
        out.push_str("- no visible state\n");
    } else {
        for line in &state.lines {
            out.push_str("- ");
            out.push_str(line);
            out.push('\n');
        }
    }

    if let Some(usage) = usage {
        out.push_str("usage:\n");
        out.push_str("- description: ");
        out.push_str(&usage.description);
        out.push('\n');
        for item in &usage.when_to_use {
            out.push_str("- when_to_use: ");
            out.push_str(item);
            out.push('\n');
        }
        if let Some(body) = usage.body_markdown.as_deref()
            && !body.trim().is_empty()
        {
            out.push_str("- notes:\n");
            out.push_str(body.trim());
            out.push('\n');
        }
    }

    if let Some(how_to_use) = how_to_use {
        out.push_str("how_to_use:\n");
        for line in &how_to_use.lines {
            out.push_str("- ");
            out.push_str(line);
            out.push('\n');
        }
        if let Some(body) = how_to_use.body_markdown.as_deref()
            && !body.trim().is_empty()
        {
            out.push_str(body.trim());
            out.push('\n');
        }
    }

    out.trim_end().to_string()
}

#[async_trait]
impl RuntimeTool for AppGetStateRuntimeTool {
    fn name(&self) -> &str {
        &self.exposed_name
    }

    fn app_tool_name(&self) -> Option<&str> {
        Some(APP_GET_STATE_TOOL_NAME)
    }

    fn description(&self) -> &str {
        "Read the current state for this app capability domain. Set include_manual=true when you also need its usage and operation notes."
    }

    fn input_spec(&self) -> AgentToolInputSpec {
        self.input_spec.clone()
    }

    fn summarize_action(&self, call: &AgentToolCall) -> miette::Result<EpisodeActionRecord> {
        let args: AppGetStateArgs = parse_tool_args(call)?;
        Ok(EpisodeActionRecord {
            kind: self.exposed_name.clone(),
            summary: format!(
                "detail={} include_manual={}",
                match args.detail.unwrap_or_default() {
                    AppStateDetail::Summary => "summary",
                    AppStateDetail::Full => "full",
                },
                args.include_manual.unwrap_or(false)
            ),
        })
    }

    fn call_ui_event(&self, call: &AgentToolCall) -> miette::Result<ToolCallUiEvent> {
        let args: AppGetStateArgs = parse_tool_args(call)?;
        let mut lines = vec![format!(
            "detail={}",
            match args.detail.unwrap_or_default() {
                AppStateDetail::Summary => "summary",
                AppStateDetail::Full => "full",
            }
        )];
        if args.include_manual.unwrap_or(false) {
            lines.push("include_manual=true".to_string());
        }
        Ok(ToolCallUiEvent::app(
            AppId::render_exposed_tool_name(&self.exposed_name),
            lines,
        ))
    }

    async fn execute(
        &self,
        context: &mut Context,
        call: &AgentToolCall,
    ) -> miette::Result<ToolExecutionResult> {
        let args: AppGetStateArgs = parse_tool_args(call)?;
        let state = context
            .apps
            .state_render_for(&self.owner_app_id)
            .ok_or_else(|| miette!("app missing for state read: {}", self.owner_app_id))?;
        let include_manual = args.include_manual.unwrap_or(false);
        let usage = include_manual
            .then(|| context.apps.usage(&self.owner_app_id))
            .flatten();
        let how_to_use = include_manual
            .then(|| context.apps.how_to_use(&self.owner_app_id))
            .flatten();
        let payload = app_state_payload(
            &self.owner_app_id,
            &state,
            args.detail.unwrap_or_default(),
            usage.as_ref(),
            how_to_use.as_ref(),
        );
        let model_content = render_app_get_state_model_content(
            &self.owner_app_id,
            &state,
            usage.as_ref(),
            how_to_use.as_ref(),
        );
        Ok(ToolExecutionResult::new(
            format!("read {} state", self.owner_app_id),
            payload,
            ToolUiEvent::app(
                AppId::render_exposed_tool_name(&self.exposed_name),
                state.lines.clone(),
            ),
        )
        .with_model_content(model_content))
    }
}

#[async_trait]
impl RuntimeTool for AppRuntimeTool {
    fn name(&self) -> &str {
        &self.exposed_name
    }

    fn app_tool_name(&self) -> Option<&str> {
        Some(&self.app_tool_name)
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_spec(&self) -> AgentToolInputSpec {
        self.input_spec.clone()
    }

    fn is_available(&self, _context: &Context) -> bool {
        true
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
            turn_epoch: context.runtime_turn_epoch,
        };
        let app_call = call.with_name(self.app_tool_name.clone());
        let result = context
            .apps
            .execute_tool_for_app(&self.owner_app_id, &app_call, &app_context)
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
    let mut tools: Vec<Box<dyn RuntimeTool>> = Vec::new();
    tools.extend(files::register_tools());
    tools.extend(work::register_tools());
    tools
}

fn build_app_runtime_tools(
    context: &Context,
    reserved_names: &HashSet<String>,
) -> Vec<Box<dyn RuntimeTool>> {
    let mut tools: Vec<Box<dyn RuntimeTool>> = Vec::new();
    let mut seen_names = reserved_names.clone();

    for (owner_app_id, app_tools) in context.apps.all_tool_specs() {
        let get_state_exposed_name = owner_app_id.mangle_tool_name(APP_GET_STATE_TOOL_NAME);
        if seen_names.insert(get_state_exposed_name.clone()) {
            tools.push(Box::new(AppGetStateRuntimeTool::new(
                owner_app_id.clone(),
                get_state_exposed_name,
            )));
        } else {
            tracing::warn!(
                "skipping generated state tool for app `{}` because exposed name `{}` conflicts with another runtime tool",
                owner_app_id,
                get_state_exposed_name
            );
        }

        for tool in &app_tools {
            if !is_valid_dynamic_tool_name(&tool.name) {
                tracing::warn!(
                    "skipping app tool `{}` from app `{}` because its name must match [A-Za-z0-9_-]+",
                    tool.name,
                    owner_app_id
                );
                continue;
            }
            let exposed_name = owner_app_id.mangle_tool_name(&tool.name);
            if !seen_names.insert(exposed_name.clone()) {
                tracing::warn!(
                    "skipping app tool `{}` from app `{}` because exposed name `{}` conflicts with another runtime tool",
                    tool.name,
                    owner_app_id,
                    exposed_name
                );
                continue;
            }
            if let Err(err) = validate_model_facing_schema(&tool.input_schema) {
                tracing::warn!(
                    "skipping app tool `{}` from app `{}` because its input schema is invalid: {}",
                    tool.name,
                    owner_app_id,
                    err
                );
                continue;
            }
            tools.push(Box::new(AppRuntimeTool {
                owner_app_id: owner_app_id.clone(),
                exposed_name,
                app_tool_name: tool.name.clone(),
                description: tool.description.clone(),
                input_spec: AgentToolInputSpec::JsonSchema {
                    schema: tool.input_schema.clone(),
                },
            }));
        }
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
    let canonical_name = canonical_runtime_tool_name(name);
    tools
        .iter()
        .find(|tool| tool.name() == canonical_name)
        .map(|tool| tool.as_ref())
        .ok_or_else(|| miette!("unknown runtime tool: {name}"))
}

pub fn build_runtime_tool_specs(context: &Context) -> Vec<AgentToolSpec> {
    let tools = build_runtime_tools(context);
    tools.into_iter().map(|tool| tool.spec()).collect()
}

fn runtime_availability_denial(
    context: &Context,
    tool: &dyn RuntimeTool,
) -> Option<(String, String)> {
    if tool.is_available(context) {
        return None;
    }

    let name = tool.name();
    Some((
        format!("`{name}` is disabled by the current runtime availability policy."),
        "Use a currently allowed tool, or satisfy the tool's required runtime state before retrying."
            .to_string(),
    ))
}

fn unavailable_tool_result(
    call: &AgentToolCall,
    reason: String,
    allowed_next_action: String,
) -> ToolExecutionResult {
    let display_tool_name = AppId::render_exposed_tool_name(&call.name);
    let model_content = format!(
        "Tool unavailable: `{}`\nReason: {reason}\nAllowed next action: {allowed_next_action}",
        display_tool_name
    );
    ToolExecutionResult::new(
        format!("{display_tool_name} unavailable"),
        json!({
            "available": false,
            "tool": call.name,
            "reason": reason,
            "allowed_next_action": allowed_next_action,
        }),
        ToolUiEvent::error(
            format!("{display_tool_name} unavailable"),
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
    let tool_call = tool_call_for_runtime_tool(tool, call);
    match tool.summarize_action(&tool_call) {
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
    let tool_call = tool_call_for_runtime_tool(tool, call);
    match tool.call_ui_event(&tool_call) {
        Ok(event) => Ok(event),
        Err(_) => context.apps.render_tool_call_ui(call),
    }
}

fn tool_call_for_runtime_tool(tool: &dyn RuntimeTool, call: &AgentToolCall) -> AgentToolCall {
    tool.app_tool_name()
        .map(|app_tool_name| call.with_name(app_tool_name))
        .unwrap_or_else(|| call.clone())
}

pub fn render_telegram_tool_result_status(
    call: &AgentToolCall,
    result: &ToolExecutionResult,
) -> Option<TelegramLiveStatus> {
    let tool_name = demangle_known_app_tool_name(&call.name);
    if telegram_status_ignored_tool(tool_name) {
        return None;
    }
    if matches!(result.ui_event, ToolUiEvent::Error(_)) {
        return telegram_tool_failure_status(tool_name);
    }

    match tool_name {
        "update_plan" => Some(telegram_status(glyph::PLAN, "Plan Updated")),
        "edit_file" => match &result.ui_event {
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
        "create_primitive_spec" => Some(telegram_status(
            glyph::WORKFLOW,
            format!(
                "Primitive Spec Created: {}",
                compact_telegram_status_detail(
                    workflow_id_from_result(&result.ui_event)
                        .or_else(|| call_arg_string(call, "id"))
                        .unwrap_or_else(|| "unknown".to_string()),
                )
            ),
        )),
        "activate_composed_primitive" => Some(telegram_status(
            glyph::WORKFLOW,
            format!(
                "Primitive Active: {}",
                compact_telegram_status_detail(
                    workflow_id_from_result(&result.ui_event)
                        .or_else(|| call_arg_string(call, "primitive_id"))
                        .or_else(|| call_arg_string(call, "workflow_id"))
                        .or_else(|| call_arg_string(call, "composition"))
                        .unwrap_or_else(|| "unknown".to_string()),
                )
            ),
        )),
        "read_primitive_spec" => Some(telegram_status(
            glyph::WORKFLOW,
            format!(
                "Primitive Spec Read: {}",
                compact_telegram_status_detail(
                    call_arg_string(call, "primitive_id")
                        .or_else(|| call_arg_string(call, "workflow_id"))
                        .unwrap_or_else(|| "unknown".to_string())
                )
            ),
        )),
        "update_primitive_spec" => Some(telegram_status(
            glyph::WORKFLOW,
            format!(
                "Primitive Spec Updated: {}",
                compact_telegram_status_detail(
                    call_arg_string(call, "primitive_id")
                        .or_else(|| call_arg_string(call, "workflow_id"))
                        .unwrap_or_else(|| "unknown".to_string())
                )
            ),
        )),
        _ => Some(telegram_status(glyph::EXEC, "App Updated")),
    }
}

fn demangle_known_app_tool_name(tool_name: &str) -> &str {
    if let Some((app_id, app_tool_name)) = tool_name.split_once(AppId::TOOL_NAME_SEPARATOR)
        && AppId::is_valid_name(app_id)
    {
        return canonical_runtime_tool_name(app_tool_name);
    }
    canonical_runtime_tool_name(tool_name)
}

fn canonical_runtime_tool_name(tool_name: &str) -> &str {
    match tool_name {
        "create_workflow" => "create_primitive_spec",
        "activate_workflow" | "activate_primitive" | "compose_workflows" | "compose_primitives" => {
            "activate_composed_primitive"
        }
        "read_workflow" => "read_primitive_spec",
        "update_workflow" => "update_primitive_spec",
        _ => tool_name,
    }
}

fn telegram_status_ignored_tool(tool_name: &str) -> bool {
    matches!(tool_name, "finish_and_send" | "notice_resolved")
}

fn telegram_tool_failure_status(tool_name: &str) -> Option<TelegramLiveStatus> {
    match tool_name {
        "finish_and_send" | "notice_resolved" => None,
        "update_plan" => Some(telegram_status(glyph::ERROR, "Plan Update Failed")),
        "edit_file" => Some(telegram_status(glyph::ERROR, "File Edit Failed")),
        "terminal_exec" => Some(telegram_status(glyph::ERROR, "Command Failed")),
        "terminal_write_stdin" => Some(telegram_status(glyph::ERROR, "Terminal Write Failed")),
        "terminal_terminate" => Some(telegram_status(glyph::ERROR, "Terminal Stop Failed")),
        "browser_open_page" | "browser_snapshot" | "browser_wait" | "browser_click"
        | "browser_fill" | "browser_back" | "browser_forward" | "browser_reload"
        | "browser_close_page" => Some(telegram_status(glyph::ERROR, "Browser Action Failed")),
        "create_primitive_spec" => Some(telegram_status(
            glyph::ERROR,
            "Primitive Spec Creation Failed",
        )),
        "activate_composed_primitive" => {
            Some(telegram_status(glyph::ERROR, "Primitive Activation Failed"))
        }
        "read_primitive_spec" => Some(telegram_status(glyph::ERROR, "Primitive Spec Read Failed")),
        "update_primitive_spec" => Some(telegram_status(
            glyph::ERROR,
            "Primitive Spec Update Failed",
        )),
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
        ToolUiEvent::CreatePrimitiveSpec(event) => Some(event.primitive_id.clone()),
        ToolUiEvent::ActivatePrimitive(event) => Some(event.primitive_id.clone()),
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
    if let Some((reason, allowed_next_action)) = runtime_availability_denial(context, tool) {
        return Ok(unavailable_tool_result(call, reason, allowed_next_action));
    }
    let app_context = AppToolExecutionContext {
        execution_cwd: context.execution_cwd.clone(),
        sandbox_policy: context.sandbox_policy.clone(),
        dashboard_tx: context.dashboard_tx.clone(),
        tool_output_max_tokens: context
            .config
            .main_model_config()
            .tool_output_max_tokens
            .max(1),
        turn_epoch: context.runtime_turn_epoch,
    };
    context.apps.before_runtime_tool_call(call, &app_context)?;
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
    use std::collections::HashMap;

    use async_trait::async_trait;
    use tempfile::TempDir;

    use crate::{
        app::{App, AppManager},
        browser_app::BrowserApp,
        coding_app::CodingApp,
        config::Config,
        context::Context,
        context_budget::TokenEstimateBaseline,
        core::Llm,
        events::EventStore,
        memory::Memory,
        openskills::OpenSkillsCatalog,
        pending_work::PendingWorkQueue,
        plan::Plan,
        reasoning::{
            compiled::CompiledPromptStore,
            runtime::{AgentTurnRequest, AgentTurnStreamResult, PromptRequest},
        },
        runtime::bootstrap::DaatLocusHomeOverride,
        sandbox::RuntimeSandboxPolicy,
        telegram_acl::TelegramAclHandle,
        telegram_transport::state::TelegramTransportState,
        terminal_app::TerminalApp,
        workflow::PrimitiveStore,
        workspace_app::WorkspaceAppRegistry,
    };

    struct UnusedLlm;

    #[async_trait]
    impl Llm for UnusedLlm {
        async fn run_json(
            &self,
            _context: &Context,
            _request: PromptRequest,
        ) -> Result<serde_json::Value> {
            Err(miette!("unused test llm"))
        }

        async fn run_agent_turn(
            &self,
            _context: &Context,
            _request: AgentTurnRequest,
        ) -> Result<AgentTurnStreamResult> {
            Err(miette!("unused test llm"))
        }
    }

    struct IsolatedTestContext {
        context: Context,
        _home_override: DaatLocusHomeOverride,
        _home: TempDir,
        _execution: TempDir,
    }

    impl IsolatedTestContext {
        async fn new() -> Self {
            let home = tempfile::tempdir().expect("test home");
            let execution = tempfile::tempdir().expect("test execution cwd");
            let home_override = DaatLocusHomeOverride::set(home.path().to_path_buf()).await;
            let config = Config::default();
            let telegram = TelegramTransportState::new();
            let (daemon_control_tx, _daemon_control_rx) = tokio::sync::mpsc::unbounded_channel();
            let apps: Vec<Box<dyn App>> = vec![
                Box::new(BrowserApp::new()),
                Box::new(TerminalApp::new()),
                Box::new(CodingApp::new()),
            ];
            let apps = AppManager::new(None, apps).await.unwrap();
            let context = Context {
                session_id: None,
                llm: Box::new(UnusedLlm),
                judge_llm: Box::new(UnusedLlm),
                efficient_llm: Box::new(UnusedLlm),
                config,
                memory: Memory::new().await,
                plan: Plan::new().await,
                events: EventStore::new().await,
                pending_work: PendingWorkQueue::new().await,
                workflows: PrimitiveStore::new().await,
                openskills: OpenSkillsCatalog::default(),
                bound_primitive_composition: None,
                bound_primitive_id: None,
                active_primitive_run: None,
                pending_primitive_run_flushes: Vec::new(),
                current_work_origin: None,
                workflow_step_started_bound_id: None,
                apps,
                workspace_apps: WorkspaceAppRegistry::default(),
                telegram: telegram.handle(),
                telegram_acl: TelegramAclHandle::load().await,
                compiled_prompts: CompiledPromptStore::from_entries(Vec::new()),
                execution_cwd: execution.path().to_path_buf(),
                coding_project_dir: None,
                sandbox_policy: RuntimeSandboxPolicy::disabled(),
                dashboard_tx: None,
                dashboard_history: None,
                daemon_control_tx,
                latest_context_composition: None,
                active_runtime_turn: false,
                active_runtime_phase: None,
                runtime_turn_started_at: None,
                runtime_turn_started_at_ms: None,
                runtime_turn_epoch: 0,
                active_app_notices: HashMap::new(),
                runtime_overflow_failures: std::sync::Arc::new(parking_lot::Mutex::new(
                    HashMap::new(),
                )),
                runtime_model_request_failures: std::sync::Arc::new(parking_lot::Mutex::new(
                    HashMap::new(),
                )),
                suppressed_app_notices: std::sync::Arc::new(
                    parking_lot::Mutex::new(HashMap::new()),
                ),
                live_progress_tx: std::sync::Arc::new(parking_lot::Mutex::new(None)),
                telegram_live_drafts: std::sync::Arc::new(parking_lot::Mutex::new(HashMap::new())),
                claimed_event_ids: Vec::new(),
                claimed_app_notices: Vec::new(),
                afterclaim_context_fingerprint: None,
                idle_since: None,
                last_idle_sleep_at: None,
                session_title: crate::runtime::session_title::SessionTitleState::default(),
                token_estimate_baseline: TokenEstimateBaseline::default(),
            };
            Self {
                context,
                _home_override: home_override,
                _home: home,
                _execution: execution,
            }
        }
    }

    fn json_contains_key(value: &Value, needle: &str) -> bool {
        match value {
            Value::Object(object) => {
                object.contains_key(needle)
                    || object
                        .values()
                        .any(|value| json_contains_key(value, needle))
            }
            Value::Array(values) => values.iter().any(|value| json_contains_key(value, needle)),
            _ => false,
        }
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
            name: "terminal__terminal_exec".to_string(),
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
    fn telegram_tool_status_renders_primitive_activation_failure() {
        let call = AgentToolCall {
            id: "call_1".to_string(),
            name: "activate_composed_primitive".to_string(),
            arguments: serde_json::json!({ "primitive_id": "repo-analysis-summary" }),
        };
        let result = tool_result(
            "activate_composed_primitive",
            serde_json::json!({ "error": "unknown primitive" }),
            ToolUiEvent::error("activate_composed_primitive failed", Vec::new()),
        );

        let status = render_telegram_tool_result_status(&call, &result).unwrap();

        assert_eq!(status.icon, glyph::ERROR);
        assert_eq!(status.text, "Primitive Activation Failed");
    }

    #[tokio::test]
    async fn terminal_write_stdin_tool_schema_does_not_use_schema_composition() {
        let isolated = IsolatedTestContext::new().await;

        let spec = build_runtime_tool_specs(&isolated.context)
            .into_iter()
            .find(|tool| tool.name == "terminal__terminal_write_stdin")
            .expect("terminal write stdin tool");
        let AgentToolInputSpec::JsonSchema { schema } = spec.input_spec else {
            panic!("terminal_write_stdin should use json schema");
        };

        crate::schema_utils::validate_model_facing_schema(&schema).unwrap();
        for key in ["oneOf", "anyOf", "allOf"] {
            assert!(!json_contains_key(&schema, key), "{schema:#}");
        }
    }

    #[tokio::test]
    async fn coding_read_code_tool_schema_does_not_use_schema_composition() {
        let isolated = IsolatedTestContext::new().await;

        let spec = build_runtime_tool_specs(&isolated.context)
            .into_iter()
            .find(|tool| tool.name == "coding__read_code")
            .expect("coding read code tool");
        let AgentToolInputSpec::JsonSchema { schema } = spec.input_spec else {
            panic!("coding_read_code should use json schema");
        };

        crate::schema_utils::validate_model_facing_schema(&schema).unwrap();
        for key in ["oneOf", "anyOf", "allOf"] {
            assert!(!json_contains_key(&schema, key), "{schema:#}");
        }
        assert!(json_contains_key(&schema, "ref"), "{schema:#}");
        assert!(!json_contains_key(&schema, "path"), "{schema:#}");
        assert!(!json_contains_key(&schema, "start_line"), "{schema:#}");
        assert!(!json_contains_key(&schema, "line_count"), "{schema:#}");
    }

    #[tokio::test]
    async fn coding_search_code_tool_schema_exposes_rg_aligned_options() {
        let isolated = IsolatedTestContext::new().await;

        let spec = build_runtime_tool_specs(&isolated.context)
            .into_iter()
            .find(|tool| tool.name == "coding__search_code")
            .expect("coding search code tool");
        let AgentToolInputSpec::JsonSchema { schema } = spec.input_spec else {
            panic!("coding_search_code should use json schema");
        };

        crate::schema_utils::validate_model_facing_schema(&schema).unwrap();
        let properties = schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .unwrap_or_else(|| panic!("schema should have object properties: {schema:#}"));
        for key in [
            "query",
            "mode",
            "path",
            "include",
            "exclude",
            "types",
            "type_not",
            "case",
            "word",
            "line",
            "hidden",
            "respect_ignore",
            "follow",
            "limit",
        ] {
            assert!(properties.contains_key(key), "missing {key}: {schema:#}");
        }
        assert!(
            !properties.contains_key("case_mode"),
            "schema should expose `case`, not internal field name: {schema:#}"
        );
        let required = schema
            .get("required")
            .and_then(serde_json::Value::as_array)
            .unwrap_or_else(|| panic!("schema should have required fields: {schema:#}"));
        assert_eq!(required.len(), properties.len(), "{schema:#}");
        for key in ["include", "exclude", "types", "type_not"] {
            assert_eq!(
                properties
                    .get(key)
                    .and_then(|value| value.get("type"))
                    .and_then(serde_json::Value::as_str),
                Some("array"),
                "{key} should be an array: {schema:#}"
            );
        }
    }

    #[tokio::test]
    async fn structured_edit_tool_schemas_do_not_use_schema_composition() {
        let isolated = IsolatedTestContext::new().await;
        let specs = build_runtime_tool_specs(&isolated.context);

        for tool_name in ["edit_file", "coding__edit_code"] {
            let spec = specs
                .iter()
                .find(|tool| tool.name == tool_name)
                .unwrap_or_else(|| panic!("{tool_name} tool"));
            let AgentToolInputSpec::JsonSchema { schema } = &spec.input_spec else {
                panic!("{tool_name} should use json schema");
            };

            crate::schema_utils::validate_model_facing_schema(schema).unwrap();
            for key in ["oneOf", "anyOf", "allOf"] {
                assert!(
                    !json_contains_key(schema, key),
                    "tool={tool_name} schema={schema:#}"
                );
            }
        }
    }

    #[tokio::test]
    async fn exposed_runtime_tool_schemas_follow_model_facing_dialect() {
        let isolated = IsolatedTestContext::new().await;
        for spec in build_runtime_tool_specs(&isolated.context) {
            match spec.input_spec {
                AgentToolInputSpec::JsonSchema { schema } => {
                    crate::schema_utils::validate_model_facing_schema(&schema).unwrap_or_else(
                        |err| panic!("tool={} schema={schema:#}\n{err}", spec.name),
                    );
                }
                AgentToolInputSpec::FreeformGrammar {
                    fallback_schema, ..
                } => {
                    crate::schema_utils::validate_model_facing_schema(&fallback_schema)
                        .unwrap_or_else(|err| {
                            panic!(
                                "tool={} fallback_schema={fallback_schema:#}\n{err}",
                                spec.name
                            )
                        });
                }
            }
        }
    }

    #[tokio::test]
    async fn read_file_returns_line_hash_anchored_lines() {
        let mut isolated = IsolatedTestContext::new().await;
        let root = isolated.context.execution_cwd.clone();
        std::fs::write(root.join("notes.txt"), "alpha\nbeta\ngamma\n").expect("write fixture");

        let call = AgentToolCall {
            id: "call_read".to_string(),
            name: "read_file".to_string(),
            arguments: json!({
                "path": "notes.txt",
                "start_line": 2,
                "line_count": 1,
            }),
        };

        let result = execute_agent_tool_call(&mut isolated.context, &call)
            .await
            .expect("read file");

        assert_eq!(
            result.model_content(),
            format!("2#{}|beta", scope_engine::patch::line_hash("beta"))
        );
        let ToolUiEvent::Explored(ui_event) = &result.ui_event else {
            panic!("read_file should render as explored activity");
        };
        assert_eq!(ui_event.stable_id, crate::tool_ui::EXPLORED_STABLE_ID);
        assert_eq!(ui_event.calls.len(), 1);
        assert_eq!(ui_event.calls[0].tool_name, "Read");
        assert_eq!(ui_event.calls[0].summary, "notes.txt#L2-L2");
    }

    #[tokio::test]
    async fn edit_file_applies_structured_line_hash_edits() {
        let mut isolated = IsolatedTestContext::new().await;
        let root = isolated.context.execution_cwd.clone();
        std::fs::write(root.join("README.md"), "old\n").expect("write markdown fixture");

        let hash = scope_engine::patch::line_hash("old");
        let call = AgentToolCall {
            id: "call_edit".to_string(),
            name: "edit_file".to_string(),
            arguments: json!({
                "edits": [{
                    "path": "README.md",
                    "op": "replace",
                    "start": format!("1#{hash}"),
                    "end": format!("1#{hash}"),
                    "content": "new"
                }]
            }),
        };

        let result = execute_agent_tool_call(&mut isolated.context, &call)
            .await
            .expect("edit file");

        assert_eq!(
            std::fs::read_to_string(root.join("README.md")).expect("read markdown fixture"),
            "new\n"
        );
        let ToolUiEvent::Explored(ui_event) = &result.ui_event else {
            panic!("edit_file should render as explored activity");
        };
        assert_eq!(ui_event.stable_id, crate::tool_ui::EXPLORED_STABLE_ID);
        assert_eq!(ui_event.calls.len(), 1);
        assert_eq!(ui_event.calls[0].tool_name, "Edit");
        assert_eq!(ui_event.calls[0].summary, "README.md");
    }

    #[tokio::test]
    async fn all_app_tools_are_exposed_by_namespace() {
        let isolated = IsolatedTestContext::new().await;

        let specs = build_runtime_tool_specs(&isolated.context);
        let names = specs
            .into_iter()
            .map(|tool| tool.name)
            .collect::<HashSet<_>>();

        assert!(names.contains("read_file"));
        assert!(names.contains("edit_file"));
        assert!(names.contains("browser__get_state"));
        assert!(names.contains("browser__browser_open_page"));
        assert!(names.contains("browser__browser_snapshot"));
        assert!(names.contains("coding__get_state"));
        assert!(names.contains("coding__open_project"));
        assert!(names.contains("coding__search_code"));
        assert!(names.contains("coding__read_code"));
        assert!(names.contains("coding__edit_code"));
        assert!(names.contains("coding__next_review"));
        assert!(names.contains("terminal__get_state"));
        assert!(names.contains("terminal__terminal_exec"));
        assert!(names.contains("terminal__terminal_write_stdin"));
        assert!(names.contains("terminal__terminal_terminate"));
        assert!(!names.contains("apply_patch"));
        assert!(!names.contains("coding__grep"));
        assert!(!names.contains("coding__glob"));
        assert!(!names.contains("open_project"));
        assert!(!names.contains("coding__coding_open_project"));
        assert!(!names.contains("coding_open_project"));
        assert!(!names.contains("terminal_exec"));
        assert!(!names.contains("browser_open_page"));
    }

    #[tokio::test]
    async fn namespaced_terminal_tool_executes_directly() {
        let mut isolated = IsolatedTestContext::new().await;
        let call = AgentToolCall {
            id: "call_1".to_string(),
            name: "terminal__terminal_exec".to_string(),
            arguments: json!({
                "command": "printf '%s\\n' delegated-terminal",
                "yield_time_ms": 100,
            }),
        };

        let result = execute_agent_tool_call(&mut isolated.context, &call)
            .await
            .unwrap();

        assert!(
            result.model_content().contains("delegated-terminal"),
            "model content was: {}",
            result.model_content()
        );
        assert!(matches!(result.ui_event, ToolUiEvent::Terminal(_)));
    }

    #[tokio::test]
    async fn project_scope_rejects_file_tool_for_scope_owned_source() {
        let mut isolated = IsolatedTestContext::new().await;
        let root = isolated.context.execution_cwd.clone();
        std::fs::write(root.join("lib.rs"), "pub fn value() -> i32 {\n    1\n}\n")
            .expect("write rust fixture");

        let open_call = AgentToolCall {
            id: "call_open".to_string(),
            name: "coding__open_project".to_string(),
            arguments: json!({
                "project_root": root,
            }),
        };
        execute_agent_tool_call(&mut isolated.context, &open_call)
            .await
            .expect("open project");

        let hash = scope_engine::patch::line_hash("    1");
        let edit_call = AgentToolCall {
            id: "call_edit".to_string(),
            name: "edit_file".to_string(),
            arguments: json!({
                "edits": [{
                    "path": "lib.rs",
                    "op": "replace",
                    "start": format!("2#{hash}"),
                    "end": format!("2#{hash}"),
                    "content": "    2"
                }]
            }),
        };

        let err = execute_agent_tool_call(&mut isolated.context, &edit_call)
            .await
            .expect_err("SCOPE-owned source edit should be rejected");

        assert!(
            err.to_string()
                .contains("edit_file is forbidden for SCOPE-owned source files"),
            "unexpected error: {err}"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("lib.rs")).expect("read rust fixture"),
            "pub fn value() -> i32 {\n    1\n}\n"
        );
    }

    #[tokio::test]
    async fn project_scope_allows_file_tool_for_non_scope_file() {
        let mut isolated = IsolatedTestContext::new().await;
        let root = isolated.context.execution_cwd.clone();
        std::fs::write(root.join("README.md"), "old\n").expect("write markdown fixture");

        let open_call = AgentToolCall {
            id: "call_open".to_string(),
            name: "coding__open_project".to_string(),
            arguments: json!({
                "project_root": root,
            }),
        };
        execute_agent_tool_call(&mut isolated.context, &open_call)
            .await
            .expect("open project");

        let hash = scope_engine::patch::line_hash("old");
        let edit_call = AgentToolCall {
            id: "call_edit".to_string(),
            name: "edit_file".to_string(),
            arguments: json!({
                "edits": [{
                    "path": "README.md",
                    "op": "replace",
                    "start": format!("1#{hash}"),
                    "end": format!("1#{hash}"),
                    "content": "new"
                }]
            }),
        };

        execute_agent_tool_call(&mut isolated.context, &edit_call)
            .await
            .expect("non-SCOPE edit should be allowed");

        assert_eq!(
            std::fs::read_to_string(root.join("README.md")).expect("read markdown fixture"),
            "new\n"
        );
    }

    #[tokio::test]
    async fn generated_get_state_tool_reads_app_state() {
        let mut isolated = IsolatedTestContext::new().await;
        let call = AgentToolCall {
            id: "call_state".to_string(),
            name: "terminal__get_state".to_string(),
            arguments: json!({ "include_manual": true }),
        };

        let result = execute_agent_tool_call(&mut isolated.context, &call)
            .await
            .unwrap();

        assert!(result.model_content().contains("app=terminal"));
        assert_eq!(result.payload["app"], "terminal");
        assert!(result.payload.get("usage").is_some());
        assert!(matches!(result.ui_event, ToolUiEvent::App(_)));
    }

    #[tokio::test]
    async fn app_tools_do_not_expose_unscoped_aliases() {
        let isolated = IsolatedTestContext::new().await;

        let names = build_runtime_tool_specs(&isolated.context)
            .into_iter()
            .map(|tool| tool.name)
            .collect::<HashSet<_>>();

        assert!(names.contains("read_file"));
        assert!(names.contains("edit_file"));
        assert!(names.contains("browser__browser_open_page"));
        assert!(names.contains("coding__open_project"));
        assert!(names.contains("terminal__terminal_exec"));
        assert!(!names.contains("apply_patch"));
        assert!(!names.contains("terminal_exec"));
        assert!(!names.contains("open_project"));
        assert!(!names.contains("coding__coding_open_project"));
        assert!(!names.contains("coding_open_project"));
        assert!(!names.contains("browser_open_page"));
    }
}
