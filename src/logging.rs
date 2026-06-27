use serde_json::to_string_pretty;
use tokio::sync::watch::Sender;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

use crate::{
    context_budget::RequestBudgetBreakdown,
    daat_locus_paths::{DaatLocusPaths, daat_locus_paths},
    dashboard::{DashboardRuntimeStatusLevel, DashboardState},
    reasoning::runtime::{
        AgentContentPart, AgentMessage, AgentTurnItem, AgentTurnRequest, AgentTurnStreamResult,
        render_assistant_tool_call_protocol_dump,
    },
};

pub const DAEMON_LOG_FILE_NAME: &str = "daat-locus.log";
pub const SESSION_LOG_FILE_NAME: &str = "session.log";
const RAW_MESSAGES_DUMP_FILE_NAME: &str = "messages.txt";
const RAW_LAST_RESPONSE_DUMP_FILE_NAME: &str = "last_response.txt";

#[derive(Clone, Copy)]
pub enum RuntimeStatusLevel {
    Info,
    Warn,
    Error,
}

impl RuntimeStatusLevel {
    fn dashboard_level(self) -> DashboardRuntimeStatusLevel {
        match self {
            Self::Info => DashboardRuntimeStatusLevel::Info,
            Self::Warn => DashboardRuntimeStatusLevel::Warn,
            Self::Error => DashboardRuntimeStatusLevel::Error,
        }
    }
}

/// Initialize file logging and return the guard that must be held until process exit.
pub async fn init_logging(session_id: Option<&str>) -> WorkerGuard {
    let paths = paths_for_logging_scope(session_id).await;
    let log_dir = paths.logs_dir();
    if let Err(err) = tokio::fs::create_dir_all(&log_dir).await {
        eprintln!(
            "failed to create log directory {}: {err}",
            log_dir.display()
        );
        // Fall back to stderr and return a /dev/null appender guard.
        let (non_blocking, guard) = tracing_appender::non_blocking(std::io::sink());
        let _ = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::new("warn"))
            .with_ansi(false)
            .with_writer(non_blocking)
            .try_init();
        return guard;
    }

    let log_file_name = if has_session_scope(session_id) {
        SESSION_LOG_FILE_NAME
    } else {
        DAEMON_LOG_FILE_NAME
    };
    let file_appender = tracing_appender::rolling::never(&log_dir, log_file_name);
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    if let Err(err) = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_ansi(false)
        .with_target(true)
        .with_thread_ids(true)
        .with_writer(non_blocking)
        .try_init()
    {
        // Global subscriber was already set, which should not normally happen; fall back to stderr.
        eprintln!("init_logging: tracing subscriber already set, falling back to stderr: {err}");
        let (nb_stderr, guard_stderr) = tracing_appender::non_blocking(std::io::stderr());
        let _ = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::new("daat_locus=info,warn"))
            .with_ansi(false)
            .with_writer(nb_stderr)
            .try_init();
        return guard_stderr;
    }

    guard
}

fn has_session_scope(session_id: Option<&str>) -> bool {
    session_id.is_some_and(|value| !value.trim().is_empty())
}

async fn paths_for_logging_scope(session_id: Option<&str>) -> DaatLocusPaths {
    if let Some(paths) = session_paths_for_logging_scope(session_id) {
        paths
    } else {
        daat_locus_paths().await
    }
}

fn session_paths_for_logging_scope(session_id: Option<&str>) -> Option<DaatLocusPaths> {
    session_id
        .filter(|value| !value.trim().is_empty())
        .map(DaatLocusPaths::for_session)
}

pub fn set_runtime_status(
    tx: Option<&Sender<DashboardState>>,
    level: RuntimeStatusLevel,
    message: impl Into<String>,
) {
    let message = message.into();
    match level {
        RuntimeStatusLevel::Info => tracing::info!("{message}"),
        RuntimeStatusLevel::Warn => tracing::warn!("{message}"),
        RuntimeStatusLevel::Error => tracing::error!("{message}"),
    }
    set_dashboard_runtime_status(tx, message, Some(level.dashboard_level()));
}

pub fn set_runtime_status_only(tx: Option<&Sender<DashboardState>>, message: impl Into<String>) {
    set_dashboard_runtime_status(tx, message.into(), None);
}

fn set_dashboard_runtime_status(
    tx: Option<&Sender<DashboardState>>,
    message: String,
    level: Option<DashboardRuntimeStatusLevel>,
) {
    if let Some(tx) = tx {
        tx.send_if_modified(|state| {
            if state.runtime_status.as_deref() == Some(message.as_str())
                && state.runtime_status_level == level
            {
                false
            } else {
                state.runtime_status = Some(message.clone());
                state.runtime_status_level = level;
                true
            }
        });
    }
}

pub fn clear_runtime_status(tx: Option<&Sender<DashboardState>>) {
    if let Some(tx) = tx {
        tx.send_if_modified(|state| {
            if state.runtime_status.is_none() && state.runtime_status_level.is_none() {
                false
            } else {
                state.runtime_status = None;
                state.runtime_status_level = None;
                true
            }
        });
    }
}

pub async fn write_current_turn_messages_dump(
    session_id: Option<&str>,
    request: &AgentTurnRequest,
    budget: &RequestBudgetBreakdown,
    model_name: Option<&str>,
) {
    let body = render_current_turn_messages_dump(request, budget, model_name);
    write_current_turn_raw_file(session_id, RAW_MESSAGES_DUMP_FILE_NAME, body).await;
}

pub async fn write_current_turn_response_dump(
    session_id: Option<&str>,
    response: &AgentTurnStreamResult,
    attempt: usize,
) {
    let body = render_current_turn_response_dump(response, attempt);
    write_current_turn_raw_file(session_id, RAW_LAST_RESPONSE_DUMP_FILE_NAME, body).await;
}

pub async fn write_current_turn_response_error_dump(
    session_id: Option<&str>,
    error: &str,
    attempt: usize,
    will_retry: bool,
) {
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
    write_current_turn_raw_file(
        session_id,
        RAW_LAST_RESPONSE_DUMP_FILE_NAME,
        lines.join("\n"),
    )
    .await;
}

async fn write_current_turn_raw_file(session_id: Option<&str>, file_name: &str, body: String) {
    let Some(paths) = session_paths_for_logging_scope(session_id) else {
        tracing::warn!(
            "skipping current turn raw dump `{file_name}` because no session id is available"
        );
        return;
    };
    let raw_dir = paths.raw_dir();
    if let Err(err) = tokio::fs::create_dir_all(&raw_dir).await {
        tracing::warn!(
            "failed to create raw directory for current turn dump {}: {err}",
            raw_dir.display()
        );
        return;
    }

    let dump_path = paths.raw_file(file_name);
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
            let mut lines = vec![
                "role=user".to_string(),
                "content:".to_string(),
                content.as_text().to_string(),
            ];
            for (index, part) in content.parts().iter().enumerate() {
                match part {
                    AgentContentPart::Text { text } => {
                        lines.push(format!("part[{index}]=text chars={}", text.chars().count()));
                    }
                    AgentContentPart::Image {
                        path,
                        media_type,
                        description,
                    } => {
                        lines.push(format!(
                            "part[{index}]=image media_type={media_type} path={path} description={}",
                            description.as_deref().unwrap_or("")
                        ));
                    }
                }
            }
            lines
        }
        AgentMessage::Assistant { content } => {
            vec![
                "role=assistant".to_string(),
                "content:".to_string(),
                content.clone(),
            ]
        }
        AgentMessage::AssistantToolCallProtocol {
            content,
            reasoning_content,
            calls,
        } => render_assistant_tool_call_protocol_dump(
            content.as_deref(),
            reasoning_content.as_deref(),
            calls,
        ),
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
