use std::{collections::HashMap, sync::Arc};

use parking_lot::Mutex;
use uuid::Uuid;

use crate::{device::DeviceId, tasks::Tasks};

#[derive(Clone, Default)]
pub struct DeviceTaskQueue {
    inner: Arc<Mutex<DeviceTaskQueueState>>,
}

#[derive(Default)]
struct DeviceTaskQueueState {
    order: Vec<String>,
    pending: HashMap<String, DeviceTaskRequest>,
    active: HashMap<String, Uuid>,
}

#[derive(Clone)]
struct DeviceTaskRequest {
    description: String,
}

impl DeviceTaskQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn enqueue_device_task(
        &self,
        device: DeviceId,
        key: impl Into<String>,
        description: impl Into<String>,
    ) {
        let dedupe_key = task_key(device, &key.into());
        let mut state = self.inner.lock();
        if !state.pending.contains_key(&dedupe_key) {
            state.order.push(dedupe_key.clone());
        }
        state.pending.insert(
            dedupe_key,
            DeviceTaskRequest {
                description: description.into(),
            },
        );
    }

    pub fn apply_to_tasks(&self, tasks: &mut Tasks) -> bool {
        let requests = {
            let mut state = self.inner.lock();
            let keys = state.order.drain(..).collect::<Vec<_>>();
            let mut requests = Vec::with_capacity(keys.len());
            for key in keys {
                if let Some(request) = state.pending.remove(&key) {
                    requests.push((key, request));
                }
            }
            requests
        };

        let mut changed = false;
        let mut state = self.inner.lock();
        for (key, request) in requests {
            let task_id = match state.active.get(&key).copied() {
                Some(existing_id) if tasks.contains_task(existing_id) => {
                    changed |=
                        tasks.update_task_description(existing_id, request.description.clone());
                    existing_id
                }
                _ => {
                    let id = tasks.add_task(request.description);
                    changed = true;
                    id
                }
            };
            state.active.insert(key, task_id);
        }
        changed
    }

    pub fn forget_task(&self, id: Uuid) {
        self.inner.lock().active.retain(|_, task_id| *task_id != id);
    }
}

fn task_key(device: DeviceId, key: &str) -> String {
    format!("{device}:{key}")
}
