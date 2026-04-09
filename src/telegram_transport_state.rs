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

use crate::{events::EventStatus, spinova_paths::spinova_paths_sync};

const TELEGRAM_TRANSPORT_STATE_FILE_NAME: &str = "telegram_transport_state";

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
    ) -> Result<()> {
        let mut state = self.inner.state.lock();
        state.ensure_chat(chat_id.clone(), chat_id.clone());
        let local_message_id = Uuid::new_v4().to_string();
        state.outbox.push_back(PendingOutboundMessage {
            local_message_id,
            chat_id,
            text,
            related_event_id,
            settle_status_on_delivery,
        });
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
    spinova_paths_sync().state_file(TELEGRAM_TRANSPORT_STATE_FILE_NAME)
}

fn load_telegram_state() -> TelegramState {
    let path = telegram_transport_state_path();
    let Ok(bytes) = std::fs::read(&path) else {
        return TelegramState::default();
    };
    let Ok(persisted) = postcard::from_bytes::<PersistedTelegramState>(&bytes) else {
        return TelegramState::default();
    };
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
    let bytes = postcard::to_stdvec(&PersistedTelegramState::from(state))
        .map_err(|err| miette!("serialize telegram transport state failed: {err}"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| miette!("create telegram transport state dir failed: {err}"))?;
    }
    std::fs::write(path, bytes)
        .map_err(|err| miette!("write telegram transport state failed: {err}"))?;
    Ok(())
}
