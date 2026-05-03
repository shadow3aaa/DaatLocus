use super::model_driver::run_agent_turn_with_retry;
use super::*;

fn enter_runtime_phase(
    context: &mut Context,
    tx: Option<&tokio::sync::watch::Sender<DashboardState>>,
    phase: RuntimeTurnPhase,
) {
    context.set_runtime_phase(Some(phase));
    if let Some(tx) = tx {
        tx.send_modify(|state| {
            state.status_output =
                crate::dashboard::render::render_status_command_output_for_dashboard(context, &[]);
        });
    }
    set_runtime_status(
        tx,
        RuntimeStatusLevel::Info,
        format!("processing: runtime turn / {}", phase.label()),
    );
}

struct RuntimeTurnAbort<'a> {
    live_draft_session: Option<TelegramLiveDraftSession>,
    claimed_input_fingerprint: Option<&'a str>,
    claimed_event_ids: &'a [String],
    claimed_app_notices: &'a [AppNoticeKey],
    observation: String,
    description: String,
}

async fn abort_runtime_turn_before_model(
    context: &mut Context,
    abort: RuntimeTurnAbort<'_>,
) -> AgentLoopStepExecution {
    let RuntimeTurnAbort {
        live_draft_session,
        claimed_input_fingerprint,
        claimed_event_ids,
        claimed_app_notices,
        observation,
        description,
    } = abort;

    context.set_runtime_phase(None);
    if let Some(session) = live_draft_session {
        session.shutdown(context).await;
    } else {
        context.install_live_progress(None);
    }
    if let Some(fingerprint) = claimed_input_fingerprint {
        context.clear_runtime_overflow_failure(fingerprint);
    }
    let output = AgentLoopStepOutput {
        observation: observation.clone(),
        description,
        current_doing: "waiting for next tool decision".to_string(),
        actions: vec![EpisodeActionRecord {
            kind: "runtime_preflight_failed".to_string(),
            summary: observation,
        }],
    };
    finalize_claimed_runtime_events(context, claimed_event_ids, &output);
    finalize_claimed_runtime_app_notices(context, claimed_app_notices, &output).await;
    context.claimed_event_ids.clear();
    context.claimed_app_notices.clear();
    record_workflow_run_evidence(context, &output).await;
    context.current_work_origin = None;
    context.workflow_step_started_bound_id = None;
    AgentLoopStepExecution {
        output,
        history_messages: Vec::new(),
    }
}

fn maybe_build_afterclaim_context_message(
    context: &mut Context,
    input: &AfterClaimContextInput,
    fingerprint: Option<&str>,
    force_reinject: bool,
) -> Option<HistoryMessage> {
    let fingerprint = fingerprint.unwrap_or("unfingerprinted");
    let message = preview_afterclaim_context_message(context, input, fingerprint, force_reinject)?;
    context.afterclaim_context_fingerprint = Some(fingerprint.to_string());
    Some(message)
}

fn preview_afterclaim_context_message(
    context: &Context,
    input: &AfterClaimContextInput,
    fingerprint: &str,
    force_reinject: bool,
) -> Option<HistoryMessage> {
    if input.is_empty() {
        return None;
    }
    let already_injected = context.afterclaim_context_fingerprint.as_deref() == Some(fingerprint);
    if already_injected && !force_reinject {
        return None;
    }

    let text = build_afterclaim_context_text(context, input);
    if text.trim().is_empty() {
        return None;
    }
    Some(HistoryMessage {
        message: AgentMessage::user_content(afterclaim_agent_content(text, input)),
        tool_ui_event: None,
        tool_call_ui_events: Vec::new(),
    })
}

fn afterclaim_agent_content(text: String, input: &AfterClaimContextInput) -> AgentContent {
    let parts = input
        .events
        .iter()
        .flat_map(|event| match &event.payload {
            EventPayload::TelegramIncoming(payload) => payload
                .attachments
                .iter()
                .map(|attachment| match attachment.kind {
                    crate::events::TelegramIncomingAttachmentKind::Image => {
                        AgentContentPart::Image {
                            path: attachment.local_path.clone(),
                            media_type: attachment.media_type.clone(),
                            description: attachment.description.clone(),
                        }
                    }
                })
                .collect::<Vec<_>>(),
            EventPayload::TerminalIncoming(_) => Vec::new(),
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        AgentContent::text(text)
    } else {
        AgentContent::multimodal(text, parts)
    }
}

fn history_has_complete_afterclaim_context(messages: &[HistoryMessage]) -> bool {
    messages
        .iter()
        .filter_map(HistoryMessage::text_content)
        .any(is_complete_afterclaim_context_text)
}

fn is_complete_afterclaim_context_text(text: &str) -> bool {
    let text = text.trim_start();
    text.starts_with("<afterclaim_context") && text.contains("</afterclaim_context>")
}

fn runtime_context_compacted_output(reason: impl Into<String>) -> AgentLoopStepOutput {
    let reason = reason.into();
    AgentLoopStepOutput {
        observation: reason.clone(),
        description: "Runtime context was compacted; the current turn ends so claimed work can be re-claimed with freshly injected context."
            .to_string(),
        current_doing: "waiting for next tool decision".to_string(),
        actions: vec![EpisodeActionRecord {
            kind: "runtime_context_compacted".to_string(),
            summary: reason,
        }],
    }
}

fn output_is_runtime_context_compaction_boundary(output: &AgentLoopStepOutput) -> bool {
    output
        .actions
        .iter()
        .any(|action| action.kind == "runtime_context_compacted")
}

pub(crate) async fn execute_agent_loop_step(
    context: &mut Context,
    tx: Option<&tokio::sync::watch::Sender<DashboardState>>,
) -> AgentLoopStepExecution {
    let runtime_turn_id = format!("runtime-turn-{}", uuid::Uuid::new_v4());
    let claimed_inputs = claim_pending_runtime_inputs(context, RUNTIME_EVENT_CLAIM_BATCH_SIZE);
    context.current_work_origin = runtime_work_origin(&claimed_inputs);
    context.workflow_step_started_bound_id = context.bound_workflow_id.clone();
    let claimed_input_fingerprint = claimed_runtime_input_fingerprint(&claimed_inputs);
    let claimed_event_ids = claimed_inputs
        .iter()
        .filter_map(|input| match input {
            ClaimedRuntimeInput::Event(event) => Some(event.event_id.to_string()),
            ClaimedRuntimeInput::AppNotice { .. } => None,
        })
        .collect::<Vec<_>>();
    context.claimed_event_ids = claimed_event_ids.clone();
    let claimed_app_notice_entries = claimed_inputs
        .iter()
        .filter_map(|input| match input {
            ClaimedRuntimeInput::Event(_) => None,
            ClaimedRuntimeInput::AppNotice { app, reason } => {
                Some(AppNoticeKey::new(app.clone(), reason.clone()))
            }
        })
        .collect::<Vec<_>>();
    context.claimed_app_notices = claimed_app_notice_entries.clone();

    let preflight_timeout = Duration::from_secs(RUNTIME_PREFLIGHT_STAGE_TIMEOUT_SECS);
    enter_runtime_phase(context, tx, RuntimeTurnPhase::PreflightMemory);
    let preflight_started_at = std::time::Instant::now();
    tracing::info!(
        "runtime preflight stage started: {}",
        RuntimeTurnPhase::PreflightMemory.label()
    );
    let prompt_memory = match tokio::time::timeout(
        preflight_timeout,
        build_hindsight_memory_context(context, &claimed_inputs),
    )
    .await
    {
        Ok(prompt_memory) => {
            tracing::info!(
                elapsed_ms = preflight_started_at.elapsed().as_millis(),
                "runtime preflight stage completed: {}",
                RuntimeTurnPhase::PreflightMemory.label()
            );
            prompt_memory
        }
        Err(_) => {
            let err = miette!(
                "runtime preflight stage `{}` timed out after {}s",
                RuntimeTurnPhase::PreflightMemory.label(),
                preflight_timeout.as_secs()
            );
            set_runtime_status(
                tx,
                RuntimeStatusLevel::Error,
                format!(
                    "runtime turn preflight timeout: {}",
                    RuntimeTurnPhase::PreflightMemory.label()
                ),
            );
            tracing::error!(
                elapsed_ms = preflight_started_at.elapsed().as_millis(),
                timeout_secs = preflight_timeout.as_secs(),
                "runtime preflight stage timed out: {}",
                RuntimeTurnPhase::PreflightMemory.label()
            );
            return abort_runtime_turn_before_model(
                context,
                RuntimeTurnAbort {
                    live_draft_session: None,
                    claimed_input_fingerprint: claimed_input_fingerprint.as_deref(),
                    claimed_event_ids: &claimed_event_ids,
                    claimed_app_notices: &claimed_app_notice_entries,
                    observation: format!("runtime preflight failed: {err}"),
                    description: "Failed to build hindsight memory context.".to_string(),
                },
            )
            .await;
        }
    };
    context.prompt_memory = prompt_memory;
    let afterclaim_context_input = afterclaim_context_input_for_claimed_inputs(&claimed_inputs);
    let claimed_event_views = claimed_inputs
        .iter()
        .filter_map(|input| match input {
            ClaimedRuntimeInput::Event(event) => Some(event.clone()),
            ClaimedRuntimeInput::AppNotice { .. } => None,
        })
        .collect::<Vec<_>>();
    let live_draft_session = maybe_start_telegram_live_draft_session(context, &claimed_event_views);
    enter_runtime_phase(context, tx, RuntimeTurnPhase::PreflightPreTurnContext);
    let preturn_started_at = std::time::Instant::now();
    tracing::info!(
        "runtime preflight stage started: {}",
        RuntimeTurnPhase::PreflightPreTurnContext.label()
    );
    let preturn_state =
        match tokio::time::timeout(preflight_timeout, PreTurnState::new(context)).await {
            Ok(preturn_state) => {
                tracing::info!(
                    elapsed_ms = preturn_started_at.elapsed().as_millis(),
                    "runtime preflight stage completed: {}",
                    RuntimeTurnPhase::PreflightPreTurnContext.label()
                );
                preturn_state
            }
            Err(_) => {
                let err = miette!(
                    "runtime preflight stage `{}` timed out after {}s",
                    RuntimeTurnPhase::PreflightPreTurnContext.label(),
                    preflight_timeout.as_secs()
                );
                set_runtime_status(
                    tx,
                    RuntimeStatusLevel::Error,
                    format!(
                        "runtime turn preflight timeout: {}",
                        RuntimeTurnPhase::PreflightPreTurnContext.label()
                    ),
                );
                tracing::error!(
                    elapsed_ms = preturn_started_at.elapsed().as_millis(),
                    timeout_secs = preflight_timeout.as_secs(),
                    "runtime preflight stage timed out: {}",
                    RuntimeTurnPhase::PreflightPreTurnContext.label()
                );
                return abort_runtime_turn_before_model(
                    context,
                    RuntimeTurnAbort {
                        live_draft_session,
                        claimed_input_fingerprint: claimed_input_fingerprint.as_deref(),
                        claimed_event_ids: &claimed_event_ids,
                        claimed_app_notices: &claimed_app_notice_entries,
                        observation: format!("runtime preflight failed: {err}"),
                        description: "Failed to build preturn context.".to_string(),
                    },
                )
                .await;
            }
        };
    let preturn_context_text = build_preturn_context_text(context, &preturn_state);
    let runtime_context_text = if afterclaim_context_input.is_empty() {
        preturn_context_text.clone()
    } else {
        format!(
            "{}\n\n{}",
            build_afterclaim_context_text(context, &afterclaim_context_input),
            preturn_context_text
        )
    };
    if let Some(tx) = tx {
        tx.send_modify(|state| {
            state.preturn_context_output = preturn_context_text.clone();
        });
    }
    let request_envelope = build_runtime_request_envelope(context);
    let initial_tools = build_runtime_tool_specs(context);
    let request_budget_limits = runtime_request_budget_limits(context);
    let runtime_conversation_budget =
        request_envelope.conversation_budget_tokens(&initial_tools, request_budget_limits);
    let mut initial_injected_context_messages = Vec::new();
    if let Some(message) = preview_afterclaim_context_message(
        context,
        &afterclaim_context_input,
        claimed_input_fingerprint
            .as_deref()
            .unwrap_or("unfingerprinted"),
        false,
    ) {
        initial_injected_context_messages.push(message);
    }
    if !preturn_context_text.trim().is_empty() {
        initial_injected_context_messages.push(HistoryMessage::user(preturn_context_text.clone()));
    }
    let runtime_conversation_summary_budget =
        RUNTIME_HISTORY_SUMMARY_MAX_TOKENS.min(runtime_conversation_budget);
    let mut pre_turn_compacted = false;
    if let Some(plan) = context
        .memory
        .plan_runtime_conversation_compaction_for_request(
            &request_envelope,
            &initial_injected_context_messages,
            &initial_tools,
            request_budget_limits,
            RUNTIME_HISTORY_MIN_MESSAGES,
            runtime_conversation_summary_budget,
        )
    {
        enter_runtime_phase(context, tx, RuntimeTurnPhase::PreflightCompaction);
        let compaction_started_at = std::time::Instant::now();
        tracing::info!(
            "runtime preflight stage started: {}",
            RuntimeTurnPhase::PreflightCompaction.label()
        );
        let summary = match tokio::time::timeout(
            preflight_timeout,
            execute_pre_turn_runtime_compaction(context, &plan),
        )
        .await
        {
            Ok(summary) => {
                tracing::info!(
                    elapsed_ms = compaction_started_at.elapsed().as_millis(),
                    "runtime preflight stage completed: {}",
                    RuntimeTurnPhase::PreflightCompaction.label()
                );
                summary
            }
            Err(_) => {
                let err = miette!(
                    "runtime preflight stage `{}` timed out after {}s",
                    RuntimeTurnPhase::PreflightCompaction.label(),
                    preflight_timeout.as_secs()
                );
                set_runtime_status(
                    tx,
                    RuntimeStatusLevel::Error,
                    format!(
                        "runtime turn preflight timeout: {}",
                        RuntimeTurnPhase::PreflightCompaction.label()
                    ),
                );
                tracing::error!(
                    elapsed_ms = compaction_started_at.elapsed().as_millis(),
                    timeout_secs = preflight_timeout.as_secs(),
                    "runtime preflight stage timed out: {}",
                    RuntimeTurnPhase::PreflightCompaction.label()
                );
                return abort_runtime_turn_before_model(
                    context,
                    RuntimeTurnAbort {
                        live_draft_session,
                        claimed_input_fingerprint: claimed_input_fingerprint.as_deref(),
                        claimed_event_ids: &claimed_event_ids,
                        claimed_app_notices: &claimed_app_notice_entries,
                        observation: format!("runtime preflight failed: {err}"),
                        description: "Failed to execute pre-turn context compaction.".to_string(),
                    },
                )
                .await;
            }
        };
        let _ = context
            .memory
            .apply_runtime_conversation_compaction(plan, summary)
            .await;
        pre_turn_compacted = true;
    }
    let mut conversation_slice = context.memory.runtime_conversation_slice(
        runtime_conversation_budget,
        RUNTIME_HISTORY_MIN_MESSAGES,
        runtime_conversation_summary_budget,
    );
    let mut injected_context_messages = Vec::new();
    if let Some(message) = maybe_build_afterclaim_context_message(
        context,
        &afterclaim_context_input,
        claimed_input_fingerprint.as_deref(),
        pre_turn_compacted && !history_has_complete_afterclaim_context(&conversation_slice),
    ) {
        injected_context_messages.push(message);
    }
    if !preturn_context_text.trim().is_empty() {
        injected_context_messages.push(HistoryMessage::user(preturn_context_text.clone()));
    }
    conversation_slice.extend(injected_context_messages.iter().cloned());
    let mut runtime_step = context
        .memory
        .begin_runtime_step_from_parts(request_envelope, conversation_slice);
    for message in injected_context_messages {
        runtime_step.push_history_message(message);
    }
    let mut tool_results = Vec::new();
    let mut actions = Vec::new();
    let mut budget_recoveries = 0usize;

    let output = 'agent_loop: loop {
        let tools = build_runtime_tool_specs(context);
        if maybe_compact_runtime_messages(context, &mut runtime_step, &tools, false).await {
            set_runtime_status(tx, RuntimeStatusLevel::Info, "Compacting runtime context");
            break 'agent_loop runtime_context_compacted_output(
                "runtime context compacted before model request; starting a new turn",
            );
        }
        let request = AgentTurnRequest {
            messages: runtime_step.clone_agent_messages(),
            tools: tools.clone(),
        };
        enter_runtime_phase(context, tx, RuntimeTurnPhase::ModelRequest);
        context.emit_live_generation_started();
        let response = match run_agent_turn_with_retry(context, request, tx).await {
            Ok(response) => response,
            Err(err) => {
                if is_context_budget_exceeded(&err)
                    && budget_recoveries < MID_TURN_COMPACTION_MAX_RECOVERIES
                    && maybe_compact_runtime_messages(context, &mut runtime_step, &tools, true)
                        .await
                {
                    budget_recoveries += 1;
                    set_runtime_status(
                        tx,
                        RuntimeStatusLevel::Warn,
                        format!(
                            "Recovering from context overflow ({budget_recoveries}/{})",
                            MID_TURN_COMPACTION_MAX_RECOVERIES
                        ),
                    );
                    break 'agent_loop runtime_context_compacted_output(format!(
                        "runtime context compacted after context overflow recovery ({budget_recoveries}/{}); starting a new turn",
                        MID_TURN_COMPACTION_MAX_RECOVERIES
                    ));
                }
                let is_overflow = is_context_budget_exceeded(&err);
                let overflow_fuse_tripped = if is_overflow {
                    handle_runtime_overflow(
                        context,
                        claimed_input_fingerprint.as_deref(),
                        &claimed_event_ids,
                        &claimed_app_notice_entries,
                        &err.to_string(),
                    )
                } else {
                    false
                };
                if is_overflow {
                    record_runtime_error_case(
                        context,
                        RuntimeErrorRecordInput {
                            turn_id: &runtime_turn_id,
                            claimed_inputs: &claimed_inputs,
                            claimed_event_ids: &claimed_event_ids,
                            claimed_app_notices: &claimed_app_notice_entries,
                            tools: &tools,
                            context_text: &runtime_context_text,
                            error_kind: RuntimeErrorKind::ContextOverflowAfterRecovery,
                            severity: 3,
                            detected_by: "runtime_model_request",
                            expected_behavior: "Runtime context compaction should recover enough budget for the model request or terminate claimed inputs through the overflow fuse.",
                            actual_behavior: "Model request still failed with context overflow after recovery attempts.",
                            evidence: &err.to_string(),
                            recoverability: if overflow_fuse_tripped {
                                "terminated_by_overflow_fuse"
                            } else {
                                "requeued_for_retry"
                            },
                            retry_count: budget_recoveries,
                            terminal_status: Some(if overflow_fuse_tripped {
                                "fuse_tripped"
                            } else {
                                "not_terminal"
                            }),
                            assistant_text: None,
                            tool_calls: &[],
                            tool_results: &tool_results,
                            actions: &actions,
                        },
                    )
                    .await;
                }
                if !is_overflow && !overflow_fuse_tripped && !claimed_event_ids.is_empty() {
                    requeue_claimed_runtime_events(context, &claimed_event_ids);
                }
                let observation = format!("agent turn failed: {err}");
                let terminal_action = EpisodeActionRecord {
                    kind: "agent_turn_failed".to_string(),
                    summary: observation.clone(),
                };
                let mut terminal_actions = actions.clone();
                terminal_actions.push(terminal_action.clone());
                runtime_step.push_history_message(HistoryMessage::assistant(observation.clone()));
                if let Some(cell) = assistant_activity_cell(&observation) {
                    append_committed_activity_cells(context, tx, vec![cell]);
                }
                break 'agent_loop AgentLoopStepOutput {
                    observation: observation.clone(),
                    description: "Model request failed.".to_string(),
                    current_doing: "waiting for next tool decision".to_string(),
                    actions: terminal_actions,
                };
            }
        };
        let mut response_assistant_messages = Vec::new();
        let mut response_tool_calls = Vec::new();
        for item in response.items {
            match item {
                AgentTurnItem::AssistantMessage { content } => {
                    if !content.trim().is_empty() {
                        response_assistant_messages.push(content);
                    }
                }
                AgentTurnItem::ToolCall { call } => response_tool_calls.push(call),
            }
        }
        let response_assistant_content = response
            .last_assistant_message
            .clone()
            .or_else(|| response_assistant_messages.last().cloned());
        if let Some(reasoning_content) = response.last_reasoning_content.as_deref()
            && !reasoning_content.trim().is_empty()
        {
            context.emit_live_reasoning_progress(reasoning_content);
        }
        if let Some(content) = response_assistant_content.as_deref()
            && !content.trim().is_empty()
        {
            context.emit_live_assistant_progress(content);
        }
        if !response_tool_calls.is_empty() {
            let calls = response_tool_calls;
            let assistant_text = if response_assistant_messages.is_empty() {
                None
            } else {
                Some(response_assistant_messages.join("\n\n"))
            };
            let tool_call_ui_events = calls
                .iter()
                .map(|call| {
                    render_tool_call_ui_event(context, call).unwrap_or_else(|_| {
                        if call.name == "apply_patch" {
                            ToolCallUiEvent::error(
                                "apply_patch".to_string(),
                                vec!["invalid patch syntax".to_string()],
                            )
                        } else {
                            ToolCallUiEvent::error(
                                call.name.clone(),
                                vec![call.arguments.to_string()],
                            )
                        }
                    })
                })
                .collect::<Vec<_>>();
            runtime_step.push_agent_message(
                AgentMessage::assistant_tool_call_protocol_with_reasoning(
                    assistant_text.clone(),
                    response.last_reasoning_content.clone(),
                    calls.clone(),
                ),
            );
            if let Some(content) = assistant_text.clone()
                && !content.trim().is_empty()
            {
                runtime_step.push_history_message(HistoryMessage::assistant(content));
            }
            runtime_step.push_history_message(HistoryMessage {
                message: AgentMessage::assistant_tool_call_protocol_with_reasoning(
                    None,
                    response.last_reasoning_content.clone(),
                    calls.clone(),
                ),
                tool_ui_event: None,
                tool_call_ui_events: tool_call_ui_events.clone(),
            });
            let mut committed_cells = Vec::new();
            if let Some(content) = assistant_text.clone()
                && let Some(cell) = assistant_activity_cell(&content)
            {
                committed_cells.push(cell);
            }
            append_committed_activity_cells(context, tx, committed_cells);
            for (call, call_ui_event) in calls.iter().zip(tool_call_ui_events.iter()) {
                let action_record =
                    summarize_action_from_tool_call(context, call).unwrap_or_else(|_| {
                        EpisodeActionRecord {
                            kind: "tool_call".to_string(),
                            summary: call.name.clone(),
                        }
                    });
                actions.push(action_record);
                enter_runtime_phase(context, tx, RuntimeTurnPhase::ToolExecution);
                if let Some(tx) = tx {
                    match call_ui_event.clone() {
                        ToolCallUiEvent::Exec(event) => {
                            tx.send_modify(|state| {
                                apply_activity_event(
                                    state,
                                    DashboardActivityEvent::ExecBegin {
                                        key: call.id.clone(),
                                        title: event.title,
                                        call_lines: event.body_lines,
                                    },
                                );
                            });
                        }
                        ToolCallUiEvent::Terminal(event)
                            if matches!(
                                event.action,
                                crate::tool_ui::TerminalUiAction::Execute
                                    | crate::tool_ui::TerminalUiAction::Continue
                            ) =>
                        {
                            tx.send_modify(|state| {
                                apply_activity_event(
                                    state,
                                    DashboardActivityEvent::ExecBegin {
                                        key: call.id.clone(),
                                        title: event.title,
                                        call_lines: event.body_lines,
                                    },
                                );
                            });
                        }
                        ToolCallUiEvent::Browser(event)
                            if !matches!(
                                event.action,
                                crate::tool_ui::BrowserUiAction::Snapshot
                            ) =>
                        {
                            tx.send_modify(|state| {
                                apply_activity_event(
                                    state,
                                    DashboardActivityEvent::BrowserBegin {
                                        key: call.id.clone(),
                                        event,
                                    },
                                );
                            });
                        }
                        _ => {}
                    }
                }
                let result = match execute_agent_tool_call(context, call).await {
                    Ok(result) => result,
                    Err(err) => {
                        let error_text = err.to_string();
                        record_runtime_error_case(
                            context,
                            RuntimeErrorRecordInput {
                                turn_id: &runtime_turn_id,
                                claimed_inputs: &claimed_inputs,
                                claimed_event_ids: &claimed_event_ids,
                                claimed_app_notices: &claimed_app_notice_entries,
                                tools: &tools,
                                context_text: &runtime_context_text,
                                error_kind: classify_tool_runtime_error(&call.name, &error_text),
                                severity: 2,
                                detected_by: "runtime_tool_executor",
                                expected_behavior: "Tool calls should use available tools with valid arguments and satisfy the tool-specific runtime contract.",
                                actual_behavior: "The tool executor rejected the tool call.",
                                evidence: &error_text,
                                recoverability: "tool_error_returned_to_model",
                                retry_count: 0,
                                terminal_status: None,
                                assistant_text: assistant_text.as_deref(),
                                tool_calls: std::slice::from_ref(call),
                                tool_results: &tool_results,
                                actions: &actions,
                            },
                        )
                        .await;
                        let ui_error_text = if call.name == "apply_patch" {
                            summarize_apply_patch_error(&error_text)
                        } else {
                            error_text.clone()
                        };
                        ToolExecutionResult::new(
                            format!("{} failed", call.name),
                            json!({
                                "error": error_text,
                            }),
                            ToolUiEvent::error(
                                format!("{} failed", call.name),
                                compact_body_lines(&ui_error_text, 6),
                            ),
                        )
                    }
                };
                if let Some(status) = render_telegram_tool_result_status(call, &result) {
                    context.emit_live_telegram_status(status);
                }
                if let Some(tx) = tx {
                    tx.send_modify(|state| {
                        apply_activity_event(
                            state,
                            DashboardActivityEvent::ExecEnd {
                                key: call.id.clone(),
                            },
                        );
                        apply_activity_event(
                            state,
                            DashboardActivityEvent::BrowserEnd {
                                key: call.id.clone(),
                            },
                        );
                    });
                }
                runtime_step.push_agent_message(AgentMessage::tool(
                    call.id.clone(),
                    call.name.clone(),
                    result.model_content(),
                ));
                runtime_step.push_history_message(HistoryMessage::tool(
                    call.id.clone(),
                    call.name.clone(),
                    result.history_content_with_budget(
                        &call.id,
                        &call.name,
                        context
                            .config
                            .main_model_config()
                            .tool_output_max_tokens
                            .max(1),
                    ),
                    result.ui_event.clone(),
                ));
                append_committed_activity_cells(
                    context,
                    tx,
                    activity_cell_from_tool_ui_event(result.ui_event.clone())
                        .into_iter()
                        .collect(),
                );
                tool_results.push(format!("{} => {}", call.name, result.summary));
                if let Some(reason) = result.turn_boundary_reason.clone() {
                    break 'agent_loop AgentLoopStepOutput {
                        observation: if tool_results.is_empty() {
                            reason.clone()
                        } else {
                            tool_results.join("\n")
                        },
                        description: format!(
                            "A tool changed the context view needed for subsequent work; the current turn ends immediately at this boundary and the world state will be re-rendered in a new turn. reason: {reason}"
                        ),
                        current_doing: "waiting for next tool decision".to_string(),
                        actions: actions.clone(),
                    };
                }
                if claimed_events_are_terminal(context, &claimed_event_ids) {
                    if actions.is_empty() {
                        actions.push(EpisodeActionRecord {
                            kind: "claimed_events_completed".to_string(),
                            summary: "claimed events reached terminal state".to_string(),
                        });
                    }
                    break 'agent_loop AgentLoopStepOutput {
                        observation: if tool_results.is_empty() {
                            "claimed events reached terminal state".to_string()
                        } else {
                            tool_results.join("\n")
                        },
                        description: "Claimed events for this turn reached a terminal or handoff state, so the turn ends immediately after the relevant tool."
                            .to_string(),
                        current_doing: "waiting for next tool decision".to_string(),
                        actions: actions.clone(),
                    };
                }
                if context.claimed_app_notices_are_resolved() {
                    if actions.is_empty() {
                        actions.push(EpisodeActionRecord {
                            kind: "claimed_app_notices_completed".to_string(),
                            summary: "claimed app notices were explicitly resolved".to_string(),
                        });
                    }
                    break 'agent_loop AgentLoopStepOutput {
                        observation: if tool_results.is_empty() {
                            "claimed app notices were explicitly resolved".to_string()
                        } else {
                            tool_results.join("\n")
                        },
                        description: "Claimed app notices for this turn were explicitly resolved, so the turn ends immediately after the relevant tool."
                            .to_string(),
                        current_doing: "waiting for next tool decision".to_string(),
                        actions: actions.clone(),
                    };
                }
            }
            continue 'agent_loop;
        }

        let content = response_assistant_content.unwrap_or_default();
        if let RuntimeFollowUpDecision::Continue { reason } = runtime_turn_follow_up_decision(
            context,
            response.raw_stream_follow_up,
            &claimed_event_ids,
        ) {
            if let Some(error_kind) = runtime_follow_up_error_kind(reason) {
                record_runtime_error_case(
                    context,
                    RuntimeErrorRecordInput {
                        turn_id: &runtime_turn_id,
                        claimed_inputs: &claimed_inputs,
                        claimed_event_ids: &claimed_event_ids,
                        claimed_app_notices: &claimed_app_notice_entries,
                        tools: &tools,
                        context_text: &runtime_context_text,
                        error_kind,
                        severity: 2,
                        detected_by: "runtime_follow_up_gate",
                        expected_behavior: reason.message(),
                        actual_behavior: "The model returned assistant text without the required completion tool.",
                        evidence: &content,
                        recoverability: "system_follow_up_message_inserted",
                        retry_count: 0,
                        terminal_status: None,
                        assistant_text: Some(&content),
                        tool_calls: &[],
                        tool_results: &tool_results,
                        actions: &actions,
                    },
                )
                .await;
            }
            runtime_step.push_agent_message(AgentMessage::system(reason.message().to_string()));
            continue 'agent_loop;
        }
        let current_doing = content
            .lines()
            .next()
            .filter(|line| !line.trim().is_empty())
            .unwrap_or("waiting for next tool decision")
            .to_string();
        let assistant_action = EpisodeActionRecord {
            kind: "assistant_message".to_string(),
            summary: current_doing.clone(),
        };
        actions.push(assistant_action.clone());
        runtime_step.set_current_doing(current_doing.clone());
        runtime_step.push_history_message(HistoryMessage::assistant(content.clone()));
        if let Some(cell) = assistant_activity_cell(&content) {
            append_committed_activity_cells(context, tx, vec![cell]);
        }
        break 'agent_loop AgentLoopStepOutput {
            observation: if tool_results.is_empty() {
                content.clone()
            } else {
                tool_results.join("\n")
            },
            description: if tool_results.is_empty() {
                "The model returned assistant text without calling a tool.".to_string()
            } else {
                content
            },
            current_doing,
            actions: actions.clone(),
        };
    };
    runtime_step.set_current_doing(output.current_doing.clone());
    context.set_runtime_phase(None);
    if let Some(session) = live_draft_session {
        session.shutdown(context).await;
    } else {
        context.install_live_progress(None);
    }
    if let Some(fingerprint) = claimed_input_fingerprint.as_deref() {
        context.clear_runtime_overflow_failure(fingerprint);
    }
    let claimed_events_finished =
        claimed_event_ids.is_empty() || claimed_events_are_terminal(context, &claimed_event_ids);
    let claimed_app_notices_finished =
        claimed_app_notice_entries.is_empty() || context.claimed_app_notices_are_resolved();
    finalize_claimed_runtime_events(context, &claimed_event_ids, &output);
    finalize_claimed_runtime_app_notices(context, &claimed_app_notice_entries, &output).await;
    if (!claimed_event_ids.is_empty() || !claimed_app_notice_entries.is_empty())
        && (claimed_events_finished && claimed_app_notices_finished
            || output_is_runtime_context_compaction_boundary(&output))
    {
        context.afterclaim_context_fingerprint = None;
    }
    context.claimed_event_ids.clear();
    context.claimed_app_notices.clear();
    let history_messages = runtime_step.history_messages().to_vec();
    if !runtime_step.is_history_empty() {
        record_runtime_history_messages(context, runtime_step.into_turn_draft()).await;
    }
    record_workflow_run_evidence(context, &output).await;
    context.current_work_origin = None;
    context.workflow_step_started_bound_id = None;
    AgentLoopStepExecution {
        output,
        history_messages,
    }
}

struct RuntimeErrorRecordInput<'a> {
    turn_id: &'a str,
    claimed_inputs: &'a [ClaimedRuntimeInput],
    claimed_event_ids: &'a [String],
    claimed_app_notices: &'a [AppNoticeKey],
    tools: &'a [crate::reasoning::runtime::AgentToolSpec],
    context_text: &'a str,
    error_kind: RuntimeErrorKind,
    severity: u8,
    detected_by: &'a str,
    expected_behavior: &'a str,
    actual_behavior: &'a str,
    evidence: &'a str,
    recoverability: &'a str,
    retry_count: usize,
    terminal_status: Option<&'a str>,
    assistant_text: Option<&'a str>,
    tool_calls: &'a [crate::reasoning::runtime::AgentToolCall],
    tool_results: &'a [String],
    actions: &'a [EpisodeActionRecord],
}

async fn record_runtime_error_case(context: &Context, input: RuntimeErrorRecordInput<'_>) {
    let case = RuntimeErrorCase::new(RuntimeErrorCaseParts {
        turn_id: input.turn_id.to_string(),
        error_kind: input.error_kind,
        severity: input.severity,
        detected_by: input.detected_by.to_string(),
        task: RuntimeErrorTaskContext {
            origin: context.current_work_origin.clone(),
            event_sources: runtime_error_event_sources(input.claimed_inputs),
            user_request_summary: runtime_error_user_request_summary(input.claimed_inputs),
            claimed_event_ids: input.claimed_event_ids.to_vec(),
            claimed_app_notices: input
                .claimed_app_notices
                .iter()
                .map(|notice| format!("{}:{}", notice.app, notice.reason))
                .collect(),
            bound_workflow_id: context.bound_workflow_id.clone(),
            workflow_origin: context
                .bound_workflow_id
                .as_deref()
                .and_then(|workflow_id| context.workflows.workflow_origin(workflow_id))
                .map(|origin| format!("{origin:?}").to_ascii_lowercase()),
        },
        runtime: RuntimeErrorRuntimeContext {
            phase: context
                .active_runtime_phase
                .map(|phase| phase.label().to_string()),
            available_tool_names: input.tools.iter().map(|tool| tool.name.clone()).collect(),
            focused_app: context.apps.focused().map(|app| app.to_string()),
            plan_summary: context
                .plan
                .steps()
                .iter()
                .take(8)
                .map(|step| {
                    format!(
                        "{:?}: {}",
                        step.status,
                        compact_runtime_error_text(&step.step, 96)
                    )
                })
                .collect(),
            compact_context_summary: Some(compact_runtime_error_text(input.context_text, 1600)),
        },
        action: RuntimeErrorActionContext {
            assistant_text_summary: input
                .assistant_text
                .map(|text| compact_runtime_error_text(text, 600)),
            tool_call_summaries: input
                .tool_calls
                .iter()
                .map(|call| {
                    format!(
                        "{} {}",
                        call.name,
                        compact_runtime_error_text(&call.arguments.to_string(), 320)
                    )
                })
                .collect(),
            tool_result_summaries: input
                .tool_results
                .iter()
                .map(|result| compact_runtime_error_text(result, 320))
                .collect(),
            previous_action_window: input
                .actions
                .iter()
                .rev()
                .take(6)
                .map(|action| {
                    format!(
                        "{}: {}",
                        action.kind,
                        compact_runtime_error_text(&action.summary, 160)
                    )
                })
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect(),
        },
        observation: RuntimeErrorObservation {
            expected_behavior: input.expected_behavior.to_string(),
            actual_behavior: input.actual_behavior.to_string(),
            evidence: compact_runtime_error_text(input.evidence, 1200),
            recoverability: input.recoverability.to_string(),
            retry_count: input.retry_count,
            terminal_status: input.terminal_status.map(ToString::to_string),
        },
        contract_refs: runtime_error_contract_refs(input.error_kind),
    });
    append_runtime_error_case(case).await;
}

fn runtime_follow_up_error_kind(reason: RuntimeFollowUpReason) -> Option<RuntimeErrorKind> {
    match reason {
        RuntimeFollowUpReason::ClaimedEventNeedsExplicitResolution => {
            Some(RuntimeErrorKind::MissingFinishAndSend)
        }
        RuntimeFollowUpReason::ClaimedAppNoticeNeedsExplicitResolution => {
            Some(RuntimeErrorKind::MissingNoticeResolved)
        }
        RuntimeFollowUpReason::RawStreamRequestedFollowUp => None,
    }
}

fn classify_tool_runtime_error(tool_name: &str, error_text: &str) -> RuntimeErrorKind {
    let lower = error_text.to_ascii_lowercase();
    if tool_name == "update_plan"
        || lower.contains("update_plan must contain")
        || lower.contains("update_plan cannot contain")
    {
        return RuntimeErrorKind::PlanContractViolation;
    }
    if tool_name == "finish_and_send" && lower.contains("no claimed event") {
        return RuntimeErrorKind::EventIdMissingOrStale;
    }
    if tool_name.starts_with("browser_") && lower.contains("stale") {
        return RuntimeErrorKind::StaleBrowserRef;
    }
    if tool_name.starts_with("terminal_")
        && (lower.contains("session") || lower.contains("stdin"))
        && (lower.contains("missing") || lower.contains("not found") || lower.contains("invalid"))
    {
        return RuntimeErrorKind::WrongTerminalSessionContinuation;
    }
    if lower.contains("invalid arguments")
        || lower.contains("missing field")
        || lower.contains("invalid type")
        || lower.contains("requires a non-empty reply_message")
    {
        return RuntimeErrorKind::InvalidToolArgs;
    }
    RuntimeErrorKind::ToolSchemaError
}

fn runtime_error_contract_refs(kind: RuntimeErrorKind) -> Vec<String> {
    match kind {
        RuntimeErrorKind::MissingFinishAndSend
        | RuntimeErrorKind::EventIdMissingOrStale
        | RuntimeErrorKind::TransportCompletionViolation => {
            vec!["event completion contract".to_string()]
        }
        RuntimeErrorKind::MissingNoticeResolved | RuntimeErrorKind::ClaimedInputLeftUnresolved => {
            vec!["app notice completion contract".to_string()]
        }
        RuntimeErrorKind::InvalidToolArgs | RuntimeErrorKind::ToolSchemaError => {
            vec!["tool argument contract".to_string()]
        }
        RuntimeErrorKind::StaleBrowserRef => vec!["browser reference freshness".to_string()],
        RuntimeErrorKind::WrongTerminalSessionContinuation => {
            vec!["terminal session continuation".to_string()]
        }
        RuntimeErrorKind::PlanContractViolation => vec!["plan contract".to_string()],
        RuntimeErrorKind::RepeatedIdenticalToolError => vec!["tool retry contract".to_string()],
        RuntimeErrorKind::ContextOverflowAfterRecovery => {
            vec!["context overflow recovery".to_string()]
        }
    }
}

fn runtime_error_event_sources(inputs: &[ClaimedRuntimeInput]) -> Vec<String> {
    let mut sources = inputs
        .iter()
        .filter_map(|input| match input {
            ClaimedRuntimeInput::Event(event) => Some(event.source.to_string()),
            ClaimedRuntimeInput::AppNotice { .. } => None,
        })
        .collect::<Vec<_>>();
    sources.sort();
    sources.dedup();
    sources
}

fn runtime_error_user_request_summary(inputs: &[ClaimedRuntimeInput]) -> Option<String> {
    let summaries = inputs
        .iter()
        .map(|input| match input {
            ClaimedRuntimeInput::Event(event) => match &event.payload {
                EventPayload::TelegramIncoming(payload) => compact_runtime_error_text(
                    &format!(
                        "telegram from {}: {}",
                        payload.sender, payload.incoming_text
                    ),
                    240,
                ),
                EventPayload::TerminalIncoming(payload) => compact_runtime_error_text(
                    &format!("terminal {}: {}", payload.origin, payload.incoming_text),
                    240,
                ),
            },
            ClaimedRuntimeInput::AppNotice { app, reason } => {
                compact_runtime_error_text(&format!("app notice {app}: {reason}"), 240)
            }
        })
        .collect::<Vec<_>>();
    if summaries.is_empty() {
        None
    } else {
        Some(summaries.join(" | "))
    }
}

fn compact_runtime_error_text(text: &str, max_chars: usize) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = compact.chars();
    let mut value = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        value.push_str("...");
    }
    value
}

fn append_committed_activity_cells(
    context: &Context,
    tx: Option<&tokio::sync::watch::Sender<DashboardState>>,
    cells: Vec<crate::dashboard::ActivityCell>,
) {
    if cells.is_empty() {
        return;
    }
    let history_items = dashboard_activity_items_from_cells(&cells);
    let persisted_window = context.dashboard_history.as_ref().and_then(|history| {
        persist_dashboard_activity_items(history, &history_items).map_or_else(
            |err| {
                tracing::warn!("persist dashboard activity history failed: {err:?}");
                None
            },
            Some,
        )
    });
    if let Some(tx) = tx {
        tx.send_modify(|state| {
            if let Some(window) = persisted_window.as_ref() {
                state.activity_history = window.clone();
            } else {
                state
                    .activity_history
                    .merge_new_items(history_items.clone());
            }
            apply_activity_event(
                state,
                DashboardActivityEvent::AppendCommittedCells { cells },
            );
        });
    }
}

fn dashboard_activity_items_from_cells(
    cells: &[crate::dashboard::ActivityCell],
) -> Vec<crate::dashboard::WebActivityItem> {
    cells
        .iter()
        .map(|cell| {
            let item_id = format!(
                "activity-{}-{}",
                chrono::Utc::now().timestamp_millis(),
                uuid::Uuid::new_v4()
            );
            web_activity_item_from_cell(cell, &item_id, false)
        })
        .collect()
}

fn persist_dashboard_activity_items(
    history: &DashboardActivityHistoryStore,
    items: &[crate::dashboard::WebActivityItem],
) -> miette::Result<DashboardActivityHistoryWindow> {
    history.append_items(items)?;
    Ok(history.load_initial_window())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn afterclaim_context_completion_detection_requires_matching_close_tag() {
        assert!(is_complete_afterclaim_context_text(
            "<afterclaim_context>\nclaimed\n</afterclaim_context>"
        ));
        assert!(!is_complete_afterclaim_context_text(
            "<afterclaim_context>\nclaimed"
        ));
    }

    #[test]
    fn afterclaim_agent_content_carries_telegram_image_parts() {
        let input = AfterClaimContextInput {
            events: vec![crate::events::EventView {
                event_id: uuid::Uuid::nil(),
                source: crate::events::EventSource::Telegram,
                status: crate::events::EventStatus::Claimed,
                arrived_at_ms: 1,
                payload: crate::events::EventPayload::TelegramIncoming(
                    crate::events::TelegramIncomingEvent {
                        chat_id: "chat-1".to_string(),
                        chat_kind: "private".to_string(),
                        chat_title: "Alice".to_string(),
                        sender: "alice".to_string(),
                        incoming_text: "inspect this".to_string(),
                        telegram_update_id: 10,
                        telegram_message_id: Some(20),
                        telegram_message_date: Some(30),
                        attachments: vec![crate::events::TelegramIncomingAttachment {
                            kind: crate::events::TelegramIncomingAttachmentKind::Image,
                            file_id: "file-1".to_string(),
                            file_unique_id: "unique-1".to_string(),
                            media_type: "image/png".to_string(),
                            local_path: "/tmp/image.png".to_string(),
                            description: Some("telegram photo 512x512".to_string()),
                        }],
                    },
                ),
                last_error: None,
            }],
            app_notices: Vec::new(),
        };

        let content = afterclaim_agent_content("claimed input".to_string(), &input);

        assert_eq!(content.as_text(), "claimed input");
        assert_eq!(
            content.parts(),
            &[AgentContentPart::Image {
                path: "/tmp/image.png".to_string(),
                media_type: "image/png".to_string(),
                description: Some("telegram photo 512x512".to_string()),
            }]
        );
    }

    #[test]
    fn runtime_compaction_boundary_output_is_detected() {
        let output = runtime_context_compacted_output("compacted");
        assert!(output_is_runtime_context_compaction_boundary(&output));
    }
}
