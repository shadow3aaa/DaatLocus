//! Structured progress events used by live external transports.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveProgressEvent {
    GenerationStarted,
    AssistantContent { content: String },
    ReasoningContent { content: String },
    ToolCallTitle { title: String, in_reasoning: bool },
}
