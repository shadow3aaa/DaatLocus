use std::{collections::HashMap, fmt::Display, path::PathBuf, sync::Arc};

use chrono::Utc;
use miette::{Result, miette};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::persistence::PersistenceStore;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    TerminalIncoming(TerminalIncomingEvent),
}

#[derive(Clone, Serialize, Deserialize)]
pub struct TelegramIncomingEvent {
    pub chat_id: String,
    #[serde(default = "default_telegram_chat_kind")]
    pub chat_kind: String,
    pub chat_title: String,
    pub sender: String,
    pub incoming_text: String,
    pub telegram_update_id: i64,
    pub telegram_message_id: Option<i64>,
    pub telegram_message_date: Option<i64>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct TerminalIncomingEvent {
    #[serde(default = "default_terminal_origin")]
    pub origin: String,
    pub incoming_text: String,
}

fn default_telegram_chat_kind() -> String {
    "unknown".to_string()
}

fn default_terminal_origin() -> String {
    "dashboard".to_string()
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
        let persistence = PersistenceStore::runtime().await;
        let path = persistence.state_file(EVENTS_FILE_NAME);
        let mut state: PersistedEventStore = persistence
            .read_postcard_state_or_default(EVENTS_FILE_NAME, "events")
            .await;
        reset_claimed_events_on_startup(&mut state);
        Self {
            inner: Arc::new(Mutex::new(EventStoreInner { path, state })),
        }
    }

    pub fn register_telegram_incoming(&self, event: TelegramIncomingEvent) -> Result<Uuid> {
        let mut inner = self.inner.lock();
        let now = Utc::now().timestamp_millis();

        if let Some(existing_id) = find_existing_telegram_event(&inner.state, &event) {
            if let Some(existing) = inner.state.events.get_mut(&existing_id) {
                let EventPayload::TelegramIncoming(existing_payload) = &mut existing.payload else {
                    return Err(miette!(
                        "existing telegram event payload had unexpected type"
                    ));
                };
                existing_payload.chat_kind = event.chat_kind;
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

    pub fn register_terminal_incoming(&self, event: TerminalIncomingEvent) -> Result<Uuid> {
        let mut inner = self.inner.lock();
        let now = Utc::now().timestamp_millis();
        let event_id = Uuid::new_v4();
        inner.state.order.push(event_id);
        inner.state.events.insert(
            event_id,
            Event {
                source: EventSource::Terminal,
                status: EventStatus::Pending,
                arrived_at_ms: now,
                last_updated_at_ms: now,
                payload: EventPayload::TerminalIncoming(event),
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
            event.last_error =
                if matches!(status, EventStatus::Failed | EventStatus::AwaitingDelivery) {
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

    pub fn clear_all(&self) -> Result<usize> {
        let mut inner = self.inner.lock();
        let cleared = inner.state.events.len();
        if cleared == 0 && inner.state.order.is_empty() {
            return Ok(0);
        }
        inner.state.events.clear();
        inner.state.order.clear();
        persist_locked(&inner)?;
        Ok(cleared)
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
        let EventPayload::TelegramIncoming(existing) = &event.payload else {
            return None;
        };
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
    crate::persistence::write_postcard_atomic_sync(
        &inner.path,
        &inner.state,
        crate::persistence::PersistenceFileMode::Default,
    )
    .map_err(|err| miette!("write events file failed: {err}"))
}

fn reset_claimed_events_on_startup(state: &mut PersistedEventStore) {
    for event in state.events.values_mut() {
        if matches!(event.status, EventStatus::Claimed) {
            event.status = EventStatus::Pending;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_EVENT_COUNTER: AtomicU64 = AtomicU64::new(1);

    fn test_store() -> EventStore {
        let unique = TEST_EVENT_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "daat-locus-events-test-{}-{}.bin",
            std::process::id(),
            unique
        ));
        let _ = std::fs::remove_file(&path);
        EventStore {
            inner: Arc::new(Mutex::new(EventStoreInner {
                path,
                state: PersistedEventStore::default(),
            })),
        }
    }

    #[test]
    fn persisted_event_store_postcard_round_trips() {
        let telegram_id =
            Uuid::parse_str("11111111-1111-4111-8111-111111111111").expect("telegram uuid");
        let terminal_id =
            Uuid::parse_str("22222222-2222-4222-8222-222222222222").expect("terminal uuid");
        let mut events = HashMap::new();
        events.insert(
            telegram_id,
            Event {
                source: EventSource::Telegram,
                status: EventStatus::AwaitingDelivery,
                arrived_at_ms: 10,
                last_updated_at_ms: 20,
                payload: EventPayload::TelegramIncoming(TelegramIncomingEvent {
                    chat_id: "chat-1".to_string(),
                    chat_kind: "private".to_string(),
                    chat_title: "Alice".to_string(),
                    sender: "alice".to_string(),
                    incoming_text: "ping".to_string(),
                    telegram_update_id: 42,
                    telegram_message_id: Some(100),
                    telegram_message_date: Some(200),
                }),
                last_error: Some("queued for delivery".to_string()),
            },
        );
        events.insert(
            terminal_id,
            Event {
                source: EventSource::Terminal,
                status: EventStatus::Failed,
                arrived_at_ms: 30,
                last_updated_at_ms: 40,
                payload: EventPayload::TerminalIncoming(TerminalIncomingEvent {
                    origin: "dashboard".to_string(),
                    incoming_text: "local command".to_string(),
                }),
                last_error: Some("failed locally".to_string()),
            },
        );
        let state = PersistedEventStore {
            order: vec![telegram_id, terminal_id],
            events,
        };

        let bytes = postcard::to_allocvec(&state).expect("encode events");
        let restored: PersistedEventStore = postcard::from_bytes(&bytes).expect("decode events");

        assert_eq!(restored.order, vec![telegram_id, terminal_id]);
        let telegram = restored.events.get(&telegram_id).expect("telegram event");
        assert_eq!(telegram.source, EventSource::Telegram);
        assert_eq!(telegram.status, EventStatus::AwaitingDelivery);
        assert_eq!(telegram.last_error.as_deref(), Some("queued for delivery"));
        match &telegram.payload {
            EventPayload::TelegramIncoming(payload) => {
                assert_eq!(payload.chat_id, "chat-1");
                assert_eq!(payload.chat_kind, "private");
                assert_eq!(payload.telegram_message_id, Some(100));
                assert_eq!(payload.telegram_message_date, Some(200));
            }
            EventPayload::TerminalIncoming(_) => panic!("expected telegram payload"),
        }
        let terminal = restored.events.get(&terminal_id).expect("terminal event");
        assert_eq!(terminal.source, EventSource::Terminal);
        assert_eq!(terminal.status, EventStatus::Failed);
        assert_eq!(terminal.last_error.as_deref(), Some("failed locally"));
        match &terminal.payload {
            EventPayload::TerminalIncoming(payload) => {
                assert_eq!(payload.origin, "dashboard");
                assert_eq!(payload.incoming_text, "local command");
            }
            EventPayload::TelegramIncoming(_) => panic!("expected terminal payload"),
        }
    }

    #[test]
    fn register_terminal_incoming_creates_terminal_event() {
        let store = test_store();
        let event_id = store
            .register_terminal_incoming(TerminalIncomingEvent {
                origin: "dashboard".to_string(),
                incoming_text: "hello from terminal".to_string(),
            })
            .expect("register terminal event");
        let event = store
            .view(&event_id.to_string())
            .expect("view terminal event");
        assert_eq!(event.source, EventSource::Terminal);
        assert_eq!(event.status, EventStatus::Pending);
        match event.payload {
            EventPayload::TerminalIncoming(payload) => {
                assert_eq!(payload.origin, "dashboard");
                assert_eq!(payload.incoming_text, "hello from terminal");
            }
            EventPayload::TelegramIncoming(_) => panic!("expected terminal payload"),
        }
    }

    #[test]
    fn startup_requeues_claimed_events() {
        let event_id = Uuid::new_v4();
        let mut state = PersistedEventStore::default();
        state.order.push(event_id);
        state.events.insert(
            event_id,
            Event {
                source: EventSource::Terminal,
                status: EventStatus::Claimed,
                arrived_at_ms: 1,
                last_updated_at_ms: 1,
                payload: EventPayload::TerminalIncoming(TerminalIncomingEvent {
                    origin: "dashboard".to_string(),
                    incoming_text: "finish this".to_string(),
                }),
                last_error: None,
            },
        );

        reset_claimed_events_on_startup(&mut state);

        assert_eq!(
            state.events.get(&event_id).map(|event| event.status),
            Some(EventStatus::Pending)
        );
    }

    #[test]
    fn requeue_if_claimed_restores_pending_attention() {
        let store = test_store();
        let event_id = store
            .register_terminal_incoming(TerminalIncomingEvent {
                origin: "dashboard".to_string(),
                incoming_text: "still needs work".to_string(),
            })
            .expect("register terminal event");
        let claimed = store
            .claim_event_if_pending(event_id)
            .expect("claim event")
            .expect("pending event should claim");
        assert_eq!(claimed.status, EventStatus::Claimed);

        assert!(
            store
                .requeue_if_claimed(&event_id.to_string())
                .expect("requeue claimed event")
        );

        let event = store.view(&event_id.to_string()).expect("view event");
        assert_eq!(event.status, EventStatus::Pending);
        assert!(event.last_error.is_none());
    }

    #[test]
    fn failed_event_status_keeps_explicit_reason() {
        let store = test_store();
        let event_id = store
            .register_terminal_incoming(TerminalIncomingEvent {
                origin: "dashboard".to_string(),
                incoming_text: "cannot complete".to_string(),
            })
            .expect("register terminal event");

        store
            .set_status(
                &event_id.to_string(),
                EventStatus::Failed,
                Some("runtime context overflow persisted after 3 attempts".to_string()),
            )
            .expect("mark failed");

        let event = store.view(&event_id.to_string()).expect("view event");
        assert_eq!(event.status, EventStatus::Failed);
        assert_eq!(
            event.last_error.as_deref(),
            Some("runtime context overflow persisted after 3 attempts")
        );
    }

    #[test]
    fn terminal_event_can_resolve_without_delivery_handoff() {
        let store = test_store();
        let event_id = store
            .register_terminal_incoming(TerminalIncomingEvent {
                origin: "dashboard".to_string(),
                incoming_text: "local runtime question".to_string(),
            })
            .expect("register terminal event");
        let _ = store
            .claim_event_if_pending(event_id)
            .expect("claim terminal event")
            .expect("terminal event should claim");

        store
            .set_status(&event_id.to_string(), EventStatus::Resolved, None)
            .expect("resolve terminal event");

        let event = store.view(&event_id.to_string()).expect("view event");
        assert_eq!(event.status, EventStatus::Resolved);
        assert!(event.last_error.is_none());
    }

    #[test]
    fn clear_all_removes_events_in_every_status() {
        let store = test_store();
        let pending = store
            .register_terminal_incoming(TerminalIncomingEvent {
                origin: "dashboard".to_string(),
                incoming_text: "pending".to_string(),
            })
            .expect("register pending");
        let claimed = store
            .register_terminal_incoming(TerminalIncomingEvent {
                origin: "dashboard".to_string(),
                incoming_text: "claimed".to_string(),
            })
            .expect("register claimed");
        store
            .claim_event_if_pending(claimed)
            .expect("claim")
            .expect("event should claim");
        let awaiting = store
            .register_terminal_incoming(TerminalIncomingEvent {
                origin: "dashboard".to_string(),
                incoming_text: "awaiting".to_string(),
            })
            .expect("register awaiting");
        store
            .set_status(
                &awaiting.to_string(),
                EventStatus::AwaitingDelivery,
                Some("queued".to_string()),
            )
            .expect("mark awaiting");
        let resolved = store
            .register_terminal_incoming(TerminalIncomingEvent {
                origin: "dashboard".to_string(),
                incoming_text: "resolved".to_string(),
            })
            .expect("register resolved");
        store
            .set_status(&resolved.to_string(), EventStatus::Resolved, None)
            .expect("mark resolved");
        let dismissed = store
            .register_terminal_incoming(TerminalIncomingEvent {
                origin: "dashboard".to_string(),
                incoming_text: "dismissed".to_string(),
            })
            .expect("register dismissed");
        store
            .set_status(&dismissed.to_string(), EventStatus::Dismissed, None)
            .expect("mark dismissed");
        let failed = store
            .register_terminal_incoming(TerminalIncomingEvent {
                origin: "dashboard".to_string(),
                incoming_text: "failed".to_string(),
            })
            .expect("register failed");
        store
            .set_status(
                &failed.to_string(),
                EventStatus::Failed,
                Some("failed".to_string()),
            )
            .expect("mark failed");

        assert_eq!(store.clear_all().expect("clear events"), 6);
        assert!(store.driver_event_statuses().is_empty());
        assert!(store.view(&pending.to_string()).is_err());
    }
}
