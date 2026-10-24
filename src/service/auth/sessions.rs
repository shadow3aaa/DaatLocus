//! Profile-local, hashed browser sessions. Never contains passwords or cookie secrets.
use crate::{
    accounts::Metadata,
    actors::ActorSystem,
    persistence::{PersistenceFileMode, write_bytes_atomic_sync},
};
use miette::{IntoDiagnostic, Result, miette};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, io::Read, path::PathBuf};

pub(super) const SESSION_SECONDS: i64 = 30 * 24 * 60 * 60;
const RENEW_AFTER_MS: i64 = 60 * 60 * 1000;
const MAX_SESSIONS: usize = 4096;
const MAX_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error("Account session limit exceeded")]
pub(super) struct SessionLimit;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct BrowserSession {
    pub account: Metadata,
    pub expires_at_ms: i64,
    pub public_id: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    version: u8,
    profile: String,
    sessions: HashMap<String, BrowserSession>,
}

pub(super) struct Sessions {
    path: PathBuf,
    saved: Saved,
}

impl Sessions {
    pub fn open(system: &ActorSystem) -> Result<Self> {
        let directory = system.account_credentials_dir();
        std::fs::create_dir_all(&directory).into_diagnostic()?;
        crate::actors::permissions::protect_private_path(&directory)?;
        let path = directory.join("browser-sessions.json");
        let profile = system.store().public_stage_id()?.to_string();
        let mut saved = Saved {
            version: 1,
            profile: profile.clone(),
            sessions: HashMap::new(),
        };
        match std::fs::File::open(&path) {
            Ok(file) => {
                crate::actors::permissions::protect_private_path(&path)?;
                let mut bytes = Vec::new();
                file.take(MAX_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .into_diagnostic()?;
                if bytes.len() as u64 > MAX_BYTES {
                    return Err(miette!("Account session budget exceeded"));
                }
                let loaded: Saved = serde_json::from_slice(&bytes).into_diagnostic()?;
                if loaded.version != 1 || loaded.sessions.len() > MAX_SESSIONS {
                    return Err(miette!("Invalid account session store"));
                }
                // A copied profile's sessions never authorize this device.
                if loaded.profile == profile {
                    saved = loaded;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).into_diagnostic(),
        }
        Ok(Self { path, saved })
    }

    pub fn get(&self, key: &str, now: i64) -> Option<&BrowserSession> {
        self.saved
            .sessions
            .get(key)
            .filter(|session| session.expires_at_ms > now)
    }

    fn replace(&mut self, sessions: HashMap<String, BrowserSession>) -> Result<()> {
        let candidate = Saved {
            version: 1,
            profile: self.saved.profile.clone(),
            sessions,
        };
        let bytes = serde_json::to_vec(&candidate).into_diagnostic()?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(miette!("Account session budget exceeded"));
        }
        // Keep the bounded disk write and memory publication under the same lock.
        // Cancelling HTTP must not leave a detached writer racing a later logout.
        write_bytes_atomic_sync(&self.path, &bytes, PersistenceFileMode::Private)
            .into_diagnostic()?;
        self.saved = candidate;
        Ok(())
    }

    pub fn insert(
        &mut self,
        key: String,
        session: BrowserSession,
        previous: Option<String>,
        now: i64,
    ) -> Result<()> {
        let mut sessions = self.saved.sessions.clone();
        sessions.retain(|_, old| {
            old.expires_at_ms > now && old.account.binding == session.account.binding
        });
        if let Some(previous) = previous {
            sessions.remove(&previous);
        }
        if sessions.len() >= MAX_SESSIONS {
            return Err(SessionLimit.into());
        }
        sessions.insert(key, session);
        self.replace(sessions)
    }

    pub fn renew(&mut self, key: &str, now: i64) -> Result<Option<BrowserSession>> {
        let Some(mut session) = self.get(key, now).cloned() else {
            return Ok(None);
        };
        if session.expires_at_ms <= now + SESSION_SECONDS * 1000 - RENEW_AFTER_MS {
            session.expires_at_ms = now + SESSION_SECONDS * 1000;
            let mut sessions = self.saved.sessions.clone();
            sessions.retain(|_, old| old.expires_at_ms > now);
            sessions.insert(key.into(), session.clone());
            self.replace(sessions)?;
        }
        Ok(Some(session))
    }

    pub fn remove(&mut self, key: &str) -> Result<()> {
        if self.saved.sessions.contains_key(key) {
            let mut sessions = self.saved.sessions.clone();
            sessions.remove(key);
            self.replace(sessions)?;
        }
        Ok(())
    }

    pub fn clear(&mut self) -> Result<()> {
        self.replace(HashMap::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const DAY: i64 = 86_400_000;
    async fn fixture() -> (tempfile::TempDir, ActorSystem, Sessions) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("workspace")).unwrap();
        let system = ActorSystem::open_account_gated(
            dir.path().join("actors"),
            dir.path().join("workspace"),
            Default::default(),
        )
        .await
        .unwrap();
        let sessions = Sessions::open(&system).unwrap();
        (dir, system, sessions)
    }
    fn session(now: i64) -> BrowserSession {
        BrowserSession {
            account: Metadata {
                account_id: "a".repeat(64),
                name: "fixture".into(),
                revision: 1,
                binding: "b".repeat(64),
            },
            expires_at_ms: now + 30 * DAY,
            public_id: "stable-client-cache-marker".into(),
        }
    }
    #[tokio::test]
    async fn renewal_survives_reopen_and_idle_expiry_cannot_be_renewed() {
        let (_dir, system, mut sessions) = fixture().await;
        let start = 1_000_000;
        let key = "c".repeat(64);
        sessions
            .insert(key.clone(), session(start), None, start)
            .unwrap();
        let mut reopened = Sessions::open(&system).unwrap();
        assert!(reopened.get(&key, start + 13 * DAY).is_some());
        let renewed = reopened.renew(&key, start + 14 * DAY).unwrap().unwrap();
        assert_eq!(renewed.public_id, "stable-client-cache-marker");
        assert_eq!(renewed.expires_at_ms, start + 44 * DAY);
        let mut reopened = Sessions::open(&system).unwrap();
        assert!(reopened.get(&key, start + 30 * DAY).is_some());
        assert!(reopened.get(&key, start + 44 * DAY).is_none());
        assert!(reopened.renew(&key, start + 44 * DAY).unwrap().is_none());
        system.shutdown().await;
    }
    #[tokio::test]
    async fn failed_persistence_does_not_publish_a_login_renewal_or_logout() {
        let (dir, system, mut sessions) = fixture().await;
        let start = 1_000_000;
        let key = "c".repeat(64);
        sessions
            .insert(key.clone(), session(start), None, start)
            .unwrap();
        let blocked = dir.path().join("not-a-directory");
        std::fs::write(&blocked, b"blocks directory creation").unwrap();
        sessions.path = blocked.join("sessions.json");
        assert!(sessions.renew(&key, start + DAY).is_err());
        assert_eq!(
            sessions.get(&key, start).unwrap().expires_at_ms,
            start + 30 * DAY
        );
        assert!(sessions.remove(&key).is_err());
        assert!(sessions.clear().is_err());
        let other = "d".repeat(64);
        assert!(
            sessions
                .insert(other.clone(), session(start), Some(key.clone()), start)
                .is_err()
        );
        assert!(sessions.get(&other, start).is_none());
        assert!(sessions.get(&key, start).is_some());
        assert!(Sessions::open(&system).unwrap().get(&key, start).is_some());
        system.shutdown().await;
    }
    #[tokio::test]
    async fn copied_session_store_cannot_authenticate_another_profile() {
        let (_first_dir, first, mut source) = fixture().await;
        let (_second_dir, second, target) = fixture().await;
        let key = "c".repeat(64);
        source
            .insert(key.clone(), session(1_000_000), None, 1_000_000)
            .unwrap();
        std::fs::copy(&source.path, &target.path).unwrap();
        assert!(
            Sessions::open(&second)
                .unwrap()
                .get(&key, 1_000_000)
                .is_none()
        );
        first.shutdown().await;
        second.shutdown().await;
    }
}
