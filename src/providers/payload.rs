use base64::Engine;

use super::*;

pub(super) fn build_agent_turn_payload_common(
    client: &OpenAIClient,
    request: AgentTurnRequest,
    stream: bool,
    flatten_orphan_tool_messages: bool,
) -> serde_json::Value {
    let strip_images = client.adapter_state_guard().vision_mode == VisionMode::Disabled;
    let messages = agent_turn_request_to_openai_messages(
        request.messages,
        flatten_orphan_tool_messages,
        is_deepseek_api_base_url(&client.base_url),
        strip_images,
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
        client.thinking_budget,
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
    strip_images: bool,
) -> serde_json::Value {
    match message {
        AgentMessage::System { content } => json!({
            "role": "system",
            "content": content,
        }),
        AgentMessage::User { content } => json!({
            "role": "user",
            "content": openai_user_content(content, strip_images),
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
    agent_message_to_openai_message(message.clone(), false, false)
}

pub(super) fn agent_turn_request_to_openai_messages(
    messages: Vec<AgentMessage>,
    flatten_orphan_tool_messages: bool,
    include_reasoning_content: bool,
    strip_images: bool,
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
                    strip_images,
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
            other => serialized.push(agent_message_to_openai_message(other, false, strip_images)),
        }
    }
    serialized
}

pub(super) fn flatten_tool_result_as_assistant_text(name: &str, content: &str) -> String {
    format!("historical tool result ({name}):\n{content}")
}

pub(super) fn image_part_data_url(part: &AgentContentPart) -> Option<String> {
    let AgentContentPart::Image {
        path, media_type, ..
    } = part
    else {
        return None;
    };
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) => {
            warn!("failed to read multimodal image attachment {path}: {err}");
            return None;
        }
    };
    let media_type = normalize_image_part_media_type(path, media_type)?;
    Some(format!(
        "data:{media_type};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

fn normalize_image_part_media_type(path: &str, media_type: &str) -> Option<String> {
    let media_type = media_type.trim();
    if media_type.starts_with("image/") {
        return Some(media_type.to_string());
    }
    match std::path::Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => Some("image/png".to_string()),
        Some("jpg") | Some("jpeg") => Some("image/jpeg".to_string()),
        Some("webp") => Some("image/webp".to_string()),
        Some("gif") => Some("image/gif".to_string()),
        _ => {
            warn!(
                "failed to infer image MIME type for multimodal attachment {path}: media_type={media_type}"
            );
            None
        }
    }
}

fn openai_user_content(content: AgentContent, strip_images: bool) -> serde_json::Value {
    if content.is_plain_text() {
        return json!(content.as_text());
    }

    let mut parts = Vec::new();
    if !content.as_text().trim().is_empty() {
        parts.push(json!({
            "type": "text",
            "text": content.as_text(),
        }));
    }
    for part in content.parts() {
        match part {
            AgentContentPart::Text { text } => {
                parts.push(json!({
                    "type": "text",
                    "text": text,
                }));
            }
            AgentContentPart::Image {
                path, description, ..
            } => {
                if strip_images {
                    parts.push(json!({
                        "type": "text",
                        "text": format!(
                            "[image: {}]",
                            description.as_deref().unwrap_or(path)
                        ),
                    }));
                    continue;
                }
                let Some(url) = image_part_data_url(part) else {
                    parts.push(json!({
                        "type": "text",
                        "text": format!(
                            "[image attachment unavailable: {}]",
                            description.as_deref().unwrap_or(path)
                        ),
                    }));
                    continue;
                };
                parts.push(json!({
                    "type": "image_url",
                    "image_url": {
                        "url": url,
                    },
                }));
            }
        }
    }
    parts.into()
}
