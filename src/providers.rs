//! 本模块实现实际的llm api调用

use async_trait::async_trait;
use serde_json::json;

use crate::{
    SYSTEM_PROMPT, TELEGRAM_PROMPT, TERMINAL_PROMPT,
    config::Config,
    context::Context,
    core::{LLM, Output},
    device::{AttentionLevel, DeviceId},
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
        let device_context_prompt = build_device_context_prompt(context);
        let payload = json!({
            "model": self.model,
            "messages": [
                {
                    "role": "system",
                    "content": SYSTEM_PROMPT,
                },
                {
                    "role": "user",
                    "content": instruction.to_string()
                },
                {
                    "role": "user",
                    "content": device_context_prompt
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
                        "description": "本次从环境中观察到的关键信息、分析结论与动作决定。observation 必须包含具体得到的事实，而不只是动作本身。",
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

fn build_device_context_prompt(context: &Context) -> String {
    let mut sections = vec![String::from(
        "设备动作约束：你只能对当前前景设备执行 `DeviceAction`。如果想查看或操作后台设备，必须先输出 `FocusDevice` 将它切到前景。",
    )];

    match context.devices.focused() {
        Some(DeviceId::Terminal) => sections.push(TERMINAL_PROMPT.to_string()),
        Some(DeviceId::Telegram) => sections.push(TELEGRAM_PROMPT.to_string()),
        None => sections.push(String::from(
            "当前没有任何前景设备。如果你需要读取设备内容或执行设备动作，请先输出 `FocusDevice`。",
        )),
    }

    let attention_hints = context
        .devices
        .peripheral_renders()
        .into_iter()
        .filter(|(_, render)| !render.is_focused)
        .filter_map(|(device_id, render)| {
            if matches!(
                render.attention,
                AttentionLevel::Notice | AttentionLevel::Urgent
            ) {
                Some(background_attention_hint(device_id, render.summary))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    if !attention_hints.is_empty() {
        sections.push(format!("后台设备提醒：\n{}", attention_hints.join("\n")));
    }

    sections.join("\n\n")
}

fn background_attention_hint(device_id: DeviceId, summary: String) -> String {
    match device_id {
        DeviceId::Terminal => format!(
            "- {} 如果你决定查看终端，请先输出 `FocusDevice` 将 `Terminal` 切到前景。",
            summary
        ),
        DeviceId::Telegram => format!(
            "- {} 如果你决定处理它，请先输出 `FocusDevice` 将 `Telegram` 切到前景；聚焦后再使用 `TelegramSelectChat` 或 `TelegramSendMessage`。",
            summary
        ),
    }
}
