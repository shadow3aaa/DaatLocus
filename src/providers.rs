//! 本模块实现实际的llm api调用

use async_trait::async_trait;
use miette::{Result, miette};
use serde_json::json;

use crate::{
    config::Config,
    context::Context,
    core::LLM,
    reasoning::runtime::{PromptRequest, PromptRole},
};
pub struct OpenAIClient {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    model: String,
}

impl OpenAIClient {
    pub fn new(config: &Config) -> Self {
        let client = reqwest::Client::new();
        let api_key = config.main_model.api_key.clone();
        let base_url = config.main_model.base_url.clone();
        let model = config.main_model.model_name.clone();
        Self {
            client,
            api_key,
            base_url,
            model,
        }
    }

    fn url(&self) -> String {
        format!(
            "{}/v1/chat/completions",
            self.base_url.trim_end_matches('/')
        )
    }

    async fn call_tool_json(
        &self,
        request: PromptRequest,
        temperature: f64,
    ) -> Result<serde_json::Value> {
        let url = self.url();
        let tool_name = request.tool_name.clone();
        let tool_description = request.tool_description.clone();
        let output_schema = request.output_schema.clone();
        let messages = request
            .messages
            .into_iter()
            .map(|message| {
                json!({
                    "role": match message.role {
                        PromptRole::System => "system",
                        PromptRole::User => "user",
                    },
                    "content": message.content,
                })
            })
            .collect::<Vec<_>>();
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
                "function": { "name": request.tool_name }
            },
            "temperature": temperature
        });
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&payload)
            .send()
            .await
            .map_err(|err| miette!("llm request failed: {err}"))?;
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
            if !content.trim().is_empty() {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(content) {
                    return Ok(value);
                }
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
}

#[async_trait]
impl LLM for OpenAIClient {
    async fn run_json(
        &self,
        context: &Context,
        request: PromptRequest,
    ) -> Result<serde_json::Value> {
        let temperature = context.config.main_model.temperature;
        self.call_tool_json(request, temperature).await
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
