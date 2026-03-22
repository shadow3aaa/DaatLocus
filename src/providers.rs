//! 本模块实现实际的llm api调用

use std::error::Error as _;

use async_trait::async_trait;
use miette::{Result, miette};
use serde_json::json;

use crate::{
    config::{Config, MainModelConfig},
    context::Context,
    core::LLM,
    reasoning::runtime::{
        AgentMessage, AgentToolCall, AgentTurnRequest, AgentTurnResponse, PromptRequest, PromptRole,
    },
};
pub struct OpenAIClient {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    model: String,
    temperature: f64,
}

impl OpenAIClient {
    pub fn new(config: &Config) -> Self {
        Self::from_model_config(&config.main_model)
    }

    pub fn from_model_config(model_config: &MainModelConfig) -> Self {
        let client = reqwest::Client::new();
        let api_key = model_config.api_key.clone();
        let base_url = model_config.base_url.clone();
        let model = model_config.model_name.clone();
        let temperature = model_config.temperature;
        Self {
            client,
            api_key,
            base_url,
            model,
            temperature,
        }
    }

    fn url(&self) -> String {
        format!(
            "{}/v1/chat/completions",
            self.base_url.trim_end_matches('/')
        )
    }

    async fn call_tool_json(&self, request: PromptRequest) -> Result<serde_json::Value> {
        let url = self.url();
        let tool_name = request.tool_name.clone();
        let tool_description = request.tool_description.clone();
        let output_schema = request.output_schema.clone();
        let request_context = vec![
            format!("message_count={}", request.all_messages().len()),
            format!("tool_name={tool_name}"),
        ];
        let messages = prompt_request_to_openai_messages(request);
        let payload = json!({
            "model": self.model,
            "messages": messages,
            "tools": [
                {
                    "type": "function",
                    "function": {
                        "strict": true,
                        "name": tool_name,
                        "description": tool_description,
                        "parameters": output_schema
                    }
                }
            ],
            "tool_choice": {
                "type": "function",
                "function": { "name": tool_name }
            },
            "temperature": self.temperature
        });
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&payload)
            .send()
            .await
            .map_err(|err| {
                format_request_error("llm request failed", &url, &request_context, &err)
            })?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|err| miette!("llm response body read failed: {err}"))?;

        if !status.is_success() {
            return Err(miette!(
                "llm api returned HTTP {}: {}",
                status,
                truncate_for_error(&body)
            ));
        }

        let response_json: serde_json::Value = serde_json::from_str(&body).map_err(|err| {
            miette!(
                "llm response is not valid JSON: {err}; body={}",
                truncate_for_error(&body)
            )
        })?;
        let content = response_json["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("");
        let Some(tool_calls) = response_json["choices"][0]["message"]["tool_calls"].as_array()
        else {
            if !content.trim().is_empty()
                && let Ok(value) = serde_json::from_str::<serde_json::Value>(content)
            {
                return Ok(value);
            }
            return Err(miette!(
                "llm response did not include tool_calls; content={}; response={}",
                truncate_for_error(content),
                truncate_for_json_error(&response_json)
            ));
        };
        let first_tool_call = tool_calls.first().ok_or_else(|| {
            miette!(
                "llm response included empty tool_calls; response={}",
                truncate_for_json_error(&response_json)
            )
        })?;
        let arguments_str = first_tool_call["function"]["arguments"]
            .as_str()
            .ok_or_else(|| {
                miette!(
                    "llm response missing function.arguments string; response={}",
                    truncate_for_json_error(&response_json)
                )
            })?;
        serde_json::from_str(arguments_str).map_err(|err| {
            miette!(
                "failed to decode tool arguments as JSON: {err}; arguments={}",
                truncate_for_error(arguments_str)
            )
        })
    }

    async fn call_agent_turn(&self, request: AgentTurnRequest) -> Result<AgentTurnResponse> {
        let url = self.url();
        let request_context = summarize_agent_turn_request(&request);
        let messages = request
            .messages
            .into_iter()
            .map(agent_message_to_openai_message)
            .collect::<Vec<_>>();
        let tools = request
            .tools
            .into_iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "strict": true,
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.input_schema,
                    }
                })
            })
            .collect::<Vec<_>>();
        let payload = json!({
            "model": self.model,
            "messages": messages,
            "tools": tools,
            "temperature": self.temperature
        });
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&payload)
            .send()
            .await
            .map_err(|err| {
                format_request_error("llm request failed", &url, &request_context, &err)
            })?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|err| miette!("llm response body read failed: {err}"))?;

        if !status.is_success() {
            return Err(miette!(
                "llm api returned HTTP {}: {}",
                status,
                truncate_for_error(&body)
            ));
        }

        let response_json: serde_json::Value = serde_json::from_str(&body).map_err(|err| {
            miette!(
                "llm response is not valid JSON: {err}; body={}",
                truncate_for_error(&body)
            )
        })?;
        let message = &response_json["choices"][0]["message"];
        let content = message["content"]
            .as_str()
            .map(|text| text.to_string())
            .unwrap_or_default();

        if let Some(tool_calls) = message["tool_calls"].as_array()
            && !tool_calls.is_empty()
        {
            let mut calls = Vec::new();
            for tool_call in tool_calls {
                let id = tool_call["id"].as_str().ok_or_else(|| {
                    miette!(
                        "llm response missing tool_call.id; response={}",
                        truncate_for_json_error(&response_json)
                    )
                })?;
                let name = tool_call["function"]["name"].as_str().ok_or_else(|| {
                    miette!(
                        "llm response missing tool function name; response={}",
                        truncate_for_json_error(&response_json)
                    )
                })?;
                let arguments_str =
                    tool_call["function"]["arguments"].as_str().ok_or_else(|| {
                        miette!(
                            "llm response missing tool function arguments; response={}",
                            truncate_for_json_error(&response_json)
                        )
                    })?;
                let arguments = serde_json::from_str(arguments_str).map_err(|err| {
                    miette!(
                        "failed to decode tool arguments as JSON: {err}; arguments={}",
                        truncate_for_error(arguments_str)
                    )
                })?;
                calls.push(AgentToolCall {
                    id: id.to_string(),
                    name: name.to_string(),
                    arguments,
                });
            }
            return Ok(AgentTurnResponse::ToolCalls {
                content: if content.trim().is_empty() {
                    None
                } else {
                    Some(content)
                },
                calls,
            });
        }

        Ok(AgentTurnResponse::Assistant { content })
    }
}

fn summarize_agent_turn_request(request: &AgentTurnRequest) -> Vec<String> {
    let message_count = request.messages.len();
    let tool_count = request.tools.len();
    let message_chars = request
        .messages
        .iter()
        .map(agent_message_char_count)
        .sum::<usize>();
    let tool_names = request
        .tools
        .iter()
        .take(8)
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    vec![
        format!("message_count={message_count}"),
        format!("tool_count={tool_count}"),
        format!("message_chars={message_chars}"),
        format!(
            "tools={}",
            if tool_names.is_empty() {
                "<none>".to_string()
            } else {
                tool_names.join(", ")
            }
        ),
    ]
}

fn agent_message_char_count(message: &AgentMessage) -> usize {
    match message {
        AgentMessage::System { content }
        | AgentMessage::User { content }
        | AgentMessage::Assistant { content } => content.chars().count(),
        AgentMessage::AssistantToolCalls { content, calls } => {
            content.as_deref().unwrap_or_default().chars().count()
                + calls
                    .iter()
                    .map(|call| {
                        call.name.chars().count()
                            + call.id.chars().count()
                            + call.arguments.to_string().chars().count()
                    })
                    .sum::<usize>()
        }
        AgentMessage::Tool {
            tool_call_id,
            name,
            content,
        } => tool_call_id.chars().count() + name.chars().count() + content.chars().count(),
    }
}

fn format_request_error(
    prefix: &str,
    url: &str,
    request_context: &[String],
    err: &reqwest::Error,
) -> miette::Report {
    let mut lines = vec![format!("{prefix}: {err}"), format!("url={url}")];
    lines.extend(request_context.iter().cloned());
    if err.is_timeout() {
        lines.push("kind=timeout".to_string());
    } else if err.is_connect() {
        lines.push("kind=connect".to_string());
    } else if err.is_request() {
        lines.push("kind=request".to_string());
    } else if err.is_body() {
        lines.push("kind=body".to_string());
    } else if err.is_decode() {
        lines.push("kind=decode".to_string());
    }

    let mut causes = Vec::new();
    let mut current = err.source();
    while let Some(source) = current {
        causes.push(source.to_string());
        current = source.source();
    }
    if !causes.is_empty() {
        lines.push("causes:".to_string());
        lines.extend(causes.into_iter().map(|cause| format!("- {cause}")));
    }

    miette!(lines.join("\n"))
}

#[async_trait]
impl LLM for OpenAIClient {
    async fn run_json(
        &self,
        _context: &Context,
        request: PromptRequest,
    ) -> Result<serde_json::Value> {
        self.call_tool_json(request).await
    }

    async fn run_agent_turn(
        &self,
        _context: &Context,
        request: AgentTurnRequest,
    ) -> Result<AgentTurnResponse> {
        self.call_agent_turn(request).await
    }
}

fn prompt_message_to_openai_message(
    message: crate::reasoning::runtime::PromptMessage,
) -> serde_json::Value {
    json!({
        "role": match message.role {
            PromptRole::System => "system",
            PromptRole::User => "user",
            PromptRole::Assistant => "assistant",
            PromptRole::Tool => "tool",
        },
        "content": message.content,
    })
}

fn prompt_request_to_openai_messages(request: PromptRequest) -> Vec<serde_json::Value> {
    request
        .system_messages
        .into_iter()
        .map(|message| json!({"role": "system", "content": message}))
        .chain(
            request
                .long_term_memory_messages
                .into_iter()
                .map(prompt_message_to_openai_message),
        )
        .chain(
            request
                .history_messages
                .into_iter()
                .map(prompt_message_to_openai_message),
        )
        .chain(std::iter::once(json!({
            "role": "user",
            "content": request.current_user_message,
        })))
        .chain(
            request
                .retry_messages
                .into_iter()
                .map(prompt_message_to_openai_message),
        )
        .collect::<Vec<_>>()
}

fn agent_message_to_openai_message(message: AgentMessage) -> serde_json::Value {
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
        AgentMessage::AssistantToolCalls { content, calls } => json!({
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
        }),
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

fn truncate_for_error(text: &str) -> String {
    const MAX_LEN: usize = 600;
    if text.chars().count() <= MAX_LEN {
        return text.to_string();
    }
    let truncated = text.chars().take(MAX_LEN).collect::<String>();
    format!("{truncated}...")
}

fn truncate_for_json_error(value: &serde_json::Value) -> String {
    truncate_for_error(&value.to_string())
}
