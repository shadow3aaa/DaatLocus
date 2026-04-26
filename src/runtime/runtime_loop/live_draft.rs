use super::*;
use crate::live_progress::LiveProgressEvent;

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
        let mut state = TelegramLiveDraftState::working();
        let mut dirty = false;
        let mut last_sent = previous_sent_text.unwrap_or_default();
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
    reasoning_content: String,
    assistant_content: String,
    reasoning_tool_titles: Vec<String>,
    tool_titles: Vec<String>,
}

impl TelegramLiveDraftState {
    fn working() -> Self {
        Self::default()
    }

    fn apply(&mut self, event: LiveProgressEvent) -> bool {
        match event {
            LiveProgressEvent::GenerationStarted => {
                *self = Self::working();
                false
            }
            LiveProgressEvent::AssistantContent { content } => {
                self.assistant_content = content;
                true
            }
            LiveProgressEvent::ReasoningContent { content } => {
                self.reasoning_content = content;
                true
            }
            LiveProgressEvent::ToolCallTitle {
                title,
                in_reasoning,
            } => {
                let title = title.trim();
                if title.is_empty() {
                    return false;
                }
                let titles = if in_reasoning {
                    &mut self.reasoning_tool_titles
                } else {
                    &mut self.tool_titles
                };
                push_unique_title(titles, title)
            }
        }
    }

    fn render_markdown_v2(&self) -> String {
        let mut sections = Vec::new();
        if let Some(thinking) = self.render_thinking_block() {
            sections.push(thinking);
        }
        if let Some(tools) = self.render_tool_block() {
            sections.push(tools);
        }
        let assistant = self.assistant_content.trim();
        if !assistant.is_empty() {
            sections.push(escape_markdown_v2(assistant));
        }
        let text = if sections.is_empty() {
            "Working\\.\\.\\.".to_string()
        } else {
            sections.join("\n\n")
        };
        truncate_markdown_v2(text)
    }

    fn render_thinking_block(&self) -> Option<String> {
        let mut quote_lines = Vec::new();
        let reasoning = self.reasoning_content.trim();
        if !reasoning.is_empty() {
            quote_lines.extend(reasoning.lines().map(escape_markdown_v2));
        }
        if !quote_lines.is_empty() && !self.reasoning_tool_titles.is_empty() {
            quote_lines.push(String::new());
        }
        quote_lines.extend(
            self.reasoning_tool_titles
                .iter()
                .map(|title| format!("· {}", escape_markdown_v2(title))),
        );
        if quote_lines.is_empty() {
            None
        } else {
            Some(render_markdown_v2_quote(&quote_lines))
        }
    }

    fn render_tool_block(&self) -> Option<String> {
        if self.tool_titles.is_empty() {
            None
        } else {
            Some(
                self.tool_titles
                    .iter()
                    .map(|title| format!("· {}", escape_markdown_v2(title)))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        }
    }
}

fn push_unique_title(titles: &mut Vec<String>, title: &str) -> bool {
    if titles.last().is_some_and(|previous| previous == title) {
        false
    } else {
        titles.push(title.to_string());
        true
    }
}

fn render_markdown_v2_quote(lines: &[String]) -> String {
    lines
        .iter()
        .map(|line| {
            if line.is_empty() {
                ">".to_string()
            } else {
                format!("> {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
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

    #[test]
    fn live_draft_replaces_working_when_content_arrives() {
        let mut state = TelegramLiveDraftState::working();
        assert_eq!(state.render_markdown_v2(), "Working\\.\\.\\.");

        state.apply(LiveProgressEvent::AssistantContent {
            content: "Hello.".to_string(),
        });

        assert_eq!(state.render_markdown_v2(), "Hello\\.");
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
        assert!(!should_send_initial_live_draft("· terminal\\_exec"));
    }

    #[test]
    fn live_draft_renders_reasoning_and_tool_titles_as_quote() {
        let mut state = TelegramLiveDraftState::working();
        state.apply(LiveProgressEvent::ReasoningContent {
            content: "checking options".to_string(),
        });
        state.apply(LiveProgressEvent::ToolCallTitle {
            title: "Execute: cargo test --all-targets".to_string(),
            in_reasoning: true,
        });
        state.apply(LiveProgressEvent::AssistantContent {
            content: "Current answer.".to_string(),
        });

        assert_eq!(
            state.render_markdown_v2(),
            "> checking options\n>\n> · Execute: cargo test \\-\\-all\\-targets\n\nCurrent answer\\."
        );
    }

    #[test]
    fn live_draft_generation_started_clears_without_forcing_working_send() {
        let mut state = TelegramLiveDraftState::working();
        state.apply(LiveProgressEvent::ReasoningContent {
            content: "old turn".to_string(),
        });
        assert!(!state.apply(LiveProgressEvent::GenerationStarted));

        assert_eq!(state.render_markdown_v2(), "Working\\.\\.\\.");

        state.apply(LiveProgressEvent::ToolCallTitle {
            title: "terminal_exec".to_string(),
            in_reasoning: false,
        });
        assert_eq!(state.render_markdown_v2(), "· terminal\\_exec");
    }

    #[test]
    fn live_draft_generation_started_cancels_unflushed_dirty_state() {
        let mut state = TelegramLiveDraftState::working();
        let mut dirty = false;

        apply_live_progress_event(
            &mut state,
            &mut dirty,
            LiveProgressEvent::ToolCallTitle {
                title: "activate_workflow".to_string(),
                in_reasoning: false,
            },
        );
        assert!(dirty);

        apply_live_progress_event(&mut state, &mut dirty, LiveProgressEvent::GenerationStarted);

        assert!(!dirty);
        assert_eq!(state.render_markdown_v2(), "Working\\.\\.\\.");
    }

    #[test]
    fn live_draft_keeps_non_reasoning_tool_titles_outside_quote() {
        let mut state = TelegramLiveDraftState::working();
        state.apply(LiveProgressEvent::ToolCallTitle {
            title: "terminal_exec".to_string(),
            in_reasoning: false,
        });
        state.apply(LiveProgressEvent::AssistantContent {
            content: "Tool started.".to_string(),
        });

        assert_eq!(
            state.render_markdown_v2(),
            "· terminal\\_exec\n\nTool started\\."
        );
    }

    #[test]
    fn live_draft_escapes_markdown_v2_dynamic_content() {
        assert_eq!(
            escape_markdown_v2("_*[]()~`>#+-=|{}.!\\"),
            "\\_\\*\\[\\]\\(\\)\\~\\`\\>\\#\\+\\-\\=\\|\\{\\}\\.\\!\\\\"
        );
    }
}
