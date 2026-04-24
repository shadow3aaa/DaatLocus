use super::*;

pub(super) fn build_agent_turn_payload_common(
    client: &OpenAIClient,
    request: AgentTurnRequest,
    stream: bool,
    flatten_orphan_tool_messages: bool,
) -> serde_json::Value {
    let messages = agent_turn_request_to_openai_messages(
        request.messages,
        flatten_orphan_tool_messages,
        is_deepseek_api_base_url(&client.base_url),
    );
    let tools = request
        .tools
        .into_iter()
        .map(|tool| {
            let (description, parameters, strict) = match tool.input_spec {
                AgentToolInputSpec::JsonSchema { schema } => (
                    tool.description,
                    normalize_provider_function_schema(schema),
                    true,
                ),
                AgentToolInputSpec::FreeformGrammar {
                    syntax,
                    definition,
                    fallback_schema,
                } => (
                    format!(
                        "{}\n\nThis is a FREEFORM grammar tool. The current provider falls back to single-string input: put the complete tool input in the `input` field.\nsyntax={syntax}\ndefinition=\n{definition}",
                        tool.description
                    ),
                    normalize_provider_function_schema(fallback_schema),
                    false,
                ),
            };
            json!({
                "type": "function",
                "function": {
                    "strict": strict,
                    "name": tool.name,
                    "description": description,
                    "parameters": parameters,
                }
            })
        })
        .collect::<Vec<_>>();
    let mut payload = json!({
        "model": client.model,
        "messages": messages,
        "tools": tools,
        "temperature": client.temperature,
        "max_tokens": max_completion_tokens_for_chat_payload(client),
        "stream": stream,
        "stream_options": if stream {
            json!({ "include_usage": true })
        } else {
            serde_json::Value::Null
        },
    });
    apply_provider_thinking_config(
        &mut payload,
        client,
        client.thinking_budget.as_deref(),
        client.adapter_state_guard().thinking_budget_mode,
    );
    payload
}

pub(super) fn history_message_to_openai_message(
    message: crate::reasoning::runtime::HistoryMessage,
    flatten_tool_messages: bool,
) -> serde_json::Value {
    provider_message_from_agent_message(&message.message, flatten_tool_messages)
}

pub(super) fn prompt_request_to_openai_messages(
    request: PromptRequest,
    flatten_tool_messages: bool,
) -> Vec<serde_json::Value> {
    request
        .system_messages
        .into_iter()
        .map(|message| json!({"role": "system", "content": message}))
        .chain(
            request
                .long_term_memory_messages
                .into_iter()
                .map(|message| history_message_to_openai_message(message, flatten_tool_messages)),
        )
        .chain(
            request
                .history_messages
                .into_iter()
                .map(|message| history_message_to_openai_message(message, flatten_tool_messages)),
        )
        .chain(std::iter::once(json!({
            "role": "user",
            "content": request.current_user_message,
        })))
        .chain(
            request
                .retry_messages
                .into_iter()
                .map(|message| history_message_to_openai_message(message, flatten_tool_messages)),
        )
        .collect::<Vec<_>>()
}

pub(super) fn agent_message_to_openai_message(
    message: AgentMessage,
    include_reasoning_content: bool,
) -> serde_json::Value {
    match message {
        AgentMessage::System { content } => json!({
            "role": "system",
            "content": content,
        }),
        AgentMessage::User { content } => json!({
            "role": "user",
            "content": content,
        }),
        AgentMessage::Assistant { content } => json!({
            "role": "assistant",
            "content": content,
        }),
        AgentMessage::AssistantToolCallProtocol {
            content,
            reasoning_content,
            calls,
        } => {
            let mut message = json!({
                "role": "assistant",
                "content": content.unwrap_or_default(),
                "tool_calls": calls.into_iter().map(|call| json!({
                    "id": call.id,
                    "type": "function",
                    "function": {
                        "name": call.name,
                        "arguments": call.arguments.to_string(),
                    }
                })).collect::<Vec<_>>(),
            });
            if include_reasoning_content
                && let Some(reasoning_content) = reasoning_content
                && !reasoning_content.trim().is_empty()
            {
                message["reasoning_content"] = json!(reasoning_content);
            }
            message
        }
        AgentMessage::Tool {
            tool_call_id,
            name,
            content,
        } => json!({
            "role": "tool",
            "tool_call_id": tool_call_id,
            "name": name,
            "content": content,
        }),
    }
}

pub(super) fn provider_message_from_agent_message(
    message: &AgentMessage,
    flatten_non_plain_messages: bool,
) -> serde_json::Value {
    if flatten_non_plain_messages {
        match message {
            AgentMessage::AssistantToolCallProtocol { content, calls, .. } => {
                return json!({
                    "role": "assistant",
                    "content": summarize_assistant_tool_call_protocol(content.as_deref(), calls),
                });
            }
            AgentMessage::Tool { name, content, .. } => {
                return json!({
                    "role": "assistant",
                    "content": flatten_tool_result_as_assistant_text(name, content),
                });
            }
            _ => {}
        }
    }
    agent_message_to_openai_message(message.clone(), false)
}

pub(super) fn agent_turn_request_to_openai_messages(
    messages: Vec<AgentMessage>,
    flatten_orphan_tool_messages: bool,
    include_reasoning_content: bool,
) -> Vec<serde_json::Value> {
    let mut valid_tool_call_ids = HashSet::new();
    let mut serialized = Vec::with_capacity(messages.len());
    for message in messages {
        match message {
            AgentMessage::AssistantToolCallProtocol {
                content,
                reasoning_content,
                calls,
            } => {
                if flatten_orphan_tool_messages {
                    valid_tool_call_ids.extend(calls.iter().map(|call| call.id.clone()));
                }
                serialized.push(agent_message_to_openai_message(
                    AgentMessage::AssistantToolCallProtocol {
                        content,
                        reasoning_content,
                        calls,
                    },
                    include_reasoning_content,
                ));
            }
            AgentMessage::Tool {
                tool_call_id,
                name,
                content,
            } if flatten_orphan_tool_messages && !valid_tool_call_ids.contains(&tool_call_id) => {
                serialized.push(json!({
                    "role": "assistant",
                    "content": flatten_tool_result_as_assistant_text(&name, &content),
                }));
            }
            other => serialized.push(agent_message_to_openai_message(other, false)),
        }
    }
    serialized
}

pub(super) fn flatten_tool_result_as_assistant_text(name: &str, content: &str) -> String {
    format!("historical tool result ({name}):\n{content}")
}
