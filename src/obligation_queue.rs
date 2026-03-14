use std::{collections::HashMap, sync::Arc};

use parking_lot::Mutex;
use uuid::Uuid;

use crate::{
    obligations::{ObligationSource, ObligationStatus, Obligations, Urgency},
    projects::ReportTarget,
};

#[derive(Clone, Default)]
pub struct ObligationQueue {
    inner: Arc<Mutex<ObligationQueueState>>,
}

#[derive(Default)]
struct ObligationQueueState {
    events: Vec<ObligationEvent>,
    active: HashMap<String, Uuid>,
}

enum ObligationEvent {
    Upsert {
        dedupe_key: String,
        source: ObligationSource,
        summary: String,
        requires_reply: bool,
        urgency: Urgency,
        linked_project: Option<Uuid>,
        reply_target: Option<ReportTarget>,
    },
    SetStatus {
        dedupe_key: String,
        status: ObligationStatus,
    },
}

impl ObligationQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn upsert(
        &self,
        source: ObligationSource,
        key: impl Into<String>,
        summary: impl Into<String>,
        requires_reply: bool,
        urgency: Urgency,
        linked_project: Option<Uuid>,
        reply_target: Option<ReportTarget>,
    ) {
        self.inner.lock().events.push(ObligationEvent::Upsert {
            dedupe_key: dedupe_key(source, &key.into()),
            source,
            summary: summary.into(),
            requires_reply,
            urgency,
            linked_project,
            reply_target,
        });
    }

    pub fn set_status(
        &self,
        source: ObligationSource,
        key: impl Into<String>,
        status: ObligationStatus,
    ) {
        self.inner.lock().events.push(ObligationEvent::SetStatus {
            dedupe_key: dedupe_key(source, &key.into()),
            status,
        });
    }

    pub fn apply_to(&self, obligations: &mut Obligations) -> bool {
        let events = {
            let mut state = self.inner.lock();
            std::mem::take(&mut state.events)
        };

        let mut changed = false;
        let mut state = self.inner.lock();
        for event in events {
            match event {
                ObligationEvent::Upsert {
                    dedupe_key,
                    source,
                    summary,
                    requires_reply,
                    urgency,
                    linked_project,
                    reply_target,
                } => {
                    let id = match state.active.get(&dedupe_key).copied() {
                        Some(existing_id) if obligations.contains(existing_id) => {
                            obligations.upsert_existing(
                                existing_id,
                                summary,
                                requires_reply,
                                urgency,
                                linked_project,
                                reply_target,
                            );
                            existing_id
                        }
                        _ => obligations.add(
                            source,
                            summary,
                            requires_reply,
                            urgency,
                            linked_project,
                            reply_target,
                        ),
                    };
                    state.active.insert(dedupe_key, id);
                    changed = true;
                }
                ObligationEvent::SetStatus { dedupe_key, status } => {
                    let Some(id) = state.active.get(&dedupe_key).copied() else {
                        continue;
                    };
                    changed |= obligations.set_status(id, status);
                }
            }
        }
        changed
    }
}

fn dedupe_key(source: ObligationSource, key: &str) -> String {
    format!("{source}:{key}")
}
