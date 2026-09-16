//! Structured progress events used by live external transports.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveProgressEvent {
    GenerationStarted,
    /// The runtime committed the current draft output (as activity cells or a
    /// final reply), so consumers must drop any buffered draft text.
    DraftReset,
    AssistantContent {
        content: String,
    },
    ReasoningContent {
        content: String,
    },
    TelegramStatus(TelegramLiveStatus),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TelegramLiveStatus {
    pub icon: String,
    pub text: String,
}
