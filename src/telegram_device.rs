use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use chrono::{Local, TimeZone, Utc};
use miette::{Result, bail, miette};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::device::{
    AttentionLevel, Device, DeviceAction, DeviceId, FocusedRender, PeripheralRender,
};

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
    background_attention: Option<BackgroundAttention>,
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

struct BackgroundAttention {
    summary: String,
}

#[derive(Clone)]
pub struct TelegramDeviceHandle {
    inner: Arc<TelegramInner>,
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
        state.background_attention = None;
        state.refresh_background_attention();
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
        state.refresh_background_attention();
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

    pub fn pending_resolution_refs(&self) -> Vec<(String, String)> {
        let state = self.inner.state.lock();
        state
            .order
            .iter()
            .filter_map(|id| state.chats.get(id))
            .filter(|chat| chat.pending_resolution)
            .map(|chat| (chat.id.clone(), chat.title.clone()))
            .collect()
    }

    pub fn has_pending_resolution(&self) -> bool {
        let state = self.inner.state.lock();
        state.chats.values().any(|chat| chat.pending_resolution)
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
        state.refresh_background_attention();
        persist_telegram_state(&self.inner, &state);
        Ok(())
    }

    pub fn selected_chat_memory_evidence(&self) -> Vec<String> {
        let state = self.inner.state.lock();
        let Some(selected_chat) = state.selected_chat.as_deref() else {
            return Vec::new();
        };
        let Some(chat) = state.chats.get(selected_chat) else {
            return Vec::new();
        };

        let mut evidence = vec![
            format!("当前 Telegram 会话：{} ({})", chat.title, chat.id),
            format!("会话待判断：{}", yes_no(chat.pending_resolution)),
            format!("会话待回复：{}", yes_no(chat.needs_reply)),
        ];

        for message in chat
            .messages
            .iter()
            .rev()
            .take(3)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            evidence.push(format!(
                "会话消息 / {} / {}: {}",
                match message.direction {
                    MessageDirection::Incoming => "incoming",
                    MessageDirection::Outgoing => "outgoing",
                },
                message.sender,
                message.text
            ));
        }

        evidence
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
        state.refresh_background_attention();
        persist_telegram_state(&self.inner, &state);
    }
}

#[async_trait]
impl Device for TelegramDevice {
    fn id(&self) -> DeviceId {
        DeviceId::Telegram
    }

    fn render_peripheral(&self, is_focused: bool) -> PeripheralRender {
        let state = self.inner.state.lock();
        let resolution_chats = state
            .chats
            .values()
            .filter(|chat| chat.pending_resolution)
            .count();
        let reply_chats = state.chats.values().filter(|chat| chat.needs_reply).count();
        let unread_chats = state.chats.values().filter(|chat| chat.unread > 0).count();
        let unread_messages = state.chats.values().map(|chat| chat.unread).sum::<usize>();

        let (attention, summary) = if is_focused {
            let focus = state
                .selected_chat
                .as_deref()
                .and_then(|id| state.chats.get(id))
                .map(|chat| chat.title.as_str())
                .unwrap_or("未打开会话");
            (
                AttentionLevel::Quiet,
                format!(
                    "设备在前景，当前会话：{focus}。共有 {unread_messages} 条未读消息分布在 {unread_chats} 个会话中，另有 {resolution_chats} 个会话待判断，{reply_chats} 个会话仍待回复。"
                ),
            )
        } else if let Some(attention) = &state.background_attention {
            (AttentionLevel::Notice, attention.summary.clone())
        } else if resolution_chats > 0 {
            (
                AttentionLevel::Notice,
                pending_resolution_summary(&state, resolution_chats),
            )
        } else if reply_chats > 0 {
            (
                AttentionLevel::Notice,
                pending_reply_summary(&state, reply_chats),
            )
        } else {
            (
                AttentionLevel::Quiet,
                "设备在后台，没有外围提醒。".to_string(),
            )
        };

        PeripheralRender {
            title: "Telegram".to_string(),
            summary,
            attention,
            is_focused,
            interactive: true,
        }
    }

    fn render_focused(&self) -> FocusedRender {
        let state = self.inner.state.lock();
        FocusedRender {
            title: "Telegram".to_string(),
            content: render_telegram_view(&state),
            interactive: true,
        }
    }

    async fn on_focus(&mut self) -> Result<()> {
        let mut state = self.inner.state.lock();
        state.is_focused = true;
        state.background_attention = None;
        Ok(())
    }

    async fn on_blur(&mut self) -> Result<()> {
        let mut state = self.inner.state.lock();
        state.is_focused = false;
        state.refresh_background_attention();
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<()> {
        let state = self.inner.state.lock();
        persist_telegram_state_result(&self.inner, &state)
    }

    fn requires_attention(&self) -> bool {
        let state = self.inner.state.lock();
        state.background_attention.is_some()
            || state
                .chats
                .values()
                .any(|chat| chat.pending_resolution || chat.needs_reply)
    }

    async fn execute(&mut self, action: DeviceAction) -> Result<()> {
        let mut state = self.inner.state.lock();
        match action {
            DeviceAction::TelegramSelectChat { chat_id } => {
                if !state.chats.contains_key(&chat_id) {
                    bail!("unknown telegram chat: {chat_id}");
                }
                state.selected_chat = Some(chat_id.clone());
                if let Some(chat) = state.chats.get_mut(&chat_id) {
                    chat.unread = 0;
                }
                state.refresh_background_attention();
                persist_telegram_state_result(&self.inner, &state)
            }
            DeviceAction::TelegramSendMessage { text } => {
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
                state.refresh_background_attention();
                persist_telegram_state_result(&self.inner, &state)
            }
            DeviceAction::TerminalInput { .. } => {
                bail!("terminal action is not supported by Telegram")
            }
        }
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

    fn refresh_background_attention(&mut self) {
        if self.is_focused {
            self.background_attention = None;
            return;
        }

        let unread_chats = self.chats.values().filter(|chat| chat.unread > 0).count();
        let unread_messages = self.chats.values().map(|chat| chat.unread).sum::<usize>();
        if unread_messages == 0 {
            self.background_attention = None;
            return;
        }

        let Some(chat) = self
            .order
            .iter()
            .rev()
            .filter_map(|id| self.chats.get(id))
            .find(|chat| chat.unread > 0)
        else {
            self.background_attention = None;
            return;
        };

        let preview = chat
            .messages
            .last()
            .map(|message| truncate_preview(message.text.trim(), 48))
            .unwrap_or_else(|| "暂无预览".to_string());

        let summary = if unread_chats == 1 {
            if chat.pending_resolution {
                format!(
                    "Telegram 在后台：{} 发来 {} 条新消息，请尽快查看并判断如何处理。最近一条：{}",
                    chat.title, unread_messages, preview
                )
            } else {
                format!(
                    "Telegram 在后台：{} 发来 {} 条新消息，请尽快查看并回复。最近一条：{}",
                    chat.title, unread_messages, preview
                )
            }
        } else {
            format!(
                "Telegram 在后台：共有 {unread_messages} 条新消息，涉及 {unread_chats} 个会话，请尽快查看并判断如何处理。最新活跃会话是 {}。",
                chat.title
            )
        };

        self.background_attention = Some(BackgroundAttention { summary });
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
            background_attention: None,
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

fn render_telegram_view(state: &TelegramState) -> String {
    let mut sections = Vec::new();
    let sorted_chat_ids = state.sorted_chat_ids();

    if sorted_chat_ids.is_empty() {
        sections.push(
            "当前没有任何会话。\n如果未来接入 transport，这里会展示聊天列表与未读状态。"
                .to_string(),
        );
    } else {
        let chat_overview = sorted_chat_ids
            .iter()
            .filter_map(|id| state.chats.get(id))
            .map(|chat| {
                render_chat_summary(
                    chat,
                    state.selected_chat.as_deref() == Some(chat.id.as_str()),
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        sections.push(format!(
            "聊天列表页（按 待判断 > 待回复 > 未读 > 最近活跃 排序）：\n{chat_overview}"
        ));
    }

    match state
        .selected_chat
        .as_deref()
        .and_then(|chat_id| state.chats.get(chat_id))
    {
        Some(chat) => sections.push(render_selected_chat(chat)),
        None => sections.push(
            "当前没有打开任何会话。\n如果要查看某个会话，请使用 `DeviceAction` -> `TelegramSelectChat`。".to_string(),
        ),
    }

    sections.push(
        "如果要发送消息，请使用 `DeviceAction` -> `TelegramSendMessage`。\n如果会话显示“待判断：是”，优先使用高阶动作去判断其语义；如果只剩“待回复：是”，说明判断已经做完，但消息还需要发送或补发。"
            .to_string(),
    );

    sections.join("\n\n")
}

fn render_chat_summary(chat: &TelegramChat, is_selected: bool) -> String {
    let latest = chat
        .messages
        .last()
        .map(|message| truncate_preview(message.text.trim(), 48))
        .unwrap_or_else(|| "暂无消息".to_string());
    let marker = if is_selected { ">" } else { " " };
    format!(
        "{marker} {} ({}) | 未读={} | 待判断={} | 待回复={} | 最近消息={}",
        chat.title,
        chat.id,
        chat.unread,
        yes_no(chat.pending_resolution),
        yes_no(chat.needs_reply),
        latest
    )
}

fn render_selected_chat(chat: &TelegramChat) -> String {
    let messages = if chat.messages.is_empty() {
        "暂无消息。".to_string()
    } else {
        chat.messages
            .iter()
            .map(render_message)
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "当前会话页：{} ({})\n待判断：{}\n待回复：{}\n--- 消息 ---\n{}\n--- 消息 ---",
        chat.title,
        chat.id,
        yes_no(chat.pending_resolution),
        yes_no(chat.needs_reply),
        messages
    )
}

fn render_message(message: &TelegramMessage) -> String {
    let timestamp = Local
        .timestamp_millis_opt(message.timestamp_ms)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "invalid_timestamp".to_string());
    let direction = match message.direction {
        MessageDirection::Incoming => "incoming",
        MessageDirection::Outgoing => "outgoing",
    };
    let delivery = match &message.delivery {
        DeliveryState::Delivered => "delivered".to_string(),
        DeliveryState::PendingTransport => "pending_transport".to_string(),
        DeliveryState::Failed(reason) => format!("failed({})", truncate_preview(reason, 32)),
    };
    format!(
        "[{timestamp}] {} / {direction} / {delivery} / {}: {}",
        message.id, message.sender, message.text
    )
}

fn truncate_preview(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let preview = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{preview}...")
    } else {
        preview
    }
}

fn pending_resolution_summary(state: &TelegramState, resolution_chats: usize) -> String {
    let Some(chat) = state
        .order
        .iter()
        .rev()
        .filter_map(|id| state.chats.get(id))
        .find(|chat| chat.pending_resolution)
    else {
        return "Telegram 在后台：有会话需要你判断如何处理。".to_string();
    };

    if resolution_chats == 1 {
        let preview = chat
            .messages
            .last()
            .map(|message| truncate_preview(message.text.trim(), 48))
            .unwrap_or_else(|| "暂无预览".to_string());
        format!(
            "Telegram 在后台：{} 发来新消息，等待你判断如何处理。最近一条：{}",
            chat.title, preview
        )
    } else {
        format!(
            "Telegram 在后台：共有 {resolution_chats} 个会话有新消息等待你判断如何处理。最新活跃会话是 {}。",
            chat.title
        )
    }
}

fn pending_reply_summary(state: &TelegramState, reply_chats: usize) -> String {
    let Some(chat) = state
        .order
        .iter()
        .rev()
        .filter_map(|id| state.chats.get(id))
        .find(|chat| chat.needs_reply)
    else {
        return "Telegram 在后台：有会话待回复，请尽快处理。".to_string();
    };

    if reply_chats == 1 {
        let preview = chat
            .messages
            .last()
            .map(|message| truncate_preview(message.text.trim(), 48))
            .unwrap_or_else(|| "暂无预览".to_string());
        format!(
            "Telegram 在后台：{} 仍在等待你的回复，请尽快返回处理。最近一条：{}",
            chat.title, preview
        )
    } else {
        format!(
            "Telegram 在后台：共有 {reply_chats} 个会话仍在等待你的回复，请优先处理。最新活跃会话是 {}。",
            chat.title
        )
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "是" } else { "否" }
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
