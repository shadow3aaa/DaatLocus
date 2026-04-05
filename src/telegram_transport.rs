use std::time::Duration;

use miette::{Result, bail, miette};
use reqwest::Client;
use serde::Deserialize;
use tokio::sync::{mpsc, watch};

use crate::{
    config::TelegramConfig,
    dashboard::{DashboardControlCommand, DashboardState, execute_remote_command},
    events::{EventStatus, EventStore, TelegramIncomingEvent},
    pending_work::{PendingWork, PendingWorkQueue},
    telegram_acl::{AccessDecision, TelegramAclHandle},
    telegram_device::TelegramDeviceHandle,
};

pub struct TelegramTransport {
    client: Client,
    config: TelegramConfig,
    acl: TelegramAclHandle,
    handle: TelegramDeviceHandle,
    events: EventStore,
    pending_work: PendingWorkQueue,
    command_state_rx: watch::Receiver<DashboardState>,
    command_control_tx: mpsc::UnboundedSender<DashboardControlCommand>,
    offset: Option<i64>,
    bot_username: Option<String>,
    commands_registered: bool,
}

impl TelegramTransport {
    pub fn new(
        config: TelegramConfig,
        handle: TelegramDeviceHandle,
        acl: TelegramAclHandle,
        events: EventStore,
        pending_work: PendingWorkQueue,
        command_state_rx: watch::Receiver<DashboardState>,
        command_control_tx: mpsc::UnboundedSender<DashboardControlCommand>,
    ) -> Self {
        Self {
            client: Client::new(),
            config,
            acl,
            handle,
            events,
            pending_work,
            command_state_rx,
            command_control_tx,
            offset: None,
            bot_username: None,
            commands_registered: false,
        }
    }

    pub async fn run(mut self) {
        loop {
            if let Err(err) = self.run_once().await {
                tracing::error!("telegram transport error: {err:?}");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }

    async fn run_once(&mut self) -> Result<()> {
        if !self.commands_registered {
            self.sync_bot_commands().await?;
            self.commands_registered = true;
        }
        self.flush_outbox().await?;
        let updates = self.get_updates().await?;
        for update in updates {
            self.offset = Some(update.update_id + 1);
            self.handle_update(update).await;
        }
        self.flush_outbox().await?;
        Ok(())
    }

    async fn sync_bot_commands(&mut self) -> Result<()> {
        let bot = self.get_me().await?;
        self.bot_username = bot
            .username
            .map(|username| username.trim().trim_start_matches('@').to_string())
            .filter(|username| !username.is_empty());
        self.set_my_commands().await
    }

    async fn flush_outbox(&mut self) -> Result<()> {
        while let Some(message) = self.handle.take_next_outbound() {
            let chat_id = message
                .chat_id
                .parse::<i64>()
                .map_err(|err| miette!("invalid telegram chat id {}: {err}", message.chat_id))?;
            if self.acl.classify(chat_id) != AccessDecision::Approved {
                let reason = "chat is not approved in telegram acl".to_string();
                if let Some(event_id) = message.related_event_id.as_deref()
                    && let Err(err) = self.events.mark_delivery_failed(event_id, reason)
                {
                    tracing::error!("mark telegram event failed failed: {err:?}");
                }
                continue;
            }

            match self.send_message(chat_id, &message.text).await {
                Ok(()) => {
                    if let Some(event_id) = message.related_event_id.as_deref()
                        && let Err(err) = self.events.set_status(
                            event_id,
                            message
                                .settle_status_on_delivery
                                .unwrap_or(EventStatus::Resolved),
                            None,
                        )
                    {
                        tracing::error!("mark telegram event delivered failed: {err:?}");
                    }
                }
                Err(err) => {
                    let reason = truncate_reason(&format!("{err:?}"));
                    if let Some(event_id) = message.related_event_id.as_deref()
                        && let Err(mark_err) = self.events.mark_delivery_failed(event_id, reason)
                    {
                        tracing::error!("mark telegram event failed failed: {mark_err:?}");
                    }
                }
            }
        }
        Ok(())
    }

    async fn handle_update(&self, update: TelegramUpdate) {
        let Some(message) = update.message else {
            return;
        };
        let text = extract_message_text(&message);
        let sender = message
            .from
            .as_ref()
            .map(render_user_name)
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| render_chat_title(&message.chat));
        let chat_title = render_chat_title(&message.chat);

        match self.acl.classify(message.chat.id) {
            AccessDecision::Approved => {
                if let Some(command) =
                    extract_telegram_command(&message, self.bot_username.as_deref())
                {
                    if let Err(err) = self.handle_command_message(message.chat.id, &command).await
                    {
                        tracing::error!("handle telegram command failed: {err:?}");
                    }
                    return;
                }
                if let Err(err) = self.acl.observe_approved(
                    message.chat.id,
                    chat_title.clone(),
                    sender.clone(),
                    truncate_reason(&text),
                    chrono::Utc::now().timestamp_millis(),
                ) {
                    tracing::error!("update approved telegram chat metadata failed: {err:?}");
                }
                let chat_id = message.chat.id.to_string();
                match self
                    .events
                    .register_telegram_incoming(TelegramIncomingEvent {
                        chat_id: chat_id.clone(),
                        chat_title: chat_title.clone(),
                        sender: sender.clone(),
                        incoming_text: text.clone(),
                        telegram_update_id: update.update_id,
                        telegram_message_id: message.message_id,
                        telegram_message_date: message.date,
                    }) {
                    Ok(event_id) => {
                        if let Err(err) = self.pending_work.enqueue(PendingWork::Event { event_id })
                        {
                            tracing::error!("enqueue pending telegram work failed: {err:?}");
                        }
                    }
                    Err(err) => {
                        tracing::error!("register telegram event failed: {err:?}");
                    }
                }
                self.handle.observe_incoming_message(
                    chat_id,
                    chat_title.clone(),
                );
            }
            AccessDecision::Blocked => (),
            AccessDecision::Unknown => {
                if let Err(err) = self.acl.register_pending(
                    message.chat.id,
                    chat_title,
                    sender,
                    truncate_reason(&text),
                    chrono::Utc::now().timestamp_millis(),
                ) {
                    tracing::error!("register pending telegram chat failed: {err:?}");
                }
            }
        }
    }

    async fn handle_command_message(&self, chat_id: i64, command: &str) -> Result<()> {
        let response = {
            let state = self.command_state_rx.borrow();
            execute_remote_command(&command, &self.acl, &state, &self.command_control_tx)
        };
        self.handle
            .register_known_chat(chat_id.to_string(), chat_id.to_string());
        self.send_message(chat_id, &response).await
    }

    async fn get_updates(&self) -> Result<Vec<TelegramUpdate>> {
        let url = self.endpoint("getUpdates");
        let response = self
            .client
            .post(url)
            .json(&serde_json::json!({
                "offset": self.offset,
                "timeout": self.config.poll_timeout_secs,
                "allowed_updates": ["message"],
            }))
            .send()
            .await
            .map_err(|err| miette!("telegram getUpdates request failed: {err}"))?
            .error_for_status()
            .map_err(|err| miette!("telegram getUpdates http error: {err}"))?;

        let payload: TelegramApiResponse<Vec<TelegramUpdate>> = response
            .json()
            .await
            .map_err(|err| miette!("telegram getUpdates json decode failed: {err}"))?;
        if payload.ok {
            Ok(payload.result)
        } else {
            bail!(
                "telegram getUpdates failed: {}",
                payload
                    .description
                    .unwrap_or_else(|| "unknown api error".to_string())
            );
        }
    }

    async fn get_me(&self) -> Result<TelegramBotProfile> {
        let response = self
            .client
            .post(self.endpoint("getMe"))
            .send()
            .await
            .map_err(|err| miette!("telegram getMe request failed: {err}"))?
            .error_for_status()
            .map_err(|err| miette!("telegram getMe http error: {err}"))?;
        let payload: TelegramApiResponse<TelegramBotProfile> = response
            .json()
            .await
            .map_err(|err| miette!("telegram getMe json decode failed: {err}"))?;
        if payload.ok {
            Ok(payload.result)
        } else {
            bail!(
                "telegram getMe failed: {}",
                payload
                    .description
                    .unwrap_or_else(|| "unknown api error".to_string())
            );
        }
    }

    async fn set_my_commands(&self) -> Result<()> {
        let response = self
            .client
            .post(self.endpoint("setMyCommands"))
            .json(&serde_json::json!({
                "commands": TELEGRAM_BOT_COMMANDS,
            }))
            .send()
            .await
            .map_err(|err| miette!("telegram setMyCommands request failed: {err}"))?
            .error_for_status()
            .map_err(|err| miette!("telegram setMyCommands http error: {err}"))?;
        let payload: TelegramApiResponse<bool> = response
            .json()
            .await
            .map_err(|err| miette!("telegram setMyCommands json decode failed: {err}"))?;
        if payload.ok && payload.result {
            Ok(())
        } else {
            bail!(
                "telegram setMyCommands failed: {}",
                payload
                    .description
                    .unwrap_or_else(|| "unknown api error".to_string())
            );
        }
    }

    async fn send_message(&self, chat_id: i64, text: &str) -> Result<()> {
        let url = self.endpoint("sendMessage");
        let response = self
            .client
            .post(url)
            .json(&serde_json::json!({
                "chat_id": chat_id,
                "text": text,
            }))
            .send()
            .await
            .map_err(|err| miette!("telegram sendMessage request failed: {err}"))?
            .error_for_status()
            .map_err(|err| miette!("telegram sendMessage http error: {err}"))?;

        let payload: TelegramApiResponse<serde_json::Value> = response
            .json()
            .await
            .map_err(|err| miette!("telegram sendMessage json decode failed: {err}"))?;
        if payload.ok {
            Ok(())
        } else {
            bail!(
                "telegram sendMessage failed: {}",
                payload
                    .description
                    .unwrap_or_else(|| "unknown api error".to_string())
            );
        }
    }

    fn endpoint(&self, method: &str) -> String {
        format!(
            "https://api.telegram.org/bot{}/{}",
            self.config.bot_token, method
        )
    }
}

#[derive(Deserialize)]
struct TelegramApiResponse<T> {
    ok: bool,
    result: T,
    description: Option<String>,
}

#[derive(Deserialize)]
struct TelegramUpdate {
    update_id: i64,
    message: Option<TelegramIncomingMessage>,
}

#[derive(Deserialize)]
struct TelegramIncomingMessage {
    message_id: Option<i64>,
    date: Option<i64>,
    chat: TelegramChat,
    from: Option<TelegramUser>,
    text: Option<String>,
    entities: Option<Vec<TelegramMessageEntity>>,
    caption: Option<String>,
    caption_entities: Option<Vec<TelegramMessageEntity>>,
}

#[derive(Deserialize)]
struct TelegramMessageEntity {
    #[serde(rename = "type")]
    kind: String,
    offset: usize,
    length: usize,
}

#[derive(Deserialize)]
struct TelegramChat {
    id: i64,
    title: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
    username: Option<String>,
}

#[derive(Deserialize)]
struct TelegramUser {
    first_name: Option<String>,
    last_name: Option<String>,
    username: Option<String>,
}

#[derive(Deserialize)]
struct TelegramBotProfile {
    username: Option<String>,
}

#[derive(serde::Serialize)]
struct TelegramBotCommand {
    command: &'static str,
    description: &'static str,
}

const TELEGRAM_BOT_COMMANDS: &[TelegramBotCommand] = &[
    TelegramBotCommand {
        command: "status",
        description: "查看当前状态",
    },
    TelegramBotCommand {
        command: "clear",
        description: "清空当前会话消息历史",
    },
    TelegramBotCommand {
        command: "persona",
        description: "查看当前人格配置",
    },
    TelegramBotCommand {
        command: "system_prompt",
        description: "查看当前系统提示词",
    },
    TelegramBotCommand {
        command: "sleep",
        description: "sleep run 或 sleep status",
    },
    TelegramBotCommand {
        command: "telegram",
        description: "telegram status/approve/reject",
    },
];

fn extract_message_text(message: &TelegramIncomingMessage) -> String {
    message
        .text
        .as_deref()
        .or(message.caption.as_deref())
        .unwrap_or("[unsupported non-text message]")
        .to_string()
}

fn render_chat_title(chat: &TelegramChat) -> String {
    if let Some(title) = chat.title.as_deref() {
        return title.to_string();
    }
    render_name_parts(
        chat.first_name.as_deref(),
        chat.last_name.as_deref(),
        chat.username.as_deref(),
    )
}

fn render_user_name(user: &TelegramUser) -> String {
    render_name_parts(
        user.first_name.as_deref(),
        user.last_name.as_deref(),
        user.username.as_deref(),
    )
}

fn render_name_parts(first: Option<&str>, last: Option<&str>, username: Option<&str>) -> String {
    let mut parts = Vec::new();
    if let Some(first) = first.filter(|s| !s.trim().is_empty()) {
        parts.push(first.trim().to_string());
    }
    if let Some(last) = last.filter(|s| !s.trim().is_empty()) {
        parts.push(last.trim().to_string());
    }
    if !parts.is_empty() {
        return parts.join(" ");
    }
    if let Some(username) = username.filter(|s| !s.trim().is_empty()) {
        return format!("@{}", username.trim());
    }
    "Unknown".to_string()
}

fn truncate_reason(text: &str) -> String {
    let mut chars = text.chars();
    let preview = chars.by_ref().take(96).collect::<String>();
    if chars.next().is_some() {
        format!("{preview}...")
    } else {
        preview
    }
}

fn extract_telegram_command(
    message: &TelegramIncomingMessage,
    bot_username: Option<&str>,
) -> Option<String> {
    let (text, entities) = if let Some(text) = message.text.as_deref() {
        (text, message.entities.as_deref()?)
    } else if let Some(caption) = message.caption.as_deref() {
        (caption, message.caption_entities.as_deref()?)
    } else {
        return None;
    };
    let entity = entities.first()?;
    if entity.kind != "bot_command" || entity.offset != 0 || entity.length == 0 {
        return None;
    }
    let command_token = text.get(..entity.length)?.trim();
    let remainder = text.get(entity.length..).unwrap_or_default().trim();
    normalize_telegram_command(command_token, remainder, bot_username)
}

fn normalize_telegram_command(
    command_token: &str,
    remainder: &str,
    bot_username: Option<&str>,
) -> Option<String> {
    let command = command_token.trim().trim_start_matches('/');
    if command.is_empty() {
        return None;
    }
    let (command_name, mentioned_username) = match command.split_once('@') {
        Some((name, username)) => (name.trim(), Some(username.trim())),
        None => (command.trim(), None),
    };
    if command_name.is_empty() {
        return None;
    }
    if let Some(mentioned_username) = mentioned_username {
        let bot_username = bot_username?;
        if !mentioned_username.eq_ignore_ascii_case(bot_username) {
            return None;
        }
    }
    let args = remainder.trim();
    Some(if args.is_empty() {
        command_name.to_string()
    } else {
        format!("{command_name} {args}")
    })
}
