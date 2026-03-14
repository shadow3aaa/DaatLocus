//! 本模块实现实际的llm api调用

use async_trait::async_trait;
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

    async fn call_tool_json(&self, request: PromptRequest, temperature: f64) -> serde_json::Value {
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
            .unwrap();
        let response_json: serde_json::Value = response.json().await.unwrap();
        let tool_calls = response_json["choices"][0]["message"]["tool_calls"]
            .as_array()
            .unwrap();
        let arguments_str = tool_calls[0]["function"]["arguments"].as_str().unwrap();
        serde_json::from_str(arguments_str).unwrap()
    }
}

#[async_trait]
impl LLM for OpenAIClient {
    async fn run_json(&self, context: &Context, request: PromptRequest) -> serde_json::Value {
        let temperature = context.config.main_model.temperature;
        self.call_tool_json(request, temperature).await
    }
}
