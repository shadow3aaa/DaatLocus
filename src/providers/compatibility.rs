//! Process-local memory of successful protocol compatibility choices.
use std::{
    any::{Any, TypeId},
    collections::HashMap,
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use reqwest::header::HeaderMap;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

use crate::config::ModelConfig;

const IDLE_TTL: Duration = Duration::from_secs(30 * 60);
const MAX_ENTRIES: usize = 512;
type Key = (TypeId, [u8; 32]);
struct CachedEntry {
    value: Arc<dyn Any + Send + Sync>,
    last_used: Instant,
}
static ENTRIES: LazyLock<Mutex<HashMap<Key, CachedEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) struct CompatibilityCache<S> {
    identity: [u8; 32],
    marker: std::marker::PhantomData<S>,
}

struct Entry<S> {
    successful: Mutex<Option<S>>,
    probe: Arc<AsyncMutex<()>>,
}

pub(super) struct Negotiation<S> {
    pub(super) state: S,
    entry: Arc<Entry<S>>,
    permit: Option<OwnedMutexGuard<()>>,
}

fn fingerprint(parts: &[&[u8]]) -> [u8; 32] {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update(part.len().to_le_bytes());
        hash.update(part);
    }
    hash.finalize().into()
}

impl<S: Copy + PartialEq + Send + 'static> CompatibilityCache<S> {
    pub(super) fn new(protocol: &str, base_url: &str, model: &ModelConfig) -> Self {
        Self {
            identity: fingerprint(&[
                protocol.as_bytes(),
                base_url.as_bytes(),
                &serde_json::to_vec(model).expect("model configuration must serialize"),
            ]),
            marker: std::marker::PhantomData,
        }
    }

    pub(super) async fn begin(
        &self,
        api_key: &str,
        headers: &HeaderMap,
        initial: S,
    ) -> Negotiation<S> {
        // Credentials and headers distinguish tenants, but only their digest is retained.
        let mut headers = headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_bytes()))
            .collect::<Vec<_>>();
        headers.sort_unstable();
        let mut parts = vec![self.identity.as_slice(), api_key.as_bytes()];
        for (name, value) in &headers {
            parts.extend([name.as_bytes(), *value]);
        }
        let key = (TypeId::of::<S>(), fingerprint(&parts));
        let entry = {
            let now = Instant::now();
            let mut entries = ENTRIES.lock();
            entries.retain(|_, entry| {
                now.duration_since(entry.last_used) < IDLE_TTL
                    || Arc::strong_count(&entry.value) > 1
            });
            if !entries.contains_key(&key) && entries.len() >= MAX_ENTRIES {
                let oldest = entries
                    .iter()
                    .filter(|(_, entry)| Arc::strong_count(&entry.value) == 1)
                    .min_by_key(|(_, entry)| entry.last_used)
                    .map(|(key, _)| *key);
                if let Some(oldest) = oldest {
                    entries.remove(&oldest);
                }
            }
            let cached = entries.entry(key).or_insert_with(|| CachedEntry {
                value: Arc::new(Entry::<S> {
                    successful: Mutex::new(None),
                    probe: Arc::new(AsyncMutex::new(())),
                }),
                last_used: now,
            });
            cached.last_used = now;
            cached
                .value
                .clone()
                .downcast::<Entry<S>>()
                .expect("typed compatibility entry")
        };
        let successful = *entry.successful.lock();
        let mut negotiation = Negotiation {
            state: successful.unwrap_or(initial),
            entry,
            permit: None,
        };
        if successful.is_none() {
            negotiation.refresh_after_failure().await;
        }
        negotiation
    }
}

impl<S: Copy + PartialEq> Negotiation<S> {
    /// Serialize cold probes and renewed negotiation, reusing another request's success.
    /// Warm requests do not hold the probe lock during HTTP I/O.
    pub(super) async fn refresh_after_failure(&mut self) -> bool {
        if self.permit.is_some() {
            return false;
        }
        self.permit = Some(self.entry.probe.clone().lock_owned().await);
        if let Some(state) = *self.entry.successful.lock()
            && state != self.state
        {
            self.state = state;
            return true;
        }
        false
    }

    pub(super) fn succeeded(&mut self) {
        if self.permit.is_some() {
            *self.entry.successful.lock() = Some(self.state);
            self.permit.take();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cache_identity_isolates_configuration_protocol_and_tenant_changes() {
        let base = format!("https://{}.invalid/v1", uuid::Uuid::new_v4());
        let model = ModelConfig::default();
        let headers = HeaderMap::new();
        let cache = CompatibilityCache::<u8>::new("chat", &base, &model);
        let mut first = cache.begin("key", &headers, 0).await;
        first.state = 7;
        first.succeeded();
        drop(first);
        let recreated = CompatibilityCache::<u8>::new("chat", &base, &model);
        assert_eq!(recreated.begin("key", &headers, 0).await.state, 7);

        let mut variants = vec![];
        let mut changed = model.clone();
        changed.model_id = "another-model".into();
        variants.push(changed);
        let mut changed = model.clone();
        changed.provider = "another-provider".into();
        variants.push(changed);
        let mut changed = model.clone();
        changed.thinking_budget = Some(crate::config::ThinkingBudget::new("high"));
        variants.push(changed);
        let mut changed = model.clone();
        changed.supports_vision = Some(true);
        variants.push(changed);
        let mut changed = model.clone();
        changed.api_style = Some("responses".into());
        variants.push(changed);
        let mut changed = model.clone();
        changed.temperature = 0.5;
        variants.push(changed);
        for changed in variants {
            let isolated = CompatibilityCache::<u8>::new("chat", &base, &changed);
            assert_eq!(isolated.begin("key", &headers, 0).await.state, 0);
        }
        for (protocol, url) in [
            ("responses", base.clone()),
            ("chat", format!("{base}/other")),
        ] {
            let isolated = CompatibilityCache::<u8>::new(protocol, &url, &model);
            assert_eq!(isolated.begin("key", &headers, 0).await.state, 0);
        }
        assert_eq!(cache.begin("other-key", &headers, 0).await.state, 0);
        let mut other_headers = HeaderMap::new();
        other_headers.insert("x-tenant", "other".parse().unwrap());
        assert_eq!(cache.begin("key", &other_headers, 0).await.state, 0);
        assert_eq!(cache.begin("key", &headers, 0).await.state, 7);
    }

    #[tokio::test]
    async fn failed_probes_and_stale_successes_cannot_poison_the_cache() {
        let base = format!("https://{}.invalid", uuid::Uuid::new_v4());
        let cache = CompatibilityCache::<u8>::new("chat", &base, &ModelConfig::default());
        let headers = HeaderMap::new();
        let mut failed = cache.begin("key", &headers, 0).await;
        failed.state = 1;
        drop(failed);
        let mut first = cache.begin("key", &headers, 0).await;
        assert_eq!(
            first.state, 0,
            "an abandoned probe must not publish its attempted state"
        );
        first.succeeded();
        let mut stale = cache.begin("key", &headers, 0).await;
        let mut changed = cache.begin("key", &headers, 0).await;
        assert!(!changed.refresh_after_failure().await);
        changed.state = 2;
        changed.succeeded();
        stale.succeeded();
        assert_eq!(cache.begin("key", &headers, 0).await.state, 2);
        assert!(stale.refresh_after_failure().await);
        assert_eq!(
            stale.state, 2,
            "another request's newer success is reused before probing"
        );
    }
}
