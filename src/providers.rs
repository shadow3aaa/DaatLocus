//! 本模块实现实际的llm api调用

use async_trait::async_trait;
use serde_json::json;

use crate::{
    SYSTEM_PROMPT, TERMINAL_PROMPT,
    config::Config,
    context::Context,
    core::{LLM, Output},
    snapshot::Snapshot,
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
}

#[async_trait]
impl LLM for OpenAIClient {
    async fn think(&self, context: &Context, input: &Snapshot, instruction: &str) -> Output {
        let url = self.url();
        let temperature = context.config.main_model.temperature;
        let output_schema = serde_json::to_value(schemars::schema_for!(Output)).unwrap();
        let system_prompt = format!("{} \n {}", SYSTEM_PROMPT, TERMINAL_PROMPT);
        let payload = json!({
            "model": self.model,
            "messages": [
                {
                    "role": "system",
                    "content": system_prompt,
                },
                {
                    "role": "user",
                    "content": instruction.to_string()
                },
                {
                    "role": "user",
                    "content": input.to_string()
                }
            ],
            "tools": [
                {
                    "type": "function",
                    "function": {
                        "strict": true,
                        "name": "submit_action",
                        "description": "本次的思考内容、分析结论与做了什么动作",
                        "parameters": output_schema
                    }
                }
            ],
            "tool_choice": {
                "type": "function",
                "function": { "name": "submit_action" }
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
        let tool_calls = match response_json["choices"][0]["message"]["tool_calls"].as_array() {
            Some(s) => s,
            None => {
                let instruction_with_error = format!(
                    "{}\n注意：你的上一次输出非法，这次请注意。错误原因：没有正确调用工具函数",
                    instruction
                );
                return self.think(context, input, &instruction_with_error).await;
            }
        };
        let arguments_str = tool_calls[0]["function"]["arguments"].as_str().unwrap();
        match serde_json::from_str(arguments_str) {
            Ok(o) => o,
            Err(e) => {
                let instruction_with_error = format!(
                    "{}\n注意：你的上一次输出非法，这次请注意。错误原因：{}",
                    instruction, e
                );
                self.think(context, input, &instruction_with_error).await
            }
        }
    }
}
