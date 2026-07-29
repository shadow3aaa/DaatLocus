use super::{
    AgentMessage, AgentTurnRequest, AgentTurnStreamResult, Context, DashboardState, Duration,
    Result, RuntimeStatusLevel, clear_runtime_status, render_dashboard_footer_context,
    set_runtime_status, set_runtime_status_only, write_current_turn_messages_dump,
    write_current_turn_response_dump, write_current_turn_response_error_dump,
};
use crate::{
    context_budget::{
        TokenEstimateBaseline, estimate_agent_message_tokens, estimate_tool_spec_tokens,
    },
    core::{
        AgentTurnRetryObserver, ModelProgressSink, ModelRequestOptions,
        complete_agent_turn_with_retry_with_observer,
    },
    dashboard::{
        DashboardContextCompositionPrefixUnit, DashboardContextCompositionSegment,
        DashboardContextCompositionSnapshot,
    },
    reasoning::prompts::{MID_TURN_SUMMARY_PREFIX, RUNTIME_HISTORY_SUMMARY_PREFIX},
    reasoning::runtime::AgentToolSpec,
    runtime::bootstrap::save_token_estimate_baseline,
};

use sha2::{Digest, Sha256};

pub(super) async fn run_agent_turn_with_retry(
    context: &mut Context,
    request: AgentTurnRequest,
    tx: Option<&tokio::sync::watch::Sender<DashboardState>>,
) -> Result<AgentTurnStreamResult> {
    let options = ModelRequestOptions::for_agent_turn(
        context.model_provider.as_ref(),
        &request,
        context.session_id.clone(),
    )?
    .with_progress(current_model_progress_sink(context));
    let estimated_input_tokens = options.budget.total_input_tokens;
    let session_id = context.session_id.clone();
    let model_name = context.model_provider.model_name();
    write_current_turn_messages_dump(
        session_id.as_deref(),
        &request,
        &options.budget,
        Some(model_name.as_str()),
    )
    .await;
    let context_composition = build_context_composition_snapshot(
        context.latest_context_composition.as_ref(),
        context,
        &request,
        options.budget.context_window_tokens,
    );
    context.latest_context_composition = Some(context_composition.clone());
    if let Some(tx) = tx {
        tx.send_modify(|state| {
            state.footer_estimated_input_tokens = Some(estimated_input_tokens);
            state.footer_context =
                render_dashboard_footer_context(context, state.footer_estimated_input_tokens);
            state.context_composition = Some(context_composition.clone());
        });
    }
    set_runtime_status_only(tx, "Working");
    let on_attempt_started = |_: usize| set_runtime_status_only(tx, "Working");
    let on_attempt_failed =
        |error_detail: &str, attempt: usize, retry_backoff: Option<Duration>| {
            if let Some(backoff) = retry_backoff {
                set_runtime_status(
                    tx,
                    RuntimeStatusLevel::Warn,
                    format!(
                        "request failed; retry #{attempt} after {:.1}s",
                        backoff.as_secs_f64()
                    ),
                );
            }
            let session_id = session_id.clone();
            let error_detail = error_detail.to_string();
            tokio::spawn(async move {
                write_current_turn_response_error_dump(
                    session_id.as_deref(),
                    &error_detail,
                    attempt,
                    retry_backoff.is_some(),
                )
                .await;
            });
        };
    match complete_agent_turn_with_retry_with_observer(
        context.model_provider.as_ref(),
        request,
        options,
        AgentTurnRetryObserver {
            on_attempt_started: Some(&on_attempt_started),
            on_attempt_failed: Some(&on_attempt_failed),
        },
    )
    .await
    {
        Ok(success) => {
            write_current_turn_response_dump(
                session_id.as_deref(),
                &success.response,
                success.attempts,
            )
            .await;
            let info = context.model_provider.token_usage_info();
            let observed_input =
                usize::try_from(info.last_token_usage.input_tokens.max(0)).unwrap_or(0);
            if observed_input > 0 {
                context.token_estimate_baseline = TokenEstimateBaseline {
                    estimated_input_tokens,
                    observed_input_tokens: Some(observed_input),
                };
                save_token_estimate_baseline(
                    context.session_id.as_deref(),
                    &context.token_estimate_baseline,
                )
                .await;
            }
            clear_runtime_status(tx);
            Ok(success.response)
        }
        Err(failure) => {
            clear_runtime_status(tx);
            Err(failure.error)
        }
    }
}

fn current_model_progress_sink(context: &Context) -> Option<ModelProgressSink> {
    context
        .live_progress_tx
        .lock()
        .as_ref()
        .cloned()
        .map(ModelProgressSink::new)
}

fn build_context_composition_snapshot(
    previous: Option<&DashboardContextCompositionSnapshot>,
    context: &Context,
    request: &AgentTurnRequest,
    model_context_window: usize,
) -> DashboardContextCompositionSnapshot {
    let mut segments = request
        .messages
        .iter()
        .enumerate()
        .map(|(index, message)| context_composition_message_segment(index, message))
        .collect::<Vec<_>>();
    segments.extend(
        request
            .tools
            .iter()
            .enumerate()
            .map(|(index, tool)| context_composition_tool_segment(index, tool)),
    );

    let total_estimated_tokens = segments.iter().map(|segment| segment.tokens).sum::<usize>();
    let total_bytes = segments.iter().map(|segment| segment.bytes).sum::<usize>();
    for segment in &mut segments {
        segment.percent = percent_of(segment.tokens, total_estimated_tokens);
    }

    let prefix_units = segments
        .iter()
        .map(|segment| DashboardContextCompositionPrefixUnit {
            hash: segment.hash.clone(),
            tokens: segment.tokens,
        })
        .collect::<Vec<_>>();
    let previous_units: &[DashboardContextCompositionPrefixUnit] =
        previous.map_or(&[], |snapshot| snapshot.prefix_units.as_slice());
    let common_unit_count = prefix_units
        .iter()
        .zip(previous_units.iter())
        .take_while(|(left, right)| left.hash == right.hash)
        .count();
    let previous_common_prefix_tokens = prefix_units
        .iter()
        .take(common_unit_count)
        .map(|unit| unit.tokens)
        .sum::<usize>();
    let stable_prefix_tokens = previous_common_prefix_tokens;
    let new_suffix_tokens = prefix_units
        .iter()
        .skip(common_unit_count)
        .map(|unit| unit.tokens)
        .sum::<usize>();
    let changed_prefix_tokens = previous_units
        .iter()
        .skip(common_unit_count)
        .map(|unit| unit.tokens)
        .sum::<usize>();
    let tools_schema_tokens = request
        .tools
        .iter()
        .map(estimate_tool_spec_tokens)
        .sum::<usize>();

    DashboardContextCompositionSnapshot {
        captured_at_ms: Some(chrono::Utc::now().timestamp_millis()),
        model: Some(context.model_provider.model_name()),
        model_context_window: Some(model_context_window),
        total_estimated_tokens,
        total_bytes,
        message_count: request.messages.len(),
        tool_count: request.tools.len(),
        tools_schema_tokens,
        stable_prefix_tokens,
        new_suffix_tokens,
        changed_prefix_tokens,
        previous_common_prefix_tokens,
        previous_request_hash: previous.and_then(|snapshot| snapshot.current_request_hash.clone()),
        current_request_hash: Some(hash_text(&request_fingerprint_input(&prefix_units))),
        segments,
        prefix_units,
    }
}

fn context_composition_message_segment(
    index: usize,
    message: &AgentMessage,
) -> DashboardContextCompositionSegment {
    let source = context_composition_message_source(message);
    let rendered = serde_json::to_string(message).unwrap_or_else(|_| source.to_string());
    let name = context_composition_message_name(message);
    DashboardContextCompositionSegment {
        label: context_composition_label_for_name(&name).to_string(),
        source: source.to_string(),
        tokens: estimate_agent_message_tokens(message),
        bytes: rendered.len(),
        percent: 0.0,
        hash: hash_text(&rendered),
        cache_role: if index == 0 { "prefix" } else { "history" }.to_string(),
        name,
    }
}

fn context_composition_tool_segment(
    index: usize,
    tool: &AgentToolSpec,
) -> DashboardContextCompositionSegment {
    let rendered = serde_json::to_string(tool).unwrap_or_else(|_| tool.name.clone());
    DashboardContextCompositionSegment {
        name: "tools_schema".to_string(),
        label: "Tools schema".to_string(),
        source: "request_tools".to_string(),
        tokens: estimate_tool_spec_tokens(tool),
        bytes: rendered.len(),
        percent: 0.0,
        hash: hash_text(&rendered),
        cache_role: if index == 0 { "tools" } else { "tools_schema" }.to_string(),
    }
}

const fn context_composition_message_source(message: &AgentMessage) -> &'static str {
    match message {
        AgentMessage::System { .. } => "system",
        AgentMessage::User { .. } => "user",
        AgentMessage::Assistant { .. } | AgentMessage::AssistantToolCallProtocol { .. } => {
            "assistant"
        }
        AgentMessage::Tool { .. } => "tool",
    }
}

fn context_composition_message_name(message: &AgentMessage) -> String {
    match message {
        AgentMessage::System { .. } => "system_messages".to_string(),
        AgentMessage::Assistant { .. } => "assistant_messages".to_string(),
        AgentMessage::AssistantToolCallProtocol { .. } => "tool_inputs".to_string(),
        AgentMessage::Tool { .. } => "tool_messages".to_string(),
        AgentMessage::User { content } => {
            let text = content.as_text();
            if text.contains("<afterclaim_context>") {
                "afterclaim_context".to_string()
            } else if text.contains("<preturn_context>") {
                "preturn_context".to_string()
            } else if text.contains("<claimed_input>") {
                "claimed_input".to_string()
            } else if text.contains(RUNTIME_HISTORY_SUMMARY_PREFIX)
                || text.contains(MID_TURN_SUMMARY_PREFIX)
            {
                "summarized_history".to_string()
            } else {
                "conversation_history".to_string()
            }
        }
    }
}

fn context_composition_label_for_name(name: &str) -> &str {
    match name {
        "system_messages" => "System messages",
        "afterclaim_context" => "Afterclaim context",
        "preturn_context" => "Preturn context",
        "claimed_input" => "Claimed input",
        "summarized_history" => "Summarized history",
        "conversation_history" => "Conversation history",
        "assistant_messages" => "Assistant messages",
        "tool_inputs" => "Tool inputs",
        "tool_messages" => "Tool outputs",
        "tools_schema" => "Tools schema",
        _ => name,
    }
}

fn percent_of(value: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        let value = u32::try_from(value).unwrap_or(u32::MAX);
        let total = u32::try_from(total).unwrap_or(u32::MAX);
        (f64::from(value) / f64::from(total)) * 100.0
    }
}

fn request_fingerprint_input(prefix_units: &[DashboardContextCompositionPrefixUnit]) -> String {
    prefix_units
        .iter()
        .map(|unit| format!("{}:{}", unit.tokens, unit.hash))
        .collect::<Vec<_>>()
        .join("|")
}

fn hash_text(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use crate::core::{model_request_error_detail, should_retry_agent_turn_error};
    use miette::miette;

    #[test]
    fn invalid_request_errors_are_not_retried() {
        let err = miette!(
            "model provider returned HTTP 400 Bad Request: {{\"error\":{{\"type\":\"invalid_request_error\",\"code\":\"invalid_value\"}}}}"
        );

        assert!(!should_retry_agent_turn_error(&err));
    }

    #[test]
    fn transient_request_errors_are_retried() {
        let err = miette!("model provider request failed: connection reset");

        assert!(should_retry_agent_turn_error(&err));
    }

    #[test]
    fn retry_error_detail_is_plain_text_not_fancy_diagnostic() {
        let err = miette!("provider stream failed\nkind=stream_body_read");
        let detail = model_request_error_detail(&err);

        assert!(detail.contains("provider stream failed"));
        assert!(detail.contains("kind=stream_body_read"));
        assert!(!detail.contains("ERROR"));
    }
}
