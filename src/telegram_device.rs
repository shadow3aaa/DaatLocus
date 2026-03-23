use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use chrono::Utc;
use miette::{Result, bail, miette};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::device::{AttentionLevel, Device, DeviceId, DeviceStateRender, DeviceToolScope};

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
    selected_chat: Option<String>,
    order: Vec<String>,
    chats: HashMap<String, TelegramChat>,
    outbox: VecDeque<PendingOutboundMessage>,
}

#[derive(Default, Serialize, Deserialize)]
struct PersistedTelegramState {
    selected_chat: Option<String>,
    order: Vec<String>,
    chats: HashMap<String, PersistedTelegramChat>,
    outbox: VecDeque<PendingOutboundMessage>,
}

struct TelegramChat {
    id: String,
    title: String,
    unread: usize,
    pending_resolution: bool,
    needs_reply: bool,
    messages: Vec<TelegramMessage>,
}

#[derive(Serialize, Deserialize)]
struct PersistedTelegramChat {
    id: String,
    title: String,
    unread: usize,
    pending_resolution: bool,
    needs_reply: bool,
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

#[derive(Serialize, Deserialize)]
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

#[derive(Clone)]
pub struct TelegramChatSummaryView {
    pub chat_id: String,
    pub title: String,
    pub unread: usize,
    pub pending_resolution: bool,
    pub needs_reply: bool,
}

#[derive(Clone)]
pub struct TelegramMessageView {
    pub sender: String,
    pub text: String,
    pub direction: &'static str,
    pub delivery: String,
}

#[derive(Clone)]
pub struct TelegramChatView {
    pub chat_id: String,
    pub title: String,
    pub unread: usize,
    pub pending_resolution: bool,
    pub needs_reply: bool,
    pub messages: Vec<TelegramMessageView>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PendingOutboundMessage {
    pub local_message_id: String,
    pub chat_id: String,
    pub text: String,
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

    pub fn ingest_incoming_message(
        &self,
        chat_id: impl Into<String>,
        chat_title: impl Into<String>,
        sender: impl Into<String>,
        text: impl Into<String>,
    ) {
        let chat_id = chat_id.into();
        let mut state = self.inner.state.lock();
        let should_count_as_unread =
            !(state.is_focused && state.selected_chat.as_deref() == Some(chat_id.as_str()));
        let chat = state.ensure_chat(chat_id, chat_title.into());
        if should_count_as_unread {
            chat.unread += 1;
        }
        chat.pending_resolution = true;
        chat.needs_reply = true;
        chat.messages.push(TelegramMessage {
            id: Uuid::new_v4().to_string(),
            sender: sender.into(),
            text: text.into(),
            direction: MessageDirection::Incoming,
            delivery: DeliveryState::Delivered,
            timestamp_ms: Utc::now().timestamp_millis(),
        });
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

    pub fn mark_outgoing_delivered(&self, local_message_id: &str) {
        self.with_message_mut(local_message_id, |chat, message| {
            message.delivery = DeliveryState::Delivered;
            chat.needs_reply = false;
        });
    }

    pub fn mark_outgoing_failed(&self, local_message_id: &str, reason: impl Into<String>) {
        let reason = reason.into();
        self.with_message_mut(local_message_id, |chat, message| {
            message.delivery = DeliveryState::Failed(reason.clone());
            chat.needs_reply = true;
        });
    }

    pub fn chat_refs(&self) -> Vec<(String, String)> {
        let state = self.inner.state.lock();
        state
            .order
            .iter()
            .filter_map(|id| state.chats.get(id))
            .map(|chat| (chat.id.clone(), chat.title.clone()))
            .collect()
    }

    pub fn list_chat_summaries(&self) -> Vec<String> {
        let state = self.inner.state.lock();
        state
            .sorted_chat_ids()
            .into_iter()
            .filter_map(|id| state.chats.get(&id))
            .map(|chat| {
                format!(
                    "chat_id={} title={} unread={} pending_resolution={} needs_reply={}",
                    chat.id, chat.title, chat.unread, chat.pending_resolution, chat.needs_reply
                )
            })
            .collect()
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
                pending_resolution: chat.pending_resolution,
                needs_reply: chat.needs_reply,
            })
            .collect()
    }

    pub fn selected_chat_view(&self, max_messages: usize) -> Option<TelegramChatView> {
        let state = self.inner.state.lock();
        let selected_chat_id = state.selected_chat.as_ref()?;
        let chat = state.chats.get(selected_chat_id)?;
        let start = chat.messages.len().saturating_sub(max_messages);
        Some(TelegramChatView {
            chat_id: chat.id.clone(),
            title: chat.title.clone(),
            unread: chat.unread,
            pending_resolution: chat.pending_resolution,
            needs_reply: chat.needs_reply,
            messages: chat
                .messages
                .iter()
                .skip(start)
                .map(|message| TelegramMessageView {
                    sender: message.sender.clone(),
                    text: message.text.clone(),
                    direction: format_direction(&message.direction),
                    delivery: format_delivery(&message.delivery).to_string(),
                })
                .collect(),
        })
    }

    pub fn read_chat(&self, chat_id: Option<&str>, max_messages: Option<usize>) -> Result<String> {
        let state = self.inner.state.lock();
        let resolved_chat_id = match chat_id {
            Some(chat_id) => chat_id.to_string(),
            None => state
                .selected_chat
                .clone()
                .ok_or_else(|| miette!("no telegram chat selected"))?,
        };
        let chat = state
            .chats
            .get(&resolved_chat_id)
            .ok_or_else(|| miette!("unknown telegram chat: {resolved_chat_id}"))?;
        let max_messages = max_messages.unwrap_or(20);
        let latest_message = chat.messages.last();
        let latest_incoming = chat
            .messages
            .iter()
            .rev()
            .find(|message| matches!(message.direction, MessageDirection::Incoming));
        let latest_outgoing = chat
            .messages
            .iter()
            .rev()
            .find(|message| matches!(message.direction, MessageDirection::Outgoing));
        let mut lines = vec![
            format!("chat_id={}", chat.id),
            format!("title={}", chat.title),
            format!("unread={}", chat.unread),
            format!("pending_resolution={}", chat.pending_resolution),
            format!("needs_reply={}", chat.needs_reply),
            format!(
                "latest_message_direction={}",
                latest_message
                    .map(|message| format_direction(&message.direction))
                    .unwrap_or("none")
            ),
            format!(
                "latest_message_sender={}",
                latest_message
                    .map(|message| message.sender.as_str())
                    .unwrap_or("none")
            ),
            format!(
                "latest_incoming_sender={}",
                latest_incoming
                    .map(|message| message.sender.as_str())
                    .unwrap_or("none")
            ),
            format!(
                "latest_incoming_text={}",
                latest_incoming
                    .map(|message| message.text.as_str())
                    .unwrap_or("<none>")
            ),
            format!(
                "latest_outgoing_text={}",
                latest_outgoing
                    .map(|message| message.text.as_str())
                    .unwrap_or("<none>")
            ),
            "messages=".to_string(),
        ];
        let start = chat.messages.len().saturating_sub(max_messages);
        for message in chat.messages.iter().skip(start) {
            lines.push(format!(
                "- [{}|{}] {}: {}",
                format_direction(&message.direction),
                format_delivery(&message.delivery),
                message.sender,
                message.text
            ));
        }
        Ok(lines.join("\n"))
    }

    pub fn resolve_chat(&self, chat_id: &str, needs_reply: Option<bool>) -> Result<()> {
        let mut state = self.inner.state.lock();
        let Some(chat) = state.chats.get_mut(chat_id) else {
            return Err(miette!("unknown telegram chat: {chat_id}"));
        };
        chat.pending_resolution = false;
        if let Some(needs_reply) = needs_reply {
            chat.needs_reply = needs_reply;
        }
        persist_telegram_state(&self.inner, &state);
        Ok(())
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

impl TelegramDevice {
    pub async fn select_chat(&mut self, chat_id: String) -> Result<()> {
        let mut state = self.inner.state.lock();
        if !state.chats.contains_key(&chat_id) {
            bail!("unknown telegram chat: {chat_id}");
        }
        state.selected_chat = Some(chat_id.clone());
        if let Some(chat) = state.chats.get_mut(&chat_id) {
            chat.unread = 0;
        }
        persist_telegram_state_result(&self.inner, &state)
    }

    pub async fn send_message(&mut self, text: String) -> Result<()> {
        let mut state = self.inner.state.lock();
        let Some(selected_chat) = state.selected_chat.clone() else {
            bail!("no telegram chat selected");
        };
        let Some(chat) = state.chats.get_mut(&selected_chat) else {
            bail!("selected telegram chat missing: {selected_chat}");
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
        chat.needs_reply = false;
        state.outbox.push_back(PendingOutboundMessage {
            local_message_id,
            chat_id: selected_chat,
            text,
        });
        persist_telegram_state_result(&self.inner, &state)
    }
}

#[async_trait]
impl Device for TelegramDevice {
    fn id(&self) -> DeviceId {
        DeviceId::Telegram
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn render_state(&self, is_focused: bool) -> DeviceStateRender {
        let state = self.inner.state.lock();
        let pending_resolution = state
            .chats
            .values()
            .filter(|chat| chat.pending_resolution)
            .count();
        let pending_reply = state.chats.values().filter(|chat| chat.needs_reply).count();
        let unread_messages = state.chats.values().map(|chat| chat.unread).sum::<usize>();
        let selected_chat = state
            .selected_chat
            .as_deref()
            .and_then(|id| state.chats.get(id))
            .map(|chat| format!("selected_chat={} ({})", chat.id, chat.title))
            .unwrap_or_else(|| "selected_chat=none".to_string());
        let attention = if pending_resolution > 0 || pending_reply > 0 {
            AttentionLevel::Notice
        } else {
            AttentionLevel::Quiet
        };
        DeviceStateRender {
            title: "Telegram".to_string(),
            lines: vec![
                format!("focused={is_focused}"),
                "kind=telegram".to_string(),
                selected_chat,
                format!("known_chats={}", state.chats.len()),
                format!("pending_resolution={pending_resolution}"),
                format!("pending_reply={pending_reply}"),
                format!("unread_messages={unread_messages}"),
            ],
            attention,
            is_focused,
        }
    }

    fn focused_tool_scopes(&self) -> &'static [DeviceToolScope] {
        &[DeviceToolScope::Telegram]
    }

    async fn on_focus(&mut self) -> Result<()> {
        let mut state = self.inner.state.lock();
        state.is_focused = true;
        Ok(())
    }

    async fn on_blur(&mut self) -> Result<()> {
        let mut state = self.inner.state.lock();
        state.is_focused = false;
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<()> {
        let state = self.inner.state.lock();
        persist_telegram_state_result(&self.inner, &state)
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
                    unread: 0,
                    pending_resolution: false,
                    needs_reply: false,
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

    fn sanitize_selected_chat(&mut self) {
        let is_valid = self
            .selected_chat
            .as_deref()
            .and_then(|id| self.chats.get(id))
            .is_some();
        if !is_valid {
            self.selected_chat = None;
        }
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
        self.messages
            .last()
            .map(|message| message.timestamp_ms)
            .unwrap_or(0)
    }

    fn priority_tuple(&self) -> (u8, u8, u8, i64) {
        (
            self.pending_resolution as u8,
            self.needs_reply as u8,
            (self.unread > 0) as u8,
            self.latest_activity_ms(),
        )
    }
}

impl From<PersistedTelegramState> for TelegramState {
    fn from(value: PersistedTelegramState) -> Self {
        let mut state = Self {
            is_focused: false,
            selected_chat: value.selected_chat,
            order: value.order,
            chats: value
                .chats
                .into_iter()
                .map(|(id, chat)| (id, chat.into()))
                .collect(),
            outbox: value.outbox,
        };
        state.sanitize_selected_chat();
        state
    }
}

impl From<&TelegramState> for PersistedTelegramState {
    fn from(value: &TelegramState) -> Self {
        Self {
            selected_chat: value.selected_chat.clone(),
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
            unread: value.unread,
            pending_resolution: value.pending_resolution,
            needs_reply: value.needs_reply,
            messages: value.messages.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<&TelegramChat> for PersistedTelegramChat {
    fn from(value: &TelegramChat) -> Self {
        Self {
            id: value.id.clone(),
            title: value.title.clone(),
            unread: value.unread,
            pending_resolution: value.pending_resolution,
            needs_reply: value.needs_reply,
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

fn format_direction(direction: &MessageDirection) -> &'static str {
    match direction {
        MessageDirection::Incoming => "incoming",
        MessageDirection::Outgoing => "outgoing",
    }
}

fn format_delivery(delivery: &DeliveryState) -> String {
    match delivery {
        DeliveryState::PendingTransport => "pending_transport".to_string(),
        DeliveryState::Delivered => "delivered".to_string(),
        DeliveryState::Failed(reason) => format!("failed:{reason}"),
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
        eprintln!("persist telegram device state failed: {err:?}");
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
