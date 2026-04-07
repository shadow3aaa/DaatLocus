use miette::Result;
use serde_json::json;

use crate::{
    app::AppToolScope,
    browser_app::{
        BrowserActionResult, BrowserPageState, BrowserSnapshotResult, BrowserWaitResult,
    },
    context::Context,
    context_budget::truncate_text_to_token_budget,
    core::{
        BrowserBackArgs, BrowserClickArgs, BrowserClosePageArgs, BrowserFillArgs,
        BrowserForwardArgs, BrowserOpenArgs, BrowserReloadArgs, BrowserSnapshotArgs,
        BrowserWaitArgs,
    },
    reasoning::{episode::EpisodeActionRecord, runtime::AgentToolCall},
    tool_ui::{ToolCallUiEvent, ToolUiEvent},
};

use super::{
    RuntimeTool, StaticRuntimeTool, ToolExecutionResult, ToolFuture, parse_tool_args,
    summarize_inline_text,
};

fn model_tool_output_token_budget(context: &Context) -> usize {
    context.config.main_model.tool_output_max_tokens.max(1)
}

pub(super) fn register_tools() -> Vec<Box<dyn RuntimeTool>> {
    vec![
        Box::new(StaticRuntimeTool::new::<BrowserOpenArgs>(
            "browser_open_page",
            "新建一个浏览器页面并打开指定 URL。返回新的 `page_id`。",
            Some(AppToolScope::Browser),
            summarize_browser_open_page_tool,
            render_browser_open_page_call_ui,
            execute_browser_open_page_tool,
        )),
        Box::new(StaticRuntimeTool::new::<BrowserSnapshotArgs>(
            "browser_snapshot",
            "读取指定页面的完整 ARIA 语义快照，返回页面元信息与可交互元素引用。",
            Some(AppToolScope::Browser),
            summarize_browser_snapshot_tool,
            render_browser_snapshot_call_ui,
            execute_browser_snapshot_tool,
        )),
        Box::new(StaticRuntimeTool::new::<BrowserWaitArgs>(
            "browser_wait",
            "等待指定页面进入稳定状态。`state` 可用 `dom` 或 `load`。",
            Some(AppToolScope::Browser),
            summarize_browser_wait_tool,
            render_browser_wait_call_ui,
            execute_browser_wait_tool,
        )),
        Box::new(StaticRuntimeTool::new::<BrowserClickArgs>(
            "browser_click",
            "基于 `element_ref` 点击页面元素；如果页面变化导致引用失效，tool 会直接报错。",
            Some(AppToolScope::Browser),
            summarize_browser_click_tool,
            render_browser_click_call_ui,
            execute_browser_click_tool,
        )),
        Box::new(StaticRuntimeTool::new::<BrowserFillArgs>(
            "browser_fill",
            "基于 `element_ref` 填写输入框；如果页面变化导致引用失效，tool 会直接报错。",
            Some(AppToolScope::Browser),
            summarize_browser_fill_tool,
            render_browser_fill_call_ui,
            execute_browser_fill_tool,
        )),
        Box::new(StaticRuntimeTool::new::<BrowserBackArgs>(
            "browser_back",
            "让指定页面后退。",
            Some(AppToolScope::Browser),
            summarize_browser_back_tool,
            render_browser_back_call_ui,
            execute_browser_back_tool,
        )),
        Box::new(StaticRuntimeTool::new::<BrowserForwardArgs>(
            "browser_forward",
            "让指定页面前进。",
            Some(AppToolScope::Browser),
            summarize_browser_forward_tool,
            render_browser_forward_call_ui,
            execute_browser_forward_tool,
        )),
        Box::new(StaticRuntimeTool::new::<BrowserReloadArgs>(
            "browser_reload",
            "刷新指定页面。",
            Some(AppToolScope::Browser),
            summarize_browser_reload_tool,
            render_browser_reload_call_ui,
            execute_browser_reload_tool,
        )),
        Box::new(StaticRuntimeTool::new::<BrowserClosePageArgs>(
            "browser_close_page",
            "关闭指定浏览器页面。关闭不再需要的页面以节省内存。",
            Some(AppToolScope::Browser),
            summarize_browser_close_page_tool,
            render_browser_close_page_call_ui,
            execute_browser_close_page_tool,
        )),
    ]
}

fn browser_page_meta(page: &BrowserPageState) -> String {
    format!(
        "page={} title={} url={}",
        page.page_id, page.title, page.url
    )
}

fn browser_action_model_content(
    summary: &str,
    page: &BrowserPageState,
    extra_lines: &[String],
    max_tokens: usize,
) -> String {
    let mut lines = vec![format!("summary={summary}"), browser_page_meta(page)];
    lines.extend(extra_lines.iter().cloned());
    truncate_text_to_token_budget(&lines.join("\n"), max_tokens)
}

fn browser_snapshot_model_content(
    summary: &str,
    result: &BrowserSnapshotResult,
    max_tokens: usize,
) -> String {
    let mut lines = vec![
        format!("summary={summary}"),
        browser_page_meta(&result.page),
        "snapshot=".to_string(),
    ];
    lines.push(result.snapshot.clone());
    truncate_text_to_token_budget(&lines.join("\n"), max_tokens)
}

fn summarize_browser_open_page_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: BrowserOpenArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "browser_open_page".to_string(),
        summary: format!("url={}", summarize_inline_text(&args.url)),
    })
}

fn render_browser_open_page_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: BrowserOpenArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::app(
        "browser_open_page",
        vec![format!("url={}", summarize_inline_text(&args.url))],
    ))
}

fn execute_browser_open_page_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: BrowserOpenArgs = parse_tool_args(call)?;
        let result = context.apps.browser_open_page(&args.url).await?;
        let summary = format!("opened page {}", result.page.page_id);
        let model_content = browser_action_model_content(
            &summary,
            &result.page,
            &Vec::new(),
            model_tool_output_token_budget(context),
        );
        Ok(ToolExecutionResult::new(
            summary,
            json!({ "page": result.page }),
            ToolUiEvent::app(
                "opened browser page",
                vec![
                    format!("page={}", result.page.page_id),
                    format!("url={}", result.page.url),
                ],
            ),
        )
        .with_model_content(model_content))
    })
}

fn summarize_browser_snapshot_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: BrowserSnapshotArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "browser_snapshot".to_string(),
        summary: format!("page={}", args.page_id),
    })
}

fn render_browser_snapshot_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: BrowserSnapshotArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::app(
        "browser_snapshot",
        vec![format!("page={}", args.page_id)],
    ))
}

fn execute_browser_snapshot_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: BrowserSnapshotArgs = parse_tool_args(call)?;
        let result = context.apps.browser_snapshot(&args.page_id).await?;
        let summary = format!("captured browser snapshot for page {}", result.page.page_id);
        let model_content = browser_snapshot_model_content(
            &summary,
            &result,
            model_tool_output_token_budget(context),
        );
        Ok(ToolExecutionResult::new(
            summary,
            json!({
                "page": result.page,
                "snapshot": result.snapshot,
            }),
            ToolUiEvent::app(
                "captured browser snapshot",
                vec![format!("page={}", result.page.page_id)],
            ),
        )
        .with_model_content(model_content))
    })
}

fn summarize_browser_wait_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: BrowserWaitArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "browser_wait".to_string(),
        summary: format!(
            "page={} state={}",
            args.page_id,
            args.state.as_deref().unwrap_or("load")
        ),
    })
}

fn render_browser_wait_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: BrowserWaitArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::app(
        "browser_wait",
        vec![
            format!("page={}", args.page_id),
            format!("state={}", args.state.as_deref().unwrap_or("load")),
        ],
    ))
}

fn execute_browser_wait_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: BrowserWaitArgs = parse_tool_args(call)?;
        let result = context
            .apps
            .browser_wait(&args.page_id, args.state.as_deref(), args.timeout_ms)
            .await?;
        browser_wait_tool_result(context, &result)
    })
}

fn summarize_browser_click_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: BrowserClickArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "browser_click".to_string(),
        summary: format!("page={} ref={}", args.page_id, args.element_ref),
    })
}

fn render_browser_click_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: BrowserClickArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::app(
        "browser_click",
        vec![
            format!("page={}", args.page_id),
            format!("ref={}", args.element_ref),
        ],
    ))
}

fn execute_browser_click_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: BrowserClickArgs = parse_tool_args(call)?;
        let result = context
            .apps
            .browser_click(&args.page_id, &args.element_ref)
            .await?;
        browser_action_tool_result(
            context,
            "clicked browser element",
            &result,
            vec![format!("ref={}", args.element_ref)],
        )
    })
}

fn summarize_browser_fill_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: BrowserFillArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "browser_fill".to_string(),
        summary: format!(
            "page={} ref={} value={}",
            args.page_id,
            args.element_ref,
            summarize_inline_text(&args.value)
        ),
    })
}

fn render_browser_fill_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: BrowserFillArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::app(
        "browser_fill",
        vec![
            format!("page={}", args.page_id),
            format!("ref={}", args.element_ref),
            format!("value={}", summarize_inline_text(&args.value)),
        ],
    ))
}

fn execute_browser_fill_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: BrowserFillArgs = parse_tool_args(call)?;
        let result = context
            .apps
            .browser_fill(&args.page_id, &args.element_ref, &args.value)
            .await?;
        browser_action_tool_result(
            context,
            "filled browser element",
            &result,
            vec![
                format!("ref={}", args.element_ref),
                format!("value={}", summarize_inline_text(&args.value)),
            ],
        )
    })
}

fn summarize_browser_back_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: BrowserBackArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "browser_back".to_string(),
        summary: format!("page={}", args.page_id),
    })
}

fn render_browser_back_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: BrowserBackArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::app(
        "browser_back",
        vec![format!("page={}", args.page_id)],
    ))
}

fn execute_browser_back_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: BrowserBackArgs = parse_tool_args(call)?;
        let result = context.apps.browser_back(&args.page_id).await?;
        browser_action_tool_result(context, "went back in browser", &result, Vec::new())
    })
}

fn summarize_browser_forward_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: BrowserForwardArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "browser_forward".to_string(),
        summary: format!("page={}", args.page_id),
    })
}

fn render_browser_forward_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: BrowserForwardArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::app(
        "browser_forward",
        vec![format!("page={}", args.page_id)],
    ))
}

fn execute_browser_forward_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: BrowserForwardArgs = parse_tool_args(call)?;
        let result = context.apps.browser_forward(&args.page_id).await?;
        browser_action_tool_result(context, "went forward in browser", &result, Vec::new())
    })
}

fn summarize_browser_reload_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: BrowserReloadArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "browser_reload".to_string(),
        summary: format!("page={}", args.page_id),
    })
}

fn render_browser_reload_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: BrowserReloadArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::app(
        "browser_reload",
        vec![format!("page={}", args.page_id)],
    ))
}

fn execute_browser_reload_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: BrowserReloadArgs = parse_tool_args(call)?;
        let result = context.apps.browser_reload(&args.page_id).await?;
        browser_action_tool_result(context, "reloaded browser page", &result, Vec::new())
    })
}

fn summarize_browser_close_page_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: BrowserClosePageArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "browser_close_page".to_string(),
        summary: format!("page={}", args.page_id),
    })
}

fn render_browser_close_page_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: BrowserClosePageArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::app(
        "browser_close_page",
        vec![format!("page={}", args.page_id)],
    ))
}

fn execute_browser_close_page_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: BrowserClosePageArgs = parse_tool_args(call)?;
        let result = context.apps.browser_close_page(&args.page_id).await?;
        browser_action_tool_result(context, "closed browser page", &result, Vec::new())
    })
}

fn browser_action_tool_result(
    context: &Context,
    title: &str,
    result: &BrowserActionResult,
    extra_lines: Vec<String>,
) -> Result<ToolExecutionResult> {
    let model_content = browser_action_model_content(
        title,
        &result.page,
        &extra_lines,
        model_tool_output_token_budget(context),
    );
    Ok(ToolExecutionResult::new(
        title,
        json!({ "page": result.page }),
        ToolUiEvent::app(title, {
            let mut lines = vec![format!("page={}", result.page.page_id)];
            lines.extend(extra_lines);
            lines
        }),
    )
    .with_model_content(model_content))
}

fn browser_wait_tool_result(
    context: &Context,
    result: &BrowserWaitResult,
) -> Result<ToolExecutionResult> {
    let extra_lines = vec![format!("wait_state={}", result.wait_state)];
    let model_content = browser_action_model_content(
        "waited for browser page",
        &result.page,
        &extra_lines,
        model_tool_output_token_budget(context),
    );
    Ok(ToolExecutionResult::new(
        "waited for browser page",
        json!({
            "page": result.page,
            "wait_state": result.wait_state,
        }),
        ToolUiEvent::app("waited for browser page", {
            let mut lines = vec![format!("page={}", result.page.page_id)];
            lines.extend(extra_lines);
            lines
        }),
    )
    .with_model_content(model_content))
}
