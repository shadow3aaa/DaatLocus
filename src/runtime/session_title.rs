use std::time::Duration;

use chrono::Utc;
use miette::{Result, miette};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{
    context::Context,
    dashboard::{DashboardSessionTitle, DashboardState},
    events::{EventPayload, EventView},
    reasoning::{
        prompts::{
            SESSION_TITLE_SYSTEM_REQUIREMENTS, SESSION_TITLE_SYSTEM_ROLE,
            SESSION_TITLE_TOOL_DESCRIPTION, SESSION_TITLE_USER_MESSAGE_PREFIX,
        },
        runtime::{HistoryMessage, PromptRequest},
    },
};

const SESSION_TITLE_REFRESH_INTERVAL_MS: i64 = 5 * 60 * 1000;
const SESSION_TITLE_GENERATE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_TITLE_CHARS: usize = 64;
const MAX_EXCERPT_ITEMS: usize = 16;
const MAX_EXCERPT_ITEM_CHARS: usize = 360;

#[derive(Debug, Clone, Default)]
pub struct SessionTitleState {
    current: Option<DashboardSessionTitle>,
    last_activity_signature: Option<String>,
    last_generated_signature: Option<String>,
    last_generated_at_ms: Option<i64>,
}

impl SessionTitleState {
    pub fn snapshot(&self) -> Option<DashboardSessionTitle> {
        self.current.clone()
    }

    fn apply_placeholder(&mut self, signature: &str, title: String, now_ms: i64) -> bool {
        self.last_activity_signature = Some(signature.to_string());
        if self
            .current
            .as_ref()
            .is_some_and(|current| current.generated || current.title.trim() == title.trim())
        {
            return false;
        }
        self.current = Some(DashboardSessionTitle {
            title,
            generated: false,
            updated_at_ms: now_ms,
        });
        true
    }

    fn should_generate(&self, signature: &str, now_ms: i64) -> bool {
        if self.last_generated_signature.as_deref() == Some(signature) {
            return false;
        }
        match self.last_generated_at_ms {
            None => true,
            Some(last) => now_ms.saturating_sub(last) >= SESSION_TITLE_REFRESH_INTERVAL_MS,
        }
    }

    fn apply_generated(&mut self, signature: String, title: String, now_ms: i64) -> bool {
        self.last_activity_signature = Some(signature.clone());
        self.last_generated_signature = Some(signature);
        self.last_generated_at_ms = Some(now_ms);
        if self
            .current
            .as_ref()
            .is_some_and(|current| current.generated && current.title.trim() == title.trim())
        {
            return false;
        }
        self.current = Some(DashboardSessionTitle {
            title,
            generated: true,
            updated_at_ms: now_ms,
        });
        true
    }
}

pub fn sync_session_title_placeholder(
    context: &mut Context,
    tx: &tokio::sync::watch::Sender<DashboardState>,
) {
    let Some(input) = SessionTitleInput::from_context(context) else {
        return;
    };
    let now_ms = Utc::now().timestamp_millis();
    if context.session_title.apply_placeholder(
        &input.activity_signature,
        input.placeholder_title,
        now_ms,
    ) {
        sync_dashboard_session_title(context, tx);
    }
}

pub async fn refresh_session_title_after_activity(
    context: &mut Context,
    tx: &tokio::sync::watch::Sender<DashboardState>,
) -> Result<()> {
    let Some(input) = SessionTitleInput::from_context(context) else {
        return Ok(());
    };
    let now_ms = Utc::now().timestamp_millis();
    if context.session_title.apply_placeholder(
        &input.activity_signature,
        input.placeholder_title.clone(),
        now_ms,
    ) {
        sync_dashboard_session_title(context, tx);
    }
    if !context
        .session_title
        .should_generate(&input.activity_signature, now_ms)
    {
        return Ok(());
    }

    let generated = match tokio::time::timeout(
        SESSION_TITLE_GENERATE_TIMEOUT,
        generate_session_title(context, &input.excerpt),
    )
    .await
    {
        Ok(result) => result?,
        Err(_) => {
            return Err(miette!(
                "session title generation timed out after {}s",
                SESSION_TITLE_GENERATE_TIMEOUT.as_secs()
            ));
        }
    };
    if context
        .session_title
        .apply_generated(input.activity_signature, generated, now_ms)
    {
        sync_dashboard_session_title(context, tx);
    }
    Ok(())
}

fn sync_dashboard_session_title(
    context: &Context,
    tx: &tokio::sync::watch::Sender<DashboardState>,
) {
    let session_title = context.session_title.snapshot();
    tx.send_modify(|state| {
        state.session_title = session_title;
    });
}

struct SessionTitleInput {
    placeholder_title: String,
    activity_signature: String,
    excerpt: String,
}

impl SessionTitleInput {
    fn from_context(context: &Context) -> Option<Self> {
        let events = context.events.views();
        let messages = context.memory.runtime_conversation_messages();
        let placeholder_title =
            first_event_title(&events).or_else(|| first_visible_history_title(&messages))?;
        let excerpt = conversation_excerpt(&events, &messages);
        if excerpt.trim().is_empty() {
            return None;
        }
        Some(Self {
            placeholder_title,
            activity_signature: activity_signature(&events, &messages),
            excerpt,
        })
    }
}

#[derive(Deserialize)]
struct TitleOutput {
    title: String,
}

async fn generate_session_title(context: &Context, excerpt: &str) -> Result<String> {
    let request = PromptRequest {
        tool_name: "set_session_title".to_string(),
        tool_description: SESSION_TITLE_TOOL_DESCRIPTION.to_string(),
        output_schema: json!({
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "A concise human-readable session title."
                }
            },
            "required": ["title"],
            "additionalProperties": false
        }),
        system_messages: vec![
            SESSION_TITLE_SYSTEM_ROLE.to_string(),
            SESSION_TITLE_SYSTEM_REQUIREMENTS.to_string(),
        ],
        long_term_memory_messages: Vec::new(),
        history_messages: Vec::new(),
        current_user_message: format!("{SESSION_TITLE_USER_MESSAGE_PREFIX}\n{excerpt}"),
        retry_messages: Vec::new(),
    };
    let value = context.efficient_llm.run_json(context, request).await?;
    let output = serde_json::from_value::<TitleOutput>(value)
        .map_err(|err| miette!("decode session title output failed: {err}"))?;
    normalize_session_title(&output.title).ok_or_else(|| miette!("session title output was empty"))
}

fn first_event_title(events: &[EventView]) -> Option<String> {
    events
        .iter()
        .filter_map(event_text)
        .find_map(first_sentence_title)
}

fn first_visible_history_title(messages: &[HistoryMessage]) -> Option<String> {
    messages
        .iter()
        .filter(|message| !message.is_system() && !message.is_tool())
        .filter_map(|message| message.text_content())
        .filter(|content| !is_runtime_context_text(content))
        .find_map(first_sentence_title)
}

fn conversation_excerpt(events: &[EventView], messages: &[HistoryMessage]) -> String {
    let mut lines = Vec::new();
    for event in events {
        if lines.len() >= MAX_EXCERPT_ITEMS {
            break;
        }
        if let Some(text) = event_text(event) {
            push_excerpt_line(&mut lines, "User", &text);
        }
    }
    for message in messages {
        if lines.len() >= MAX_EXCERPT_ITEMS {
            break;
        }
        if message.is_system() || message.is_tool() {
            continue;
        }
        let Some(text) = message.text_content() else {
            continue;
        };
        if is_runtime_context_text(text) {
            continue;
        }
        let role = if message.is_user() {
            "User"
        } else {
            "Assistant"
        };
        push_excerpt_line(&mut lines, role, text);
    }
    lines.join("\n")
}

fn push_excerpt_line(lines: &mut Vec<String>, role: &str, text: &str) {
    let compact = compact_inline(text);
    if compact.is_empty() {
        return;
    }
    lines.push(format!(
        "{role}: {}",
        truncate_chars(&compact, MAX_EXCERPT_ITEM_CHARS)
    ));
}

fn activity_signature(events: &[EventView], messages: &[HistoryMessage]) -> String {
    let mut hasher = Sha256::new();
    for event in events {
        hasher.update(event.event_id.as_bytes());
        if let Some(text) = event_text(event) {
            hasher.update(text.as_bytes());
        }
    }
    for message in messages {
        if message.is_system() || message.is_tool() {
            continue;
        }
        let Some(text) = message.text_content() else {
            continue;
        };
        if is_runtime_context_text(text) {
            continue;
        }
        hasher.update(message.role_name().as_bytes());
        hasher.update(text.as_bytes());
    }
    hex::encode(hasher.finalize())
}

fn event_text(event: &EventView) -> Option<String> {
    match &event.payload {
        EventPayload::TelegramIncoming(payload) => Some(payload.incoming_text.clone()),
        EventPayload::TerminalIncoming(payload) => Some(payload.incoming_text.clone()),
    }
    .map(|text| text.trim().to_string())
    .filter(|text| !text.is_empty())
}

fn first_sentence_title(text: impl AsRef<str>) -> Option<String> {
    let compact = compact_inline(text.as_ref());
    if compact.is_empty() {
        return None;
    }
    let sentence_end = compact.char_indices().find_map(|(index, ch)| {
        matches!(ch, '.' | '!' | '?' | '。' | '！' | '？').then_some(index)
    });
    let candidate = match sentence_end {
        Some(index) => compact[..index].trim(),
        None => compact.trim(),
    };
    normalize_session_title(candidate)
}

fn normalize_session_title(title: &str) -> Option<String> {
    let compact = compact_inline(title)
        .trim_matches(|ch: char| matches!(ch, '"' | '\'' | '`' | '“' | '”' | '‘' | '’'))
        .trim()
        .to_string();
    if compact.is_empty() {
        return None;
    }
    Some(truncate_chars(&compact, MAX_TITLE_CHARS))
}

fn compact_inline(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        truncated.trim_end().to_string()
    } else {
        truncated
    }
}

fn is_runtime_context_text(text: &str) -> bool {
    let text = text.trim_start();
    text.starts_with("<preturn_context") || text.starts_with("<afterclaim_context")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        events::{EventSource, EventStatus, TelegramIncomingEvent},
        reasoning::runtime::HistoryMessage,
    };

    fn event(id: &str, text: &str) -> EventView {
        EventView {
            event_id: uuid::Uuid::parse_str(id).expect("uuid"),
            source: EventSource::Telegram,
            status: EventStatus::Resolved,
            reply_message: None,
            arrived_at_ms: 0,
            payload: EventPayload::TelegramIncoming(TelegramIncomingEvent {
                chat_id: "1".to_string(),
                chat_kind: "private".to_string(),
                chat_title: "chat".to_string(),
                sender: "alice".to_string(),
                incoming_text: text.to_string(),
                telegram_update_id: 1,
                telegram_message_id: None,
                telegram_message_date: None,
                attachments: Vec::new(),
            }),
            last_error: None,
        }
    }

    #[test]
    fn placeholder_uses_first_event_sentence() {
        let events = vec![event(
            "11111111-1111-4111-8111-111111111111",
            "Please inspect the repository. Then commit it.",
        )];

        assert_eq!(
            first_event_title(&events).as_deref(),
            Some("Please inspect the repository")
        );
    }

    #[test]
    fn visible_history_title_skips_runtime_context() {
        let messages = vec![
            HistoryMessage::user("<preturn_context>state</preturn_context>"),
            HistoryMessage::user("Fix the Telegram session routing. Thanks"),
        ];

        assert_eq!(
            first_visible_history_title(&messages).as_deref(),
            Some("Fix the Telegram session routing")
        );
    }

    #[test]
    fn generation_requires_changed_activity_and_interval() {
        let mut state = SessionTitleState::default();
        assert!(state.should_generate("a", 0));
        assert!(state.apply_generated("a".to_string(), "Initial".to_string(), 0));
        assert!(!state.should_generate("a", SESSION_TITLE_REFRESH_INTERVAL_MS + 1));
        assert!(!state.should_generate("b", SESSION_TITLE_REFRESH_INTERVAL_MS - 1));
        assert!(state.should_generate("b", SESSION_TITLE_REFRESH_INTERVAL_MS));
    }
}
