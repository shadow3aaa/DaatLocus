use super::responses_protocol::{
    history_messages_to_responses_parts, messages_to_responses_parts, reasoning_payload,
    sanitize_text_format_name, tool_to_responses_tool,
};
use super::transport::{Authentication, BackendHttp};
use std::time::Duration;

use async_trait::async_trait;
use miette::{Result, miette};
use serde_json::{Value, json};
use tracing::warn;

use super::extract_json_value_from_content;
use super::io::{
    looks_like_context_window_error, read_response_text_with_idle_timeout, truncate_for_error,
};
use crate::context_budget::{ContextBudgetExceededError, RequestBudgetLimits};
use crate::core::{
    ModelProgressSink, ModelProvider, ModelRequestOptions, TokenUsage, TokenUsageInfo,
};
use crate::model_catalog::catalog_model_capacity;
use crate::model_protocol::{AgentTurnRequest, AgentTurnStreamResult, PromptRequest};

#[derive(Clone, Copy, PartialEq)]
struct ResponsesCompatibility {
    supports_vision: bool,
    supports_reasoning_summary: bool,
}

pub struct ResponsesCompatibleClient {
    transport: BackendHttp,
    api_key: String,
    /// Extra headers attached to each request (e.g. the OpenCode org id).
    extra_headers: reqwest::header::HeaderMap,
    base_url: String,
    model: String,
    thinking_budget: Option<String>,
    header_timeout: Option<Duration>,
    stream_idle_timeout: Option<Duration>,
    context_window_tokens: usize,
    effective_context_window_tokens: usize,
    auto_compact_threshold_tokens: usize,
    reserved_output_tokens: usize,
    token_usage: std::sync::Mutex<TokenUsageInfo>,
    initial_compatibility: ResponsesCompatibility,
    compatibility: super::compatibility::CompatibilityCache<ResponsesCompatibility>,
}

impl ResponsesCompatibleClient {
    #[cfg(test)]
    pub(super) fn set_test_client(&mut self, client: reqwest::Client) {
        self.transport.set_test_client(client);
    }
    /// Replace the bearer credential used for subsequent requests.
    pub(crate) fn set_api_key(&mut self, api_key: String) {
        self.api_key = api_key;
    }

    /// Install (or replace) one extra header sent with every request.
    pub(crate) fn set_extra_header(&mut self, name: &'static str, value: String) {
        if let Ok(value) = reqwest::header::HeaderValue::from_str(&value) {
            self.extra_headers.insert(name, value);
        }
    }

    pub(crate) fn new(
        api_key: &str,
        base_url: &str,
        model_config: &crate::config::ModelConfig,
    ) -> Self {
        let base_url = crate::config::normalize_provider_base_url(base_url);
        let header_timeout = model_config.response_header_timeout();
        let stream_idle_timeout = model_config.stream_idle_timeout();
        let transport = BackendHttp::new(&base_url, model_config);
        let context_window_tokens = model_config.context_window_tokens();
        let effective_context_window_tokens = model_config.effective_context_window_tokens();
        let auto_compact_threshold_tokens = model_config.auto_compact_token_limit();
        let reserved_output_tokens = model_config.reserved_output_tokens();
        let supports_vision = model_config.supports_vision.unwrap_or_else(|| {
            catalog_model_capacity(&model_config.model_id).is_some_and(|c| c.supports_vision)
        });
        Self {
            transport,
            api_key: api_key.to_string(),
            extra_headers: reqwest::header::HeaderMap::new(),
            base_url: base_url.clone(),
            model: model_config.model_id.clone(),
            thinking_budget: model_config
                .thinking_budget()
                .map(|budget| budget.as_str().to_string()),
            header_timeout,
            stream_idle_timeout,
            context_window_tokens,
            effective_context_window_tokens,
            auto_compact_threshold_tokens,
            reserved_output_tokens,
            token_usage: std::sync::Mutex::new(TokenUsageInfo {
                total_token_usage: TokenUsage::default(),
                last_token_usage: TokenUsage::default(),
                model_context_window: i64::try_from(context_window_tokens).ok(),
                daily_token_usage: Vec::new(),
            }),
            initial_compatibility: ResponsesCompatibility {
                supports_vision,
                supports_reasoning_summary: true,
            },
            compatibility: super::compatibility::CompatibilityCache::new(
                "responses",
                &base_url,
                model_config,
            ),
        }
    }

    fn url(&self) -> String {
        format!("{}/responses", self.base_url.trim_end_matches('/'))
    }

    const fn request_budget_limits(&self) -> RequestBudgetLimits {
        RequestBudgetLimits {
            context_window_tokens: self.effective_context_window_tokens,
            auto_compact_threshold_tokens: self.auto_compact_threshold_tokens,
            reserved_output_tokens: self.reserved_output_tokens,
        }
    }

    fn record_last_usage(&self, usage: TokenUsage) {
        if let Ok(mut info) = self.token_usage.lock() {
            info.model_context_window = i64::try_from(self.context_window_tokens).ok();
            info.append_last_usage(usage);
        }
    }

    async fn parse_responses_stream(
        &self,
        progress: Option<&ModelProgressSink>,
        response: reqwest::Response,
        emit_progress: bool,
        actor: Option<&str>,
    ) -> Result<AgentTurnStreamResult> {
        let mut diagnostics = super::stream_diagnostics::StreamDiagnostics::new(
            &response,
            "responses",
            &self.model,
            actor,
        );
        let result = super::responses_protocol::parse_stream(
            response,
            progress.filter(|_| emit_progress),
            self.stream_idle_timeout,
            &self.model,
            &self.url(),
            &mut diagnostics,
            |usage| self.record_last_usage(usage),
        )
        .await;
        diagnostics.finish(result)
    }
}

#[async_trait]
impl ModelProvider for ResponsesCompatibleClient {
    async fn complete_json(
        &self,
        request: PromptRequest,
        options: ModelRequestOptions,
    ) -> Result<Value> {
        let budget = &options.budget;
        let request_context = super::io::summarize_prompt_request(&request, Some(budget));
        let backend_request = self.transport.request(
            "/responses",
            Authentication::Bearer(&self.api_key),
            &self.extra_headers,
            &options,
        )?;
        let (instructions, input) =
            history_messages_to_responses_parts(request.all_messages(), false);
        let mut payload = base_payload(
            self,
            &instructions,
            &input,
            &Vec::new(),
            reasoning_payload(self.thinking_budget.as_deref(), false),
        );
        payload["text"] = json!({
            "format": {
                "type": "json_schema",
                "name": sanitize_text_format_name(&request.tool_name),
                "strict": crate::schema_utils::supports_strict_schema(&request.output_schema),
                "schema": request.output_schema.clone(),
            }
        });
        let response = backend_request
            .post(
                &payload,
                &request_context,
                self.header_timeout,
                self.stream_idle_timeout,
            )
            .await?;
        let status = response.status();
        if !status.is_success() {
            let url = self.url();
            let body = read_response_text_with_idle_timeout(
                response,
                self.stream_idle_timeout,
                "responses-compatible body read failed",
                &url,
                &request_context,
            )
            .await?;
            if let Some(error) =
                super::ModelRequestFailure::opencode_response(&self.base_url, status, &body)
            {
                return Err(error);
            }
            return Err(miette!(
                "responses-compatible returned HTTP {}: {}",
                status,
                truncate_for_error(&body)
            ));
        }
        let result = self
            .parse_responses_stream(
                options.progress.as_ref(),
                response,
                false,
                options.conversation_id.as_deref(),
            )
            .await?;
        let content = result.last_assistant_message.as_deref().unwrap_or_default();
        if let Some(value) = extract_json_value_from_content(content) {
            return Ok(value);
        }
        Err(miette!(
            "responses-compatible JSON request did not return a JSON object; content={}",
            truncate_for_error(content)
        ))
    }

    async fn complete_agent_turn(
        &self,
        request: AgentTurnRequest,
        options: ModelRequestOptions,
    ) -> Result<AgentTurnStreamResult> {
        let budget = &options.budget;
        let request_context = super::io::summarize_agent_turn_request(&request, Some(budget));
        let backend_request = self.transport.request(
            "/responses",
            Authentication::Bearer(&self.api_key),
            &self.extra_headers,
            &options,
        )?;
        let mut negotiation = self
            .compatibility
            .begin(
                &self.api_key,
                &self.extra_headers,
                self.initial_compatibility,
            )
            .await;
        loop {
            let payload = build_agent_payload(self, &request, negotiation.state);
            let response = backend_request
                .post(
                    &payload,
                    &request_context,
                    self.header_timeout,
                    self.stream_idle_timeout,
                )
                .await?;
            let status = response.status();
            if status.is_success() {
                negotiation.succeeded();
                return self
                    .parse_responses_stream(
                        options.progress.as_ref(),
                        response,
                        true,
                        options.conversation_id.as_deref(),
                    )
                    .await;
            }
            let url = self.url();
            let body = read_response_text_with_idle_timeout(
                response,
                self.stream_idle_timeout,
                "responses-compatible body read failed",
                &url,
                &request_context,
            )
            .await?;
            if looks_like_context_window_error(&body) {
                return Err(ContextBudgetExceededError::for_request(
                    "agent turn",
                    &self.model,
                    budget,
                    Some(&format!(
                        "provider_status={status}; provider_body={}",
                        truncate_for_error(&body)
                    )),
                )
                .into());
            }
            if let Some(error) =
                super::ModelRequestFailure::opencode_response(&self.base_url, status, &body)
            {
                return Err(error);
            }
            if status != reqwest::StatusCode::BAD_REQUEST {
                return Err(miette!(
                    "responses-compatible returned HTTP {status}: {}",
                    truncate_for_error(&body)
                ));
            }
            if negotiation.refresh_after_failure().await {
                continue;
            }
            if super::io::should_retry_request_without_reasoning_summary(&body)
                && negotiation.state.supports_reasoning_summary
            {
                negotiation.state.supports_reasoning_summary = false;
                warn!(
                    "responses-compatible rejected reasoning.summary; retrying without reasoning summaries\n{}",
                    request_context.join("\n")
                );
                continue;
            }
            if super::io::looks_like_vision_unsupported_error(&body)
                && negotiation.state.supports_vision
            {
                negotiation.state.supports_vision = false;
                warn!(
                    "responses-compatible rejected image input; retrying without images\n{}",
                    request_context.join("\n")
                );
                continue;
            }
            return Err(miette!(
                "responses-compatible returned HTTP {}: {}",
                status,
                truncate_for_error(&body)
            ));
        }
    }

    fn request_budget_limits(&self) -> RequestBudgetLimits {
        self.request_budget_limits()
    }

    fn token_usage_info(&self) -> TokenUsageInfo {
        self.token_usage
            .lock()
            .ok()
            .map(|info| info.clone())
            .unwrap_or_default()
    }

    fn model_name(&self) -> String {
        self.model.clone()
    }
}

// ---------------------------------------------------------------------------
// Payload construction
// ---------------------------------------------------------------------------

fn base_payload(
    client: &ResponsesCompatibleClient,
    instructions: &str,
    input: &[Value],
    tools: &[Value],
    reasoning: Option<Value>,
) -> Value {
    let mut payload = json!({
        "model": client.model,
        "instructions": instructions,
        "input": input,
        "tools": tools,
        "tool_choice": "auto",
        "parallel_tool_calls": true,
        "store": false,
        "stream": true,
        "include": ["reasoning.encrypted_content"],
    });
    if let Some(reasoning) = reasoning {
        payload["reasoning"] = reasoning;
    }
    payload
}

fn build_agent_payload(
    client: &ResponsesCompatibleClient,
    request: &AgentTurnRequest,
    state: ResponsesCompatibility,
) -> Value {
    let (instructions, input) =
        messages_to_responses_parts(&request.messages, !state.supports_vision);
    let tools = request
        .tools
        .iter()
        .cloned()
        .map(tool_to_responses_tool)
        .collect::<Vec<_>>();
    base_payload(
        client,
        &instructions,
        &input,
        &tools,
        reasoning_payload(
            client.thinking_budget.as_deref(),
            state.supports_reasoning_summary,
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ModelConfig;
    use crate::model_protocol::AgentMessage;

    #[test]
    fn responses_compat_payload_omits_max_output_tokens_parameter() {
        let client = ResponsesCompatibleClient::new(
            "test-key",
            "https://example.test/v1",
            &ModelConfig {
                model_id: "gpt-5.5".to_string(),
                provider: "responses-compatible".to_string(),
                max_completion_tokens: 128_000,
                ..ModelConfig::default()
            },
        );

        let payload = base_payload(&client, "instructions", &[], &[], None);

        assert!(payload.get("max_output_tokens").is_none(), "{payload:#}");
    }

    #[test]
    fn responses_compat_agent_payload_requests_reasoning_summary_and_include() {
        let client = ResponsesCompatibleClient::new(
            "test-key",
            "https://example.test/v1",
            &ModelConfig {
                model_id: "gpt-5.5".to_string(),
                provider: "responses-compatible".to_string(),
                ..ModelConfig::default()
            },
        );

        let payload = build_agent_payload(
            &client,
            &AgentTurnRequest {
                messages: vec![AgentMessage::system("base"), AgentMessage::user("work")],
                tools: Vec::new(),
            },
            client.initial_compatibility,
        );

        assert_eq!(payload["reasoning"]["summary"], "auto");
        assert_eq!(payload["include"], json!(["reasoning.encrypted_content"]));
    }

    #[test]
    fn responses_compat_prompt_payload_omits_reasoning_summary() {
        let client = ResponsesCompatibleClient::new(
            "test-key",
            "https://example.test/v1",
            &ModelConfig {
                model_id: "gpt-5.5".to_string(),
                provider: "responses-compatible".to_string(),
                ..ModelConfig::default()
            },
        );

        let payload = base_payload(
            &client,
            "instructions",
            &[],
            &[],
            reasoning_payload(client.thinking_budget.as_deref(), false),
        );

        assert!(payload.get("reasoning").is_none(), "{payload:#}");
        assert_eq!(payload["include"], json!(["reasoning.encrypted_content"]));
    }
}
