use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::Utc;
use miette::{Result, bail, miette};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::events::EventStatus;

const TELEGRAM_DEVICE_FILE_NAME: &str = "telegram_device.json";

pub struct TelegramDevice {
    inner: Arc<TelegramInner>,
}

struct TelegramInner {
    state: Mutex<TelegramState>,
    persistence_path: PathBuf,
}

#[derive(Default)]
struct TelegramState {
    is_focused: bool,
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
    unread: usize,
    latest_incoming_sender: Option<String>,
    latest_incoming_preview: Option<String>,
    latest_incoming_at_ms: i64,
    messages: Vec<TelegramMessage>,
}

#[derive(Serialize, Deserialize)]
struct PersistedTelegramChat {
    id: String,
    title: String,
    unread: usize,
    #[serde(default)]
    latest_incoming_sender: Option<String>,
    #[serde(default)]
    latest_incoming_preview: Option<String>,
    #[serde(default)]
    latest_incoming_at_ms: i64,
    messages: Vec<PersistedTelegramMessage>,
}

struct TelegramMessage {
    id: String,
    sender: String,
    text: String,
    direction: MessageDirection,
    delivery: DeliveryState,
    timestamp_ms: i64,
}

#[derive(Serialize, Deserialize)]
struct PersistedTelegramMessage {
    id: String,
    sender: String,
    text: String,
    direction: MessageDirection,
    delivery: DeliveryState,
    timestamp_ms: i64,
}

#[derive(Clone, Serialize, Deserialize)]
enum MessageDirection {
    Incoming,
    Outgoing,
}

#[derive(Serialize, Deserialize)]
enum DeliveryState {
    Delivered,
    PendingTransport,
    Failed(String),
}

#[derive(Clone)]
pub struct TelegramDeviceHandle {
    inner: Arc<TelegramInner>,
}

#[derive(Clone, Serialize)]
pub struct TelegramChatSummaryView {
    pub chat_id: String,
    pub title: String,
    pub unread: usize,
    pub last_activity_at_ms: i64,
    pub latest_incoming_preview: Option<String>,
    pub latest_outgoing_preview: Option<String>,
    pub pending_outbound_count: usize,
    pub failed_delivery_count: usize,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PendingOutboundMessage {
    pub local_message_id: String,
    pub chat_id: String,
    pub text: String,
    pub related_event_id: Option<String>,
    pub settle_status_on_delivery: Option<EventStatus>,
}

impl TelegramDevice {
    pub fn new() -> Self {
        Self::with_state(load_telegram_state())
    }

    pub fn empty() -> Self {
        Self::with_state(TelegramState::default())
    }

    fn with_state(mut state: TelegramState) -> Self {
        state.is_focused = false;
        Self {
            inner: Arc::new(TelegramInner {
                state: Mutex::new(state),
                persistence_path: telegram_device_state_path(),
            }),
        }
    }

    pub fn handle(&self) -> TelegramDeviceHandle {
        TelegramDeviceHandle {
            inner: self.inner.clone(),
        }
    }
}

impl TelegramDeviceHandle {
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
        sender: impl Into<String>,
        text: impl Into<String>,
        timestamp_ms: i64,
    ) {
        let chat_id = chat_id.into();
        let mut state = self.inner.state.lock();
        let chat = state.ensure_chat(chat_id, chat_title.into());
        chat.unread += 1;
        if timestamp_ms >= chat.latest_incoming_at_ms {
            chat.latest_incoming_sender = Some(sender.into());
            chat.latest_incoming_preview = Some(text.into());
            chat.latest_incoming_at_ms = timestamp_ms;
        }
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
        let Some(chat) = state.chats.get_mut(&chat_id) else {
            bail!("unknown telegram chat: {chat_id}");
        };
        let local_message_id = Uuid::new_v4().to_string();
        chat.messages.push(TelegramMessage {
            id: local_message_id.clone(),
            sender: "Spinova".to_string(),
            text: text.clone(),
            direction: MessageDirection::Outgoing,
            delivery: DeliveryState::PendingTransport,
            timestamp_ms: Utc::now().timestamp_millis(),
        });
        state.outbox.push_back(PendingOutboundMessage {
            local_message_id,
            chat_id,
            text,
            related_event_id,
            settle_status_on_delivery,
        });
        persist_telegram_state_result(&self.inner, &state)
    }

    pub fn mark_outgoing_delivered(&self, local_message_id: &str) {
        self.with_message_mut(local_message_id, |_, message| {
            message.delivery = DeliveryState::Delivered;
        });
    }

    pub fn mark_outgoing_failed(&self, local_message_id: &str, reason: impl Into<String>) {
        let reason = reason.into();
        self.with_message_mut(local_message_id, |_, message| {
            message.delivery = DeliveryState::Failed(reason.clone());
        });
    }

    pub fn latest_outgoing_preview(&self, chat_id: &str) -> Option<String> {
        let state = self.inner.state.lock();
        state.chats.get(chat_id).and_then(|chat| {
            chat.messages
                .iter()
                .rev()
                .find(|message| matches!(message.direction, MessageDirection::Outgoing))
                .map(|message| message.text.clone())
        })
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
                unread: chat.unread,
                last_activity_at_ms: chat.latest_activity_ms(),
                latest_incoming_preview: chat.latest_incoming_preview(),
                latest_outgoing_preview: chat.latest_outgoing_preview(),
                pending_outbound_count: chat.pending_outbound_count(),
                failed_delivery_count: chat.failed_delivery_count(),
            })
            .collect()
    }

    fn with_message_mut(
        &self,
        local_message_id: &str,
        f: impl FnOnce(&mut TelegramChat, &mut TelegramMessage),
    ) {
        let mut state = self.inner.state.lock();
        for chat in state.chats.values_mut() {
            if let Some(index) = chat
                .messages
                .iter()
                .position(|message| message.id == local_message_id)
            {
                let mut message = chat.messages.remove(index);
                f(chat, &mut message);
                chat.messages.insert(index, message);
                break;
            }
        }
        persist_telegram_state(&self.inner, &state);
    }
}

impl TelegramDevice {}

impl TelegramState {
    fn ensure_chat(&mut self, chat_id: String, title: String) -> &mut TelegramChat {
        if !self.chats.contains_key(&chat_id) {
            self.order.push(chat_id.clone());
            self.chats.insert(
                chat_id.clone(),
                TelegramChat {
                    id: chat_id.clone(),
                    title,
                    unread: 0,
                    latest_incoming_sender: None,
                    latest_incoming_preview: None,
                    latest_incoming_at_ms: 0,
                    messages: Vec::new(),
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
                (Some(left_chat), Some(right_chat)) => right_chat
                    .priority_tuple()
                    .cmp(&left_chat.priority_tuple())
                    .then_with(|| left_chat.title.cmp(&right_chat.title)),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => left.cmp(right),
            }
        });
        ids
    }
}

impl TelegramChat {
    fn latest_activity_ms(&self) -> i64 {
        self.latest_incoming_at_ms.max(
            self.messages
                .last()
                .map(|message| message.timestamp_ms)
                .unwrap_or(0),
        )
    }

    fn latest_incoming_preview(&self) -> Option<String> {
        self.latest_incoming_preview.clone()
    }

    fn latest_outgoing_preview(&self) -> Option<String> {
        self.messages
            .iter()
            .rev()
            .find(|message| matches!(message.direction, MessageDirection::Outgoing))
            .map(|message| message.text.clone())
    }

    fn pending_outbound_count(&self) -> usize {
        self.messages
            .iter()
            .filter(|message| matches!(message.delivery, DeliveryState::PendingTransport))
            .count()
    }

    fn failed_delivery_count(&self) -> usize {
        self.messages
            .iter()
            .filter(|message| matches!(message.delivery, DeliveryState::Failed(_)))
            .count()
    }

    fn priority_tuple(&self) -> (usize, usize, usize, i64) {
        (
            self.failed_delivery_count(),
            self.pending_outbound_count(),
            self.unread,
            self.latest_activity_ms(),
        )
    }
}

impl TelegramState {}

impl From<PersistedTelegramState> for TelegramState {
    fn from(value: PersistedTelegramState) -> Self {
        Self {
            is_focused: false,
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
        let mut latest_incoming_sender = value.latest_incoming_sender;
        let mut latest_incoming_preview = value.latest_incoming_preview;
        let mut latest_incoming_at_ms = value.latest_incoming_at_ms;
        let mut messages = Vec::new();
        for message in value.messages {
            match message.direction.clone() {
                MessageDirection::Incoming => {
                    if message.timestamp_ms >= latest_incoming_at_ms {
                        latest_incoming_sender = Some(message.sender);
                        latest_incoming_preview = Some(message.text);
                        latest_incoming_at_ms = message.timestamp_ms;
                    }
                }
                MessageDirection::Outgoing => messages.push(message.into()),
            }
        }
        Self {
            id: value.id,
            title: value.title,
            unread: value.unread,
            latest_incoming_sender,
            latest_incoming_preview,
            latest_incoming_at_ms,
            messages,
        }
    }
}

impl From<&TelegramChat> for PersistedTelegramChat {
    fn from(value: &TelegramChat) -> Self {
        Self {
            id: value.id.clone(),
            title: value.title.clone(),
            unread: value.unread,
            latest_incoming_sender: value.latest_incoming_sender.clone(),
            latest_incoming_preview: value.latest_incoming_preview.clone(),
            latest_incoming_at_ms: value.latest_incoming_at_ms,
            messages: value.messages.iter().map(Into::into).collect(),
        }
    }
}

impl From<PersistedTelegramMessage> for TelegramMessage {
    fn from(value: PersistedTelegramMessage) -> Self {
        Self {
            id: value.id,
            sender: value.sender,
            text: value.text,
            direction: value.direction,
            delivery: value.delivery,
            timestamp_ms: value.timestamp_ms,
        }
    }
}

impl From<&TelegramMessage> for PersistedTelegramMessage {
    fn from(value: &TelegramMessage) -> Self {
        Self {
            id: value.id.clone(),
            sender: value.sender.clone(),
            text: value.text.clone(),
            direction: match value.direction {
                MessageDirection::Incoming => MessageDirection::Incoming,
                MessageDirection::Outgoing => MessageDirection::Outgoing,
            },
            delivery: match &value.delivery {
                DeliveryState::Delivered => DeliveryState::Delivered,
                DeliveryState::PendingTransport => DeliveryState::PendingTransport,
                DeliveryState::Failed(reason) => DeliveryState::Failed(reason.clone()),
            },
            timestamp_ms: value.timestamp_ms,
        }
    }
}

fn telegram_device_state_path() -> PathBuf {
    spinova_home_sync().join(TELEGRAM_DEVICE_FILE_NAME)
}

fn spinova_home_sync() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let path = home.join(".spinova");
    let _ = std::fs::create_dir_all(&path);
    path
}

fn load_telegram_state() -> TelegramState {
    let path = telegram_device_state_path();
    let Ok(bytes) = std::fs::read(&path) else {
        return TelegramState::default();
    };
    let Ok(state) = serde_json::from_slice::<PersistedTelegramState>(&bytes) else {
        return TelegramState::default();
    };
    state.into()
}

fn persist_telegram_state(inner: &TelegramInner, state: &TelegramState) {
    if let Err(err) = persist_telegram_state_result(inner, state) {
        tracing::error!("persist telegram device state failed: {err:?}");
    }
}

fn persist_telegram_state_result(inner: &TelegramInner, state: &TelegramState) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(&PersistedTelegramState::from(state))
        .map_err(|err| miette!("serialize telegram device state failed: {err}"))?;
    if let Some(parent) = Path::new(&inner.persistence_path).parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| miette!("create telegram device dir failed: {err}"))?;
    }
    std::fs::write(&inner.persistence_path, bytes)
        .map_err(|err| miette!("write telegram device state failed: {err}"))?;
    Ok(())
}
