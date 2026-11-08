use miette::{IntoDiagnostic, Result, miette};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(
    Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Opencode,
    Tavily,
    Exa,
    Brave,
    Serper,
    Firecrawl,
    Parallel,
}
impl Provider {
    pub const ALL: [Self; 7] = [
        Self::Opencode,
        Self::Tavily,
        Self::Exa,
        Self::Brave,
        Self::Serper,
        Self::Firecrawl,
        Self::Parallel,
    ];
    pub fn endpoint(self) -> &'static str {
        match self {
            Self::Opencode => "https://opencode.ai/console/api/websearch",
            Self::Tavily => "https://api.tavily.com/search",
            Self::Exa => "https://api.exa.ai/search",
            Self::Brave => "https://api.search.brave.com/res/v1/web/search",
            Self::Serper => "https://google.serper.dev/search",
            Self::Firecrawl => "https://api.firecrawl.dev/v2/search",
            Self::Parallel => "https://api.parallel.ai/v1/search",
        }
    }
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub provider: Option<Provider>,
    pub keys: BTreeMap<Provider, String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    pub provider: Provider,
    /// Missing/null preserves this provider's key; empty clears it.
    pub api_key: Option<String>,
}
pub struct Store {
    path: PathBuf,
    value: Mutex<Settings>,
}
impl Store {
    pub fn open(path: PathBuf) -> Result<Self> {
        let value = match std::fs::read(&path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|_| miette!("Invalid WebSearch settings"))?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Settings::default(),
            Err(error) => return Err(error).into_diagnostic(),
        };
        Ok(Self {
            path,
            value: Mutex::new(value),
        })
    }
    pub fn get(&self) -> Settings {
        self.value.lock().clone()
    }
    pub fn update(&self, patch: Patch) -> Result<Settings> {
        let mut value = self.value.lock();
        let mut candidate = value.clone();
        candidate.provider = Some(patch.provider);
        if let Some(key) = patch.api_key {
            let key = key.trim();
            if patch.provider == Provider::Opencode && !key.is_empty() {
                return Err(miette!("OpenCode WebSearch uses Console OAuth"));
            }
            if key.len() > 8192 || key.chars().any(char::is_control) {
                return Err(miette!("Invalid WebSearch credential"));
            }
            if key.is_empty() {
                candidate.keys.remove(&patch.provider);
            } else {
                candidate.keys.insert(patch.provider, key.to_owned());
            }
        }
        crate::persistence::write_bytes_atomic_sync(
            &self.path,
            &serde_json::to_vec(&candidate).into_diagnostic()?,
            crate::persistence::PersistenceFileMode::Private,
        )
        .into_diagnostic()?;
        *value = candidate.clone();
        Ok(candidate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settings_persist_provider_bound_keys_and_failed_writes_keep_live_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("websearch.json");
        let store = Store::open(path.clone()).unwrap();
        store
            .update(Patch {
                provider: Provider::Tavily,
                api_key: Some("secret-tavily".into()),
            })
            .unwrap();
        store
            .update(Patch {
                provider: Provider::Brave,
                api_key: Some("secret-brave".into()),
            })
            .unwrap();
        store
            .update(Patch {
                provider: Provider::Tavily,
                api_key: None,
            })
            .unwrap();
        let reopened = Store::open(path).unwrap();
        assert_eq!(reopened.get().provider, Some(Provider::Tavily));
        assert_eq!(reopened.get().keys[&Provider::Tavily], "secret-tavily");
        assert_eq!(reopened.get().keys[&Provider::Brave], "secret-brave");
        reopened
            .update(Patch {
                provider: Provider::Tavily,
                api_key: Some(String::new()),
            })
            .unwrap();
        assert!(!reopened.get().keys.contains_key(&Provider::Tavily));
        let blocked = dir.path().join("not-a-directory");
        std::fs::write(&blocked, b"file").unwrap();
        let store = Store {
            path: blocked.join("settings.json"),
            value: Mutex::new(Settings::default()),
        };
        assert!(
            store
                .update(Patch {
                    provider: Provider::Exa,
                    api_key: Some("key".into())
                })
                .is_err()
        );
        assert!(store.get().provider.is_none());
        assert!(store.get().keys.is_empty());
    }
}
