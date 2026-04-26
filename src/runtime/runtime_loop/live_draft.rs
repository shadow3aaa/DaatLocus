use super::*;
use crate::live_progress::{LiveProgressEvent, TelegramLiveStatus};

const TELEGRAM_MESSAGE_LIMIT: usize = 4096;
const MARKDOWN_V2_ELLIPSIS: &str = "\\.\\.\\.";

pub(super) struct TelegramLiveDraftSession {
    join: JoinHandle<()>,
}

impl TelegramLiveDraftSession {
    pub(super) async fn shutdown(self, context: &mut Context) {
        context.install_live_progress(None);
        let _ = tokio::time::timeout(Duration::from_secs(2), self.join).await;
    }
}

pub(super) fn maybe_start_telegram_live_draft_session(
    context: &mut Context,
    claimed_event_views: &[EventView],
) -> Option<TelegramLiveDraftSession> {
    if claimed_event_views.len() != 1 {
        return None;
    }
    let event = claimed_event_views.first()?;
    let EventPayload::TelegramIncoming(payload) = &event.payload else {
        return None;
    };
    if payload.chat_kind != "private" {
        return None;
    }
    if !context.config.telegram.enabled || !context.config.telegram.has_real_credentials() {
        return None;
    }
    let chat_id = payload.chat_id.parse::<i64>().ok()?;
    let event_id = event.event_id.to_string();
    let (draft_id, previous_sent_text) = context
        .get_or_create_telegram_live_draft(event_id.clone(), stable_live_draft_id(event.event_id));
    let live_drafts = context.telegram_live_drafts.clone();
    let client = TelegramLiveDraftClient::new(context.config.telegram.clone());
    let (tx, mut rx) = mpsc::unbounded_channel::<LiveProgressEvent>();
    context.install_live_progress(Some(tx));
    let join = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(900));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut last_sent = previous_sent_text.unwrap_or_default();
        let mut state = TelegramLiveDraftState::from_previous_sent(&last_sent);
        let mut dirty = false;
        let initial_draft_text = state.render_markdown_v2();
        if should_send_initial_live_draft(&last_sent) {
            if let Err(err) = client
                .send_message_draft(chat_id, draft_id, &initial_draft_text)
                .await
            {
                tracing::warn!("telegram initial live draft send failed: {err:?}");
            } else {
                record_live_draft_sent(&live_drafts, &event_id, &initial_draft_text);
                last_sent = initial_draft_text;
            }
        }
        loop {
            tokio::select! {
                maybe_event = rx.recv() => {
                    match maybe_event {
                        Some(event) => {
                            apply_live_progress_event(&mut state, &mut dirty, event);
                        }
                        None => break,
                    }
                }
                _ = interval.tick() => {
                    if dirty {
                        let draft_text = state.render_markdown_v2();
                        if draft_text != last_sent {
                            if let Err(err) = client
                                .send_message_draft(chat_id, draft_id, &draft_text)
                                .await
                            {
                                tracing::warn!("telegram live draft update failed: {err:?}");
                            } else {
                                record_live_draft_sent(&live_drafts, &event_id, &draft_text);
                                last_sent = draft_text;
                            }
                        }
                        dirty = false;
                    }
                }
            }
        }
        if dirty {
            let draft_text = state.render_markdown_v2();
            if draft_text != last_sent
                && let Err(err) = client
                    .send_message_draft(chat_id, draft_id, &draft_text)
                    .await
            {
                tracing::warn!("telegram final live draft flush failed: {err:?}");
            } else if draft_text != last_sent {
                record_live_draft_sent(&live_drafts, &event_id, &draft_text);
            }
        }
    });
    Some(TelegramLiveDraftSession { join })
}

fn stable_live_draft_id(event_id: uuid::Uuid) -> i64 {
    ((event_id.as_u128() % i64::MAX as u128) + 1) as i64
}

fn should_send_initial_live_draft(last_sent: &str) -> bool {
    last_sent.is_empty()
}

fn record_live_draft_sent(
    live_drafts: &crate::context::TelegramLiveDraftRegistry,
    event_id: &str,
    text: &str,
) {
    if let Some(record) = live_drafts.lock().get_mut(event_id) {
        record.last_sent_text = Some(text.to_string());
    }
}

fn apply_live_progress_event(
    state: &mut TelegramLiveDraftState,
    dirty: &mut bool,
    event: LiveProgressEvent,
) {
    match event {
        LiveProgressEvent::GenerationStarted => {
            state.apply(LiveProgressEvent::GenerationStarted);
            *dirty = false;
        }
        event => {
            if state.apply(event) {
                *dirty = true;
            }
        }
    }
}

#[derive(Default)]
struct TelegramLiveDraftState {
    previous_markdown_v2: Option<String>,
    status: Option<TelegramLiveStatus>,
}

impl TelegramLiveDraftState {
    fn working() -> Self {
        Self::default()
    }

    fn from_previous_sent(previous_sent_text: &str) -> Self {
        let mut state = Self::working();
        if !previous_sent_text.trim().is_empty() && previous_sent_text != "Working\\.\\.\\." {
            state.previous_markdown_v2 = Some(previous_sent_text.to_string());
        }
        state
    }

    fn apply(&mut self, event: LiveProgressEvent) -> bool {
        match event {
            LiveProgressEvent::GenerationStarted => false,
            LiveProgressEvent::AssistantContent { .. }
            | LiveProgressEvent::ReasoningContent { .. } => false,
            LiveProgressEvent::TelegramStatus(status) => {
                let icon = status.icon.trim();
                let text = status.text.trim();
                if icon.is_empty() || text.is_empty() {
                    return false;
                }
                let status = TelegramLiveStatus {
                    icon: icon.to_string(),
                    text: text.to_string(),
                };
                let changed =
                    self.status.as_ref() != Some(&status) || self.previous_markdown_v2.is_some();
                self.status = Some(status);
                self.previous_markdown_v2 = None;
                changed
            }
        }
    }

    fn render_markdown_v2(&self) -> String {
        if let Some(status) = &self.status {
            return truncate_markdown_v2(render_status_markdown_v2(status));
        }
        if let Some(previous) = &self.previous_markdown_v2 {
            return truncate_markdown_v2(previous.clone());
        }
        "Working\\.\\.\\.".to_string()
    }
}

fn render_status_markdown_v2(status: &TelegramLiveStatus) -> String {
    format!(
        "{} {}",
        escape_markdown_v2(status.icon.trim()),
        escape_markdown_v2(status.text.trim())
    )
}

fn escape_markdown_v2(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '_' | '*' | '[' | ']' | '(' | ')' | '~' | '`' | '>' | '#' | '+' | '-' | '=' | '|'
            | '{' | '}' | '.' | '!' | '\\' => {
                escaped.push('\\');
                escaped.push(ch);
            }
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn truncate_markdown_v2(text: String) -> String {
    if text.chars().count() <= TELEGRAM_MESSAGE_LIMIT {
        return text;
    }
    let max_prefix_len = TELEGRAM_MESSAGE_LIMIT - MARKDOWN_V2_ELLIPSIS.chars().count();
    let mut truncated = text.chars().take(max_prefix_len).collect::<String>();
    while truncated.ends_with('\\') {
        truncated.pop();
    }
    truncated.push_str(MARKDOWN_V2_ELLIPSIS);
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(icon: &str, text: &str) -> TelegramLiveStatus {
        TelegramLiveStatus {
            icon: icon.to_string(),
            text: text.to_string(),
        }
    }

    #[test]
    fn live_draft_replaces_working_when_status_arrives() {
        let mut state = TelegramLiveDraftState::working();
        assert_eq!(state.render_markdown_v2(), "Working\\.\\.\\.");

        state.apply(LiveProgressEvent::TelegramStatus(status(
            crate::tool_ui::glyph::PLAN,
            "Plan Updated",
        )));

        assert_eq!(state.render_markdown_v2(), "∷ Plan Updated");
    }

    #[test]
    fn live_draft_id_is_stable_for_event() {
        let event_id = uuid::Uuid::parse_str("65d9e9f8-ae4f-455c-af4f-1cc6d7d0368c").unwrap();

        assert_eq!(
            stable_live_draft_id(event_id),
            stable_live_draft_id(event_id)
        );
        assert!(stable_live_draft_id(event_id) > 0);
    }

    #[test]
    fn live_draft_initial_working_is_sent_only_without_prior_text() {
        assert!(should_send_initial_live_draft(""));
        assert!(!should_send_initial_live_draft("Working\\.\\.\\."));
        assert!(!should_send_initial_live_draft("∷ Plan Updated"));
    }

    #[test]
    fn live_draft_ignores_reasoning_and_assistant_content() {
        let mut state = TelegramLiveDraftState::working();
        assert!(!state.apply(LiveProgressEvent::ReasoningContent {
            content: "checking options".to_string(),
        }));
        assert!(!state.apply(LiveProgressEvent::AssistantContent {
            content: "Current answer.".to_string(),
        }));

        assert_eq!(state.render_markdown_v2(), "Working\\.\\.\\.");
    }

    #[test]
    fn live_draft_generation_started_preserves_last_status() {
        let mut state = TelegramLiveDraftState::working();
        state.apply(LiveProgressEvent::TelegramStatus(status(
            crate::tool_ui::glyph::MEMORY,
            "Recalled 3 Memories",
        )));
        assert!(!state.apply(LiveProgressEvent::GenerationStarted));

        assert_eq!(state.render_markdown_v2(), "⟲ Recalled 3 Memories");
    }

    #[test]
    fn live_draft_generation_started_cancels_unflushed_dirty_state() {
        let mut state = TelegramLiveDraftState::working();
        let mut dirty = false;

        apply_live_progress_event(
            &mut state,
            &mut dirty,
            LiveProgressEvent::TelegramStatus(status(
                crate::tool_ui::glyph::WORKFLOW,
                "Workflow Active: repo-analysis",
            )),
        );
        assert!(dirty);

        apply_live_progress_event(&mut state, &mut dirty, LiveProgressEvent::GenerationStarted);

        assert!(!dirty);
        assert_eq!(
            state.render_markdown_v2(),
            "⌘ Workflow Active: repo\\-analysis"
        );
    }

    #[test]
    fn live_draft_restores_previous_sent_text_for_next_session() {
        let mut state = TelegramLiveDraftState::from_previous_sent("∷ Plan Updated");

        assert_eq!(state.render_markdown_v2(), "∷ Plan Updated");

        state.apply(LiveProgressEvent::TelegramStatus(status(
            crate::tool_ui::glyph::EXEC,
            "Command Ran",
        )));
        assert_eq!(state.render_markdown_v2(), "• Command Ran");
    }

    #[test]
    fn live_draft_replaces_previous_status() {
        let mut state = TelegramLiveDraftState::working();
        state.apply(LiveProgressEvent::TelegramStatus(status(
            crate::tool_ui::glyph::PLAN,
            "Plan Updated",
        )));
        state.apply(LiveProgressEvent::TelegramStatus(status(
            crate::tool_ui::glyph::MEMORY,
            "Recalled 1 Memory",
        )));

        assert_eq!(state.render_markdown_v2(), "⟲ Recalled 1 Memory");
    }

    #[test]
    fn live_draft_escapes_markdown_v2_dynamic_content() {
        assert_eq!(
            escape_markdown_v2("_*[]()~`>#+-=|{}.!\\"),
            "\\_\\*\\[\\]\\(\\)\\~\\`\\>\\#\\+\\-\\=\\|\\{\\}\\.\\!\\\\"
        );
    }
}
