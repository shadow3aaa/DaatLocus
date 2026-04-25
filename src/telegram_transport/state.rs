use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
};

use miette::{Result, miette};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use uuid::Uuid;

use crate::{events::EventStatus, persistence::PersistenceStore};

const TELEGRAM_TRANSPORT_STATE_FILE_NAME: &str = "telegram_transport_state";
const TELEGRAM_MESSAGE_CHAR_LIMIT: usize = 4096;
const TELEGRAM_CHUNK_BODY_CHAR_LIMIT: usize = 3900;

pub struct TelegramTransportState {
    inner: Arc<TelegramInner>,
}

struct TelegramInner {
    state: Mutex<TelegramState>,
    outbound_notify: Notify,
    persistence_path: PathBuf,
}

#[derive(Default)]
struct TelegramState {
    order: Vec<String>,
    chats: HashMap<String, TelegramChat>,
    outbox: VecDeque<PendingOutboundMessage>,
}

#[derive(Default, Serialize, Deserialize)]
struct PersistedTelegramState {
    order: Vec<String>,
    chats: HashMap<String, PersistedTelegramChat>,
    outbox: VecDeque<PendingOutboundMessage>,
}

struct TelegramChat {
    id: String,
    title: String,
}

#[derive(Serialize, Deserialize)]
struct PersistedTelegramChat {
    id: String,
    title: String,
}

#[derive(Clone)]
pub struct TelegramTransportStateHandle {
    inner: Arc<TelegramInner>,
}

#[derive(Clone, Serialize)]
pub struct TelegramChatSummaryView {
    pub chat_id: String,
    pub title: String,
    pub pending_outbound_count: usize,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PendingOutboundMessage {
    pub local_message_id: String,
    pub chat_id: String,
    pub text: String,
    pub related_event_id: Option<String>,
    pub settle_status_on_delivery: Option<EventStatus>,
    #[serde(default)]
    pub settle_note_on_delivery: Option<String>,
}

impl TelegramTransportState {
    pub fn new() -> Self {
        Self::with_state(load_telegram_state())
    }

    fn with_state(state: TelegramState) -> Self {
        Self {
            inner: Arc::new(TelegramInner {
                state: Mutex::new(state),
                outbound_notify: Notify::new(),
                persistence_path: telegram_transport_state_path(),
            }),
        }
    }

    pub fn handle(&self) -> TelegramTransportStateHandle {
        TelegramTransportStateHandle {
            inner: self.inner.clone(),
        }
    }
}

impl TelegramTransportStateHandle {
    pub fn register_known_chat(&self, chat_id: impl Into<String>, chat_title: impl Into<String>) {
        let chat_id = chat_id.into();
        let mut state = self.inner.state.lock();
        state.ensure_chat(chat_id, chat_title.into());
        persist_telegram_state(&self.inner, &state);
    }

    pub fn observe_incoming_message(
        &self,
        chat_id: impl Into<String>,
        chat_title: impl Into<String>,
    ) {
        let chat_id = chat_id.into();
        let mut state = self.inner.state.lock();
        state.ensure_chat(chat_id, chat_title.into());
        persist_telegram_state(&self.inner, &state);
    }

    pub fn take_next_outbound(&self) -> Option<PendingOutboundMessage> {
        let mut state = self.inner.state.lock();
        let outbound = state.outbox.pop_front();
        if outbound.is_some() {
            persist_telegram_state(&self.inner, &state);
        }
        outbound
    }

    pub fn enqueue_outgoing_message(
        &self,
        chat_id: String,
        text: String,
        related_event_id: Option<String>,
        settle_status_on_delivery: Option<EventStatus>,
        settle_note_on_delivery: Option<String>,
    ) -> Result<()> {
        let mut state = self.inner.state.lock();
        state.ensure_chat(chat_id.clone(), chat_id.clone());
        let chunks = split_telegram_message_text(&text);
        let last_index = chunks.len().saturating_sub(1);
        for (index, chunk) in chunks.into_iter().enumerate() {
            let is_last = index == last_index;
            state.outbox.push_back(PendingOutboundMessage {
                local_message_id: Uuid::new_v4().to_string(),
                chat_id: chat_id.clone(),
                text: chunk,
                related_event_id: if is_last {
                    related_event_id.clone()
                } else {
                    None
                },
                settle_status_on_delivery: if is_last {
                    settle_status_on_delivery.clone()
                } else {
                    None
                },
                settle_note_on_delivery: if is_last {
                    settle_note_on_delivery.clone()
                } else {
                    None
                },
            });
        }
        persist_telegram_state_result(&self.inner, &state)?;
        self.inner.outbound_notify.notify_one();
        Ok(())
    }

    pub fn requeue_outbound_front(&self, message: PendingOutboundMessage) -> Result<()> {
        let mut state = self.inner.state.lock();
        state.outbox.push_front(message);
        persist_telegram_state_result(&self.inner, &state)?;
        self.inner.outbound_notify.notify_one();
        Ok(())
    }

    pub async fn wait_for_outbound(&self) {
        self.inner.outbound_notify.notified().await;
    }

    pub fn chat_summaries_view(&self) -> Vec<TelegramChatSummaryView> {
        let state = self.inner.state.lock();
        state
            .sorted_chat_ids()
            .into_iter()
            .filter_map(|id| state.chats.get(&id))
            .map(|chat| TelegramChatSummaryView {
                chat_id: chat.id.clone(),
                title: chat.title.clone(),
                pending_outbound_count: state
                    .outbox
                    .iter()
                    .filter(|message| message.chat_id == chat.id)
                    .count(),
            })
            .collect()
    }
}

pub(crate) fn split_telegram_message_text(text: &str) -> Vec<String> {
    if text.chars().count() <= TELEGRAM_MESSAGE_CHAR_LIMIT {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_chars = 0usize;

    for segment in text.split_inclusive('\n') {
        let segment_chars = segment.chars().count();
        if segment_chars > TELEGRAM_CHUNK_BODY_CHAR_LIMIT {
            push_non_empty_chunk(&mut chunks, &mut current, &mut current_chars);
            push_hard_wrapped_segment(&mut chunks, segment);
            continue;
        }

        if current_chars + segment_chars > TELEGRAM_CHUNK_BODY_CHAR_LIMIT {
            push_non_empty_chunk(&mut chunks, &mut current, &mut current_chars);
        }
        current.push_str(segment);
        current_chars += segment_chars;
    }
    push_non_empty_chunk(&mut chunks, &mut current, &mut current_chars);

    if chunks.is_empty() {
        chunks.push(String::new());
    }
    if chunks.len() == 1 {
        return chunks;
    }

    let total = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| format!("[{}/{}]\n{}", index + 1, total, chunk))
        .collect()
}

fn push_non_empty_chunk(chunks: &mut Vec<String>, current: &mut String, current_chars: &mut usize) {
    if current.is_empty() {
        return;
    }
    chunks.push(std::mem::take(current));
    *current_chars = 0;
}

fn push_hard_wrapped_segment(chunks: &mut Vec<String>, segment: &str) {
    let mut current = String::new();
    let mut current_chars = 0usize;
    for ch in segment.chars() {
        if current_chars == TELEGRAM_CHUNK_BODY_CHAR_LIMIT {
            chunks.push(std::mem::take(&mut current));
            current_chars = 0;
        }
        current.push(ch);
        current_chars += 1;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
}

impl TelegramState {
    fn ensure_chat(&mut self, chat_id: String, title: String) -> &mut TelegramChat {
        if !self.chats.contains_key(&chat_id) {
            self.order.push(chat_id.clone());
            self.chats.insert(
                chat_id.clone(),
                TelegramChat {
                    id: chat_id.clone(),
                    title,
                },
            );
        } else if let Some(chat) = self.chats.get_mut(&chat_id) {
            chat.title = title;
        }

        self.chats
            .get_mut(&chat_id)
            .expect("chat should exist after ensure_chat")
    }

    fn sorted_chat_ids(&self) -> Vec<String> {
        let mut ids = self.order.clone();
        ids.sort_by(|left, right| {
            let left_chat = self.chats.get(left);
            let right_chat = self.chats.get(right);
            match (left_chat, right_chat) {
                (Some(left_chat), Some(right_chat)) => {
                    let left_queued = self
                        .outbox
                        .iter()
                        .filter(|message| message.chat_id == left_chat.id)
                        .count();
                    let right_queued = self
                        .outbox
                        .iter()
                        .filter(|message| message.chat_id == right_chat.id)
                        .count();
                    right_queued
                        .cmp(&left_queued)
                        .then_with(|| left_chat.title.cmp(&right_chat.title))
                }
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => left.cmp(right),
            }
        });
        ids
    }
}

impl From<PersistedTelegramState> for TelegramState {
    fn from(value: PersistedTelegramState) -> Self {
        Self {
            order: value.order,
            chats: value
                .chats
                .into_iter()
                .map(|(id, chat)| (id, chat.into()))
                .collect(),
            outbox: value.outbox,
        }
    }
}

impl From<&TelegramState> for PersistedTelegramState {
    fn from(value: &TelegramState) -> Self {
        Self {
            order: value.order.clone(),
            chats: value
                .chats
                .iter()
                .map(|(id, chat)| (id.clone(), chat.into()))
                .collect(),
            outbox: value.outbox.clone(),
        }
    }
}

impl From<PersistedTelegramChat> for TelegramChat {
    fn from(value: PersistedTelegramChat) -> Self {
        Self {
            id: value.id,
            title: value.title,
        }
    }
}

impl From<&TelegramChat> for PersistedTelegramChat {
    fn from(value: &TelegramChat) -> Self {
        Self {
            id: value.id.clone(),
            title: value.title.clone(),
        }
    }
}

fn telegram_transport_state_path() -> PathBuf {
    PersistenceStore::runtime_sync().state_file(TELEGRAM_TRANSPORT_STATE_FILE_NAME)
}

fn load_telegram_state() -> TelegramState {
    let persisted: PersistedTelegramState = PersistenceStore::runtime_sync()
        .read_postcard_state_or_default_sync(
            TELEGRAM_TRANSPORT_STATE_FILE_NAME,
            "telegram transport state",
        );
    persisted.into()
}

fn persist_telegram_state(inner: &TelegramInner, state: &TelegramState) {
    if let Err(err) = persist_telegram_state_result(inner, state) {
        tracing::error!("persist telegram transport state failed: {err:?}");
    }
}

fn persist_telegram_state_result(inner: &TelegramInner, state: &TelegramState) -> Result<()> {
    persist_telegram_state_bytes(&inner.persistence_path, state)
}

fn persist_telegram_state_bytes(path: &Path, state: &TelegramState) -> Result<()> {
    crate::persistence::write_postcard_atomic_sync(
        path,
        &PersistedTelegramState::from(state),
        crate::persistence::PersistenceFileMode::Default,
    )
    .map_err(|err| miette!("write telegram transport state failed: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::events::EventStatus;
    use parking_lot::Mutex;
    use tokio::sync::Notify;

    #[test]
    fn short_telegram_message_is_not_chunked() {
        let chunks = split_telegram_message_text("hello");
        assert_eq!(chunks, vec!["hello".to_string()]);
    }

    #[test]
    fn long_telegram_message_is_chunked_under_api_limit() {
        let text = "x".repeat(TELEGRAM_MESSAGE_CHAR_LIMIT + 1);
        let chunks = split_telegram_message_text(&text);

        assert!(chunks.len() > 1);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.chars().count() <= TELEGRAM_MESSAGE_CHAR_LIMIT)
        );
        assert!(chunks[0].starts_with("[1/"));
    }

    #[test]
    fn long_telegram_message_chunking_respects_unicode_boundaries() {
        let text = "x".repeat(TELEGRAM_MESSAGE_CHAR_LIMIT + 1);
        let chunks = split_telegram_message_text(&text);

        assert!(chunks.len() > 1);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.chars().count() <= TELEGRAM_MESSAGE_CHAR_LIMIT)
        );
    }

    #[test]
    fn enqueue_long_outbound_settles_only_after_last_chunk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let transport = TelegramTransportState {
            inner: Arc::new(TelegramInner {
                state: Mutex::new(TelegramState::default()),
                outbound_notify: Notify::new(),
                persistence_path: dir.path().join("telegram_state"),
            }),
        };
        let handle = transport.handle();

        handle
            .enqueue_outgoing_message(
                "1".to_string(),
                "x".repeat(TELEGRAM_MESSAGE_CHAR_LIMIT + 1),
                Some("event-1".to_string()),
                Some(EventStatus::Resolved),
                Some("done".to_string()),
            )
            .expect("enqueue");

        let state = transport.inner.state.lock();
        assert!(state.outbox.len() > 1);
        let last_index = state.outbox.len() - 1;
        for (index, message) in state.outbox.iter().enumerate() {
            assert!(message.text.chars().count() <= TELEGRAM_MESSAGE_CHAR_LIMIT);
            if index == last_index {
                assert_eq!(message.related_event_id.as_deref(), Some("event-1"));
                assert_eq!(
                    message.settle_status_on_delivery,
                    Some(EventStatus::Resolved)
                );
                assert_eq!(message.settle_note_on_delivery.as_deref(), Some("done"));
            } else {
                assert!(message.related_event_id.is_none());
                assert!(message.settle_status_on_delivery.is_none());
                assert!(message.settle_note_on_delivery.is_none());
            }
        }
    }
}
