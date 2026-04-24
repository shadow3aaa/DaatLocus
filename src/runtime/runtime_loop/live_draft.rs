use super::*;

pub(super) struct TelegramLiveDraftSession {
    join: JoinHandle<()>,
}

impl TelegramLiveDraftSession {
    pub(super) async fn shutdown(self, context: &mut Context) {
        context.install_live_assistant_progress(None);
        let _ = tokio::time::timeout(Duration::from_secs(2), self.join).await;
    }
}

pub(super) fn maybe_start_telegram_live_draft_session(
    context: &mut Context,
    claimed_event_views: &[EventView],
) -> Option<TelegramLiveDraftSession> {
    if claimed_event_views.len() != 1 {
        return None;
    }
    let event = claimed_event_views.first()?;
    let EventPayload::TelegramIncoming(payload) = &event.payload else {
        return None;
    };
    if payload.chat_kind != "private" {
        return None;
    }
    if !context.config.telegram.enabled || !context.config.telegram.has_real_credentials() {
        return None;
    }
    let chat_id = payload.chat_id.parse::<i64>().ok()?;
    let draft_id = Utc::now().timestamp_millis().unsigned_abs().max(1) as i64;
    let client = TelegramLiveDraftClient::new(context.config.telegram.clone());
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    context.install_live_assistant_progress(Some(tx));
    let join = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(900));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut latest_text: Option<String> = None;
        let mut last_sent = String::new();
        let initial_draft_text = format_telegram_live_draft_text("");
        if let Err(err) = client
            .send_message_draft(chat_id, draft_id, &initial_draft_text)
            .await
        {
            tracing::warn!("telegram initial live draft send failed: {err:?}");
        } else {
            last_sent = initial_draft_text;
        }
        loop {
            tokio::select! {
                maybe_text = rx.recv() => {
                    match maybe_text {
                        Some(text) => latest_text = Some(text),
                        None => break,
                    }
                }
                _ = interval.tick() => {
                    if let Some(text) = latest_text.take() {
                        let draft_text = format_telegram_live_draft_text(&text);
                        if draft_text != last_sent {
                            if let Err(err) = client
                                .send_message_draft(chat_id, draft_id, &draft_text)
                                .await
                            {
                                tracing::warn!("telegram live draft update failed: {err:?}");
                            } else {
                                last_sent = draft_text;
                            }
                        }
                    }
                }
            }
        }
        if let Some(text) = latest_text.take() {
            let draft_text = format_telegram_live_draft_text(&text);
            if draft_text != last_sent
                && let Err(err) = client
                    .send_message_draft(chat_id, draft_id, &draft_text)
                    .await
            {
                tracing::warn!("telegram final live draft flush failed: {err:?}");
            }
        }
    });
    Some(TelegramLiveDraftSession { join })
}

fn format_telegram_live_draft_text(content: &str) -> String {
    let trimmed = content.trim();
    let base = if trimmed.is_empty() {
        "Working...".to_string()
    } else {
        format!("Working...\n{trimmed}")
    };
    if base.chars().count() <= 4096 {
        return base;
    }
    let truncated = base.chars().take(4093).collect::<String>();
    format!("{truncated}...")
}
