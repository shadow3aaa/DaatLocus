use std::{collections::HashMap, fmt::Display, path::PathBuf, sync::Arc};

use chrono::Utc;
use miette::{Result, miette};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::spinova_paths::spinova_paths;

const EVENTS_FILE_NAME: &str = "events";

#[derive(Clone)]
pub struct EventStore {
    inner: Arc<Mutex<EventStoreInner>>,
}

struct EventStoreInner {
    path: PathBuf,
    state: PersistedEventStore,
}

#[derive(Default, Serialize, Deserialize)]
struct PersistedEventStore {
    order: Vec<Uuid>,
    events: HashMap<Uuid, Event>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Event {
    pub source: EventSource,
    pub status: EventStatus,
    pub arrived_at_ms: i64,
    pub last_updated_at_ms: i64,
    pub payload: EventPayload,
    pub last_error: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventSource {
    Telegram,
    Terminal,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventStatus {
    Pending,
    Claimed,
    AwaitingDelivery,
    Resolved,
    Dismissed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum EventDisposition {
    Resolved,
    Dismissed,
    Failed,
}

#[derive(Clone, Serialize, Deserialize)]
pub enum EventPayload {
    TelegramIncoming(TelegramIncomingEvent),
}

#[derive(Clone, Serialize, Deserialize)]
pub struct TelegramIncomingEvent {
    pub chat_id: String,
    pub chat_title: String,
    pub sender: String,
    pub incoming_text: String,
    pub telegram_update_id: i64,
    pub telegram_message_id: Option<i64>,
    pub telegram_message_date: Option<i64>,
}

#[derive(Clone)]
pub struct EventView {
    pub event_id: Uuid,
    pub source: EventSource,
    pub status: EventStatus,
    pub arrived_at_ms: i64,
    pub payload: EventPayload,
    pub last_error: Option<String>,
}

impl EventStore {
    pub async fn new() -> Self {
        let path = spinova_paths().await.state_file(EVENTS_FILE_NAME);
        let mut state = tokio::fs::read(&path)
            .await
            .ok()
            .and_then(|bytes| postcard::from_bytes::<PersistedEventStore>(&bytes).ok())
            .unwrap_or_default();
        for event in state.events.values_mut() {
            if matches!(event.status, EventStatus::Claimed) {
                event.status = EventStatus::Pending;
            }
        }
        Self {
            inner: Arc::new(Mutex::new(EventStoreInner { path, state })),
        }
    }

    pub fn register_telegram_incoming(&self, event: TelegramIncomingEvent) -> Result<Uuid> {
        let mut inner = self.inner.lock();
        let now = Utc::now().timestamp_millis();

        if let Some(existing_id) = find_existing_telegram_event(&inner.state, &event) {
            if let Some(existing) = inner.state.events.get_mut(&existing_id) {
                let EventPayload::TelegramIncoming(existing_payload) = &mut existing.payload;
                existing_payload.chat_title = event.chat_title;
                existing_payload.sender = event.sender;
                existing_payload.incoming_text = event.incoming_text;
                existing_payload.telegram_message_id = existing_payload
                    .telegram_message_id
                    .or(event.telegram_message_id);
                existing_payload.telegram_message_date = existing_payload
                    .telegram_message_date
                    .or(event.telegram_message_date);
                existing.last_updated_at_ms = now;
            }
            persist_locked(&inner)?;
            return Ok(existing_id);
        }

        let event_id = Uuid::new_v4();
        inner.state.order.push(event_id);
        inner.state.events.insert(
            event_id,
            Event {
                source: EventSource::Telegram,
                status: EventStatus::Pending,
                arrived_at_ms: now,
                last_updated_at_ms: now,
                payload: EventPayload::TelegramIncoming(event),
                last_error: None,
            },
        );
        persist_locked(&inner)?;
        Ok(event_id)
    }

    pub fn claim_event_if_pending(&self, event_id: Uuid) -> Result<Option<EventView>> {
        let mut inner = self.inner.lock();
        let Some(event) = inner.state.events.get_mut(&event_id) else {
            return Ok(None);
        };
        if event.status != EventStatus::Pending {
            return Ok(None);
        }

        event.status = EventStatus::Claimed;
        event.last_updated_at_ms = Utc::now().timestamp_millis();
        let view = EventView {
            event_id,
            source: event.source,
            status: event.status,
            arrived_at_ms: event.arrived_at_ms,
            payload: event.payload.clone(),
            last_error: event.last_error.clone(),
        };
        persist_locked(&inner)?;
        Ok(Some(view))
    }

    pub fn attention_events(&self) -> Vec<EventView> {
        let inner = self.inner.lock();
        let mut views = inner
            .state
            .order
            .iter()
            .rev()
            .filter_map(|event_id| {
                let event = inner.state.events.get(event_id)?;
                event.status.requires_attention().then(|| EventView {
                    event_id: *event_id,
                    source: event.source,
                    status: event.status,
                    arrived_at_ms: event.arrived_at_ms,
                    payload: event.payload.clone(),
                    last_error: event.last_error.clone(),
                })
            })
            .collect::<Vec<_>>();
        views.sort_by(|left, right| {
            right
                .arrived_at_ms
                .cmp(&left.arrived_at_ms)
                .then_with(|| left.event_id.cmp(&right.event_id))
        });
        views
    }

    pub fn driver_event_statuses(&self) -> Vec<(Uuid, EventStatus)> {
        let inner = self.inner.lock();
        inner
            .state
            .order
            .iter()
            .filter_map(|event_id| {
                inner
                    .state
                    .events
                    .get(event_id)
                    .map(|event| (*event_id, event.status))
            })
            .collect()
    }

    pub fn view(&self, event_id: &str) -> Result<EventView> {
        let event_id = Uuid::parse_str(event_id)
            .map_err(|err| miette!("invalid event id {event_id}: {err}"))?;
        let inner = self.inner.lock();
        let event = inner
            .state
            .events
            .get(&event_id)
            .ok_or_else(|| miette!("unknown event: {event_id}"))?;
        Ok(EventView {
            event_id,
            source: event.source,
            status: event.status,
            arrived_at_ms: event.arrived_at_ms,
            payload: event.payload.clone(),
            last_error: event.last_error.clone(),
        })
    }

    pub fn prepare_telegram_delivery(&self, event_id: &str) -> Result<()> {
        self.with_event_mut_from_str(event_id, |event| {
            if !matches!(
                event.status,
                EventStatus::Pending | EventStatus::Claimed | EventStatus::Failed
            ) {
                return Err(miette!(
                    "event {event_id} cannot enter delivery from status {}",
                    event.status
                ));
            }
            event.status = EventStatus::AwaitingDelivery;
            event.last_error = None;
            Ok(())
        })
    }

    pub fn set_status(
        &self,
        event_id: &str,
        status: EventStatus,
        note: Option<String>,
    ) -> Result<()> {
        self.with_event_mut_from_str(event_id, |event| {
            event.status = status;
            event.last_error = if matches!(status, EventStatus::Failed) {
                note
            } else {
                None
            };
            Ok(())
        })
    }

    pub fn mark_delivery_failed(&self, event_id: &str, reason: impl Into<String>) -> Result<()> {
        self.set_status(event_id, EventStatus::Failed, Some(reason.into()))
    }

    pub fn requeue_if_claimed(&self, event_id: &str) -> Result<bool> {
        self.with_event_mut_from_str(event_id, |event| {
            if event.status != EventStatus::Claimed {
                return Ok(false);
            }
            event.status = EventStatus::Pending;
            event.last_error = None;
            Ok(true)
        })
    }

    pub async fn shutdown(self) {
        let inner = self.inner.lock();
        let _ = persist_locked(&inner);
    }

    fn with_event_mut_from_str<T>(
        &self,
        event_id: &str,
        f: impl FnOnce(&mut Event) -> Result<T>,
    ) -> Result<T> {
        let event_id = Uuid::parse_str(event_id)
            .map_err(|err| miette!("invalid event id {event_id}: {err}"))?;
        self.with_event_mut(event_id, f)
    }

    fn with_event_mut<T>(
        &self,
        event_id: Uuid,
        f: impl FnOnce(&mut Event) -> Result<T>,
    ) -> Result<T> {
        let mut inner = self.inner.lock();
        let event = inner
            .state
            .events
            .get_mut(&event_id)
            .ok_or_else(|| miette!("unknown event: {event_id}"))?;
        let result = f(event)?;
        event.last_updated_at_ms = Utc::now().timestamp_millis();
        persist_locked(&inner)?;
        Ok(result)
    }
}

impl EventStatus {
    pub fn requires_attention(self) -> bool {
        matches!(self, Self::Pending)
    }
}

impl Display for EventSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Telegram => write!(f, "Telegram"),
            Self::Terminal => write!(f, "Terminal"),
            Self::System => write!(f, "System"),
        }
    }
}

impl Display for EventStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "Pending"),
            Self::Claimed => write!(f, "Claimed"),
            Self::AwaitingDelivery => write!(f, "AwaitingDelivery"),
            Self::Resolved => write!(f, "Resolved"),
            Self::Dismissed => write!(f, "Dismissed"),
            Self::Failed => write!(f, "Failed"),
        }
    }
}

fn find_existing_telegram_event(
    state: &PersistedEventStore,
    incoming: &TelegramIncomingEvent,
) -> Option<Uuid> {
    state.order.iter().rev().find_map(|event_id| {
        let event = state.events.get(event_id)?;
        let EventPayload::TelegramIncoming(existing) = &event.payload;
        if existing.telegram_update_id == incoming.telegram_update_id {
            return Some(*event_id);
        }
        if existing.chat_id == incoming.chat_id
            && existing.telegram_message_id.is_some()
            && existing.telegram_message_id == incoming.telegram_message_id
        {
            return Some(*event_id);
        }
        None
    })
}

fn persist_locked(inner: &EventStoreInner) -> Result<()> {
    let bytes = postcard::to_allocvec(&inner.state)
        .map_err(|err| miette!("serialize events failed: {err}"))?;
    std::fs::write(&inner.path, bytes).map_err(|err| miette!("write events file failed: {err}"))?;
    Ok(())
}
