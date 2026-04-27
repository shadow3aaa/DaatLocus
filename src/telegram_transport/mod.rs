pub mod state;

use std::{path::Path, time::Duration};

use miette::{Result, bail, miette};
use reqwest::{Client, header::CONTENT_TYPE};
use serde::Deserialize;
use tokio::sync::{mpsc, watch};

use crate::telegram_transport::state::{TelegramTransportStateHandle, split_telegram_message_text};
use crate::{
    config::TelegramConfig,
    daat_locus_paths::daat_locus_paths_sync,
    dashboard::{
        DashboardControlCommand, DashboardState, execute_control_command, remote_dashboard_commands,
    },
    events::{
        EventStatus, EventStore, TelegramIncomingAttachment, TelegramIncomingAttachmentKind,
        TelegramIncomingEvent,
    },
    pending_work::{PendingWork, PendingWorkQueue},
    telegram_acl::{AccessDecision, TelegramAclHandle},
};

pub struct TelegramTransport {
    client: Client,
    config: TelegramConfig,
    acl: TelegramAclHandle,
    handle: TelegramTransportStateHandle,
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
        handle: TelegramTransportStateHandle,
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
        tokio::select! {
            updates = self.get_updates() => {
                let updates = updates?;
                for update in updates {
                    self.offset = Some(update.update_id + 1);
                    self.handle_update(update).await;
                }
                self.flush_outbox().await?;
            }
            _ = self.handle.wait_for_outbound() => {
                self.flush_outbox().await?;
            }
        }
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
                            message.settle_note_on_delivery.clone(),
                        )
                    {
                        tracing::error!("mark telegram event delivered failed: {err:?}");
                    }
                }
                Err(err) => {
                    let reason = truncate_reason(&format!("{err:?}"));
                    if let Some(event_id) = message.related_event_id.as_deref()
                        && let Err(mark_err) = self.events.set_status(
                            event_id,
                            EventStatus::AwaitingDelivery,
                            Some(reason.clone()),
                        )
                    {
                        tracing::error!(
                            "mark telegram event awaiting delivery failed: {mark_err:?}"
                        );
                    }
                    if let Err(requeue_err) = self.handle.requeue_outbound_front(message) {
                        tracing::error!(
                            "requeue telegram outbound message failed: {requeue_err:?}"
                        );
                    }
                    return Err(miette!("telegram outbound delivery failed: {reason}"));
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
                    if let Err(err) = self.handle_command_message(message.chat.id, &command).await {
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
                let attachments = self
                    .download_incoming_attachments(update.update_id, &message)
                    .await;
                match self
                    .events
                    .register_telegram_incoming(TelegramIncomingEvent {
                        chat_id: chat_id.clone(),
                        chat_kind: message.chat.kind.clone(),
                        chat_title: chat_title.clone(),
                        sender: sender.clone(),
                        incoming_text: text.clone(),
                        telegram_update_id: update.update_id,
                        telegram_message_id: message.message_id,
                        telegram_message_date: message.date,
                        attachments,
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
                self.handle
                    .observe_incoming_message(chat_id, chat_title.clone());
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
            execute_control_command(command, &self.acl, &state, &self.command_control_tx)
        };
        self.handle
            .register_known_chat(chat_id.to_string(), chat_id.to_string());
        self.send_text(chat_id, &response).await
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
            .map_err(|err| miette!("telegram getUpdates request failed: {}", err.without_url()))?
            .error_for_status()
            .map_err(|err| miette!("telegram getUpdates http error: {}", err.without_url()))?;

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
            .map_err(|err| miette!("telegram getMe request failed: {}", err.without_url()))?
            .error_for_status()
            .map_err(|err| miette!("telegram getMe http error: {}", err.without_url()))?;
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
        let commands = remote_dashboard_commands();
        let response = self
            .client
            .post(self.endpoint("setMyCommands"))
            .json(&serde_json::json!({
                "commands": commands,
            }))
            .send()
            .await
            .map_err(|err| {
                miette!(
                    "telegram setMyCommands request failed: {}",
                    err.without_url()
                )
            })?
            .error_for_status()
            .map_err(|err| miette!("telegram setMyCommands http error: {}", err.without_url()))?;
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

    async fn download_incoming_attachments(
        &self,
        update_id: i64,
        message: &TelegramIncomingMessage,
    ) -> Vec<TelegramIncomingAttachment> {
        let Some(photo) = message.largest_photo() else {
            return Vec::new();
        };
        match self
            .download_photo_attachment(update_id, message.message_id, photo)
            .await
        {
            Ok(attachment) => vec![attachment],
            Err(err) => {
                tracing::warn!("failed to download telegram photo attachment: {err:?}");
                Vec::new()
            }
        }
    }

    async fn download_photo_attachment(
        &self,
        update_id: i64,
        message_id: Option<i64>,
        photo: &TelegramPhotoSize,
    ) -> Result<TelegramIncomingAttachment> {
        let file = self.get_file(&photo.file_id).await?;
        let (bytes, media_type) = self.download_file_bytes(&file.file_path).await?;
        let media_type = normalize_photo_media_type(media_type.as_deref(), &file.file_path);
        let extension = extension_for_file(&file.file_path, &media_type);
        let file_unique_id = if file.file_unique_id.trim().is_empty() {
            photo.file_unique_id.clone()
        } else {
            file.file_unique_id
        };
        let message_id = message_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let file_name = format!(
            "update-{update_id}-message-{message_id}-{}.{}",
            sanitize_file_component(&file_unique_id),
            extension
        );
        let dir = daat_locus_paths_sync()
            .state_dir()
            .join("telegram_attachments");
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|err| miette!("create telegram attachment directory failed: {err}"))?;
        let path = dir.join(file_name);
        tokio::fs::write(&path, bytes)
            .await
            .map_err(|err| miette!("write telegram attachment failed: {err}"))?;
        Ok(TelegramIncomingAttachment {
            kind: TelegramIncomingAttachmentKind::Image,
            file_id: photo.file_id.clone(),
            file_unique_id,
            media_type,
            local_path: path.display().to_string(),
            description: Some(format!("telegram photo {}x{}", photo.width, photo.height)),
        })
    }

    async fn get_file(&self, file_id: &str) -> Result<TelegramFile> {
        let response = self
            .client
            .post(self.endpoint("getFile"))
            .json(&serde_json::json!({ "file_id": file_id }))
            .send()
            .await
            .map_err(|err| miette!("telegram getFile request failed: {}", err.without_url()))?
            .error_for_status()
            .map_err(|err| miette!("telegram getFile http error: {}", err.without_url()))?;
        let payload: TelegramApiResponse<TelegramFile> = response
            .json()
            .await
            .map_err(|err| miette!("telegram getFile json decode failed: {err}"))?;
        if payload.ok {
            Ok(payload.result)
        } else {
            bail!(
                "telegram getFile failed: {}",
                payload
                    .description
                    .unwrap_or_else(|| "unknown api error".to_string())
            );
        }
    }

    async fn download_file_bytes(&self, file_path: &str) -> Result<(Vec<u8>, Option<String>)> {
        let response = self
            .client
            .get(format!(
                "https://api.telegram.org/file/bot{}/{}",
                self.config.bot_token, file_path
            ))
            .send()
            .await
            .map_err(|err| {
                miette!(
                    "telegram file download request failed: {}",
                    err.without_url()
                )
            })?
            .error_for_status()
            .map_err(|err| miette!("telegram file download http error: {}", err.without_url()))?;
        let media_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        let bytes = response
            .bytes()
            .await
            .map_err(|err| miette!("telegram file download read failed: {err}"))?
            .to_vec();
        Ok((bytes, media_type))
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
            .map_err(|err| miette!("telegram sendMessage request failed: {}", err.without_url()))?
            .error_for_status()
            .map_err(|err| miette!("telegram sendMessage http error: {}", err.without_url()))?;

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

    async fn send_text(&self, chat_id: i64, text: &str) -> Result<()> {
        for chunk in split_telegram_message_text(text) {
            self.send_message(chat_id, &chunk).await?;
        }
        Ok(())
    }

    fn endpoint(&self, method: &str) -> String {
        format!(
            "https://api.telegram.org/bot{}/{}",
            self.config.bot_token, method
        )
    }
}

#[derive(Clone)]
pub struct TelegramLiveDraftClient {
    client: Client,
    config: TelegramConfig,
}

impl TelegramLiveDraftClient {
    pub fn new(config: TelegramConfig) -> Self {
        Self {
            client: Client::new(),
            config,
        }
    }

    pub async fn send_message_draft(&self, chat_id: i64, draft_id: i64, text: &str) -> Result<()> {
        let response = self
            .client
            .post(self.endpoint("sendMessageDraft"))
            .json(&serde_json::json!({
                "chat_id": chat_id,
                "draft_id": draft_id,
                "text": text,
                "parse_mode": "MarkdownV2",
            }))
            .send()
            .await
            .map_err(|err| {
                miette!(
                    "telegram sendMessageDraft request failed: {}",
                    err.without_url()
                )
            })?
            .error_for_status()
            .map_err(|err| {
                miette!(
                    "telegram sendMessageDraft http error: {}",
                    err.without_url()
                )
            })?;

        let payload: TelegramApiResponse<bool> = response
            .json()
            .await
            .map_err(|err| miette!("telegram sendMessageDraft json decode failed: {err}"))?;
        if payload.ok {
            Ok(())
        } else {
            bail!(
                "telegram sendMessageDraft failed: {}",
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
    photo: Option<Vec<TelegramPhotoSize>>,
}

impl TelegramIncomingMessage {
    fn largest_photo(&self) -> Option<&TelegramPhotoSize> {
        self.photo.as_ref()?.iter().max_by_key(|photo| {
            photo
                .file_size
                .unwrap_or_else(|| i64::from(photo.width) * i64::from(photo.height))
        })
    }
}

#[derive(Clone, Deserialize)]
struct TelegramPhotoSize {
    file_id: String,
    file_unique_id: String,
    width: i32,
    height: i32,
    file_size: Option<i64>,
}

#[derive(Deserialize)]
struct TelegramFile {
    file_unique_id: String,
    file_path: String,
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
    #[serde(rename = "type")]
    kind: String,
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

fn extract_message_text(message: &TelegramIncomingMessage) -> String {
    message
        .text
        .as_deref()
        .or(message.caption.as_deref())
        .or_else(|| message.photo.as_ref().map(|_| "[telegram photo]"))
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

fn sanitize_file_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "file".to_string()
    } else {
        sanitized
    }
}

fn normalize_photo_media_type(download_media_type: Option<&str>, file_path: &str) -> String {
    if let Some(media_type) = download_media_type
        .map(str::trim)
        .filter(|value| value.starts_with("image/"))
    {
        return media_type.to_string();
    }
    infer_image_media_type(file_path)
}

fn infer_image_media_type(file_path: &str) -> String {
    match Path::new(file_path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => "image/png".to_string(),
        Some("webp") => "image/webp".to_string(),
        Some("gif") => "image/gif".to_string(),
        _ => "image/jpeg".to_string(),
    }
}

fn extension_for_file(file_path: &str, media_type: &str) -> String {
    if let Some(extension) = Path::new(file_path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::trim)
        .filter(|extension| {
            !extension.is_empty()
                && extension
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        })
    {
        return extension.to_ascii_lowercase();
    }
    match media_type {
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "jpg",
    }
    .to_string()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn private_chat() -> TelegramChat {
        TelegramChat {
            id: 42,
            kind: "private".to_string(),
            title: None,
            first_name: Some("Ada".to_string()),
            last_name: None,
            username: None,
        }
    }

    fn message_with_photo(caption: Option<&str>) -> TelegramIncomingMessage {
        TelegramIncomingMessage {
            message_id: Some(7),
            date: Some(123),
            chat: private_chat(),
            from: None,
            text: None,
            entities: None,
            caption: caption.map(ToString::to_string),
            caption_entities: None,
            photo: Some(vec![
                TelegramPhotoSize {
                    file_id: "small-id".to_string(),
                    file_unique_id: "small-unique".to_string(),
                    width: 64,
                    height: 64,
                    file_size: Some(1_000),
                },
                TelegramPhotoSize {
                    file_id: "large-id".to_string(),
                    file_unique_id: "large-unique".to_string(),
                    width: 512,
                    height: 512,
                    file_size: Some(10_000),
                },
            ]),
        }
    }

    #[test]
    fn photo_only_messages_have_text_placeholder() {
        let message = message_with_photo(None);

        assert_eq!(extract_message_text(&message), "[telegram photo]");
    }

    #[test]
    fn photo_caption_is_used_as_message_text() {
        let message = message_with_photo(Some("please inspect this"));

        assert_eq!(extract_message_text(&message), "please inspect this");
    }

    #[test]
    fn largest_photo_prefers_telegram_file_size() {
        let message = message_with_photo(None);

        assert_eq!(message.largest_photo().unwrap().file_id, "large-id");
    }

    #[test]
    fn attachment_file_helpers_are_stable() {
        assert_eq!(sanitize_file_component("abc/def?.png"), "abc-def--png");
        assert_eq!(infer_image_media_type("photos/demo.webp"), "image/webp");
        assert_eq!(
            normalize_photo_media_type(Some("application/octet-stream"), "photos/demo.jpg"),
            "image/jpeg"
        );
        assert_eq!(
            normalize_photo_media_type(Some("image/png"), "photos/demo.jpg"),
            "image/png"
        );
        assert_eq!(extension_for_file("photos/demo", "image/png"), "png");
    }
}
