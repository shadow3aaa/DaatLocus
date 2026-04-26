use super::model_driver::run_agent_turn_with_retry;
use super::*;

fn enter_runtime_phase(
    context: &mut Context,
    tx: Option<&tokio::sync::watch::Sender<DashboardState>>,
    phase: RuntimeTurnPhase,
) {
    context.set_runtime_phase(Some(phase));
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

pub(crate) async fn execute_agent_loop_step(
    context: &mut Context,
    tx: Option<&tokio::sync::watch::Sender<DashboardState>>,
) -> AgentLoopStepExecution {
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
    let claimed_input_messages = claimed_inputs
        .iter()
        .map(|input| prompt_message_for_claimed_input(context, input))
        .collect::<Vec<_>>();
    let claimed_event_views = claimed_inputs
        .iter()
        .filter_map(|input| match input {
            ClaimedRuntimeInput::Event(event) => Some(event.clone()),
            ClaimedRuntimeInput::AppNotice { .. } => None,
        })
        .collect::<Vec<_>>();
    let live_draft_session = maybe_start_telegram_live_draft_session(context, &claimed_event_views);
    enter_runtime_phase(context, tx, RuntimeTurnPhase::PreflightSnapshot);
    let snapshot_started_at = std::time::Instant::now();
    tracing::info!(
        "runtime preflight stage started: {}",
        RuntimeTurnPhase::PreflightSnapshot.label()
    );
    let snapshot = match tokio::time::timeout(
        preflight_timeout,
        Snapshot::new_with_claimed_events(context, &claimed_event_views),
    )
    .await
    {
        Ok(snapshot) => {
            tracing::info!(
                elapsed_ms = snapshot_started_at.elapsed().as_millis(),
                "runtime preflight stage completed: {}",
                RuntimeTurnPhase::PreflightSnapshot.label()
            );
            snapshot
        }
        Err(_) => {
            let err = miette!(
                "runtime preflight stage `{}` timed out after {}s",
                RuntimeTurnPhase::PreflightSnapshot.label(),
                preflight_timeout.as_secs()
            );
            set_runtime_status(
                tx,
                RuntimeStatusLevel::Error,
                format!(
                    "runtime turn preflight timeout: {}",
                    RuntimeTurnPhase::PreflightSnapshot.label()
                ),
            );
            tracing::error!(
                elapsed_ms = snapshot_started_at.elapsed().as_millis(),
                timeout_secs = preflight_timeout.as_secs(),
                "runtime preflight stage timed out: {}",
                RuntimeTurnPhase::PreflightSnapshot.label()
            );
            return abort_runtime_turn_before_model(
                context,
                RuntimeTurnAbort {
                    live_draft_session,
                    claimed_input_fingerprint: claimed_input_fingerprint.as_deref(),
                    claimed_event_ids: &claimed_event_ids,
                    claimed_app_notices: &claimed_app_notice_entries,
                    observation: format!("runtime preflight failed: {err}"),
                    description: "Failed to build runtime snapshot.".to_string(),
                },
            )
            .await;
        }
    };
    let snapshot_text = build_runtime_snapshot_text(context, &snapshot);
    if let Some(tx) = tx {
        tx.send_modify(|state| {
            state.snapshot_output = snapshot_text.clone();
        });
    }
    let request_envelope = build_runtime_request_envelope(context, &snapshot_text);
    let initial_tools = build_runtime_tool_specs(context);
    let runtime_conversation_budget = request_envelope
        .conversation_budget_tokens(&initial_tools, runtime_request_budget_limits(context));
    let runtime_conversation_summary_budget =
        RUNTIME_HISTORY_SUMMARY_MAX_TOKENS.min(runtime_conversation_budget);
    if let Some(plan) = context.memory.plan_runtime_conversation_compaction(
        runtime_conversation_budget,
        RUNTIME_HISTORY_MIN_MESSAGES,
        runtime_conversation_summary_budget,
    ) {
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
    }
    let mut conversation_slice = context.memory.runtime_conversation_slice(
        runtime_conversation_budget,
        RUNTIME_HISTORY_MIN_MESSAGES,
        runtime_conversation_summary_budget,
    );
    conversation_slice.extend(claimed_input_messages.iter().cloned());
    let mut runtime_step = context
        .memory
        .begin_runtime_step_from_parts(request_envelope, conversation_slice);
    let mut tool_results = Vec::new();
    let mut actions = Vec::new();
    let mut budget_recoveries = 0usize;

    let output = 'agent_loop: loop {
        let tools = build_runtime_tool_specs(context);
        if maybe_compact_runtime_messages(context, &mut runtime_step, &tools, false).await {
            set_runtime_status(tx, RuntimeStatusLevel::Info, "Compacting runtime context");
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
                    continue 'agent_loop;
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
                    append_committed_activity_cells(tx, vec![cell]);
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
        let tool_titles_in_reasoning = response
            .last_reasoning_content
            .as_deref()
            .is_some_and(|content| !content.trim().is_empty());

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
            append_committed_activity_cells(tx, committed_cells);
            for (call, call_ui_event) in calls.iter().zip(tool_call_ui_events.iter()) {
                if let Some(title) = live_tool_call_title(call_ui_event) {
                    context.emit_live_tool_call_title(title, tool_titles_in_reasoning);
                }
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
            append_committed_activity_cells(tx, vec![cell]);
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
    finalize_claimed_runtime_events(context, &claimed_event_ids, &output);
    finalize_claimed_runtime_app_notices(context, &claimed_app_notice_entries, &output).await;
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

fn live_tool_call_title(event: &ToolCallUiEvent) -> Option<&str> {
    match event {
        ToolCallUiEvent::Exec(event)
        | ToolCallUiEvent::Plan(event)
        | ToolCallUiEvent::CreateWorkflow(event)
        | ToolCallUiEvent::ActivateWorkflow(event)
        | ToolCallUiEvent::DeepRecall(event)
        | ToolCallUiEvent::App(event)
        | ToolCallUiEvent::Error(event) => Some(event.title.as_str()),
        ToolCallUiEvent::Terminal(event) => Some(event.title.as_str()),
        ToolCallUiEvent::Browser(event) => Some(event.title.as_str()),
        ToolCallUiEvent::Telegram(event) => Some(event.title.as_str()),
        ToolCallUiEvent::Patch(event) => Some(event.summary_line.as_str()),
    }
}

fn append_committed_activity_cells(
    tx: Option<&tokio::sync::watch::Sender<DashboardState>>,
    cells: Vec<crate::dashboard::ActivityCell>,
) {
    if cells.is_empty() {
        return;
    }
    if let Some(tx) = tx {
        tx.send_modify(|state| {
            apply_activity_event(
                state,
                DashboardActivityEvent::AppendCommittedCells { cells },
            );
        });
    }
}
