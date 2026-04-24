use super::*;

pub(super) async fn build_hindsight_memory_context(
    context: &mut Context,
    claimed_inputs: &[ClaimedRuntimeInput],
) -> PromptMemoryContext {
    let hindsight = context.hindsight.clone();
    let current_input = current_turn_input_for_hindsight(claimed_inputs);

    let query = build_hindsight_recall_query(
        current_input.as_deref(),
        select_recent_runtime_conversation_for_hindsight(
            &context.memory.runtime_conversation_messages(),
            current_input.as_deref(),
        ),
    );

    let observations = hindsight
        .recall(
            &query,
            HindsightRecallOptions {
                types: vec!["observation".to_string()],
                max_tokens: 1200,
                budget: Some("mid".to_string()),
                include_chunks: false,
                max_chunk_tokens: 0,
                include_source_facts: false,
                max_source_facts_tokens: 0,
                ..Default::default()
            },
        )
        .await;
    let observations = match observations {
        Ok(response) => response
            .results
            .into_iter()
            .take(4)
            .map(Into::into)
            .collect::<Vec<_>>(),
        Err(err) => {
            tracing::warn!("hindsight observation recall failed: {err:?}");
            Vec::new()
        }
    };

    let raw_memories = hindsight
        .recall(
            &query,
            HindsightRecallOptions {
                types: vec!["world".to_string(), "experience".to_string()],
                max_tokens: 1400,
                budget: Some("mid".to_string()),
                include_chunks: false,
                max_chunk_tokens: 0,
                include_source_facts: true,
                max_source_facts_tokens: 1200,
                ..Default::default()
            },
        )
        .await;
    let raw_memories = match raw_memories {
        Ok(response) => response
            .results
            .into_iter()
            .take(4)
            .map(Into::into)
            .collect::<Vec<_>>(),
        Err(err) => {
            tracing::warn!("hindsight raw memory recall failed: {err:?}");
            Vec::new()
        }
    };

    let citations = build_prompt_memory_citations(&observations, &raw_memories);
    tracing::debug!(
        "hindsight memory context observations={} raw_memories={} citations={}",
        observations.len(),
        raw_memories.len(),
        citations.len()
    );

    PromptMemoryContext {
        observations,
        raw_memories,
        citations,
    }
}

fn build_prompt_memory_citations(
    observations: &[crate::reasoning::runtime::PromptMemoryFact],
    raw_memories: &[crate::reasoning::runtime::PromptMemoryFact],
) -> Vec<PromptMemoryCitation> {
    let mut citations = Vec::new();
    citations.extend(observations.iter().map(|memory| PromptMemoryCitation {
        kind: "observation".to_string(),
        id: memory.id.clone(),
        summary: summarize_hindsight_query_value(&memory.text, 96),
    }));
    citations.extend(raw_memories.iter().map(|memory| {
        PromptMemoryCitation {
            kind: memory
                .memory_type
                .clone()
                .unwrap_or_else(|| "memory".to_string()),
            id: memory.id.clone(),
            summary: summarize_hindsight_query_value(&memory.text, 96),
        }
    }));
    citations
}

fn build_hindsight_recall_query(
    current_input: Option<&str>,
    recent_messages: Vec<String>,
) -> String {
    let mut lines = vec!["问题：召回最相关的历史经验，帮助继续推进当前任务。".to_string()];
    if !recent_messages.is_empty() {
        lines.push("前文:".to_string());
        lines.extend(
            recent_messages
                .into_iter()
                .map(|line| format!("- {}", summarize_hindsight_query_value(&line, 120))),
        );
    }
    if let Some(current_input) = current_input.filter(|value| !value.trim().is_empty()) {
        lines.push("当前输入:".to_string());
        lines.push(summarize_hindsight_query_value(current_input, 240));
    }

    let mut query = lines.join("\n");
    if approx_token_count(&query) > HINDSIGHT_RECALL_QUERY_MAX_TOKENS {
        query = truncate_hindsight_query_preserving_latest_input(
            &query,
            current_input.unwrap_or_default(),
            HINDSIGHT_RECALL_QUERY_MAX_TOKENS,
        );
    }
    query
}

fn current_turn_input_for_hindsight(claimed_inputs: &[ClaimedRuntimeInput]) -> Option<String> {
    claimed_inputs.first().and_then(|input| match input {
        ClaimedRuntimeInput::Event(event) => match &event.payload {
            EventPayload::TelegramIncoming(payload) => {
                let text = payload.incoming_text.trim();
                (!text.is_empty()).then(|| text.to_string())
            }
            EventPayload::TerminalIncoming(payload) => {
                let text = payload.incoming_text.trim();
                (!text.is_empty()).then(|| text.to_string())
            }
        },
        ClaimedRuntimeInput::AppNotice { app, reason } => {
            let reason = reason.trim();
            (!reason.is_empty()).then(|| format!("app notice from {app}: {reason}"))
        }
    })
}

fn select_recent_runtime_conversation_for_hindsight(
    messages: &[HistoryMessage],
    latest_input: Option<&str>,
) -> Vec<String> {
    let contextual_messages = slice_recent_runtime_conversation_turns(messages, 1);
    contextual_messages
        .into_iter()
        .filter_map(|message| format_runtime_message_for_hindsight(message, latest_input))
        .collect()
}

fn slice_recent_runtime_conversation_turns(
    messages: &[HistoryMessage],
    turns: usize,
) -> &[HistoryMessage] {
    if messages.is_empty() || turns == 0 {
        return &[];
    }

    let mut user_turns_seen = 0usize;
    let mut start_index = None;
    for (index, message) in messages.iter().enumerate().rev() {
        if message.is_user() {
            user_turns_seen += 1;
            if user_turns_seen >= turns {
                start_index = Some(index);
                break;
            }
        }
    }

    match start_index {
        Some(index) => &messages[index..],
        None => messages,
    }
}

fn format_runtime_message_for_hindsight(
    message: &HistoryMessage,
    latest_input: Option<&str>,
) -> Option<String> {
    let content = message.text_content()?.trim();
    if content.is_empty()
        || is_runtime_summary_message_for_hindsight(content)
        || content.starts_with("assistant tool-call protocol:")
    {
        return None;
    }

    let role = match &message.message {
        AgentMessage::User { .. } => "user",
        AgentMessage::Assistant { .. } => "assistant",
        AgentMessage::AssistantToolCallProtocol { .. } => "assistant",
        AgentMessage::System { .. } | AgentMessage::Tool { .. } => return None,
    };

    if role == "user" && latest_input.is_some_and(|latest| latest.trim() == content) {
        return None;
    }

    Some(format!("{role}: {content}"))
}

fn is_runtime_summary_message_for_hindsight(content: &str) -> bool {
    content.starts_with("Earlier runtime history summary:")
        || content.starts_with("Earlier tool/context progress summary:")
}

fn truncate_hindsight_query_preserving_latest_input(
    query: &str,
    latest_input: &str,
    max_tokens: usize,
) -> String {
    let latest_input = latest_input.trim();
    if latest_input.is_empty() {
        return truncate_text_to_token_budget(query, max_tokens);
    }

    let latest_only =
        format!("问题：召回最相关的历史经验，帮助继续推进当前任务。\n当前输入:\n{latest_input}");
    if approx_token_count(&latest_only) > max_tokens {
        return truncate_text_to_token_budget(&latest_only, max_tokens);
    }

    let marker = "\n当前输入:\n";
    let Some(marker_index) = query.find(marker) else {
        return truncate_text_to_token_budget(query, max_tokens);
    };
    let suffix = &query[marker_index..];
    if approx_token_count(suffix) >= max_tokens {
        return truncate_text_to_token_budget(&latest_only, max_tokens);
    }

    let prefix = &query[..marker_index];
    let prefix_lines = prefix.lines().collect::<Vec<_>>();
    let mut kept_prefix_lines = Vec::new();
    for line in prefix_lines.into_iter().rev() {
        kept_prefix_lines.insert(0, line);
        let candidate = format!("{}\n{}", kept_prefix_lines.join("\n"), suffix.trim_start());
        if approx_token_count(&candidate) > max_tokens {
            kept_prefix_lines.remove(0);
            break;
        }
    }
    let mut result = if kept_prefix_lines.is_empty() {
        latest_only
    } else {
        format!("{}\n{}", kept_prefix_lines.join("\n"), suffix.trim_start())
    };
    if approx_token_count(&result) > max_tokens {
        result = truncate_text_to_token_budget(&result, max_tokens);
    }
    result
}

fn summarize_hindsight_query_value(value: &str, max_chars: usize) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let char_count = compact.chars().count();
    if char_count <= max_chars {
        return compact;
    }
    let head = compact.chars().take(max_chars).collect::<String>();
    format!("{head}...")
}
