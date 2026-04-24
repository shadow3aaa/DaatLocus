use super::*;

pub(super) async fn run_agent_turn_with_retry(
    context: &Context,
    request: AgentTurnRequest,
    tx: Option<&tokio::sync::watch::Sender<DashboardState>>,
) -> Result<AgentTurnStreamResult> {
    let budget = estimate_agent_turn_request(
        &request.messages,
        &request.tools,
        runtime_request_budget_limits(context),
    );
    let estimated_input_tokens = budget.total_input_tokens;
    write_current_turn_messages_dump(&request, &budget, context.llm.model_name().as_deref()).await;
    if let Some(tx) = tx {
        tx.send_modify(|state| {
            state.footer_estimated_input_tokens = Some(estimated_input_tokens);
            state.footer_context =
                render_dashboard_footer_context(context, state.footer_estimated_input_tokens);
        });
    }
    let request_timeout =
        Duration::from_secs(context.config.main_model_config().request_timeout_secs());
    let model_name = context
        .llm
        .model_name()
        .unwrap_or_else(|| context.config.main_model_config().model_id.clone());
    let mut attempt = 1usize;
    loop {
        set_runtime_status(tx, RuntimeStatusLevel::Debug, "Working");
        let turn_result = tokio::time::timeout(
            request_timeout,
            context.llm.run_agent_turn(context, request.clone()),
        )
        .await;
        match turn_result {
            Err(_) => {
                let err = miette!(
                    "agent turn timed out after {}s (model={}, messages={}, tools={}, estimated_input_tokens={estimated_input_tokens})",
                    request_timeout.as_secs(),
                    model_name,
                    request.messages.len(),
                    request.tools.len(),
                );
                let will_retry = true;
                write_current_turn_response_error_dump(&err.to_string(), attempt, will_retry).await;
                let capped_shift = (attempt.saturating_sub(1)).min(6) as u32;
                let backoff_ms = 300u64.saturating_mul(1u64 << capped_shift).min(30_000);
                let summary = format!(
                    "model request timed out; retry #{attempt} after {:.1}s",
                    backoff_ms as f64 / 1000.0
                );
                set_runtime_status(tx, RuntimeStatusLevel::Warn, summary);
                tracing::warn!(
                    "run_agent_turn timed out after {}s; retry #{attempt} in {backoff_ms}ms (model={}, messages={}, tools={}, estimated_input_tokens={estimated_input_tokens})",
                    request_timeout.as_secs(),
                    model_name,
                    request.messages.len(),
                    request.tools.len(),
                );
                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                attempt += 1;
            }
            Ok(Ok(response)) => {
                write_current_turn_response_dump(&response, attempt).await;
                clear_runtime_status(tx);
                return Ok(response);
            }
            Ok(Err(err)) => {
                let will_retry = !is_context_budget_exceeded(&err);
                write_current_turn_response_error_dump(&err.to_string(), attempt, will_retry).await;
                if is_context_budget_exceeded(&err) {
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
                tracing::warn!("run_agent_turn retry #{attempt} after {backoff_ms}ms: {err}");
                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                attempt += 1;
            }
        }
    }
}
