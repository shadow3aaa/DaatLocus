use miette::Result;
use serde_json::json;

use crate::{
    browser_device::{
        BrowserActionResult, BrowserFindResult, BrowserPageState, BrowserSnapshotResult,
        BrowserWaitResult,
    },
    context::Context,
    context_budget::truncate_text_to_token_budget,
    core::{
        BrowserBackArgs, BrowserClickArgs, BrowserClosePageArgs, BrowserFillArgs,
        BrowserFindInPageArgs, BrowserForwardArgs, BrowserOpenArgs, BrowserReloadArgs,
        BrowserSnapshotArgs, BrowserWaitArgs,
    },
    dashboard::{DashboardActivityEvent, apply_activity_event},
    device::DeviceToolScope,
    reasoning::{episode::EpisodeActionRecord, runtime::AgentToolCall},
    tool_ui::{ToolCallUiEvent, ToolUiEvent, compact_body_lines},
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
            "browser_open",
            "新建一个浏览器页面并打开指定 URL。返回新的 `page_id`。",
            Some(DeviceToolScope::Browser),
            summarize_browser_open_tool,
            render_browser_open_call_ui,
            execute_browser_open_tool,
        )),
        Box::new(StaticRuntimeTool::new::<BrowserSnapshotArgs>(
            "browser_snapshot",
            "抓取指定页面的最新语义快照。返回 `snapshot_id` 和可交互元素 `element_ref`。",
            Some(DeviceToolScope::Browser),
            summarize_browser_snapshot_tool,
            render_browser_snapshot_call_ui,
            execute_browser_snapshot_tool,
        )),
        Box::new(StaticRuntimeTool::new::<BrowserFindInPageArgs>(
            "browser_find_in_page",
            "在指定页面中查找包含目标文本的元素，返回匹配元素的 `element_ref`、角色和文本摘要。",
            Some(DeviceToolScope::Browser),
            summarize_browser_find_in_page_tool,
            render_browser_find_in_page_call_ui,
            execute_browser_find_in_page_tool,
        )),
        Box::new(StaticRuntimeTool::new::<BrowserWaitArgs>(
            "browser_wait",
            "等待指定页面进入稳定状态。`state` 可用 `dom` 或 `load`；等待后旧 `snapshot_id` 会失效。",
            Some(DeviceToolScope::Browser),
            summarize_browser_wait_tool,
            render_browser_wait_call_ui,
            execute_browser_wait_tool,
        )),
        Box::new(StaticRuntimeTool::new::<BrowserClickArgs>(
            "browser_click",
            "基于最新快照中的 `element_ref` 点击页面元素。点击后旧 `snapshot_id` 会失效。",
            Some(DeviceToolScope::Browser),
            summarize_browser_click_tool,
            render_browser_click_call_ui,
            execute_browser_click_tool,
        )),
        Box::new(StaticRuntimeTool::new::<BrowserFillArgs>(
            "browser_fill",
            "基于最新快照中的 `element_ref` 填写输入框。填写后旧 `snapshot_id` 会失效。",
            Some(DeviceToolScope::Browser),
            summarize_browser_fill_tool,
            render_browser_fill_call_ui,
            execute_browser_fill_tool,
        )),
        Box::new(StaticRuntimeTool::new::<BrowserBackArgs>(
            "browser_back",
            "让指定页面后退。后退后旧 `snapshot_id` 会失效。",
            Some(DeviceToolScope::Browser),
            summarize_browser_back_tool,
            render_browser_back_call_ui,
            execute_browser_back_tool,
        )),
        Box::new(StaticRuntimeTool::new::<BrowserForwardArgs>(
            "browser_forward",
            "让指定页面前进。前进后旧 `snapshot_id` 会失效。",
            Some(DeviceToolScope::Browser),
            summarize_browser_forward_tool,
            render_browser_forward_call_ui,
            execute_browser_forward_tool,
        )),
        Box::new(StaticRuntimeTool::new::<BrowserReloadArgs>(
            "browser_reload",
            "刷新指定页面。刷新后旧 `snapshot_id` 会失效。",
            Some(DeviceToolScope::Browser),
            summarize_browser_reload_tool,
            render_browser_reload_call_ui,
            execute_browser_reload_tool,
        )),
        Box::new(StaticRuntimeTool::new::<BrowserClosePageArgs>(
            "browser_close_page",
            "关闭指定浏览器页面。关闭不再需要的页面以节省内存。",
            Some(DeviceToolScope::Browser),
            summarize_browser_close_page_tool,
            render_browser_close_page_call_ui,
            execute_browser_close_page_tool,
        )),
    ]
}

fn browser_page_meta(page: &BrowserPageState) -> String {
    format!(
        "page={} title={} url={} snapshot={}",
        page.page_id,
        page.title,
        page.url,
        page.last_snapshot_id.as_deref().unwrap_or("none")
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
    let lines = vec![
        format!("summary={summary}"),
        browser_page_meta(&result.page),
        format!("snapshot_id={}", result.snapshot_id),
        "snapshot=".to_string(),
        result.snapshot.clone(),
    ];
    truncate_text_to_token_budget(&lines.join("\n"), max_tokens)
}

fn browser_find_model_content(
    summary: &str,
    result: &BrowserFindResult,
    max_tokens: usize,
) -> String {
    let mut lines = vec![
        format!("summary={summary}"),
        browser_page_meta(&result.page),
        format!("query={}", summarize_inline_text(&result.query)),
        format!("match_count={}", result.matches.len()),
    ];
    for (index, item) in result.matches.iter().enumerate() {
        lines.push(format!(
            "match[{index}]=ref={} role={} name={} text={}",
            item.element_ref.as_deref().unwrap_or("none"),
            item.role.as_deref().unwrap_or("none"),
            item.name
                .as_deref()
                .map(summarize_inline_text)
                .unwrap_or_else(|| "none".to_string()),
            summarize_inline_text(&item.text_snippet)
        ));
    }
    truncate_text_to_token_budget(&lines.join("\n"), max_tokens)
}

fn summarize_browser_open_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: BrowserOpenArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "browser_open".to_string(),
        summary: format!("url={}", summarize_inline_text(&args.url)),
    })
}

fn render_browser_open_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: BrowserOpenArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::device(
        "browser_open",
        vec![format!("url={}", summarize_inline_text(&args.url))],
    ))
}

fn execute_browser_open_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: BrowserOpenArgs = parse_tool_args(call)?;
        let result = context.devices.browser_open(&args.url).await?;
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
            ToolUiEvent::device(
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
    Ok(ToolCallUiEvent::device(
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
        let dashboard_tx = context.dashboard_tx.clone();
        let result = context.devices.browser_snapshot(&args.page_id).await?;
        if let Some(tx) = &dashboard_tx {
            tx.send_modify(|state| {
                apply_activity_event(
                    state,
                    DashboardActivityEvent::ExecUpdate {
                        key: call.id.clone(),
                        meta: Some(format!("page={}", result.page.page_id)),
                        output_lines: compact_body_lines(&result.snapshot, 10),
                    },
                );
            });
        }
        let summary = format!(
            "captured browser snapshot {} for page {}",
            result.snapshot_id, result.page.page_id
        );
        let model_content = browser_snapshot_model_content(
            &summary,
            &result,
            model_tool_output_token_budget(context),
        );
        Ok(ToolExecutionResult::new(
            summary,
            json!({
                "page": result.page,
                "snapshot_id": result.snapshot_id,
                "snapshot": result.snapshot,
            }),
            ToolUiEvent::device(
                "captured browser snapshot",
                vec![
                    format!("page={}", result.page.page_id),
                    format!("snapshot_id={}", result.snapshot_id),
                ],
            ),
        )
        .with_model_content(model_content))
    })
}

fn summarize_browser_find_in_page_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: BrowserFindInPageArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "browser_find_in_page".to_string(),
        summary: format!(
            "page={} query={}",
            args.page_id,
            summarize_inline_text(&args.query)
        ),
    })
}

fn render_browser_find_in_page_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: BrowserFindInPageArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::device(
        "browser_find_in_page",
        vec![
            format!("page={}", args.page_id),
            format!("query={}", summarize_inline_text(&args.query)),
        ],
    ))
}

fn execute_browser_find_in_page_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: BrowserFindInPageArgs = parse_tool_args(call)?;
        let result = context
            .devices
            .browser_find_in_page(&args.page_id, &args.query, args.max_results.unwrap_or(5))
            .await?;
        let summary = format!(
            "found {} browser matches for query on page {}",
            result.matches.len(),
            result.page.page_id
        );
        let model_content =
            browser_find_model_content(&summary, &result, model_tool_output_token_budget(context));
        Ok(ToolExecutionResult::new(
            summary,
            json!({
                "page": result.page,
                "query": result.query,
                "matches": result.matches,
            }),
            ToolUiEvent::device(
                "found browser matches",
                vec![
                    format!("page={}", result.page.page_id),
                    format!("matches={}", result.matches.len()),
                ],
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
    Ok(ToolCallUiEvent::device(
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
            .devices
            .browser_wait(&args.page_id, args.state.as_deref(), args.timeout_ms)
            .await?;
        browser_wait_tool_result(context, &result)
    })
}

fn summarize_browser_click_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: BrowserClickArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "browser_click".to_string(),
        summary: format!(
            "page={} snapshot={} ref={}",
            args.page_id, args.snapshot_id, args.element_ref
        ),
    })
}

fn render_browser_click_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: BrowserClickArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::device(
        "browser_click",
        vec![
            format!("page={}", args.page_id),
            format!("snapshot_id={}", args.snapshot_id),
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
            .devices
            .browser_click(&args.page_id, &args.snapshot_id, &args.element_ref)
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
            "page={} snapshot={} ref={} value={}",
            args.page_id,
            args.snapshot_id,
            args.element_ref,
            summarize_inline_text(&args.value)
        ),
    })
}

fn render_browser_fill_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: BrowserFillArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::device(
        "browser_fill",
        vec![
            format!("page={}", args.page_id),
            format!("snapshot_id={}", args.snapshot_id),
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
            .devices
            .browser_fill(
                &args.page_id,
                &args.snapshot_id,
                &args.element_ref,
                &args.value,
            )
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
    Ok(ToolCallUiEvent::device(
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
        let result = context.devices.browser_back(&args.page_id).await?;
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
    Ok(ToolCallUiEvent::device(
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
        let result = context.devices.browser_forward(&args.page_id).await?;
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
    Ok(ToolCallUiEvent::device(
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
        let result = context.devices.browser_reload(&args.page_id).await?;
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
    Ok(ToolCallUiEvent::device(
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
        let result = context.devices.browser_close_page(&args.page_id).await?;
        browser_action_tool_result(context, "closed browser page", &result, Vec::new())
    })
}

fn browser_action_tool_result(
    context: &Context,
    title: &str,
    result: &BrowserActionResult,
    mut extra_lines: Vec<String>,
) -> Result<ToolExecutionResult> {
    if let Some(snapshot_id) = result.invalidated_snapshot_id.as_deref() {
        extra_lines.push(format!("invalidated_snapshot_id={snapshot_id}"));
    }
    let model_content = browser_action_model_content(
        title,
        &result.page,
        &extra_lines,
        model_tool_output_token_budget(context),
    );
    Ok(ToolExecutionResult::new(
        title,
        json!({
            "page": result.page,
            "invalidated_snapshot_id": result.invalidated_snapshot_id,
        }),
        ToolUiEvent::device(title, {
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
    let mut extra_lines = vec![format!("wait_state={}", result.wait_state)];
    if let Some(snapshot_id) = result.invalidated_snapshot_id.as_deref() {
        extra_lines.push(format!("invalidated_snapshot_id={snapshot_id}"));
    }
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
            "invalidated_snapshot_id": result.invalidated_snapshot_id,
        }),
        ToolUiEvent::device("waited for browser page", {
            let mut lines = vec![format!("page={}", result.page.page_id)];
            lines.extend(extra_lines);
            lines
        }),
    )
    .with_model_content(model_content))
}
