use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use async_trait::async_trait;
use chrono::{DateTime, Local, Utc};
use miette::{Result, bail};
use parking_lot::Mutex;
use uuid::Uuid;

use crate::device::{
    AttentionLevel, Device, DeviceAction, DeviceId, FocusedRender, PeripheralRender,
};

pub struct TelegramDevice {
    state: Arc<Mutex<TelegramState>>,
}

#[derive(Default)]
struct TelegramState {
    is_focused: bool,
    selected_chat: Option<String>,
    order: Vec<String>,
    chats: HashMap<String, TelegramChat>,
    outbox: VecDeque<PendingOutboundMessage>,
}

struct TelegramChat {
    id: String,
    title: String,
    unread: usize,
    messages: Vec<TelegramMessage>,
}

struct TelegramMessage {
    id: String,
    sender: String,
    text: String,
    direction: MessageDirection,
    delivery: DeliveryState,
    timestamp: DateTime<Utc>,
}

enum MessageDirection {
    Incoming,
    Outgoing,
}

enum DeliveryState {
    Delivered,
    PendingTransport,
    Failed(String),
}

#[derive(Clone)]
pub struct TelegramDeviceHandle {
    state: Arc<Mutex<TelegramState>>,
}

#[derive(Clone)]
pub struct PendingOutboundMessage {
    pub local_message_id: String,
    pub chat_id: String,
    pub text: String,
}

impl TelegramDevice {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(TelegramState::default())),
        }
    }

    pub fn handle(&self) -> TelegramDeviceHandle {
        TelegramDeviceHandle {
            state: self.state.clone(),
        }
    }
}

impl TelegramDeviceHandle {
    pub fn ingest_incoming_message(
        &self,
        chat_id: impl Into<String>,
        chat_title: impl Into<String>,
        sender: impl Into<String>,
        text: impl Into<String>,
    ) {
        let chat_id = chat_id.into();
        let mut state = self.state.lock();
        let should_count_as_unread = !(state.is_focused
            && state.selected_chat.as_deref() == Some(chat_id.as_str()));
        let chat = state.ensure_chat(chat_id, chat_title.into());
        if should_count_as_unread {
            chat.unread += 1;
        }
        chat.messages.push(TelegramMessage {
            id: Uuid::new_v4().to_string(),
            sender: sender.into(),
            text: text.into(),
            direction: MessageDirection::Incoming,
            delivery: DeliveryState::Delivered,
            timestamp: Utc::now(),
        });
    }

    pub fn take_next_outbound(&self) -> Option<PendingOutboundMessage> {
        self.state.lock().outbox.pop_front()
    }

    pub fn mark_outgoing_delivered(&self, local_message_id: &str) {
        self.with_message_mut(local_message_id, |message| {
            message.delivery = DeliveryState::Delivered;
        });
    }

    pub fn mark_outgoing_failed(&self, local_message_id: &str, reason: impl Into<String>) {
        let reason = reason.into();
        self.with_message_mut(local_message_id, |message| {
            message.delivery = DeliveryState::Failed(reason.clone());
        });
    }

    fn with_message_mut(
        &self,
        local_message_id: &str,
        f: impl FnOnce(&mut TelegramMessage),
    ) {
        let mut state = self.state.lock();
        for chat in state.chats.values_mut() {
            if let Some(message) = chat.messages.iter_mut().find(|msg| msg.id == local_message_id) {
                f(message);
                break;
            }
        }
    }
}

#[async_trait]
impl Device for TelegramDevice {
    fn id(&self) -> DeviceId {
        DeviceId::Telegram
    }

    fn render_peripheral(&self, is_focused: bool) -> PeripheralRender {
        let state = self.state.lock();
        let unread_chats = state.chats.values().filter(|chat| chat.unread > 0).count();
        let unread_messages = state.chats.values().map(|chat| chat.unread).sum::<usize>();
        let latest_unread_chat = state
            .order
            .iter()
            .rev()
            .filter_map(|id| state.chats.get(id))
            .find(|chat| chat.unread > 0);

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
                    "设备在前景，当前会话：{focus}。共有 {unread_messages} 条未读消息分布在 {unread_chats} 个会话中。"
                ),
            )
        } else if unread_messages > 0 {
            let summary = match latest_unread_chat {
                Some(chat) if unread_chats == 1 => {
                    let preview = chat
                        .messages
                        .last()
                        .map(|message| truncate_preview(message.text.trim(), 48))
                        .unwrap_or_else(|| "暂无预览".to_string());
                    format!(
                        "Telegram 在后台：{} 发来 {} 条新消息，请尽快查看并回复。最近一条：{}",
                        chat.title, unread_messages, preview
                    )
                }
                Some(chat) => format!(
                    "Telegram 在后台：共有 {unread_messages} 条新消息，涉及 {unread_chats} 个会话，请尽快处理。最新活跃会话是 {}。",
                    chat.title
                ),
                None => format!(
                    "Telegram 在后台：共有 {unread_messages} 条新消息，涉及 {unread_chats} 个会话，请尽快处理。"
                ),
            };
            (AttentionLevel::Notice, summary)
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
        let state = self.state.lock();
        let content = render_telegram_view(&state);
        FocusedRender {
            title: "Telegram".to_string(),
            content,
            interactive: true,
        }
    }

    async fn on_focus(&mut self) -> Result<()> {
        self.state.lock().is_focused = true;
        Ok(())
    }

    async fn on_blur(&mut self) -> Result<()> {
        self.state.lock().is_focused = false;
        Ok(())
    }

    async fn execute(&mut self, action: DeviceAction) -> Result<()> {
        let mut state = self.state.lock();
        match action {
            DeviceAction::TelegramSelectChat { chat_id } => {
                if !state.chats.contains_key(&chat_id) {
                    bail!("unknown telegram chat: {chat_id}");
                }
                state.selected_chat = Some(chat_id.clone());
                if let Some(chat) = state.chats.get_mut(&chat_id) {
                    chat.unread = 0;
                }
                Ok(())
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
                    timestamp: Utc::now(),
                });
                state.outbox.push_back(PendingOutboundMessage {
                    local_message_id,
                    chat_id: selected_chat,
                    text,
                });
                Ok(())
            }
            DeviceAction::TerminalInput { .. } => bail!("terminal action is not supported by Telegram"),
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
}

fn render_telegram_view(state: &TelegramState) -> String {
    let mut sections = Vec::new();

    if state.order.is_empty() {
        sections.push(
            "当前没有任何会话。\n如果未来接入 transport，这里会展示聊天列表与未读状态。".to_string(),
        );
    } else {
        let chat_overview = state
            .order
            .iter()
            .filter_map(|id| state.chats.get(id))
            .map(render_chat_summary)
            .collect::<Vec<_>>()
            .join("\n");
        sections.push(format!("会话列表：\n{chat_overview}"));
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
        "如果要发送消息，请使用 `DeviceAction` -> `TelegramSendMessage`。\n当前 Telegram 仍是本地设备模型，尚未接入真实网络 transport。"
            .to_string(),
    );

    sections.join("\n\n")
}

fn render_chat_summary(chat: &TelegramChat) -> String {
    let latest = chat
        .messages
        .last()
        .map(|message| truncate_preview(message.text.trim(), 48))
        .unwrap_or_else(|| "暂无消息".to_string());
    format!(
        "- {} ({}) | 未读={} | 最近消息={}",
        chat.title, chat.id, chat.unread, latest
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
        "当前会话：{} ({})\n--- 消息 ---\n{}\n--- 消息 ---",
        chat.title, chat.id, messages
    )
}

fn render_message(message: &TelegramMessage) -> String {
    let timestamp = message
        .timestamp
        .with_timezone(&Local)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let direction = match message.direction {
        MessageDirection::Incoming => "incoming",
        MessageDirection::Outgoing => "outgoing",
    };
    let delivery = match message.delivery {
        DeliveryState::Delivered => "delivered",
        DeliveryState::PendingTransport => "pending_transport",
        DeliveryState::Failed(ref reason) => return format!(
            "[{timestamp}] {} / {direction} / failed({}) / {}: {}",
            message.id,
            truncate_preview(reason, 32),
            message.sender,
            message.text
        ),
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
