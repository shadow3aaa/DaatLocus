use daat_locus_macros::model_schema;
use miette::{Result, miette};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::{
    activity_event::{TextActivityDescriptor, ToolCallActivityEvent},
    context::Context,
    context_budget::APPROX_BYTES_PER_TOKEN,
    dashboard::SessionActivityEvent,
    dashboard::{DashboardActivityHistoryStore, HistoryArchiveItem, HistoryArchiveQueryMode},
    reasoning::{episode::EpisodeActionRecord, runtime::AgentToolCall},
    runtime_tools::{
        RuntimeTool, StaticRuntimeTool, ToolExecutionResult, ToolFuture, parse_tool_args,
    },
    schema_utils::model_schema_for,
};

const DEFAULT_HISTORY_QUERY_LIMIT: usize = 40;
const HISTORY_QUERY_LIMIT_MAX: usize = 200;

#[model_schema]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadHistoryArgs {
    /// recent = newest-first page; range = forward page from start_seq;
    /// search = keyword (substring, case-insensitive) match in archived messages.
    #[serde(default)]
    mode: Option<HistoryArchiveQueryMode>,
    /// Maximum number of messages to return (1..=200, default 40).
    #[serde(default)]
    limit: Option<usize>,
    /// Continue paging into older history (recent/search): only seq < before_seq.
    #[serde(default)]
    before_seq: Option<i64>,
    /// range mode start cursor (inclusive); ignored by recent/search.
    #[serde(default)]
    start_seq: Option<i64>,
    /// Substring filter for range/recent; required for search.
    #[serde(default)]
    query: Option<String>,
    /// Set true to include full tool outputs instead of an omission placeholder.
    #[serde(default)]
    include_tool_output: Option<bool>,
}

pub(super) fn register_tools() -> Vec<Box<dyn RuntimeTool>> {
    vec![Box::new(
        StaticRuntimeTool::new_with_schema_and_availability(
            "read_history",
            "Read archived conversation history that was cleared by a runtime context compaction. Use it to recover the task state after the context was reset with the recovery prompt. recent returns the newest messages; range reads forward from start_seq; search finds messages whose text contains query. Each page returns next_seq for continued paging; tool outputs are omitted unless include_tool_output=true.",
            model_schema_for::<ReadHistoryArgs>(),
            |context: &Context| context.dashboard_history.is_some(),
            summarize_read_history_tool,
            render_read_history_call_ui,
            execute_read_history_runtime_tool,
        ),
    )]
}

fn history_mode_str(mode: HistoryArchiveQueryMode) -> &'static str {
    match mode {
        HistoryArchiveQueryMode::Recent => "recent",
        HistoryArchiveQueryMode::Range => "range",
        HistoryArchiveQueryMode::Search => "search",
    }
}

fn summarize_read_history_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: ReadHistoryArgs = parse_tool_args(call)?;
    let mode = args.mode.unwrap_or(HistoryArchiveQueryMode::Recent);
    Ok(EpisodeActionRecord {
        kind: "read_history".to_string(),
        summary: format!(
            "mode={} limit={} query={}",
            history_mode_str(mode),
            args.limit.unwrap_or(DEFAULT_HISTORY_QUERY_LIMIT),
            args.query.as_deref().unwrap_or("")
        ),
    })
}

fn render_read_history_call_ui(call: &AgentToolCall) -> Result<ToolCallActivityEvent> {
    let args: ReadHistoryArgs = parse_tool_args(call)?;
    let mode = args.mode.unwrap_or(HistoryArchiveQueryMode::Recent);
    Ok(ToolCallActivityEvent::app(
        "Read History",
        vec![format!(
            "mode={} limit={} query={}",
            history_mode_str(mode),
            args.limit.unwrap_or(DEFAULT_HISTORY_QUERY_LIMIT),
            args.query.as_deref().unwrap_or("")
        )],
    ))
}

fn execute_read_history_runtime_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let store = context
            .dashboard_history
            .as_ref()
            .ok_or_else(|| miette!("read_history requires an active session history store"))?;
        let max_tokens = context
            .config
            .main_model_config()
            .tool_output_max_tokens
            .max(1);
        execute_read_history_with_store(call, store, max_tokens).await
    })
}

/// Worker-facing entry: workflow workers reach the archive store through the
/// worker runtime tool call context instead of a full `Context`.
pub fn execute_worker_read_history<'a>(
    call: &'a AgentToolCall,
    store: &'a DashboardActivityHistoryStore,
    tool_output_max_tokens: usize,
) -> ToolFuture<'a> {
    Box::pin(
        async move { execute_read_history_with_store(call, store, tool_output_max_tokens).await },
    )
}

async fn execute_read_history_with_store(
    call: &AgentToolCall,
    store: &DashboardActivityHistoryStore,
    tool_output_max_tokens: usize,
) -> Result<ToolExecutionResult> {
    let args: ReadHistoryArgs = parse_tool_args(call)?;
    let mode = args.mode.unwrap_or(HistoryArchiveQueryMode::Recent);
    let limit = args
        .limit
        .unwrap_or(DEFAULT_HISTORY_QUERY_LIMIT)
        .clamp(1, HISTORY_QUERY_LIMIT_MAX);
    let query = args.query.unwrap_or_default();
    let include_tool_output = args.include_tool_output.unwrap_or(false);
    let items =
        store.query_history_archive(mode, limit, args.before_seq, args.start_seq, &query)?;
    let total = store.count_history_archive(mode, &query)?;
    let max_tokens = tool_output_max_tokens.max(1);
    let max_chars = max_tokens.saturating_mul(APPROX_BYTES_PER_TOKEN).max(1);

    let mut content = String::new();
    let mut rendered: Vec<HistoryArchiveItem> = Vec::new();
    let mut truncated = false;
    let mut next_seq = None;
    for item in &items {
        let text = if item.role == "tool" && !include_tool_output {
            match &item.tool_name {
                Some(name) => format!(
                    "<tool `{name}` output omitted; call read_history with include_tool_output=true to read it>"
                ),
                None => {
                    "<tool output omitted; call read_history with include_tool_output=true to read it>"
                        .to_string()
                }
            }
        } else {
            item.content.clone()
        };
        let line = format!("seq={} [{}] {}\n", item.seq, item.role, text);
        if content.chars().count().saturating_add(line.chars().count()) > max_chars {
            truncated = true;
            next_seq = Some(item.seq);
            break;
        }
        content.push_str(&line);
        rendered.push(HistoryArchiveItem {
            seq: item.seq,
            role: item.role.clone(),
            tool_name: item.tool_name.clone(),
            content: text,
        });
    }
    if !truncated {
        next_seq = items.last().map(|item| match mode {
            HistoryArchiveQueryMode::Recent | HistoryArchiveQueryMode::Search => {
                item.seq.saturating_sub(1)
            }
            HistoryArchiveQueryMode::Range => item.seq.saturating_add(1),
        });
    }
    let mode_str = history_mode_str(mode);
    let header = format!(
        "mode={mode_str} limit={limit} returned={} total={total} next_seq={} truncated={truncated}",
        rendered.len(),
        next_seq.map_or_else(|| "none".to_string(), |seq| seq.to_string()),
    );
    let model_content = format!("{header}\n{content}").trim_end().to_string();
    let payload = json!({
        "mode": mode_str,
        "total": total,
        "returned": rendered.len(),
        "next_seq": next_seq,
        "truncated": truncated,
        "items": rendered,
    });
    Ok(ToolExecutionResult::from_activity_event(
        format!("read history ({mode_str}, {} of {total})", rendered.len()),
        payload,
        Some(SessionActivityEvent::GenericApp(
            TextActivityDescriptor {
                title: "Read History".to_string(),
                body_lines: vec![header],
            }
            .into(),
        )),
    )
    .with_model_content(model_content))
}
