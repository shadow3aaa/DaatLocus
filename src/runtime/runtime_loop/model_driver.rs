use super::*;
use crate::{
    context_budget::{
        TokenEstimateBaseline, estimate_agent_message_tokens, estimate_tool_spec_tokens,
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
    let limits = runtime_request_budget_limits(context);
    let estimated_input_tokens = {
        let raw_budget = estimate_agent_turn_request(&request.messages, &request.tools, limits);
        let calibrated_budget =
            raw_budget.with_calibrated_input_tokens(&context.token_estimate_baseline);
        calibrated_budget.total_input_tokens
    };
    let budget = estimate_agent_turn_request(&request.messages, &request.tools, limits)
        .with_calibrated_input_tokens(&context.token_estimate_baseline);
    let session_id = context.session_id.clone();
    write_current_turn_messages_dump(
        session_id.as_deref(),
        &request,
        &budget,
        context.llm.model_name().as_deref(),
    )
    .await;
    let context_composition = build_context_composition_snapshot(
        context.latest_context_composition.as_ref(),
        context,
        &request,
        limits.context_window_tokens,
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
    let model_name = context
        .llm
        .model_name()
        .unwrap_or_else(|| context.config.main_model_config().model_id.clone());
    let mut attempt = 1usize;
    loop {
        set_runtime_status_only(tx, "Working");
        let turn_result = context.llm.run_agent_turn(context, request.clone()).await;
        match turn_result {
            Ok(response) => {
                write_current_turn_response_dump(session_id.as_deref(), &response, attempt).await;
                if let Some(info) = context.llm.token_usage_info() {
                    let observed_input =
                        usize::try_from(info.last_token_usage.input_tokens.max(0)).unwrap_or(0);
                    if observed_input > 0 {
                        context.token_estimate_baseline = TokenEstimateBaseline {
                            estimated_input_tokens,
                            observed_input_tokens: Some(observed_input),
                        };
                        save_token_estimate_baseline(&context.token_estimate_baseline).await;
                    }
                }
                clear_runtime_status(tx);
                return Ok(response);
            }
            Err(err) => {
                let will_retry = should_retry_agent_turn_error(&err);
                let error_detail = plain_report_text(&err);
                write_current_turn_response_error_dump(
                    session_id.as_deref(),
                    &error_detail,
                    attempt,
                    will_retry,
                )
                .await;
                if !will_retry {
                    clear_runtime_status(tx);
                    return Err(err);
                }
                let capped_shift = (attempt.saturating_sub(1)).min(6) as u32;
                let backoff_ms = 300u64.saturating_mul(1u64 << capped_shift).min(30_000);
                let summary = format!(
                    "request failed; retry #{attempt} after {:.1}s",
                    backoff_ms as f64 / 1000.0
                );
                set_runtime_status(tx, RuntimeStatusLevel::Warn, summary);
                tracing::warn!(
                    "run_agent_turn retry #{attempt} after {backoff_ms}ms (model={}, messages={}, tools={}, estimated_input_tokens={estimated_input_tokens}): {error_detail}",
                    model_name,
                    request.messages.len(),
                    request.tools.len(),
                );
                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                attempt += 1;
            }
        }
    }
}

fn plain_report_text(err: &miette::Report) -> String {
    let mut lines = vec![err.to_string()];
    let mut causes = Vec::new();
    let mut current = err.source();
    while let Some(source) = current {
        let cause = source.to_string();
        if !cause.trim().is_empty() {
            causes.push(cause);
        }
        current = source.source();
    }
    if !causes.is_empty() {
        lines.push("causes:".to_string());
        lines.extend(causes.into_iter().map(|cause| format!("- {cause}")));
    }
    lines.join("\n")
}

fn should_retry_agent_turn_error(err: &miette::Report) -> bool {
    if is_context_budget_exceeded(err) {
        return false;
    }
    !is_permanent_model_request_error(&err.to_string())
}

pub(super) fn is_permanent_model_request_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("http 400 bad request")
        || lower.contains("invalid_request_error")
        || lower.contains("invalid_value")
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
    let previous_units = previous
        .map(|snapshot| snapshot.prefix_units.as_slice())
        .unwrap_or(&[]);
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
        model: context
            .llm
            .model_name()
            .or_else(|| Some(context.config.main_model_config().model_id.clone())),
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

fn context_composition_message_source(message: &AgentMessage) -> &'static str {
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
        (value as f64 / total as f64) * 100.0
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
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let detail = plain_report_text(&err);

        assert!(detail.contains("provider stream failed"));
        assert!(detail.contains("kind=stream_body_read"));
        assert!(!detail.contains("ERROR"));
    }
}
