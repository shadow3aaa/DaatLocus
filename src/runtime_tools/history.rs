use daat_locus_macros::model_schema;
use miette::{Result, miette};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::{
    activity_event::{
        ReadHistoryActivityDescriptor, ReadHistoryEntryActivityDescriptor, ToolCallActivityEvent,
    },
    context::Context,
    context_budget::APPROX_BYTES_PER_TOKEN,
    dashboard::SessionActivityEvent,
    dashboard::{DashboardActivityHistoryStore, HistoryQueryItem, HistoryQueryMode},
    reasoning::{episode::EpisodeActionRecord, runtime::AgentToolCall},
    runtime_tools::{
        RuntimeTool, StaticRuntimeTool, ToolExecutionResult, ToolFuture, parse_tool_args,
    },
    schema_utils::model_schema_for,
};

const DEFAULT_HISTORY_QUERY_LIMIT: usize = 40;
const HISTORY_QUERY_LIMIT_MAX: usize = 200;
/// The activity card previews only the head of a page; the full page still
/// reaches the model through the tool output.
const READ_HISTORY_UI_ITEM_LIMIT: usize = 12;
const READ_HISTORY_PREVIEW_MAX_CHARS: usize = 160;

#[model_schema]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadHistoryArgs {
    /// recent = newest-first page; range = forward page from start_seq;
    /// search = keyword (substring, case-insensitive) match in the session history.
    #[serde(default)]
    mode: Option<HistoryQueryMode>,
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
}

pub(super) fn register_tools() -> Vec<Box<dyn RuntimeTool>> {
    vec![Box::new(
        StaticRuntimeTool::new_with_schema_and_availability(
            "read_history",
            "Page over this session's conversation history. Use it to recover the task state after a context overflow reset the runtime history with the recovery prompt. recent returns the newest entries; range reads forward from start_seq; search finds entries whose text contains query. Each page returns next_seq for continued paging.",
            model_schema_for::<ReadHistoryArgs>(),
            |context: &Context| context.dashboard_history.is_some(),
            summarize_read_history_tool,
            render_read_history_call_ui,
            execute_read_history_runtime_tool,
        ),
    )]
}

fn history_mode_str(mode: HistoryQueryMode) -> &'static str {
    match mode {
        HistoryQueryMode::Recent => "recent",
        HistoryQueryMode::Range => "range",
        HistoryQueryMode::Search => "search",
    }
}

fn summarize_read_history_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: ReadHistoryArgs = parse_tool_args(call)?;
    let mode = args.mode.unwrap_or(HistoryQueryMode::Recent);
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
    let mode = args.mode.unwrap_or(HistoryQueryMode::Recent);
    Ok(ToolCallActivityEvent::read_history(
        ReadHistoryActivityDescriptor {
            mode: history_mode_str(mode).to_string(),
            query: args.query.clone(),
            limit: args.limit.unwrap_or(DEFAULT_HISTORY_QUERY_LIMIT),
            returned: 0,
            total: 0,
            next_seq: None,
            truncated: false,
            loading: true,
            items: Vec::new(),
        },
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
    let mode = args.mode.unwrap_or(HistoryQueryMode::Recent);
    let limit = args
        .limit
        .unwrap_or(DEFAULT_HISTORY_QUERY_LIMIT)
        .clamp(1, HISTORY_QUERY_LIMIT_MAX);
    let query = args.query.unwrap_or_default();
    let items = store.query_history(mode, limit, args.before_seq, args.start_seq, &query)?;
    // `count_history` is SQL `COUNT(*)` for both empty and non-empty needles.
    let total = store.count_history(&query)?;
    let max_tokens = tool_output_max_tokens.max(1);
    let max_chars = max_tokens.saturating_mul(APPROX_BYTES_PER_TOKEN).max(1);

    let mut content = String::new();
    let mut rendered: Vec<HistoryQueryItem> = Vec::new();
    let mut truncated = false;
    let mut next_seq = None;
    for item in &items {
        let line = format!("seq={} [{}] {}\n", item.seq, item.role, item.content);
        if content.chars().count().saturating_add(line.chars().count()) > max_chars {
            truncated = true;
            next_seq = Some(item.seq);
            break;
        }
        content.push_str(&line);
        rendered.push(item.clone());
    }
    if !truncated {
        next_seq = items.last().map(|item| match mode {
            HistoryQueryMode::Recent | HistoryQueryMode::Search => item.seq.saturating_sub(1),
            HistoryQueryMode::Range => item.seq.saturating_add(1),
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
    let items = rendered
        .iter()
        .take(READ_HISTORY_UI_ITEM_LIMIT)
        .map(|item| ReadHistoryEntryActivityDescriptor {
            seq: item.seq,
            role: item.role.clone(),
            preview: history_entry_preview(&item.content),
        })
        .collect();
    Ok(ToolExecutionResult::from_activity_event(
        format!("read history ({mode_str}, {} of {total})", rendered.len()),
        payload,
        Some(SessionActivityEvent::ReadHistory(
            ReadHistoryActivityDescriptor {
                mode: mode_str.to_string(),
                query: (!query.trim().is_empty()).then(|| query.clone()),
                limit,
                returned: rendered.len(),
                total,
                next_seq,
                truncated,
                loading: false,
                items,
            }
            .into(),
        )),
    )
    .with_model_content(model_content))
}

/// Compact one-line preview of a history entry for the activity card; the full
/// entry text stays in the model-facing output.
fn history_entry_preview(content: &str) -> String {
    let first_line = content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let mut preview: String = first_line
        .chars()
        .take(READ_HISTORY_PREVIEW_MAX_CHARS)
        .collect();
    if first_line.chars().count() > READ_HISTORY_PREVIEW_MAX_CHARS {
        preview.push('\u{2026}');
    }
    preview
}
