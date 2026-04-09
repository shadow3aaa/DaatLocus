use std::sync::OnceLock;

use serde_json::to_string_pretty;
use tokio::sync::watch::Sender;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

use crate::{
    context_budget::RequestBudgetBreakdown,
    daat_locus_paths::daat_locus_paths,
    dashboard::DashboardState,
    reasoning::runtime::{
        AgentMessage, AgentTurnItem, AgentTurnRequest, AgentTurnStreamResult,
        render_assistant_tool_call_protocol_dump,
    },
};

static LOG_GUARD: OnceLock<WorkerGuard> = OnceLock::new();

#[derive(Clone, Copy)]
pub enum RuntimeStatusLevel {
    Debug,
    Info,
    Warn,
    Error,
}

pub async fn init_logging() {
    let log_dir = daat_locus_paths().await.logs_dir();
    if let Err(err) = tokio::fs::create_dir_all(&log_dir).await {
        eprintln!(
            "failed to create log directory {}: {err}",
            log_dir.display()
        );
        return;
    }

    let file_appender = tracing_appender::rolling::never(log_dir, "daat-locus.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    let _ = LOG_GUARD.set(guard);

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("daat_locus=info,warn"));

    let _ = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_ansi(false)
        .with_target(true)
        .with_thread_ids(true)
        .with_writer(non_blocking)
        .try_init();
}

pub fn set_runtime_status(
    tx: Option<&Sender<DashboardState>>,
    level: RuntimeStatusLevel,
    message: impl Into<String>,
) {
    let message = message.into();
    match level {
        RuntimeStatusLevel::Debug => tracing::debug!("{message}"),
        RuntimeStatusLevel::Info => tracing::info!("{message}"),
        RuntimeStatusLevel::Warn => tracing::warn!("{message}"),
        RuntimeStatusLevel::Error => tracing::error!("{message}"),
    }
    if let Some(tx) = tx {
        tx.send_modify(|state| state.runtime_status = Some(message.clone()));
    }
}

pub fn clear_runtime_status(tx: Option<&Sender<DashboardState>>) {
    if let Some(tx) = tx {
        tx.send_modify(|state| state.runtime_status = None);
    }
}

pub async fn write_current_turn_messages_dump(
    request: &AgentTurnRequest,
    budget: &RequestBudgetBreakdown,
    model_name: Option<&str>,
) {
    let body = render_current_turn_messages_dump(request, budget, model_name);
    write_current_turn_log_file("current_turn_messages.txt", body).await;
}

pub async fn write_current_turn_response_dump(response: &AgentTurnStreamResult, attempt: usize) {
    let body = render_current_turn_response_dump(response, attempt);
    write_current_turn_log_file("current_turn_response.txt", body).await;
}

pub async fn write_current_turn_response_error_dump(error: &str, attempt: usize, will_retry: bool) {
    let mut lines = vec![
        "status=error".to_string(),
        format!("attempt={attempt}"),
        format!("will_retry={will_retry}"),
        String::new(),
        "error:".to_string(),
        error.to_string(),
    ];
    lines.push(String::new());
    lines.push("note=contents reflect the latest LLM request outcome for this turn".to_string());
    write_current_turn_log_file("current_turn_response.txt", lines.join("\n")).await;
}

async fn write_current_turn_log_file(file_name: &str, body: String) {
    let paths = daat_locus_paths().await;
    let log_dir = paths.logs_dir();
    if let Err(err) = tokio::fs::create_dir_all(&log_dir).await {
        tracing::warn!(
            "failed to create log directory for current turn dump {}: {err}",
            log_dir.display()
        );
        return;
    }

    let dump_path = paths.logs_file(file_name);
    if let Err(err) = tokio::fs::write(&dump_path, body).await {
        tracing::warn!(
            "failed to write current turn dump {}: {err}",
            dump_path.display()
        );
    }
}

fn render_current_turn_messages_dump(
    request: &AgentTurnRequest,
    budget: &RequestBudgetBreakdown,
    model_name: Option<&str>,
) -> String {
    let mut lines = vec![
        format!(
            "model={}",
            model_name
                .filter(|value| !value.trim().is_empty())
                .unwrap_or("unknown")
        ),
        format!("message_count={}", request.messages.len()),
        format!("tool_count={}", request.tools.len()),
    ];
    lines.extend(budget.summary_lines());

    if !request.tools.is_empty() {
        lines.push(String::new());
        lines.push("== Tools ==".to_string());
        for tool in &request.tools {
            lines.push(format!("- {}", tool.name));
        }
    }

    for (index, message) in request.messages.iter().enumerate() {
        lines.push(String::new());
        lines.push(format!("== Message {} ==", index + 1));
        lines.extend(render_agent_message_dump(message));
    }

    lines.join("\n")
}

fn render_current_turn_response_dump(response: &AgentTurnStreamResult, attempt: usize) -> String {
    let mut lines = vec![
        "status=ok".to_string(),
        format!("attempt={attempt}"),
        format!("item_count={}", response.items.len()),
        format!(
            "raw_stream_follow_up={}",
            if response.raw_stream_follow_up {
                "true"
            } else {
                "false"
            }
        ),
        format!(
            "last_assistant_message={}",
            response
                .last_assistant_message
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or("none")
        ),
    ];

    for (index, item) in response.items.iter().enumerate() {
        lines.push(String::new());
        lines.push(format!("== Item {} ==", index + 1));
        lines.extend(render_agent_turn_item_dump(item));
    }

    lines.join("\n")
}

fn render_agent_message_dump(message: &AgentMessage) -> Vec<String> {
    match message {
        AgentMessage::System { content } => {
            vec![
                "role=system".to_string(),
                "content:".to_string(),
                content.clone(),
            ]
        }
        AgentMessage::User { content } => {
            vec![
                "role=user".to_string(),
                "content:".to_string(),
                content.clone(),
            ]
        }
        AgentMessage::Assistant { content } => {
            vec![
                "role=assistant".to_string(),
                "content:".to_string(),
                content.clone(),
            ]
        }
        AgentMessage::AssistantToolCallProtocol { content, calls } => {
            render_assistant_tool_call_protocol_dump(content.as_deref(), calls)
        }
        AgentMessage::Tool {
            tool_call_id,
            name,
            content,
        } => vec![
            "role=tool".to_string(),
            format!("tool_call_id={tool_call_id}"),
            format!("name={name}"),
            "content:".to_string(),
            content.clone(),
        ],
    }
}

fn render_agent_turn_item_dump(item: &AgentTurnItem) -> Vec<String> {
    match item {
        AgentTurnItem::AssistantMessage { content } => vec![
            "kind=assistant_message".to_string(),
            "content:".to_string(),
            content.clone(),
        ],
        AgentTurnItem::ToolCall { call } => vec![
            "kind=tool_call".to_string(),
            format!("id={}", call.id),
            format!("name={}", call.name),
            "arguments:".to_string(),
            to_string_pretty(&call.arguments).unwrap_or_else(|_| call.arguments.to_string()),
        ],
    }
}
