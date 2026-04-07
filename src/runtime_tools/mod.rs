use std::{future::Future, pin::Pin};

use async_trait::async_trait;
use miette::{Result, miette};
use schemars::schema_for;
use serde_json::Value;

use crate::{
    app::AppToolScope,
    context::Context,
    context_budget::truncate_text_to_token_budget,
    reasoning::{
        episode::EpisodeActionRecord,
        runtime::{AgentToolCall, AgentToolInputSpec, AgentToolSpec},
    },
    tool_ui::{ToolCallUiEvent, ToolUiEvent},
};

mod browser;
mod terminal;
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
            self.model_content_override = Some(truncate_text_to_token_budget(
                &self.default_content_for_payload(&self.payload),
                max_tokens,
            ));
        }
        self
    }
}

#[async_trait]
pub trait RuntimeTool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
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

    fn new_with_availability<T: schemars::JsonSchema>(
        name: &'static str,
        description: &'static str,
        scope: Option<AppToolScope>,
        availability: ToolAvailability,
        summarize: ToolSummarizer,
        call_ui: ToolCallUiBuilder,
        execute: ToolExecutor,
    ) -> Self {
        let mut tool = Self::new::<T>(name, description, scope, summarize, call_ui, execute);
        tool.availability = Some(availability);
        tool
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
    fn name(&self) -> &'static str {
        "apply_patch"
    }

    fn description(&self) -> &'static str {
        r#"使用 `apply_patch` 按严格 patch grammar 编辑文件。

补丁必须满足：
- 以 `*** Begin Patch` 开始
- 以 `*** End Patch` 结束
- 只能包含 `*** Add File:` / `*** Delete File:` / `*** Update File:` 三种文件操作头
- `@@` 只能出现在 `*** Update File:` 之后，作为 hunk 头

完整 grammar：
Patch := Begin { FileOp } End
Begin := "*** Begin Patch" NEWLINE
End := "*** End Patch" NEWLINE
FileOp := AddFile | DeleteFile | UpdateFile
AddFile := "*** Add File: " path NEWLINE { "+" line NEWLINE }
DeleteFile := "*** Delete File: " path NEWLINE
UpdateFile := "*** Update File: " path NEWLINE { Hunk }
Hunk := "@@" [ header ] NEWLINE { HunkLine }
HunkLine := (" " | "-" | "+") text NEWLINE

示例：
*** Begin Patch
*** Add File: hello.txt
+Hello world
*** Update File: src/app.py
@@
-print("Hi")
+print("Hello, world!")
*** Delete File: obsolete.txt
*** End Patch

注意：
- 新文件的每一行都必须以 `+` 开头
- patch 必须使用相对路径，不能使用绝对路径
- 不要输出 unified diff 的 `---` / `+++` 文件头
- 不要省略 `*** Update File:` 后就直接写 `@@`"#
    }

    fn input_spec(&self) -> AgentToolInputSpec {
        AgentToolInputSpec::FreeformGrammar {
            syntax: "lark".to_string(),
            definition: r#"start: begin_patch hunk+ end_patch
begin_patch: "*** Begin Patch" LF
end_patch: "*** End Patch" LF?
hunk: add_hunk | delete_hunk | update_hunk
add_hunk: "*** Add File: " filename LF add_line+
delete_hunk: "*** Delete File: " filename LF
update_hunk: "*** Update File: " filename LF change?
filename: /(.+)/
add_line: "+" /(.*)/ LF
change: (change_context | change_line)+ eof_line?
change_context: ("@@" | "@@ " /(.+)/) LF
change_line: ("+" | "-" | " ") /(.*)/ LF
eof_line: "*** End of File" LF
%import common.LF"#
                .to_string(),
            fallback_schema: freeform_string_fallback_schema(
                "The entire contents of the apply_patch command",
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

pub fn build_runtime_tools() -> Vec<Box<dyn RuntimeTool>> {
    let mut tools: Vec<Box<dyn RuntimeTool>> = vec![Box::new(ApplyPatchRuntimeTool)];
    tools.extend(work::register_tools());
    tools.extend(browser::register_tools());
    tools.extend(terminal::register_tools());
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

pub fn summarize_action_from_tool_call(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
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
    let result = tool.execute(context, call).await?;
    Ok(result
        .ensure_model_content_with_budget(context.config.main_model.tool_output_max_tokens.max(1)))
}
