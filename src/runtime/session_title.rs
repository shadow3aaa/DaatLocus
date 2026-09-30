use chrono::Utc;
use sha2::{Digest, Sha256};
use tracing::{debug, warn};

use crate::{
    context::Context,
    core::ModelRequestOptions,
    dashboard::{DashboardSessionTitle, DashboardState},
    events::{EventPayload, EventView},
    reasoning::{
        prompts::{
            SESSION_TITLE_SYSTEM_REQUIREMENTS, SESSION_TITLE_SYSTEM_ROLE,
            SESSION_TITLE_USER_MESSAGE_PREFIX,
        },
        runtime::{AgentMessage, AgentTurnRequest, HistoryMessage},
    },
};

const MAX_TITLE_CHARS: usize = 64;
const MAX_EXCERPT_ITEMS: usize = 16;
const MAX_EXCERPT_ITEM_CHARS: usize = 360;

fn title_generation_system_prompt() -> String {
    format!("{SESSION_TITLE_SYSTEM_ROLE}\n\n{SESSION_TITLE_SYSTEM_REQUIREMENTS}")
}
const TITLE_GENERATION_USER_PROMPT: &str = SESSION_TITLE_USER_MESSAGE_PREFIX;

fn build_session_title_request(excerpt: &str) -> AgentTurnRequest {
    AgentTurnRequest {
        messages: vec![
            AgentMessage::system(title_generation_system_prompt()),
            AgentMessage::user(format!("{TITLE_GENERATION_USER_PROMPT}\n{excerpt}")),
        ],
        tools: Vec::new(),
    }
}

#[derive(Debug, Clone, Default)]
pub struct SessionTitleState {
    current: Option<DashboardSessionTitle>,
    last_activity_signature: Option<String>,
    last_generated_at_ms: Option<i64>,
    last_generated_signature: Option<String>,
    last_activity_event_ids: Vec<uuid::Uuid>,
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

    fn should_generate_activity(&self, event_ids: &[uuid::Uuid]) -> bool {
        self.last_generated_signature.is_none()
            || self.last_activity_event_ids.as_slice() != event_ids
    }

    fn should_generate(&self, signature: &str) -> bool {
        self.last_generated_signature.as_deref() != Some(signature)
    }

    fn apply_generation_result(
        &mut self,
        result: SessionTitleGenerationResult,
        now_ms: i64,
    ) -> bool {
        if self.last_generated_signature.as_deref() != Some(result.activity_signature.as_str()) {
            return false;
        }
        let Some(title) = result.title else {
            return false;
        };
        if self
            .current
            .as_ref()
            .is_some_and(|current| current.generated && current.title == title)
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

fn sync_dashboard_session_title(
    context: &Context,
    tx: &tokio::sync::watch::Sender<DashboardState>,
) {
    let session_title = context.session_title.snapshot();
    tx.send_modify(|state| {
        state.session_title = session_title;
    });
}

pub fn apply_session_title_generation_result(
    context: &mut Context,
    tx: &tokio::sync::watch::Sender<DashboardState>,
    result: SessionTitleGenerationResult,
) {
    if context
        .session_title
        .apply_generation_result(result, Utc::now().timestamp_millis())
    {
        sync_dashboard_session_title(context, tx);
    }
}

#[derive(Debug, Clone)]
pub struct SessionTitleGenerationResult {
    pub activity_signature: String,
    pub title: Option<String>,
}

impl SessionTitleGenerationResult {
    fn from_text(activity_signature: String, output: &str) -> Self {
        Self {
            activity_signature,
            title: normalize_session_title(output),
        }
    }
}

pub fn spawn_session_title_generation(
    context: &mut Context,
    results_tx: &tokio::sync::mpsc::UnboundedSender<SessionTitleGenerationResult>,
) {
    let event_ids = context
        .events
        .driver_event_statuses()
        .into_iter()
        .map(|(event_id, _)| event_id)
        .collect::<Vec<_>>();
    if !context.session_title.should_generate_activity(&event_ids) {
        return;
    }

    let events = context.events.views();
    let messages = context.memory.runtime_conversation_messages();
    let prepared = prepare_session_title_activity(&events, &messages);
    if !context
        .session_title
        .should_generate(&prepared.activity_signature)
    {
        return;
    }
    if prepared.excerpt.trim().is_empty() {
        return;
    }

    // Eagerly mark as generated to prevent duplicate spawns.
    let now_ms = Utc::now().timestamp_millis();
    context.session_title.last_generated_signature = Some(prepared.activity_signature.clone());
    context.session_title.last_generated_at_ms = Some(now_ms);
    context.session_title.last_activity_event_ids = event_ids;

    let request = build_session_title_request(&prepared.excerpt);

    let model_provider = context.efficient_model_provider.clone();
    let options = match ModelRequestOptions::for_agent_turn(
        model_provider.as_ref(),
        &request,
        context.session_id.clone(),
    ) {
        Ok(options) => options,
        Err(err) => {
            warn!("failed to create session title request options: {err:?}");
            return;
        }
    };
    let results_tx = results_tx.clone();
    let signature = prepared.activity_signature;
    tokio::spawn(async move {
        match model_provider.complete_agent_turn(request, options).await {
            Ok(response) => {
                let Some(output) = response.protocol().final_assistant_message else {
                    warn!("session title generation returned empty assistant content");
                    return;
                };
                let result = SessionTitleGenerationResult::from_text(signature, &output);
                if let Some(title) = result.title.as_deref() {
                    debug!(title, "session title generated");
                }
                let _ = results_tx.send(result);
            }
            Err(err) => {
                warn!("session title generation failed: {err:?}");
            }
        }
    });
}

struct SessionTitleInput {
    placeholder_title: String,
    activity_signature: String,
}

impl SessionTitleInput {
    fn from_context(context: &Context) -> Option<Self> {
        let events = context.events.views();
        let messages = context.memory.runtime_conversation_messages();
        let prepared = prepare_session_title_activity(&events, &messages);
        let placeholder_title = prepared.placeholder_title?;
        if prepared.excerpt.trim().is_empty() {
            return None;
        }

        Some(Self {
            placeholder_title,
            activity_signature: prepared.activity_signature,
        })
    }
}

struct PreparedSessionTitleActivity {
    placeholder_title: Option<String>,
    excerpt: String,
    activity_signature: String,
}

fn prepare_session_title_activity(
    events: &[EventView],
    messages: &[HistoryMessage],
) -> PreparedSessionTitleActivity {
    let mut hasher = Sha256::new();
    let mut lines = Vec::new();
    let mut placeholder_title = None;

    for event in events {
        hasher.update(event.event_id.as_bytes());
        let Some(text) = event_text(event) else {
            continue;
        };
        hasher.update(text.as_bytes());
        if placeholder_title.is_none() {
            placeholder_title = first_sentence_title(&text);
        }
        if lines.len() < MAX_EXCERPT_ITEMS {
            push_excerpt_line(&mut lines, "User", &text);
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
        if placeholder_title.is_none() {
            placeholder_title = first_sentence_title(text);
        }
        if lines.len() < MAX_EXCERPT_ITEMS {
            let role = if message.is_user() {
                "User"
            } else {
                "Assistant"
            };
            push_excerpt_line(&mut lines, role, text);
        }
    }

    PreparedSessionTitleActivity {
        placeholder_title,
        excerpt: lines.join("\n"),
        activity_signature: hex::encode(hasher.finalize()),
    }
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
    let candidate = sentence_end.map_or_else(|| compact.trim(), |index| compact[..index].trim());
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
        events::{EventStatus, TelegramIncomingEvent},
        reasoning::runtime::HistoryMessage,
    };

    fn event(id: &str, text: &str) -> EventView {
        EventView {
            event_id: uuid::Uuid::parse_str(id).expect("uuid"),
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
            prepare_session_title_activity(&events, &[])
                .placeholder_title
                .as_deref(),
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
            prepare_session_title_activity(&[], &messages)
                .placeholder_title
                .as_deref(),
            Some("Fix the Telegram session routing")
        );
    }

    #[test]
    fn title_generation_request_is_a_tool_free_text_turn() {
        let request = build_session_title_request("User: Design hidden-state memory compaction");

        assert!(request.tools.is_empty());
        assert!(matches!(
            request.messages.first(),
            Some(AgentMessage::System { .. })
        ));
        assert!(matches!(
            request.messages.last(),
            Some(AgentMessage::User { content })
                if content.as_text().contains("Design hidden-state memory compaction")
        ));
    }

    #[test]
    fn title_generation_accepts_plain_assistant_text() {
        let result = SessionTitleGenerationResult::from_text(
            "activity".to_string(),
            "  \"隐状态压缩记忆方案\"  ",
        );

        assert_eq!(result.title.as_deref(), Some("隐状态压缩记忆方案"));
    }

    #[test]
    fn generation_result_only_updates_the_current_activity() {
        let mut state = SessionTitleState {
            current: Some(DashboardSessionTitle {
                title: "placeholder".to_string(),
                generated: false,
                updated_at_ms: 0,
            }),
            last_generated_signature: Some("current activity".to_string()),
            ..SessionTitleState::default()
        };

        assert!(!state.apply_generation_result(
            SessionTitleGenerationResult {
                activity_signature: "stale activity".to_string(),
                title: Some("Stale title".to_string()),
            },
            1,
        ));
        assert_eq!(
            state.snapshot().expect("placeholder title").title,
            "placeholder"
        );

        assert!(state.apply_generation_result(
            SessionTitleGenerationResult {
                activity_signature: "current activity".to_string(),
                title: Some("Current title".to_string()),
            },
            2,
        ));
        let title = state.snapshot().expect("generated title");
        assert_eq!(title.title, "Current title");
        assert!(title.generated);
        assert_eq!(title.updated_at_ms, 2);
    }
}
