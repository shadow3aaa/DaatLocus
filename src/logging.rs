use serde_json::to_string_pretty;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

use crate::{
    context_budget::RequestBudgetBreakdown,
    daat_locus_paths::DaatLocusPaths,
    model_protocol::{
        AgentContentPart, AgentMessage, AgentTurnItem, AgentTurnRequest, AgentTurnStreamResult,
        render_assistant_tool_call_protocol_dump,
    },
};

pub const DAEMON_LOG_FILE_NAME: &str = "daat-locus.log";
const RAW_MESSAGES_DUMP_FILE_NAME: &str = "messages.txt";
const RAW_LAST_RESPONSE_DUMP_FILE_NAME: &str = "last_response.txt";

/// Initialize file logging and return the guard that must be held until process exit.
pub async fn init_logging(paths: &DaatLocusPaths) -> WorkerGuard {
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
            .with_timer(tracing_subscriber::fmt::time::ChronoLocal::rfc_3339())
            .with_writer(non_blocking)
            .try_init();
        return guard;
    }

    let file_appender = tracing_appender::rolling::never(&log_dir, DAEMON_LOG_FILE_NAME);
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    if let Err(err) = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_ansi(false)
        .with_target(true)
        .with_thread_ids(true)
        .with_timer(tracing_subscriber::fmt::time::ChronoLocal::rfc_3339())
        .with_writer(non_blocking)
        .try_init()
    {
        // Global subscriber was already set, which should not normally happen; fall back to stderr.
        eprintln!("init_logging: tracing subscriber already set, falling back to stderr: {err}");
        let (nb_stderr, guard_stderr) = tracing_appender::non_blocking(std::io::stderr());
        let _ = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::new("daat_locus=info,warn"))
            .with_ansi(false)
            .with_timer(tracing_subscriber::fmt::time::ChronoLocal::rfc_3339())
            .with_writer(nb_stderr)
            .try_init();
        return guard_stderr;
    }

    guard
}

pub async fn write_current_turn_messages_dump(
    system: &crate::actors::ActorSystem,
    actor: &str,
    request: &AgentTurnRequest,
    budget: &RequestBudgetBreakdown,
    model_name: Option<&str>,
) -> miette::Result<()> {
    let body = render_current_turn_messages_dump(request, budget, model_name);
    write_current_turn_raw_file(system, actor, RAW_MESSAGES_DUMP_FILE_NAME, body).await
}

pub async fn write_current_turn_response_dump(
    system: &crate::actors::ActorSystem,
    actor: &str,
    response: &AgentTurnStreamResult,
    attempt: usize,
) -> miette::Result<()> {
    let body = render_current_turn_response_dump(response, attempt);
    write_current_turn_raw_file(system, actor, RAW_LAST_RESPONSE_DUMP_FILE_NAME, body).await
}

pub async fn write_current_turn_response_error_dump(
    system: &crate::actors::ActorSystem,
    actor: &str,
    error: &str,
    attempt: usize,
    will_retry: bool,
) -> miette::Result<()> {
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
        system,
        actor,
        RAW_LAST_RESPONSE_DUMP_FILE_NAME,
        lines.join("\n"),
    )
    .await
}

async fn write_current_turn_raw_file(
    system: &crate::actors::ActorSystem,
    actor: &str,
    file_name: &str,
    body: String,
) -> miette::Result<()> {
    use miette::IntoDiagnostic;
    let directory = system.actor_state_dir(actor).join("diagnostics");
    crate::file_io::ensure_directory(
        &crate::sandbox::RuntimeSandboxPolicy::disabled(),
        &directory,
    )?;
    crate::actors::permissions::protect_private_path(&directory)?;
    crate::persistence::write_bytes_atomic(
        directory.join(file_name),
        system.redact_secrets(&body).into_bytes(),
        crate::persistence::PersistenceFileMode::Private,
    )
    .await
    .into_diagnostic()
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
                    AgentContentPart::ImageData {
                        media_type,
                        data_base64,
                        ..
                    } => lines.push(format!(
                        "part[{index}]=resolved-image media_type={media_type} encoded_bytes={}",
                        data_base64.len()
                    )),
                    AgentContentPart::ResourceImage { resource, .. } => lines.push(format!(
                        "part[{index}]=image resource={} media_type={}",
                        resource.id, resource.media_type
                    )),
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
        AgentMessage::Assistant { content, .. } => {
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
            ..
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

#[cfg(test)]
mod tests {
    use tracing_subscriber::fmt::format::Writer;
    use tracing_subscriber::fmt::time::FormatTime;

    #[test]
    fn log_timer_emits_local_timezone_offset() {
        let timer = tracing_subscriber::fmt::time::ChronoLocal::rfc_3339();
        let mut out = String::new();
        timer
            .format_time(&mut Writer::new(&mut out))
            .expect("timer should format into the writer");

        let local_minus_utc = chrono::Local::now().offset().local_minus_utc();
        if local_minus_utc == 0 {
            // Machine timezone is UTC; RFC3339 may render `Z` or `+00:00`.
            assert!(
                out.ends_with('Z') || out.ends_with("+00:00"),
                "unexpected UTC timestamp format: {out}"
            );
        } else {
            // Machine timezone is not UTC: the timestamp must carry the local offset,
            // never the UTC `Z` suffix.
            assert!(
                !out.ends_with('Z') && !out.ends_with("+00:00"),
                "log timestamp uses UTC instead of local timezone: {out}"
            );
            let local_offset = chrono::Local::now().format("%:z").to_string();
            assert!(
                out.ends_with(&local_offset),
                "log timestamp {out} does not end with local offset {local_offset}"
            );
        }
    }
}
