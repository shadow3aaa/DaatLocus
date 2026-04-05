use std::time::Duration;

use miette::{Result, bail, miette};
use reqwest::Client;
use serde::Deserialize;

use crate::{
    config::TelegramConfig,
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
    offset: Option<i64>,
}

impl TelegramTransport {
    pub fn new(
        config: TelegramConfig,
        handle: TelegramDeviceHandle,
        acl: TelegramAclHandle,
        events: EventStore,
        pending_work: PendingWorkQueue,
    ) -> Self {
        Self {
            client: Client::new(),
            config,
            acl,
            handle,
            events,
            pending_work,
            offset: None,
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
        self.flush_outbox().await?;
        let updates = self.get_updates().await?;
        for update in updates {
            self.offset = Some(update.update_id + 1);
            self.handle_update(update);
        }
        self.flush_outbox().await?;
        Ok(())
    }

    async fn flush_outbox(&mut self) -> Result<()> {
        while let Some(message) = self.handle.take_next_outbound() {
            let chat_id = message
                .chat_id
                .parse::<i64>()
                .map_err(|err| miette!("invalid telegram chat id {}: {err}", message.chat_id))?;
            if self.acl.classify(chat_id) != AccessDecision::Approved {
                self.handle.mark_outgoing_failed(
                    &message.local_message_id,
                    "chat is not approved in telegram acl",
                );
                continue;
            }

            match self.send_message(chat_id, &message.text).await {
                Ok(()) => {
                    self.handle
                        .mark_outgoing_delivered(&message.local_message_id);
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
                    self.handle
                        .mark_outgoing_failed(&message.local_message_id, reason.clone());
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

    fn handle_update(&self, update: TelegramUpdate) {
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
                        latest_outgoing_preview: self.handle.latest_outgoing_preview(&chat_id),
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
                    sender,
                    text.clone(),
                    message
                        .date
                        .map(|seconds| seconds.saturating_mul(1000))
                        .unwrap_or_else(|| chrono::Utc::now().timestamp_millis()),
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
    caption: Option<String>,
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
