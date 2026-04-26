//! Structured progress events used by live external transports.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveProgressEvent {
    GenerationStarted,
    AssistantContent { content: String },
    ReasoningContent { content: String },
    TelegramStatus(TelegramLiveStatus),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TelegramLiveStatus {
    pub icon: String,
    pub text: String,
}
