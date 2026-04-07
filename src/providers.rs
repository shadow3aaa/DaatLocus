//! 本模块实现实际的llm api调用

use std::{
    error::Error as _,
    sync::Mutex,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use futures_util::StreamExt;
use miette::{Result, miette};
use serde_json::json;
use tracing::warn;

use crate::{
    config::{Config, MainModelConfig},
    context::Context,
    context_budget::{
        ContextBudgetExceededError, RequestBudgetBreakdown, RequestBudgetLimits,
        estimate_agent_turn_request, estimate_prompt_request,
    },
    core::{LLM, TokenUsage, TokenUsageInfo},
    reasoning::runtime::{
        AgentMessage, AgentToolCall, AgentToolInputSpec, AgentTurnItem, AgentTurnRequest,
        AgentTurnStreamResult, PromptRequest, PromptRole, assistant_tool_call_protocol_char_count,
    },
};

pub struct OpenAIClient {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    model: String,
    temperature: f64,
    context_window_tokens: usize,
    effective_context_window_tokens: usize,
    auto_compact_threshold_tokens: usize,
    max_completion_tokens: usize,
    token_usage: Mutex<TokenUsageInfo>,
}

#[derive(Default, Clone)]
struct StreamingToolCallBuilder {
    id: String,
    name: String,
    arguments: String,
}

impl StreamingToolCallBuilder {
    fn apply_delta(&mut self, delta: &serde_json::Value) {
        if let Some(id) = delta["id"].as_str() {
            self.id.push_str(id);
        }
        if let Some(name) = delta["function"]["name"].as_str() {
            self.name.push_str(name);
        }
        if let Some(arguments) = delta["function"]["arguments"].as_str() {
            self.arguments.push_str(arguments);
        }
    }

    fn try_build(&self) -> Option<AgentToolCall> {
        if self.id.is_empty() || self.name.is_empty() {
            return None;
        }
        let arguments = serde_json::from_str(&self.arguments).ok()?;
        Some(AgentToolCall {
            id: self.id.clone(),
            name: self.name.clone(),
            arguments,
        })
    }
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
        let context_window_tokens = model_config.context_window_tokens();
        let effective_context_window_tokens = model_config.effective_context_window_tokens();
        let auto_compact_threshold_tokens = model_config.auto_compact_token_limit();
        let max_completion_tokens = model_config.max_completion_tokens();
        Self {
            client,
            api_key,
            base_url,
            model,
            temperature,
            context_window_tokens,
            effective_context_window_tokens,
            auto_compact_threshold_tokens,
            max_completion_tokens,
            token_usage: Mutex::new(TokenUsageInfo {
                total_token_usage: TokenUsage::default(),
                last_token_usage: TokenUsage::default(),
                model_context_window: Some(context_window_tokens as i64),
            }),
        }
    }

    fn url(&self) -> String {
        format!(
            "{}/v1/chat/completions",
            self.base_url.trim_end_matches('/')
        )
    }

    fn request_budget_limits(&self) -> RequestBudgetLimits {
        RequestBudgetLimits {
            context_window_tokens: self.effective_context_window_tokens,
            auto_compact_threshold_tokens: self.auto_compact_threshold_tokens,
            reserved_output_tokens: self.max_completion_tokens,
        }
    }

    async fn post_json_with_rate_limit_retry(
        &self,
        url: &str,
        payload: &serde_json::Value,
        request_context: &[String],
    ) -> Result<reqwest::Response> {
        const MAX_429_RETRIES: usize = 4;
        const MAX_5XX_RETRIES: usize = 3;

        let mut rate_limit_attempt = 0usize;
        let mut transient_attempt = 0usize;
        loop {
            let response = self
                .client
                .post(url)
                .bearer_auth(&self.api_key)
                .json(payload)
                .send()
                .await
                .map_err(|err| {
                    format_request_error("llm request failed", url, request_context, &err)
                })?;

            if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let retry_after = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(parse_retry_after_seconds);
                let body = response
                    .text()
                    .await
                    .map_err(|err| miette!("llm response body read failed: {err}"))?;

                if rate_limit_attempt >= MAX_429_RETRIES {
                    return Err(miette!(
                        "llm api returned HTTP 429 after {} retries: {}",
                        MAX_429_RETRIES,
                        truncate_for_error(&body)
                    ));
                }

                let delay = retry_after
                    .map(Duration::from_secs)
                    .unwrap_or_else(|| default_rate_limit_backoff(rate_limit_attempt));
                let delay_ms = delay.as_millis();
                warn!(
                    "llm api returned HTTP 429; retrying request in {} ms (attempt {}/{})\n{}",
                    delay_ms,
                    rate_limit_attempt + 1,
                    MAX_429_RETRIES,
                    request_context.join("\n")
                );
                tokio::time::sleep(delay).await;
                rate_limit_attempt += 1;
                continue;
            }

            if response.status().is_server_error() {
                let status = response.status();
                let body = response
                    .text()
                    .await
                    .map_err(|err| miette!("llm response body read failed: {err}"))?;

                if transient_attempt >= MAX_5XX_RETRIES {
                    return Err(miette!(
                        "llm api returned HTTP {} after {} retries: {}",
                        status,
                        MAX_5XX_RETRIES,
                        truncate_for_error(&body)
                    ));
                }

                let delay = Duration::from_millis(400 * (1u64 << transient_attempt));
                let delay_ms = delay.as_millis();
                warn!(
                    "llm api returned HTTP {}; retrying request in {} ms (attempt {}/{})\n{}",
                    status,
                    delay_ms,
                    transient_attempt + 1,
                    MAX_5XX_RETRIES,
                    request_context.join("\n")
                );
                tokio::time::sleep(delay).await;
                transient_attempt += 1;
                continue;
            }

            {
                return Ok(response);
            }
        }
    }

    async fn call_tool_json(&self, request: PromptRequest) -> Result<serde_json::Value> {
        let url = self.url();
        let tool_name = request.tool_name.clone();
        let tool_description = request.tool_description.clone();
        let output_schema = request.output_schema.clone();
        let budget = estimate_prompt_request(&request, self.request_budget_limits());
        if !budget.within_context_window() {
            return Err(ContextBudgetExceededError::for_request(
                "prompt request",
                &self.model,
                &budget,
                None,
            )
            .into());
        }
        let request_context = summarize_prompt_request(&request, Some(&budget));
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
            "temperature": self.temperature,
            "max_tokens": self.max_completion_tokens,
        });
        let response = self
            .post_json_with_rate_limit_retry(&url, &payload, &request_context)
            .await?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|err| miette!("llm response body read failed: {err}"))?;

        if !status.is_success() {
            if looks_like_context_window_error(&body) {
                return Err(ContextBudgetExceededError::for_request(
                    "prompt request",
                    &self.model,
                    &budget,
                    Some(&format!(
                        "provider_status={status}; provider_body={}",
                        truncate_for_error(&body)
                    )),
                )
                .into());
            }
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
        self.record_usage_from_response(&response_json);
        let content = response_json["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("");
        let Some(tool_calls) = response_json["choices"][0]["message"]["tool_calls"].as_array()
        else {
            if let Some(value) = extract_json_value_from_content(content) {
                return Ok(value);
            }
            return Err(miette!(
                "llm response did not include tool_calls; content={}; response={}",
                truncate_for_error(content),
                truncate_for_json_error(&response_json)
            ));
        };
        let first_tool_call = if let Some(first_tool_call) = tool_calls.first() {
            first_tool_call
        } else if let Some(value) = extract_json_value_from_content(content) {
            return Ok(value);
        } else {
            return Err(miette!(
                "llm response included empty tool_calls; content={}; response={}",
                truncate_for_error(content),
                truncate_for_json_error(&response_json)
            ));
        };
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

    fn build_agent_turn_payload(
        &self,
        request: AgentTurnRequest,
        stream: bool,
    ) -> serde_json::Value {
        let messages = request
            .messages
            .into_iter()
            .map(agent_message_to_openai_message)
            .collect::<Vec<_>>();
        let tools = request
            .tools
            .into_iter()
            .map(|tool| {
                let (description, parameters, strict) = match tool.input_spec {
                    AgentToolInputSpec::JsonSchema { schema } => (tool.description, schema, true),
                    AgentToolInputSpec::FreeformGrammar {
                        syntax,
                        definition,
                        fallback_schema,
                    } => (
                        format!(
                            "{}\n\n这是一个 FREEFORM grammar tool。当前 provider 回退为单字符串输入：请把完整工具输入放进 `input` 字段。\nsyntax={syntax}\ndefinition=\n{definition}",
                            tool.description
                        ),
                        fallback_schema,
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
        json!({
            "model": self.model,
            "messages": messages,
            "tools": tools,
            "temperature": self.temperature,
            "max_tokens": self.max_completion_tokens,
            "stream": stream,
            "stream_options": if stream {
                json!({ "include_usage": true })
            } else {
                serde_json::Value::Null
            },
        })
    }

    async fn call_agent_turn(
        &self,
        context: &Context,
        request: AgentTurnRequest,
    ) -> Result<AgentTurnStreamResult> {
        let url = self.url();
        let budget = estimate_agent_turn_request(
            &request.messages,
            &request.tools,
            self.request_budget_limits(),
        );
        if !budget.within_context_window() {
            return Err(ContextBudgetExceededError::for_request(
                "agent turn",
                &self.model,
                &budget,
                None,
            )
            .into());
        }
        let request_context = summarize_agent_turn_request(&request, Some(&budget));
        let payload = self.build_agent_turn_payload(request, true);
        let response = self
            .post_json_with_rate_limit_retry(&url, &payload, &request_context)
            .await?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();

        if !status.is_success() {
            let body = response
                .text()
                .await
                .map_err(|err| miette!("llm response body read failed: {err}"))?;
            if looks_like_context_window_error(&body) {
                return Err(ContextBudgetExceededError::for_request(
                    "agent turn",
                    &self.model,
                    &budget,
                    Some(&format!(
                        "provider_status={status}; provider_body={}",
                        truncate_for_error(&body)
                    )),
                )
                .into());
            }
            return Err(miette!(
                "llm api returned HTTP {}: {}",
                status,
                truncate_for_error(&body)
            ));
        }

        if !content_type.contains("text/event-stream") {
            let body = response
                .text()
                .await
                .map_err(|err| miette!("llm response body read failed: {err}"))?;
            let response_json: serde_json::Value = serde_json::from_str(&body).map_err(|err| {
                miette!(
                    "llm response is not valid JSON: {err}; body={}",
                    truncate_for_error(&body)
                )
            })?;
            self.record_usage_from_response(&response_json);
            return parse_agent_turn_stream_result_from_json(&response_json);
        }

        let mut buffer = String::new();
        let mut content = String::new();
        let mut tool_calls: Vec<StreamingToolCallBuilder> = Vec::new();
        let mut last_usage = None;
        let mut last_progress_emit_at = Instant::now();
        let mut last_progress_char_len = 0usize;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|err| miette!("llm streaming chunk read failed: {err}"))?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            normalize_sse_buffer(&mut buffer);
            while let Some(event) = take_next_sse_event(&mut buffer) {
                let data = event
                    .lines()
                    .filter_map(|line| line.strip_prefix("data:"))
                    .map(str::trim_start)
                    .collect::<Vec<_>>()
                    .join("\n");
                if data.is_empty() {
                    continue;
                }
                if data == "[DONE]" {
                    break;
                }
                let response_json: serde_json::Value =
                    serde_json::from_str(&data).map_err(|err| {
                        miette!(
                            "llm streaming chunk is not valid JSON: {err}; data={}",
                            truncate_for_error(&data)
                        )
                    })?;
                if let Some(usage) = parse_usage_from_response_json(&response_json) {
                    last_usage = Some(usage);
                }
                let choice = &response_json["choices"][0];
                let delta = &choice["delta"];
                if let Some(delta_content) = delta["content"].as_str() {
                    content.push_str(delta_content);
                    let should_emit = content
                        .chars()
                        .count()
                        .saturating_sub(last_progress_char_len)
                        >= 64
                        || last_progress_emit_at.elapsed() >= Duration::from_millis(800);
                    if should_emit && !content.trim().is_empty() {
                        context.emit_live_assistant_progress(&content);
                        last_progress_emit_at = Instant::now();
                        last_progress_char_len = content.chars().count();
                    }
                }
                if let Some(delta_tool_calls) = delta["tool_calls"].as_array() {
                    for tool_call in delta_tool_calls {
                        let Some(index) = tool_call["index"].as_u64().map(|index| index as usize)
                        else {
                            continue;
                        };
                        while tool_calls.len() <= index {
                            tool_calls.push(StreamingToolCallBuilder::default());
                        }
                        tool_calls[index].apply_delta(tool_call);
                    }
                }
            }
        }
        if !content.trim().is_empty() && content.chars().count() != last_progress_char_len {
            context.emit_live_assistant_progress(&content);
        }
        if let Some(usage) = last_usage {
            self.record_last_usage(usage);
        }

        if !tool_calls.is_empty() {
            let mut calls = Vec::with_capacity(tool_calls.len());
            for (index, builder) in tool_calls.into_iter().enumerate() {
                let call = builder.try_build().ok_or_else(|| {
                    miette!(
                        "llm streaming response ended with incomplete tool call at index {index}"
                    )
                })?;
                calls.push(call);
            }
            let assistant_message = if content.trim().is_empty() {
                None
            } else {
                Some(content)
            };
            let mut items =
                Vec::with_capacity(calls.len() + usize::from(assistant_message.is_some()));
            if let Some(content) = assistant_message.clone() {
                items.push(AgentTurnItem::AssistantMessage { content });
            }
            items.extend(
                calls
                    .into_iter()
                    .map(|call| AgentTurnItem::ToolCall { call }),
            );
            return Ok(AgentTurnStreamResult {
                items,
                raw_stream_follow_up: true,
                last_assistant_message: assistant_message,
            });
        }

        let last_assistant_message = if content.trim().is_empty() {
            None
        } else {
            Some(content)
        };
        Ok(AgentTurnStreamResult {
            items: last_assistant_message
                .clone()
                .into_iter()
                .map(|content| AgentTurnItem::AssistantMessage { content })
                .collect(),
            raw_stream_follow_up: false,
            last_assistant_message,
        })
    }

    fn record_usage_from_response(&self, response_json: &serde_json::Value) {
        if let Some(usage) = parse_usage_from_response_json(response_json) {
            self.record_last_usage(usage);
        }
    }

    fn record_last_usage(&self, usage: TokenUsage) {
        if let Ok(mut info) = self.token_usage.lock() {
            info.model_context_window = Some(self.context_window_tokens as i64);
            info.append_last_usage(usage);
        }
    }
}

fn extract_json_value_from_content(content: &str) -> Option<serde_json::Value> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return Some(value);
    }
    let fenced = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```JSON"))
        .or_else(|| trimmed.strip_prefix("```"));
    if let Some(fenced) = fenced {
        let fenced = fenced.trim();
        let fenced = fenced.strip_suffix("```").unwrap_or(fenced).trim();
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(fenced) {
            return Some(value);
        }
    }
    None
}

fn summarize_agent_turn_request(
    request: &AgentTurnRequest,
    budget: Option<&RequestBudgetBreakdown>,
) -> Vec<String> {
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
    let mut lines = vec![
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
    ];
    if let Some(budget) = budget {
        lines.extend(budget.summary_lines());
    }
    lines
}

fn summarize_prompt_request(
    request: &PromptRequest,
    budget: Option<&RequestBudgetBreakdown>,
) -> Vec<String> {
    let mut lines = vec![
        format!("message_count={}", request.all_messages().len()),
        format!("tool_name={}", request.tool_name),
    ];
    if let Some(budget) = budget {
        lines.extend(budget.summary_lines());
    }
    lines
}

fn agent_message_char_count(message: &AgentMessage) -> usize {
    match message {
        AgentMessage::System { content }
        | AgentMessage::User { content }
        | AgentMessage::Assistant { content } => content.chars().count(),
        AgentMessage::AssistantToolCallProtocol { content, calls } => {
            assistant_tool_call_protocol_char_count(content.as_deref(), calls)
        }
        AgentMessage::Tool {
            tool_call_id,
            name,
            content,
        } => tool_call_id.chars().count() + name.chars().count() + content.chars().count(),
    }
}

fn parse_agent_turn_stream_result_from_json(
    response_json: &serde_json::Value,
) -> Result<AgentTurnStreamResult> {
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
                    truncate_for_json_error(response_json)
                )
            })?;
            let name = tool_call["function"]["name"].as_str().ok_or_else(|| {
                miette!(
                    "llm response missing tool function name; response={}",
                    truncate_for_json_error(response_json)
                )
            })?;
            let arguments_str = tool_call["function"]["arguments"].as_str().ok_or_else(|| {
                miette!(
                    "llm response missing tool function arguments; response={}",
                    truncate_for_json_error(response_json)
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
        let assistant_message = if content.trim().is_empty() {
            None
        } else {
            Some(content)
        };
        let mut items = Vec::with_capacity(calls.len() + usize::from(assistant_message.is_some()));
        if let Some(content) = assistant_message.clone() {
            items.push(AgentTurnItem::AssistantMessage { content });
        }
        items.extend(
            calls
                .into_iter()
                .map(|call| AgentTurnItem::ToolCall { call }),
        );
        return Ok(AgentTurnStreamResult {
            items,
            raw_stream_follow_up: true,
            last_assistant_message: assistant_message,
        });
    }

    let last_assistant_message = if content.trim().is_empty() {
        None
    } else {
        Some(content)
    };
    Ok(AgentTurnStreamResult {
        items: last_assistant_message
            .clone()
            .into_iter()
            .map(|content| AgentTurnItem::AssistantMessage { content })
            .collect(),
        raw_stream_follow_up: false,
        last_assistant_message,
    })
}

fn parse_usage_from_response_json(response_json: &serde_json::Value) -> Option<TokenUsage> {
    let usage = response_json.get("usage")?;
    let input_tokens = usage
        .get("prompt_tokens")
        .and_then(|value| value.as_i64())
        .unwrap_or_default();
    let output_tokens = usage
        .get("completion_tokens")
        .and_then(|value| value.as_i64())
        .unwrap_or_default();
    let total_tokens = usage
        .get("total_tokens")
        .and_then(|value| value.as_i64())
        .unwrap_or_else(|| input_tokens + output_tokens);
    let cached_input_tokens = usage
        .get("prompt_tokens_details")
        .and_then(|value| value.get("cached_tokens"))
        .and_then(|value| value.as_i64())
        .unwrap_or_default();
    let reasoning_output_tokens = usage
        .get("completion_tokens_details")
        .and_then(|value| value.get("reasoning_tokens"))
        .and_then(|value| value.as_i64())
        .unwrap_or_default();
    let usage = TokenUsage {
        input_tokens,
        cached_input_tokens,
        output_tokens,
        reasoning_output_tokens,
        total_tokens,
    };
    if usage.is_zero() { None } else { Some(usage) }
}

fn normalize_sse_buffer(buffer: &mut String) {
    if buffer.contains('\r') {
        *buffer = buffer.replace("\r\n", "\n").replace('\r', "\n");
    }
}

fn take_next_sse_event(buffer: &mut String) -> Option<String> {
    let delimiter_index = buffer.find("\n\n")?;
    let event = buffer[..delimiter_index].to_string();
    buffer.drain(..delimiter_index + 2);
    Some(event)
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
        context: &Context,
        request: AgentTurnRequest,
    ) -> Result<AgentTurnStreamResult> {
        self.call_agent_turn(context, request).await
    }

    fn token_usage_info(&self) -> Option<TokenUsageInfo> {
        self.token_usage.lock().ok().map(|info| info.clone())
    }

    fn model_name(&self) -> Option<String> {
        Some(self.model.clone())
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
        AgentMessage::AssistantToolCallProtocol { content, calls } => json!({
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

fn looks_like_context_window_error(body: &str) -> bool {
    let normalized = body.to_ascii_lowercase();
    normalized.contains("context length")
        || normalized.contains("context window")
        || normalized.contains("maximum context length")
        || normalized.contains("too many tokens")
        || normalized.contains("max context")
}

fn truncate_for_json_error(value: &serde_json::Value) -> String {
    truncate_for_error(&value.to_string())
}

fn parse_retry_after_seconds(value: &str) -> Option<u64> {
    value.trim().parse::<u64>().ok()
}

fn default_rate_limit_backoff(attempt: usize) -> Duration {
    let seconds = match attempt {
        0 => 2,
        1 => 4,
        2 => 8,
        _ => 12,
    };
    Duration::from_secs(seconds)
}
