pub mod state;

use std::{path::Path, sync::Arc, time::Duration};

use async_trait::async_trait;
use miette::{Result, bail, miette};
use reqwest::{Client, header::CONTENT_TYPE};
use serde::Deserialize;
use tokio::sync::{mpsc, watch};

use crate::telegram_transport::state::{
    PendingOutboundMessage, TelegramTransportStateHandle, split_telegram_message_text,
};
use crate::{
    config::TelegramConfig,
    daat_locus_paths::daat_locus_paths_sync,
    dashboard::{
        DashboardControlCommand, DashboardState, execute_control_command, remote_dashboard_commands,
    },
    events::{TelegramIncomingAttachment, TelegramIncomingAttachmentKind, TelegramIncomingEvent},
    telegram_acl::{AccessDecision, TelegramAclHandle},
};

const TELEGRAM_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const TELEGRAM_FILE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const TELEGRAM_LONG_POLL_TIMEOUT_GRACE: Duration = Duration::from_secs(10);

#[async_trait]
pub trait TelegramInputRouter: Send + Sync {
    async fn route_telegram_event(&self, event: TelegramIncomingEvent) -> Result<()>;
}

#[async_trait]
pub trait TelegramAuthVerifier: Send + Sync {
    async fn authorize_telegram_verification(&self, token: &str) -> bool;
}

#[async_trait]
pub trait TelegramSessionCommandHandler: Send + Sync {
    async fn handle_session_command(
        &self,
        chat_id: &str,
        chat_title: &str,
        command: &str,
    ) -> Result<Option<String>>;
}

#[derive(Clone)]
pub struct TelegramDeliveryClient {
    client: Client,
    config: TelegramConfig,
    acl: TelegramAclHandle,
}

impl TelegramDeliveryClient {
    pub fn new(config: TelegramConfig, acl: TelegramAclHandle) -> Self {
        Self {
            client: Client::new(),
            config,
            acl,
        }
    }

    pub async fn send_pending_outbound(&self, message: &PendingOutboundMessage) -> Result<()> {
        let chat_id = message
            .chat_id
            .parse::<i64>()
            .map_err(|err| miette!("invalid telegram chat id {}: {err}", message.chat_id))?;
        if self.acl.classify(chat_id) != AccessDecision::Approved {
            return Err(miette!("chat is not approved in telegram acl"));
        }
        if let Some(draft_id) = message.draft_id {
            self.send_message_draft(chat_id, draft_id, &message.text)
                .await
        } else {
            self.send_text(chat_id, &message.text).await
        }
    }

    async fn send_text(&self, chat_id: i64, text: &str) -> Result<()> {
        for chunk in split_telegram_message_text(text) {
            self.send_message(chat_id, &chunk).await?;
        }
        Ok(())
    }

    async fn send_message(&self, chat_id: i64, text: &str) -> Result<()> {
        let response = self
            .client
            .post(self.endpoint("sendMessage"))
            .timeout(TELEGRAM_REQUEST_TIMEOUT)
            .json(&serde_json::json!({
                "chat_id": chat_id,
                "text": render_markdown_as_telegram_html(text),
                "parse_mode": "HTML",
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

    async fn send_message_draft(&self, chat_id: i64, draft_id: i64, text: &str) -> Result<()> {
        let response = self
            .client
            .post(self.endpoint("sendMessageDraft"))
            .timeout(TELEGRAM_REQUEST_TIMEOUT)
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

pub struct TelegramTransport {
    client: Client,
    config: TelegramConfig,
    acl: TelegramAclHandle,
    handle: TelegramTransportStateHandle,
    auth_verifier: Arc<dyn TelegramAuthVerifier>,
    input_router: Arc<dyn TelegramInputRouter>,
    session_command_handler: Arc<dyn TelegramSessionCommandHandler>,
    dashboard_commands: TelegramDashboardCommandBridge,
    offset: Option<i64>,
    bot_username: Option<String>,
    commands_registered: bool,
}

pub struct TelegramDashboardCommandBridge {
    state_rx: watch::Receiver<DashboardState>,
    control_tx: mpsc::UnboundedSender<DashboardControlCommand>,
}

impl TelegramDashboardCommandBridge {
    pub fn new(
        state_rx: watch::Receiver<DashboardState>,
        control_tx: mpsc::UnboundedSender<DashboardControlCommand>,
    ) -> Self {
        Self {
            state_rx,
            control_tx,
        }
    }
}

impl TelegramTransport {
    pub fn new(
        config: TelegramConfig,
        handle: TelegramTransportStateHandle,
        acl: TelegramAclHandle,
        auth_verifier: Arc<dyn TelegramAuthVerifier>,
        input_router: Arc<dyn TelegramInputRouter>,
        session_command_handler: Arc<dyn TelegramSessionCommandHandler>,
        dashboard_commands: TelegramDashboardCommandBridge,
    ) -> Self {
        let offset = handle.next_update_offset();
        Self {
            client: Client::new(),
            config,
            acl,
            handle,
            auth_verifier,
            input_router,
            session_command_handler,
            dashboard_commands,
            offset,
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
                    let next_offset = update.update_id + 1;
                    self.offset = Some(next_offset);
                    let persist_before_handling = should_persist_update_offset_before_handling(
                        &update,
                        self.bot_username.as_deref(),
                    );
                    if persist_before_handling {
                        // Bot commands such as /restart can stop the daemon before
                        // the next poll confirms the update with Telegram.
                        self.store_next_update_offset(next_offset);
                    }
                    self.handle_update(update).await;
                    if !persist_before_handling {
                        self.store_next_update_offset(next_offset);
                    }
                }
                self.flush_outbox().await?;
            }
            _ = self.handle.wait_for_outbound() => {
                self.flush_outbox().await?;
            }
        }
        Ok(())
    }

    fn store_next_update_offset(&self, offset: i64) {
        if let Err(err) = self.handle.store_next_update_offset(offset) {
            tracing::error!("persist telegram update offset failed: {err:?}");
        }
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
                tracing::warn!("dropping telegram outbound message for unapproved chat {chat_id}");
                continue;
            }

            let send_result = if let Some(draft_id) = message.draft_id {
                self.send_message_draft(chat_id, draft_id, &message.text)
                    .await
            } else {
                self.send_message(chat_id, &message.text).await
            };
            match send_result {
                Ok(()) => {}
                Err(err) => {
                    let reason = truncate_reason(&format!("{err:?}"));
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
        let command = extract_telegram_command(&message, self.bot_username.as_deref());

        match self.acl.classify(message.chat.id) {
            AccessDecision::Approved => {
                if let Some(command) = command {
                    if let Err(err) = self
                        .handle_command_message(message.chat.id, &chat_title, &command)
                        .await
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
                let attachments = self
                    .download_incoming_attachments(update.update_id, &message)
                    .await;
                match self
                    .input_router
                    .route_telegram_event(TelegramIncomingEvent {
                        chat_id: chat_id.clone(),
                        chat_kind: message.chat.kind.clone(),
                        chat_title: chat_title.clone(),
                        sender: sender.clone(),
                        incoming_text: text.clone(),
                        telegram_update_id: update.update_id,
                        telegram_message_id: message.message_id,
                        telegram_message_date: message.date,
                        attachments,
                    })
                    .await
                {
                    Ok(()) => {}
                    Err(err) => {
                        tracing::error!("route telegram event failed: {err:?}");
                    }
                }
                self.handle
                    .observe_incoming_message(chat_id, chat_title.clone());
            }
            AccessDecision::Blocked | AccessDecision::Unknown => {
                if let Err(err) = self
                    .handle_unverified_message(
                        message.chat.id,
                        &chat_title,
                        &sender,
                        &text,
                        command,
                    )
                    .await
                {
                    tracing::error!("handle unverified telegram message failed: {err:?}");
                }
            }
        }
    }

    async fn handle_unverified_message(
        &self,
        chat_id: i64,
        chat_title: &str,
        sender: &str,
        text: &str,
        command: Option<String>,
    ) -> Result<()> {
        let seen_at_ms = chrono::Utc::now().timestamp_millis();
        let preview = truncate_reason(text);
        let Some(command) = command else {
            self.register_pending_unverified(chat_id, chat_title, sender, &preview, seen_at_ms);
            return self
                .send_text(chat_id, telegram_verify_instructions())
                .await;
        };
        let Some(token) = parse_verify_command(&command) else {
            self.register_pending_unverified(chat_id, chat_title, sender, &preview, seen_at_ms);
            return self
                .send_text(chat_id, telegram_verify_instructions())
                .await;
        };
        if token.is_empty() {
            return self
                .send_text(chat_id, telegram_verify_usage_message())
                .await;
        }
        if !self
            .auth_verifier
            .authorize_telegram_verification(token)
            .await
        {
            self.register_pending_unverified(chat_id, chat_title, sender, &preview, seen_at_ms);
            return self
                .send_text(chat_id, telegram_verify_failed_message())
                .await;
        }
        self.acl
            .approve_verified(
                chat_id,
                chat_title.to_string(),
                sender.to_string(),
                preview,
                seen_at_ms,
            )
            .map_err(|err| miette!("approve verified telegram chat failed: {err:?}"))?;
        self.handle
            .register_known_chat(chat_id.to_string(), chat_title.to_string());
        self.send_text(
            chat_id,
            "Telegram verification complete. Send a message to Daat Locus or use /session_list.",
        )
        .await
    }

    fn register_pending_unverified(
        &self,
        chat_id: i64,
        chat_title: &str,
        sender: &str,
        preview: &str,
        seen_at_ms: i64,
    ) {
        if let Err(err) = self.acl.register_pending(
            chat_id,
            chat_title.to_string(),
            sender.to_string(),
            preview.to_string(),
            seen_at_ms,
        ) {
            tracing::error!("register pending telegram chat failed: {err:?}");
        }
    }

    async fn handle_command_message(
        &self,
        chat_id: i64,
        chat_title: &str,
        command: &str,
    ) -> Result<()> {
        let chat_id_string = chat_id.to_string();
        self.handle
            .register_known_chat(chat_id_string.clone(), chat_title.to_string());
        match self
            .session_command_handler
            .handle_session_command(&chat_id_string, chat_title, command)
            .await
        {
            Ok(Some(response)) => return self.send_text(chat_id, &response).await,
            Ok(None) => {}
            Err(err) => {
                return self
                    .send_text(chat_id, &format!("session command failed: {err:?}"))
                    .await;
            }
        }
        let response = {
            let state = self.dashboard_commands.state_rx.borrow();
            execute_control_command(command, &state, &self.dashboard_commands.control_tx)
        };
        self.send_text(chat_id, &response).await
    }

    async fn get_updates(&self) -> Result<Vec<TelegramUpdate>> {
        let url = self.endpoint("getUpdates");
        let response = self
            .client
            .post(url)
            .timeout(self.get_updates_timeout())
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

    fn get_updates_timeout(&self) -> Duration {
        Duration::from_secs(self.config.poll_timeout_secs.max(1))
            .saturating_add(TELEGRAM_LONG_POLL_TIMEOUT_GRACE)
    }

    async fn get_me(&self) -> Result<TelegramBotProfile> {
        let response = self
            .client
            .post(self.endpoint("getMe"))
            .timeout(TELEGRAM_REQUEST_TIMEOUT)
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
        let commands = telegram_bot_commands();
        let response = self
            .client
            .post(self.endpoint("setMyCommands"))
            .timeout(TELEGRAM_REQUEST_TIMEOUT)
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
            .timeout(TELEGRAM_REQUEST_TIMEOUT)
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
            .timeout(TELEGRAM_FILE_DOWNLOAD_TIMEOUT)
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
            .timeout(TELEGRAM_REQUEST_TIMEOUT)
            .json(&serde_json::json!({
                "chat_id": chat_id,
                "text": render_markdown_as_telegram_html(text),
                "parse_mode": "HTML",
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

    async fn send_message_draft(&self, chat_id: i64, draft_id: i64, text: &str) -> Result<()> {
        let response = self
            .client
            .post(self.endpoint("sendMessageDraft"))
            .timeout(TELEGRAM_REQUEST_TIMEOUT)
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

const TELEGRAM_SESSION_COMMANDS: &[(&str, &str)] = &[
    (
        "session_list",
        "list sessions and the current chat attachment",
    ),
    ("session_new", "create and attach a new session"),
    ("session_attach", "attach this chat to an existing session"),
    ("session_delete", "delete an existing session"),
];

fn telegram_bot_commands() -> Vec<serde_json::Value> {
    let mut commands = vec![serde_json::json!({
        "command": "verify",
        "description": "verify this chat with a daemon auth token",
    })];
    commands.extend(remote_dashboard_commands().into_iter().map(|command| {
        serde_json::json!({
            "command": command.command,
            "description": command.description,
        })
    }));
    commands.extend(
        TELEGRAM_SESSION_COMMANDS
            .iter()
            .map(|(command, description)| {
                serde_json::json!({
                    "command": command,
                    "description": description,
                })
            }),
    );
    commands
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

fn render_markdown_as_telegram_html(text: &str) -> String {
    let mut rendered = String::with_capacity(text.len());
    let mut remaining = text;

    while let Some(start) = remaining.find("```") {
        render_inline_markdown_as_telegram_html(&remaining[..start], &mut rendered);
        remaining = &remaining[start + 3..];
        let (language, code_start) = parse_fenced_code_language(remaining);
        if let Some(end) = remaining[code_start..].find("```") {
            let code = &remaining[code_start..code_start + end];
            push_telegram_pre(&mut rendered, language, code);
            remaining = &remaining[code_start + end + 3..];
        } else {
            rendered.push_str("```");
            escape_telegram_html_into(remaining, &mut rendered);
            return rendered;
        }
    }

    render_inline_markdown_as_telegram_html(remaining, &mut rendered);
    rendered
}

fn parse_fenced_code_language(text: &str) -> (Option<&str>, usize) {
    let first_line_end = text.find('\n');
    let Some(first_line_end) = first_line_end else {
        return (None, 0);
    };
    let first_line = &text[..first_line_end];
    if first_line.trim().is_empty() {
        return (None, first_line_end + 1);
    }
    if first_line
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '+' || ch == '#')
    {
        (Some(first_line.trim()), first_line_end + 1)
    } else {
        (None, 0)
    }
}

fn push_telegram_pre(rendered: &mut String, language: Option<&str>, code: &str) {
    if let Some(language) = language.filter(|language| !language.is_empty()) {
        rendered.push_str("<pre><code class=\"language-");
        escape_telegram_html_into(language, rendered);
        rendered.push_str("\">");
        escape_telegram_html_into(code, rendered);
        rendered.push_str("</code></pre>");
    } else {
        rendered.push_str("<pre>");
        escape_telegram_html_into(code, rendered);
        rendered.push_str("</pre>");
    }
}

fn render_inline_markdown_as_telegram_html(text: &str, rendered: &mut String) {
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(code) = rest.strip_prefix('`')
            && let Some(end) = code.find('`')
        {
            rendered.push_str("<code>");
            escape_telegram_html_into(&code[..end], rendered);
            rendered.push_str("</code>");
            rest = &code[end + 1..];
            continue;
        }
        if let Some(bold) = rest.strip_prefix("**")
            && let Some(end) = bold.find("**")
        {
            rendered.push_str("<b>");
            render_inline_markdown_as_telegram_html(&bold[..end], rendered);
            rendered.push_str("</b>");
            rest = &bold[end + 2..];
            continue;
        }
        if let Some(strong) = rest.strip_prefix("__")
            && let Some(end) = strong.find("__")
        {
            rendered.push_str("<b>");
            render_inline_markdown_as_telegram_html(&strong[..end], rendered);
            rendered.push_str("</b>");
            rest = &strong[end + 2..];
            continue;
        }
        if let Some(strike) = rest.strip_prefix("~~")
            && let Some(end) = strike.find("~~")
        {
            rendered.push_str("<s>");
            render_inline_markdown_as_telegram_html(&strike[..end], rendered);
            rendered.push_str("</s>");
            rest = &strike[end + 2..];
            continue;
        }
        if let Some(italic) = rest.strip_prefix('*')
            && !italic.starts_with('*')
            && let Some(end) = italic.find('*')
        {
            rendered.push_str("<i>");
            render_inline_markdown_as_telegram_html(&italic[..end], rendered);
            rendered.push_str("</i>");
            rest = &italic[end + 1..];
            continue;
        }
        if let Some(italic) = rest.strip_prefix('_')
            && !italic.starts_with('_')
            && let Some(end) = italic.find('_')
        {
            rendered.push_str("<i>");
            render_inline_markdown_as_telegram_html(&italic[..end], rendered);
            rendered.push_str("</i>");
            rest = &italic[end + 1..];
            continue;
        }

        let ch = rest.chars().next().expect("non-empty rest has char");
        escape_telegram_html_char_into(ch, rendered);
        rest = &rest[ch.len_utf8()..];
    }
}

fn escape_telegram_html_into(text: &str, rendered: &mut String) {
    for ch in text.chars() {
        escape_telegram_html_char_into(ch, rendered);
    }
}

fn escape_telegram_html_char_into(ch: char, rendered: &mut String) {
    match ch {
        '&' => rendered.push_str("&amp;"),
        '<' => rendered.push_str("&lt;"),
        '>' => rendered.push_str("&gt;"),
        _ => rendered.push(ch),
    }
}

fn extract_telegram_command(
    message: &TelegramIncomingMessage,
    bot_username: Option<&str>,
) -> Option<String> {
    let (text, entities) = if let Some(text) = message.text.as_deref() {
        (text, message.entities.as_deref()?)
    } else {
        let caption = message.caption.as_deref()?;
        (caption, message.caption_entities.as_deref()?)
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

fn parse_verify_command(command: &str) -> Option<&str> {
    let mut parts = command.split_whitespace();
    if parts.next()? != "verify" {
        return None;
    }
    let token = parts.next().unwrap_or_default();
    if parts.next().is_some() {
        return Some("");
    }
    Some(token)
}

fn telegram_verify_instructions() -> &'static str {
    "This Telegram chat is not verified yet.\nRun `daat-locus token create telegram` locally, then send `/verify <token>` here."
}

fn telegram_verify_usage_message() -> &'static str {
    "Usage: /verify <token>\nRun `daat-locus token create telegram` locally to create a daemon auth token."
}

fn telegram_verify_failed_message() -> &'static str {
    "Telegram verification failed. Create a daemon auth token locally with `daat-locus token create telegram`, then send `/verify <token>` here."
}

fn should_persist_update_offset_before_handling(
    update: &TelegramUpdate,
    bot_username: Option<&str>,
) -> bool {
    update
        .message
        .as_ref()
        .and_then(|message| extract_telegram_command(message, bot_username))
        .is_some()
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

    fn text_message(
        text: &str,
        entities: Option<Vec<TelegramMessageEntity>>,
    ) -> TelegramIncomingMessage {
        TelegramIncomingMessage {
            message_id: Some(7),
            date: Some(123),
            chat: private_chat(),
            from: None,
            text: Some(text.to_string()),
            entities,
            caption: None,
            caption_entities: None,
            photo: None,
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
    fn command_updates_persist_offset_before_handling() {
        let update = TelegramUpdate {
            update_id: 99,
            message: Some(text_message(
                "/restart",
                Some(vec![TelegramMessageEntity {
                    kind: "bot_command".to_string(),
                    offset: 0,
                    length: "/restart".len(),
                }]),
            )),
        };

        assert!(should_persist_update_offset_before_handling(&update, None));
    }

    #[test]
    fn normal_messages_persist_offset_after_handling() {
        let update = TelegramUpdate {
            update_id: 99,
            message: Some(text_message("restart", None)),
        };

        assert!(!should_persist_update_offset_before_handling(&update, None));
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
    fn agent_markdown_is_rendered_as_telegram_html() {
        assert_eq!(
            render_markdown_as_telegram_html(
                "**done** and *soon* with `x < y` plus ~~old~~ & plain <tag>"
            ),
            "<b>done</b> and <i>soon</i> with <code>x &lt; y</code> plus <s>old</s> &amp; plain &lt;tag&gt;"
        );
    }

    #[test]
    fn fenced_code_blocks_are_rendered_as_telegram_pre() {
        assert_eq!(
            render_markdown_as_telegram_html(
                "before\n```rust\nfn main() {\n    a < b;\n}\n```\nafter"
            ),
            "before\n<pre><code class=\"language-rust\">fn main() {\n    a &lt; b;\n}\n</code></pre>\nafter"
        );
    }

    #[test]
    fn unmatched_markdown_is_left_readable_and_safe() {
        assert_eq!(
            render_markdown_as_telegram_html("**open and `unterminated <code>"),
            "**open and `unterminated &lt;code&gt;"
        );
    }

    #[test]
    fn nested_inline_markdown_is_supported() {
        assert_eq!(
            render_markdown_as_telegram_html("**bold `code` _italic_**"),
            "<b>bold <code>code</code> <i>italic</i></b>"
        );
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

    #[test]
    fn verify_command_accepts_exactly_one_token() {
        assert_eq!(parse_verify_command("verify abc123"), Some("abc123"));
        assert_eq!(parse_verify_command("verify"), Some(""));
        assert_eq!(parse_verify_command("verify abc123 extra"), Some(""));
        assert_eq!(parse_verify_command("status"), None);
    }

    #[test]
    fn bot_commands_include_telegram_session_controls() {
        let commands = telegram_bot_commands()
            .into_iter()
            .filter_map(|command| command.get("command")?.as_str().map(ToString::to_string))
            .collect::<Vec<_>>();

        assert!(commands.contains(&"verify".to_string()));
        assert!(commands.contains(&"session_list".to_string()));
        assert!(commands.contains(&"session_new".to_string()));
        assert!(commands.contains(&"session_attach".to_string()));
        assert!(commands.contains(&"session_delete".to_string()));
    }
}
