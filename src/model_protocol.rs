//! Provider-facing model messages, tool schemas, and response protocol.
//! Execution scheduling, history storage, and client rendering are outside this module.

use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeStruct};
use serde_json::Value;
#[derive(Clone, Serialize, Deserialize)]
pub struct PromptRequest {
    pub tool_name: String,
    pub tool_description: String,
    pub output_schema: Value,
    pub system_messages: Vec<String>,
    pub history_messages: Vec<HistoryMessage>,
    pub current_user_message: String,
    #[serde(default)]
    pub retry_messages: Vec<HistoryMessage>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AgentToolSpec {
    pub name: String,
    pub description: String,
    pub input_spec: AgentToolInputSpec,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentToolInputSpec {
    JsonSchema {
        schema: Value,
    },
    FreeformGrammar {
        syntax: String,
        definition: String,
        fallback_schema: Value,
    },
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Debug)]
pub struct AgentToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

impl AgentToolCall {
    pub fn with_name(&self, name: impl Into<String>) -> Self {
        Self {
            id: self.id.clone(),
            name: name.into(),
            arguments: self.arguments.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentContent {
    text: String,
    parts: Vec<AgentContentPart>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentContentPart {
    Text {
        text: String,
    },
    Image {
        path: String,
        media_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    /// Durable image identity. Device-local paths are resolved only at the provider boundary.
    ResourceImage {
        resource: crate::resources::Resource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    /// Transient provider input. Never persisted, streamed, or sent as an App result.
    ImageData {
        media_type: String,
        data_base64: String,
        description: Option<String>,
    },
}

impl AgentContentPart {
    pub fn image_label(&self) -> Option<&str> {
        match self {
            Self::Image {
                path, description, ..
            } => Some(description.as_deref().unwrap_or(path)),
            Self::ResourceImage {
                resource,
                description,
            } => Some(description.as_deref().unwrap_or(&resource.name)),
            Self::Text { .. } => None,
            Self::ImageData { description, .. } => Some(description.as_deref().unwrap_or("image")),
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AgentContentWire {
    Text(String),
    Rich {
        #[serde(default)]
        text: String,
        #[serde(default)]
        parts: Vec<AgentContentPart>,
    },
}

impl AgentContent {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            parts: Vec::new(),
        }
    }

    pub fn multimodal(text: impl Into<String>, parts: Vec<AgentContentPart>) -> Self {
        Self {
            text: text.into(),
            parts,
        }
    }

    pub fn as_text(&self) -> &str {
        &self.text
    }

    pub fn parts(&self) -> &[AgentContentPart] {
        &self.parts
    }

    pub const fn is_plain_text(&self) -> bool {
        self.parts.is_empty()
    }

    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text = text.into();
        self
    }
}

impl From<String> for AgentContent {
    fn from(value: String) -> Self {
        Self::text(value)
    }
}

impl From<&str> for AgentContent {
    fn from(value: &str) -> Self {
        Self::text(value)
    }
}

impl Serialize for AgentContent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if self.parts.is_empty() {
            return serializer.serialize_str(&self.text);
        }
        let mut state = serializer.serialize_struct("AgentContent", 2)?;
        state.serialize_field("text", &self.text)?;
        state.serialize_field("parts", &self.parts)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for AgentContent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match AgentContentWire::deserialize(deserializer)? {
            AgentContentWire::Text(text) => Ok(Self::text(text)),
            AgentContentWire::Rich { text, parts } => Ok(Self::multimodal(text, parts)),
        }
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind")]
pub enum AgentMessage {
    System {
        content: String,
    },
    User {
        content: AgentContent,
    },
    Assistant {
        content: String,
        /// Original Responses output items, including message phase and encrypted
        /// reasoning. Other providers use the visible content instead.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        responses_output: Option<Vec<Value>>,
    },
    AssistantToolCallProtocol {
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_content: Option<String>,
        /// Opaque provider-side signature required to replay `reasoning_content`
        /// in later turns, such as the Anthropic extended-thinking signature.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_signature: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        responses_output: Option<Vec<Value>>,
        calls: Vec<AgentToolCall>,
    },
    Tool {
        tool_call_id: String,
        name: String,
        content: String,
    },
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AgentTurnRequest {
    pub messages: Vec<AgentMessage>,
    pub tools: Vec<AgentToolSpec>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum AgentTurnItem {
    AssistantMessage { content: String },
    ToolCall { call: AgentToolCall },
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AgentTurnStreamResult {
    pub items: Vec<AgentTurnItem>,
    #[serde(alias = "needs_follow_up")]
    pub raw_stream_follow_up: bool,
    pub last_assistant_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reasoning_signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub responses_output: Option<Vec<Value>>,
}

impl AgentTurnStreamResult {
    pub fn protocol(self) -> AgentTurnResponse {
        let mut assistant_messages = Vec::new();
        let mut tool_calls = Vec::new();
        for item in self.items {
            match item {
                AgentTurnItem::AssistantMessage { content } if !content.trim().is_empty() => {
                    assistant_messages.push(content);
                }
                AgentTurnItem::ToolCall { call } => tool_calls.push(call),
                AgentTurnItem::AssistantMessage { .. } => {}
            }
        }
        let final_assistant_message = self
            .last_assistant_message
            .filter(|content| !content.trim().is_empty())
            .or_else(|| assistant_messages.last().cloned());
        let assistant_text =
            (!assistant_messages.is_empty()).then(|| assistant_messages.join("\n\n"));
        AgentTurnResponse {
            tool_calls,
            assistant_text,
            final_assistant_message,
            reasoning_content: self.last_reasoning_content,
            reasoning_signature: self.last_reasoning_signature,
            responses_output: self.responses_output,
            raw_stream_follow_up: self.raw_stream_follow_up,
        }
    }
}

pub struct AgentTurnResponse {
    pub tool_calls: Vec<AgentToolCall>,
    pub assistant_text: Option<String>,
    pub final_assistant_message: Option<String>,
    pub reasoning_content: Option<String>,
    pub reasoning_signature: Option<String>,
    pub responses_output: Option<Vec<Value>>,
    pub raw_stream_follow_up: bool,
}

impl AgentTurnResponse {
    /// Persistent history stores each executed call separately. Keep reasoning
    /// with the first call and retain only the matching wire call on each row.
    pub fn responses_output_for_tool_call(
        &self,
        call_id: &str,
        include_reasoning: bool,
    ) -> Option<Vec<Value>> {
        self.responses_output.as_ref().map(|output| {
            output
                .iter()
                .filter(|item| match item["type"].as_str() {
                    Some("reasoning") => include_reasoning,
                    Some("function_call" | "custom_tool_call") => {
                        item["call_id"].as_str() == Some(call_id)
                    }
                    _ => false,
                })
                .cloned()
                .collect()
        })
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct HistoryMessage {
    pub message: AgentMessage,
}

impl HistoryMessage {
    pub fn with_responses_output(mut self, output: Option<Vec<Value>>) -> Self {
        self.message = self.message.with_responses_output(output);
        self
    }

    pub fn system(content: impl Into<String>) -> Self {
        let content = content.into();
        Self {
            message: AgentMessage::system(content),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        let content = content.into();
        Self {
            message: AgentMessage::user(content),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        let content = content.into();
        Self {
            message: AgentMessage::assistant(content),
        }
    }

    pub fn tool(
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        let tool_call_id = tool_call_id.into();
        let name = name.into();
        let content = content.into();
        Self {
            message: AgentMessage::tool(tool_call_id, name, content),
        }
    }

    pub const fn role_name(&self) -> &'static str {
        match &self.message {
            AgentMessage::System { .. } => "system",
            AgentMessage::User { .. } => "user",
            AgentMessage::Assistant { .. } | AgentMessage::AssistantToolCallProtocol { .. } => {
                "assistant"
            }
            AgentMessage::Tool { .. } => "tool",
        }
    }

    pub const fn is_user(&self) -> bool {
        matches!(self.message, AgentMessage::User { .. })
    }

    pub const fn is_system(&self) -> bool {
        matches!(self.message, AgentMessage::System { .. })
    }

    pub const fn is_tool(&self) -> bool {
        matches!(self.message, AgentMessage::Tool { .. })
    }

    pub fn text_content(&self) -> Option<&str> {
        match &self.message {
            AgentMessage::System { content } | AgentMessage::Assistant { content, .. } => {
                Some(content.as_str())
            }
            AgentMessage::User { content } => Some(content.as_text()),
            AgentMessage::AssistantToolCallProtocol { content, .. } => content.as_deref(),
            AgentMessage::Tool { content, .. } => Some(content.as_str()),
        }
    }
}

impl PromptRequest {
    pub fn all_messages(&self) -> Vec<HistoryMessage> {
        let mut messages = self
            .system_messages
            .iter()
            .cloned()
            .map(HistoryMessage::system)
            .collect::<Vec<_>>();
        messages.extend(self.history_messages.clone());
        messages.push(HistoryMessage::user(self.current_user_message.clone()));
        messages.extend(self.retry_messages.clone());
        messages
    }
}

impl AgentMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self::System {
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::User {
            content: AgentContent::text(content),
        }
    }

    pub fn user_content(content: impl Into<AgentContent>) -> Self {
        Self::User {
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::Assistant {
            content: content.into(),
            responses_output: None,
        }
    }

    /// Convenience constructor without a reasoning signature. Production turns
    /// always use [`Self::assistant_tool_call_protocol_with_signed_reasoning`].
    #[cfg(test)]
    pub const fn assistant_tool_call_protocol_with_reasoning(
        content: Option<String>,
        reasoning_content: Option<String>,
        calls: Vec<AgentToolCall>,
    ) -> Self {
        Self::AssistantToolCallProtocol {
            content,
            reasoning_content,
            reasoning_signature: None,
            responses_output: None,
            calls,
        }
    }

    /// Like [`Self::assistant_tool_call_protocol_with_reasoning`], but also
    /// keeps the provider-side reasoning signature so the reasoning block can be
    /// replayed on the next request.
    pub const fn assistant_tool_call_protocol_with_signed_reasoning(
        content: Option<String>,
        reasoning_content: Option<String>,
        reasoning_signature: Option<String>,
        calls: Vec<AgentToolCall>,
    ) -> Self {
        Self::AssistantToolCallProtocol {
            content,
            reasoning_content,
            reasoning_signature,
            responses_output: None,
            calls,
        }
    }

    pub fn with_responses_output(mut self, output: Option<Vec<Value>>) -> Self {
        match &mut self {
            Self::Assistant {
                responses_output, ..
            }
            | Self::AssistantToolCallProtocol {
                responses_output, ..
            } => {
                *responses_output = output.filter(|items| !items.is_empty());
            }
            _ => {}
        }
        self
    }

    pub fn responses_output(&self) -> Option<&[Value]> {
        match self {
            Self::Assistant {
                responses_output, ..
            }
            | Self::AssistantToolCallProtocol {
                responses_output, ..
            } => responses_output.as_deref(),
            _ => None,
        }
    }

    pub fn tool(
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self::Tool {
            tool_call_id: tool_call_id.into(),
            name: name.into(),
            content: content.into(),
        }
    }
}

pub fn summarize_assistant_tool_call_protocol(
    content: Option<&str>,
    calls: &[AgentToolCall],
) -> String {
    let tool_names = calls
        .iter()
        .map(|call| call.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let note = content
        .map(summarize_agent_inline_text)
        .filter(|text| !text.is_empty());
    note.map_or_else(
        || format!("assistant tool-call protocol [{tool_names}]"),
        |note| format!("assistant tool-call protocol [{tool_names}] with note: {note}"),
    )
}

pub fn assistant_tool_call_protocol_char_count(
    content: Option<&str>,
    reasoning_content: Option<&str>,
    calls: &[AgentToolCall],
) -> usize {
    content.unwrap_or_default().chars().count()
        + reasoning_content.unwrap_or_default().chars().count()
        + calls
            .iter()
            .map(|call| {
                call.name.chars().count()
                    + call.id.chars().count()
                    + call.arguments.to_string().chars().count()
            })
            .sum::<usize>()
}

pub fn estimate_assistant_tool_call_protocol_tokens(
    calls: &[AgentToolCall],
    estimate_json_value_tokens: impl Fn(&Value) -> usize,
    approx_token_count: impl Fn(&str) -> usize,
) -> usize {
    calls
        .iter()
        .map(|call| {
            approx_token_count(&call.id)
                .saturating_add(approx_token_count(&call.name))
                .saturating_add(estimate_json_value_tokens(&call.arguments))
                .saturating_add(16)
        })
        .sum()
}

pub fn render_assistant_tool_call_protocol_dump(
    content: Option<&str>,
    reasoning_content: Option<&str>,
    calls: &[AgentToolCall],
) -> Vec<String> {
    let mut lines = vec!["role=assistant".to_string()];
    if let Some(content) = content
        && !content.trim().is_empty()
    {
        lines.push("content:".to_string());
        lines.push(content.to_string());
    }
    if reasoning_content.is_some_and(|text| !text.trim().is_empty()) {
        lines.push("reasoning_content=<redacted>".to_string());
    }
    lines.push(format!("tool_call_count={}", calls.len()));
    for (index, call) in calls.iter().enumerate() {
        lines.push(format!("tool_call[{}].id={}", index, call.id));
        lines.push(format!("tool_call[{}].name={}", index, call.name));
        lines.push(format!("tool_call[{index}].arguments="));
        lines.push(
            serde_json::to_string_pretty(&call.arguments)
                .unwrap_or_else(|_| call.arguments.to_string()),
        );
    }
    lines
}

fn summarize_agent_inline_text(text: &str) -> String {
    const MAX_CHARS: usize = 120;
    let compact = text.replace('\n', "\\n");
    let mut chars = compact.chars();
    let summary = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{summary}...")
    } else {
        summary
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn agent_content_keeps_plain_text_wire_shape() {
        let content = AgentContent::text("hello");

        assert_eq!(serde_json::to_value(&content).unwrap(), json!("hello"));

        let message = AgentMessage::user("hello");
        assert_eq!(
            serde_json::to_value(&message).unwrap(),
            json!({
                "kind": "User",
                "content": "hello"
            })
        );

        let decoded: AgentMessage = serde_json::from_value(json!({
            "kind": "User",
            "content": "hello"
        }))
        .unwrap();
        match decoded {
            AgentMessage::User { content } => {
                assert_eq!(content.as_text(), "hello");
                assert!(content.parts().is_empty());
            }
            _ => panic!("expected user message"),
        }
    }

    #[test]
    fn agent_content_decodes_rich_user_content() {
        let decoded: AgentMessage = serde_json::from_value(json!({
            "kind": "User",
            "content": {
                "text": "look at this",
                "parts": [{
                    "type": "image",
                    "path": "/tmp/demo.png",
                    "media_type": "image/png",
                    "description": "demo image"
                }]
            }
        }))
        .unwrap();

        match decoded {
            AgentMessage::User { content } => {
                assert_eq!(content.as_text(), "look at this");
                assert_eq!(content.parts().len(), 1);
                assert_eq!(
                    content.parts()[0],
                    AgentContentPart::Image {
                        path: "/tmp/demo.png".to_string(),
                        media_type: "image/png".to_string(),
                        description: Some("demo image".to_string()),
                    }
                );
            }
            _ => panic!("expected user message"),
        }
    }

    #[test]
    fn agent_turn_protocol_filters_empty_messages_and_prefers_final_content() {
        let response = AgentTurnStreamResult {
            items: vec![
                AgentTurnItem::AssistantMessage {
                    content: "  ".to_string(),
                },
                AgentTurnItem::AssistantMessage {
                    content: "intermediate".to_string(),
                },
                AgentTurnItem::ToolCall {
                    call: AgentToolCall {
                        id: "call-1".to_string(),
                        name: "example".to_string(),
                        arguments: json!({}),
                    },
                },
            ],
            raw_stream_follow_up: false,
            last_assistant_message: Some("final".to_string()),
            last_reasoning_content: None,
            last_reasoning_signature: None,
            responses_output: None,
        }
        .protocol();

        assert_eq!(response.assistant_text.as_deref(), Some("intermediate"));
        assert_eq!(response.final_assistant_message.as_deref(), Some("final"));
        assert_eq!(response.tool_calls.len(), 1);
        assert!(!response.raw_stream_follow_up);
    }

    #[test]
    fn agent_turn_protocol_falls_back_to_last_nonempty_message() {
        let response = AgentTurnStreamResult {
            items: vec![
                AgentTurnItem::AssistantMessage {
                    content: "first".to_string(),
                },
                AgentTurnItem::AssistantMessage {
                    content: "second".to_string(),
                },
            ],
            raw_stream_follow_up: true,
            last_assistant_message: Some(" ".to_string()),
            last_reasoning_content: None,
            last_reasoning_signature: None,
            responses_output: None,
        }
        .protocol();

        assert!(response.raw_stream_follow_up);
    }
}
