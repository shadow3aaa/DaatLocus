//! Session metadata and registry persistence for the Manager daemon.

use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use miette::{Result, miette};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::daat_locus_paths::{DaatLocusPaths, daat_locus_paths};

const SESSION_REGISTRY_FILE_NAME: &str = "sessions.json";
const SESSION_IPC_DIR_NAME: &str = "sessions-ipc";
const SESSION_IPC_TOKEN_FILE_NAME: &str = "ipc-token";
const TELEGRAM_SESSION_DEFAULTS_FILE_NAME: &str = "telegram-session-defaults.json";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    pub fn from_string(value: String) -> Result<Self> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(miette!("session_id cannot be empty"));
        }
        Ok(Self(trimmed.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionScope {
    General,
    Project { project_dir: PathBuf },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Dormant,
    Starting,
    Ready,
    Stopping,
    Dead,
    Failed,
}

impl SessionStatus {
    pub fn is_process_backed(self) -> bool {
        matches!(self, Self::Starting | Self::Ready | Self::Stopping)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: SessionId,
    pub scope: SessionScope,
    pub pid: Option<u32>,
    pub status: SessionStatus,
    pub ipc_name: Option<String>,
    pub ipc_token_hash: Option<String>,
    pub project_dir: Option<PathBuf>,
    pub title: Option<String>,
    pub started_at_ms: i64,
    pub last_seen_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session_id: SessionId,
    pub scope: SessionScope,
    pub project_dir: Option<PathBuf>,
    pub title: Option<String>,
    pub started_at_ms: i64,
    pub last_seen_at_ms: Option<i64>,
}

impl From<SessionInfo> for SessionSummary {
    fn from(info: SessionInfo) -> Self {
        Self {
            session_id: info.session_id,
            scope: info.scope,
            project_dir: info.project_dir,
            title: info.title,
            started_at_ms: info.started_at_ms,
            last_seen_at_ms: info.last_seen_at_ms,
        }
    }
}

impl SessionInfo {
    pub fn new(session_id: SessionId, scope: SessionScope, title: Option<String>) -> Self {
        let project_dir = match &scope {
            SessionScope::General => None,
            SessionScope::Project { project_dir } => Some(project_dir.clone()),
        };
        Self {
            session_id,
            scope,
            pid: None,
            status: SessionStatus::Dormant,
            ipc_name: None,
            ipc_token_hash: None,
            project_dir,
            title,
            started_at_ms: chrono::Utc::now().timestamp_millis(),
            last_seen_at_ms: None,
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct PersistedSessionRegistry {
    sessions: BTreeMap<SessionId, SessionInfo>,
}

#[derive(Default, Serialize, Deserialize)]
struct PersistedTelegramSessionDefaults {
    chats: BTreeMap<String, SessionId>,
}

#[derive(Clone)]
pub struct SessionRegistry {
    path: PathBuf,
    inner: Arc<RwLock<PersistedSessionRegistry>>,
}

impl SessionRegistry {
    pub async fn load() -> Result<Self> {
        let paths = daat_locus_paths().await;
        let path = session_registry_path(&paths);
        Self::load_from_path(path).await
    }

    async fn load_from_path(path: PathBuf) -> Result<Self> {
        let state = match tokio::fs::read(&path).await {
            Ok(bytes) => {
                serde_json::from_slice::<PersistedSessionRegistry>(&bytes).map_err(|err| {
                    miette!("decode session registry {} failed: {err}", path.display())
                })?
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                PersistedSessionRegistry::default()
            }
            Err(err) => {
                return Err(miette!(
                    "read session registry {} failed: {err}",
                    path.display()
                ));
            }
        };
        Ok(Self {
            path,
            inner: Arc::new(RwLock::new(state)),
        })
    }

    pub fn list(&self) -> Vec<SessionInfo> {
        self.inner.read().sessions.values().cloned().collect()
    }

    pub fn get(&self, session_id: &SessionId) -> Option<SessionInfo> {
        self.inner.read().sessions.get(session_id).cloned()
    }

    pub async fn create(&self, scope: SessionScope, title: Option<String>) -> Result<SessionInfo> {
        let info = SessionInfo::new(SessionId::new(), scope, normalize_title(title));
        self.insert(info.clone()).await?;
        Ok(info)
    }

    pub async fn insert(&self, info: SessionInfo) -> Result<()> {
        {
            let mut inner = self.inner.write();
            inner.sessions.insert(info.session_id.clone(), info);
        }
        self.persist().await
    }

    pub async fn remove(&self, session_id: &SessionId) -> Result<Option<SessionInfo>> {
        let removed = {
            let mut inner = self.inner.write();
            inner.sessions.remove(session_id)
        };
        if removed.is_some() {
            self.persist().await?;
        }
        Ok(removed)
    }

    pub async fn set_title(&self, session_id: &SessionId, title: String) -> Result<bool> {
        let changed = {
            let mut inner = self.inner.write();
            let Some(info) = inner.sessions.get_mut(session_id) else {
                return Ok(false);
            };
            info.title = normalize_title(Some(title));
            true
        };
        self.persist().await?;
        Ok(changed)
    }

    pub async fn mark_starting(
        &self,
        session_id: &SessionId,
        pid: u32,
        ipc_name: String,
        ipc_token: &str,
    ) -> Result<()> {
        self.update(session_id, |info| {
            info.pid = Some(pid);
            info.status = SessionStatus::Starting;
            info.ipc_name = Some(ipc_name);
            info.ipc_token_hash = Some(hash_ipc_token(ipc_token));
            info.last_seen_at_ms = Some(chrono::Utc::now().timestamp_millis());
        })
        .await
    }

    pub async fn mark_ready(&self, session_id: &SessionId) -> Result<()> {
        self.update(session_id, |info| {
            info.status = SessionStatus::Ready;
            info.last_seen_at_ms = Some(chrono::Utc::now().timestamp_millis());
        })
        .await
    }

    pub async fn mark_dead(&self, session_id: &SessionId) -> Result<()> {
        self.update(session_id, |info| {
            info.pid = None;
            info.status = SessionStatus::Dead;
            info.ipc_name = None;
            info.ipc_token_hash = None;
            info.last_seen_at_ms = Some(chrono::Utc::now().timestamp_millis());
        })
        .await
    }

    async fn update<F>(&self, session_id: &SessionId, update: F) -> Result<()>
    where
        F: FnOnce(&mut SessionInfo),
    {
        {
            let mut inner = self.inner.write();
            let Some(info) = inner.sessions.get_mut(session_id) else {
                return Err(miette!("unknown session `{session_id}`"));
            };
            update(info);
        }
        self.persist().await
    }

    async fn persist(&self) -> Result<()> {
        let bytes = {
            let inner = self.inner.read();
            serde_json::to_vec_pretty(&*inner)
                .map_err(|err| miette!("encode session registry failed: {err}"))?
        };
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|err| {
                miette!(
                    "create session registry dir {} failed: {err}",
                    parent.display()
                )
            })?;
        }
        let tmp = self.path.with_extension("json.tmp");
        tokio::fs::write(&tmp, bytes).await.map_err(|err| {
            miette!(
                "write session registry temp {} failed: {err}",
                tmp.display()
            )
        })?;
        tokio::fs::rename(&tmp, &self.path).await.map_err(|err| {
            miette!(
                "replace session registry {} failed: {err}",
                self.path.display()
            )
        })?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct TelegramSessionDefaults {
    path: PathBuf,
    inner: Arc<RwLock<PersistedTelegramSessionDefaults>>,
}

impl TelegramSessionDefaults {
    pub async fn load() -> Result<Self> {
        let paths = daat_locus_paths().await;
        let path = paths
            .runtime_dir()
            .join(TELEGRAM_SESSION_DEFAULTS_FILE_NAME);
        Self::load_from_path(path).await
    }

    async fn load_from_path(path: PathBuf) -> Result<Self> {
        let state = match tokio::fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice::<PersistedTelegramSessionDefaults>(&bytes)
                .map_err(|err| {
                    miette!(
                        "decode telegram session defaults {} failed: {err}",
                        path.display()
                    )
                })?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                PersistedTelegramSessionDefaults::default()
            }
            Err(err) => {
                return Err(miette!(
                    "read telegram session defaults {} failed: {err}",
                    path.display()
                ));
            }
        };
        Ok(Self {
            path,
            inner: Arc::new(RwLock::new(state)),
        })
    }

    pub fn get(&self, chat_id: &str) -> Option<SessionId> {
        self.inner.read().chats.get(chat_id).cloned()
    }

    pub async fn set(&self, chat_id: impl Into<String>, session_id: SessionId) -> Result<()> {
        {
            let mut inner = self.inner.write();
            inner.chats.insert(chat_id.into(), session_id);
        }
        self.persist().await
    }

    pub async fn remove_by_session(&self, session_id: &SessionId) -> Result<Vec<String>> {
        let removed = {
            let mut inner = self.inner.write();
            let chat_ids = inner
                .chats
                .iter()
                .filter_map(|(chat_id, mapped_session_id)| {
                    (mapped_session_id == session_id).then_some(chat_id.clone())
                })
                .collect::<Vec<_>>();
            for chat_id in &chat_ids {
                inner.chats.remove(chat_id);
            }
            chat_ids
        };
        if !removed.is_empty() {
            self.persist().await?;
        }
        Ok(removed)
    }

    async fn persist(&self) -> Result<()> {
        let bytes = {
            let inner = self.inner.read();
            serde_json::to_vec_pretty(&*inner)
                .map_err(|err| miette!("encode telegram session defaults failed: {err}"))?
        };
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|err| {
                miette!(
                    "create telegram session defaults dir {} failed: {err}",
                    parent.display()
                )
            })?;
        }
        let tmp = self.path.with_extension("json.tmp");
        tokio::fs::write(&tmp, bytes).await.map_err(|err| {
            miette!(
                "write telegram session defaults temp {} failed: {err}",
                tmp.display()
            )
        })?;
        tokio::fs::rename(&tmp, &self.path).await.map_err(|err| {
            miette!(
                "replace telegram session defaults {} failed: {err}",
                self.path.display()
            )
        })?;
        Ok(())
    }
}

pub fn session_state_paths(session_id: &SessionId) -> DaatLocusPaths {
    DaatLocusPaths::for_session(session_id.as_str())
}

pub fn generate_ipc_token() -> String {
    Uuid::new_v4().to_string()
}

pub fn hash_ipc_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    let digest = hasher.finalize();
    digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

pub async fn session_ipc_path(session_id: &SessionId) -> Result<PathBuf> {
    let paths = daat_locus_paths().await;
    let dir = paths.runtime_dir().join(SESSION_IPC_DIR_NAME);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|err| miette!("create session ipc dir {} failed: {err}", dir.display()))?;
    Ok(dir.join(format!("{}.sock", session_id.as_str())))
}

pub async fn store_session_ipc_token(session_id: &SessionId, token: &str) -> Result<()> {
    let path = session_state_paths(session_id).state_file(SESSION_IPC_TOKEN_FILE_NAME);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|err| {
            miette!(
                "create session token dir {} failed: {err}",
                parent.display()
            )
        })?;
    }
    tokio::fs::write(&path, token)
        .await
        .map_err(|err| miette!("write session IPC token {} failed: {err}", path.display()))?;
    Ok(())
}

pub async fn load_session_ipc_token(session_id: &SessionId) -> Result<Option<String>> {
    let path = session_state_paths(session_id).state_file(SESSION_IPC_TOKEN_FILE_NAME);
    match tokio::fs::read_to_string(&path).await {
        Ok(token) => Ok(Some(token.trim().to_string()).filter(|token| !token.is_empty())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(miette!(
            "read session IPC token {} failed: {err}",
            path.display()
        )),
    }
}

fn session_registry_path(paths: &DaatLocusPaths) -> PathBuf {
    paths.runtime_dir().join(SESSION_REGISTRY_FILE_NAME)
}

fn normalize_title(title: Option<String>) -> Option<String> {
    title
        .map(|value| value.trim().chars().take(80).collect::<String>())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_session_id(value: &str) -> SessionId {
        SessionId::from_string(value.to_string()).expect("valid session id")
    }

    #[tokio::test]
    async fn session_registry_persists_lifecycle_and_allows_project_duplicates() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("runtime").join("sessions.json");
        let project_dir = temp.path().join("project");
        tokio::fs::create_dir_all(&project_dir)
            .await
            .expect("create project dir");
        let scope = SessionScope::Project {
            project_dir: project_dir.clone(),
        };

        let registry = SessionRegistry::load_from_path(path.clone())
            .await
            .expect("load empty registry");
        let first = registry
            .create(scope.clone(), Some("  Project Session  ".to_string()))
            .await
            .expect("create first project session");
        let second = registry
            .create(scope, Some("Project Session 2".to_string()))
            .await
            .expect("create second project session");

        assert_ne!(first.session_id, second.session_id);
        assert_eq!(registry.list().len(), 2);
        assert_eq!(first.project_dir.as_ref(), Some(&project_dir));
        assert_eq!(first.title.as_deref(), Some("Project Session"));

        registry
            .mark_starting(
                &first.session_id,
                1234,
                "/tmp/session.sock".to_string(),
                "raw-token",
            )
            .await
            .expect("mark starting");
        let starting = registry.get(&first.session_id).expect("starting session");
        assert_eq!(starting.status, SessionStatus::Starting);
        assert_eq!(starting.pid, Some(1234));
        assert_eq!(starting.ipc_name.as_deref(), Some("/tmp/session.sock"));
        assert_eq!(starting.ipc_token_hash, Some(hash_ipc_token("raw-token")));
        assert_ne!(starting.ipc_token_hash.as_deref(), Some("raw-token"));

        registry
            .mark_ready(&first.session_id)
            .await
            .expect("mark ready");
        assert_eq!(
            registry
                .get(&first.session_id)
                .expect("ready session")
                .status,
            SessionStatus::Ready
        );

        registry
            .mark_dead(&first.session_id)
            .await
            .expect("mark dead");
        let dead = registry.get(&first.session_id).expect("dead session");
        assert_eq!(dead.status, SessionStatus::Dead);
        assert_eq!(dead.pid, None);
        assert_eq!(dead.ipc_name, None);
        assert_eq!(dead.ipc_token_hash, None);

        registry
            .set_title(&first.session_id, "   ".to_string())
            .await
            .expect("clear title");
        assert_eq!(registry.get(&first.session_id).unwrap().title, None);

        let reloaded = SessionRegistry::load_from_path(path.clone())
            .await
            .expect("reload registry");
        assert_eq!(reloaded.list().len(), 2);
        assert_eq!(
            reloaded
                .get(&first.session_id)
                .expect("reloaded first")
                .status,
            SessionStatus::Dead
        );

        let removed = reloaded
            .remove(&second.session_id)
            .await
            .expect("remove second");
        assert!(removed.is_some());
        let reloaded_again = SessionRegistry::load_from_path(path)
            .await
            .expect("reload after remove");
        assert_eq!(reloaded_again.list().len(), 1);
        assert!(reloaded_again.get(&second.session_id).is_none());
    }

    #[test]
    fn session_summary_serialization_excludes_process_and_ipc_metadata() {
        let mut info = SessionInfo::new(
            fixed_session_id("session-a"),
            SessionScope::General,
            Some("Visible".to_string()),
        );
        info.pid = Some(42);
        info.ipc_name = Some("/tmp/session-a.sock".to_string());
        info.ipc_token_hash = Some("secret-hash".to_string());
        let value = serde_json::to_value(SessionSummary::from(info)).expect("serialize summary");
        let object = value.as_object().expect("summary object");

        assert!(object.contains_key("session_id"));
        assert!(object.contains_key("scope"));
        assert!(!object.contains_key("status"));
        assert!(!object.contains_key("pid"));
        assert!(!object.contains_key("ipc_name"));
        assert!(!object.contains_key("ipc_token_hash"));
        assert!(!value.to_string().contains("secret-hash"));
        assert!(!value.to_string().contains("session-a.sock"));
    }

    #[tokio::test]
    async fn telegram_session_defaults_persist_and_overwrite_chat_mapping() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("telegram-session-defaults.json");
        let defaults = TelegramSessionDefaults::load_from_path(path.clone())
            .await
            .expect("load defaults");
        let first = fixed_session_id("session-first");
        let second = fixed_session_id("session-second");

        assert_eq!(defaults.get("12345"), None);
        defaults
            .set("12345", first.clone())
            .await
            .expect("set first mapping");
        assert_eq!(defaults.get("12345"), Some(first.clone()));

        let reloaded = TelegramSessionDefaults::load_from_path(path.clone())
            .await
            .expect("reload defaults");
        assert_eq!(reloaded.get("12345"), Some(first));

        reloaded
            .set("12345", second.clone())
            .await
            .expect("overwrite mapping");
        let reloaded_again = TelegramSessionDefaults::load_from_path(path)
            .await
            .expect("reload overwritten defaults");
        assert_eq!(reloaded_again.get("12345"), Some(second));
    }

    #[tokio::test]
    async fn telegram_session_defaults_remove_mappings_by_session() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("telegram-session-defaults.json");
        let defaults = TelegramSessionDefaults::load_from_path(path.clone())
            .await
            .expect("load defaults");
        let first = fixed_session_id("session-first");
        let second = fixed_session_id("session-second");

        defaults
            .set("chat-a", first.clone())
            .await
            .expect("set first chat");
        defaults
            .set("chat-b", first.clone())
            .await
            .expect("set second chat");
        defaults
            .set("chat-c", second.clone())
            .await
            .expect("set third chat");

        let mut removed = defaults
            .remove_by_session(&first)
            .await
            .expect("remove by session");
        removed.sort();
        assert_eq!(removed, vec!["chat-a".to_string(), "chat-b".to_string()]);
        assert_eq!(defaults.get("chat-a"), None);
        assert_eq!(defaults.get("chat-b"), None);
        assert_eq!(defaults.get("chat-c"), Some(second.clone()));

        let reloaded = TelegramSessionDefaults::load_from_path(path)
            .await
            .expect("reload defaults");
        assert_eq!(reloaded.get("chat-a"), None);
        assert_eq!(reloaded.get("chat-b"), None);
        assert_eq!(reloaded.get("chat-c"), Some(second));
    }
}
