//! Responses history replay shared by API-key and ChatGPT-authenticated clients.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::reasoning::runtime::{AgentContent, AgentMessage, AgentToolInputSpec, AgentToolSpec};

use super::{flatten_tool_result_as_assistant_text, io::responses_safe_call_id};

pub(super) fn messages_to_responses_parts(
    messages: &[AgentMessage],
    strip_images: bool,
    user_message: fn(&AgentContent, bool) -> Value,
) -> (String, Vec<Value>) {
    let mut instructions = Vec::new();
    let mut input = Vec::new();
    let mut call_types = HashMap::new();
    for message in messages {
        // Replay the wire items verbatim: rebuilding them from display text loses
        // message phases, encrypted reasoning, and custom tool input formats.
        if let Some(output) = message.responses_output() {
            for item in output {
                if let Some(call_id) = item["call_id"].as_str() {
                    call_types.insert(call_id.to_string(), item["type"] == "custom_tool_call");
                }
                let mut item = item.clone();
                if let Some(call_id) = item["call_id"].as_str() {
                    item["call_id"] = json!(responses_safe_call_id(call_id));
                }
                input.push(item);
            }
            continue;
        }
        match message {
            AgentMessage::System { content } => instructions.push(content.clone()),
            AgentMessage::User { content } => input.push(user_message(content, strip_images)),
            AgentMessage::Assistant { content, .. } => input.push(assistant_message(content)),
            AgentMessage::AssistantToolCallProtocol { content, calls, .. } => {
                if let Some(content) = content.as_deref().filter(|text| !text.trim().is_empty()) {
                    input.push(assistant_message(content));
                }
                for call in calls {
                    let custom = call.arguments.is_string();
                    call_types.insert(call.id.clone(), custom);
                    input.push(if custom {
                        json!({
                            "type": "custom_tool_call",
                            "call_id": responses_safe_call_id(&call.id),
                            "name": call.name,
                            "input": call.arguments.as_str().unwrap_or_default(),
                        })
                    } else {
                        json!({
                            "type": "function_call",
                            "call_id": responses_safe_call_id(&call.id),
                            "name": call.name,
                            "arguments": call.arguments.to_string(),
                        })
                    });
                }
            }
            AgentMessage::Tool {
                tool_call_id,
                name,
                content,
            } => {
                if let Some(custom) = call_types.get(tool_call_id) {
                    input.push(json!({
                        "type": if *custom { "custom_tool_call_output" } else { "function_call_output" },
                        "call_id": responses_safe_call_id(tool_call_id),
                        "output": content,
                    }));
                } else {
                    input.push(assistant_message(&flatten_tool_result_as_assistant_text(
                        name, content,
                    )));
                }
            }
        }
    }
    (instructions.join("\n\n"), input)
}

fn assistant_message(content: &str) -> Value {
    json!({ "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": content }] })
}

/// Only output domains supported by the runtime are persisted for replay.
pub(super) fn replayable_output_item(item: &Value) -> bool {
    matches!(
        item["type"].as_str(),
        Some("message" | "reasoning" | "function_call" | "custom_tool_call")
    )
}

pub(super) fn output_needs_follow_up(output: &[Value], end_turn: Option<bool>) -> bool {
    end_turn == Some(false)
        || matches!(
            output
                .iter()
                .rev()
                .find(|item| item["type"] == "message")
                .and_then(|item| item["phase"].as_str()),
            Some("commentary" | "partial_answer")
        )
}

pub(super) fn tool_to_responses_tool(tool: AgentToolSpec) -> Value {
    match tool.input_spec {
        AgentToolInputSpec::JsonSchema { schema } => json!({
            "type": "function", "name": tool.name, "description": tool.description,
            "strict": true, "parameters": schema,
        }),
        AgentToolInputSpec::FreeformGrammar {
            syntax,
            definition,
            fallback_schema,
        } => {
            if matches!(syntax.as_str(), "lark" | "regex") {
                json!({
                    "type": "custom", "name": tool.name, "description": tool.description,
                    "format": { "type": "grammar", "syntax": syntax, "definition": definition },
                })
            } else {
                json!({
                    "type": "function", "name": tool.name,
                    "description": format!("{}\n\nPut the complete freeform tool input in the `input` field.\nsyntax={syntax}\ndefinition=\n{definition}", tool.description),
                    "strict": true, "parameters": fallback_schema,
                })
            }
        }
    }
}

#[cfg(test)]
pub(super) async fn test_sse_response(events: &[Value], done: bool) -> reqwest::Response {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let mut body = events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect::<String>();
    if done {
        body.push_str("data: [DONE]\n\n");
    }
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        socket.read(&mut request).await.unwrap();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
        socket.shutdown().await.unwrap();
    });
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(format!("http://{address}"))
        .send()
        .await
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reasoning::runtime::AgentToolCall;

    fn user_message(content: &AgentContent, _: bool) -> Value {
        json!({"type": "message", "role": "user", "content": content.as_text()})
    }

    #[test]
    fn commentary_and_partial_answers_continue_until_final_answer() {
        for phase in ["commentary", "partial_answer"] {
            let output = vec![json!({"type": "message", "phase": phase})];
            assert!(output_needs_follow_up(&output, None));
        }
        let output = vec![json!({"type": "message", "phase": "final_answer"})];
        assert!(!output_needs_follow_up(&output, None));
        assert!(output_needs_follow_up(&output, Some(false)));
        assert!(!output_needs_follow_up(&[json!({"type": "message"})], None));
    }

    #[test]
    fn replay_preserves_phase_encrypted_reasoning_and_custom_tool_pairing() {
        let output = vec![
            json!({"type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "opaque"}),
            json!({"type": "message", "id": "msg_1", "role": "assistant", "phase": "commentary", "content": [{"type": "output_text", "text": "Editing"}]}),
            json!({"type": "custom_tool_call", "id": "ctc_1", "call_id": "call_1", "name": "patch", "input": "*** patch"}),
        ];
        let messages = vec![
            AgentMessage::assistant_tool_call_protocol_with_signed_reasoning(
                Some("Editing".into()),
                None,
                None,
                vec![AgentToolCall {
                    id: "call_1".into(),
                    name: "patch".into(),
                    arguments: json!("*** patch"),
                }],
            )
            .with_responses_output(Some(output.clone())),
            AgentMessage::tool("call_1", "patch", "applied"),
        ];
        let persisted = serde_json::to_string(&messages).unwrap();
        let restored: Vec<AgentMessage> = serde_json::from_str(&persisted).unwrap();
        let (_, input) = messages_to_responses_parts(&restored, false, user_message);
        assert_eq!(&input[..3], &output);
        assert_eq!(
            input[3],
            json!({"type": "custom_tool_call_output", "call_id": "call_1", "output": "applied"})
        );
    }

    #[test]
    fn legacy_custom_history_uses_custom_input_and_output() {
        let messages = vec![
            AgentMessage::assistant_tool_call_protocol_with_reasoning(
                None,
                None,
                vec![
                    AgentToolCall {
                        id: "custom".into(),
                        name: "patch".into(),
                        arguments: json!("raw input"),
                    },
                    AgentToolCall {
                        id: "function".into(),
                        name: "exec".into(),
                        arguments: json!({"cmd": "pwd"}),
                    },
                ],
            ),
            AgentMessage::tool("custom", "patch", "done"),
            AgentMessage::tool("function", "exec", "cwd"),
        ];
        let (_, input) = messages_to_responses_parts(&messages, false, user_message);
        assert_eq!(input[0]["type"], "custom_tool_call");
        assert_eq!(input[0]["input"], "raw input");
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[2]["type"], "custom_tool_call_output");
        assert_eq!(input[3]["type"], "function_call_output");
    }

    #[test]
    fn grammar_tools_use_native_formats_or_strict_schema_fallback() {
        for syntax in ["lark", "regex", "unsupported"] {
            let schema = json!({"type": "object", "properties": {"input": {"type": "string"}}, "required": ["input"], "additionalProperties": false});
            let tool = tool_to_responses_tool(AgentToolSpec {
                name: "patch".into(),
                description: "Patch".into(),
                input_spec: AgentToolInputSpec::FreeformGrammar {
                    syntax: syntax.into(),
                    definition: "grammar".into(),
                    fallback_schema: schema.clone(),
                },
            });
            if syntax == "unsupported" {
                assert_eq!(tool["type"], "function");
                assert_eq!(tool["strict"], true);
                assert_eq!(tool["parameters"], schema);
            } else {
                assert_eq!(tool["type"], "custom");
                assert_eq!(tool["format"]["syntax"], syntax);
            }
        }
    }
}
