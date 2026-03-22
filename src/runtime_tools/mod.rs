use std::{future::Future, pin::Pin};

use async_trait::async_trait;
use miette::{Result, miette};
use schemars::schema_for;
use serde_json::Value;

use crate::{
    context::Context,
    core::ApplyPatchArgs,
    device::DeviceToolScope,
    reasoning::{
        episode::EpisodeActionRecord,
        runtime::{AgentToolCall, AgentToolSpec},
    },
    tool_ui::{ToolCallUiEvent, ToolUiEvent},
};

mod telegram;
mod terminal;
mod work;

pub(super) type ToolFuture<'a> =
    Pin<Box<dyn Future<Output = miette::Result<ToolExecutionResult>> + Send + 'a>>;
type ToolExecutor = for<'a> fn(&'a mut Context, &'a AgentToolCall) -> ToolFuture<'a>;
type ToolSummarizer = fn(&AgentToolCall) -> miette::Result<EpisodeActionRecord>;
type ToolCallUiBuilder = fn(&AgentToolCall) -> miette::Result<ToolCallUiEvent>;

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

fn normalize_tool_input_schema(mut schema: serde_json::Value) -> serde_json::Value {
    if let Some(object) = schema.as_object_mut()
        && object.get("type").and_then(|value| value.as_str()) == Some("object")
    {
        object
            .entry("properties".to_string())
            .or_insert_with(|| serde_json::json!({}));
        object
            .entry("additionalProperties".to_string())
            .or_insert_with(|| serde_json::json!(false));
    }
    schema
}

#[derive(Clone, Debug)]
pub struct ToolExecutionResult {
    pub summary: String,
    pub payload: Value,
    pub ui_event: ToolUiEvent,
}

impl ToolExecutionResult {
    pub fn new(summary: impl Into<String>, payload: Value, ui_event: ToolUiEvent) -> Self {
        Self {
            summary: summary.into(),
            payload,
            ui_event,
        }
    }

    pub fn model_content(&self) -> String {
        if self.payload.is_null() {
            format!("summary={}", self.summary)
        } else {
            format!(
                "summary={}\npayload=\n{}",
                self.summary,
                serde_json::to_string_pretty(&self.payload)
                    .unwrap_or_else(|_| self.payload.to_string())
            )
        }
    }

    pub fn history_content(&self, tool_call_id: &str, tool_name: &str) -> String {
        format!(
            "tool_call_id={tool_call_id}\nname={tool_name}\n{}",
            self.model_content()
        )
    }
}

#[async_trait]
pub trait RuntimeTool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn input_schema(&self) -> Value;

    fn is_available(&self, _context: &Context) -> bool {
        true
    }

    fn spec(&self) -> AgentToolSpec {
        AgentToolSpec {
            name: self.name().to_string(),
            description: self.description().to_string(),
            input_schema: self.input_schema(),
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
    input_schema: Value,
    scope: Option<DeviceToolScope>,
    summarize: ToolSummarizer,
    call_ui: ToolCallUiBuilder,
    execute: ToolExecutor,
}

impl StaticRuntimeTool {
    fn new<T: schemars::JsonSchema>(
        name: &'static str,
        description: &'static str,
        scope: Option<DeviceToolScope>,
        summarize: ToolSummarizer,
        call_ui: ToolCallUiBuilder,
        execute: ToolExecutor,
    ) -> Self {
        Self {
            name,
            description,
            input_schema: normalize_tool_input_schema(
                serde_json::to_value(schema_for!(T)).unwrap(),
            ),
            scope,
            summarize,
            call_ui,
            execute,
        }
    }
}

#[async_trait]
impl RuntimeTool for StaticRuntimeTool {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn input_schema(&self) -> Value {
        self.input_schema.clone()
    }

    fn is_available(&self, context: &Context) -> bool {
        match self.scope {
            None => true,
            Some(scope) => context.devices.focused_tool_scopes().contains(&scope),
        }
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

pub fn build_runtime_tools() -> Vec<Box<dyn RuntimeTool>> {
    let mut tools: Vec<Box<dyn RuntimeTool>> = vec![Box::new(StaticRuntimeTool::new::<
        ApplyPatchArgs,
    >(
        "apply_patch",
        "使用 apply_patch 工具按 patch grammar 精确编辑文件。patch 必须以 `*** Begin Patch` 开始，以 `*** End Patch` 结束。",
        Some(DeviceToolScope::Terminal),
        work::summarize_apply_patch_tool,
        work::render_apply_patch_call_ui,
        work::execute_apply_patch_runtime_tool,
    ))];
    tools.extend(work::register_tools());
    tools.extend(terminal::register_tools());
    tools.extend(telegram::register_tools());
    tools
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
    build_runtime_tools()
        .into_iter()
        .filter(|tool| tool.is_available(context))
        .map(|tool| tool.spec())
        .collect()
}

pub fn summarize_primary_action_from_tool_call(
    call: &AgentToolCall,
) -> Result<EpisodeActionRecord> {
    let tools = build_runtime_tools();
    find_runtime_tool(&tools, &call.name)?.summarize_action(call)
}

pub fn render_tool_call_ui_event(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let tools = build_runtime_tools();
    find_runtime_tool(&tools, &call.name)?.call_ui_event(call)
}

pub async fn execute_agent_tool_call(
    context: &mut Context,
    call: &AgentToolCall,
) -> Result<ToolExecutionResult> {
    let tools = build_runtime_tools();
    let tool = find_runtime_tool(&tools, &call.name)?;
    if !tool.is_available(context) {
        return Err(miette!("tool `{}` is not currently available", call.name));
    }
    tool.execute(context, call).await
}
