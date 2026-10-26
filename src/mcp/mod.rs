//! Profile-local MCP installations. The wire contract remains the server's own.
mod apps;
mod transport;
use crate::{actors::ActorError, app::AppToolExecutionContext};
pub use apps::{AppsManager, McpApp};
use miette::{IntoDiagnostic, Result, miette};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    sync::Arc,
};

#[daat_locus_macros::model_schema]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Timeouts {
    /// Timeout for establishing an individual network connection, in milliseconds.
    pub connect_timeout_ms: u64,
    /// Total process startup and MCP handshake deadline; null waits until cancelled.
    pub startup_timeout_ms: Option<u64>,
    /// Per-request deadline, including tool discovery; null waits until cancelled.
    pub request_timeout_ms: Option<u64>,
}
impl Default for Timeouts {
    fn default() -> Self {
        Self {
            connect_timeout_ms: 30_000,
            startup_timeout_ms: None,
            request_timeout_ms: None,
        }
    }
}
impl Timeouts {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.connect_timeout_ms == 0
            || self.startup_timeout_ms == Some(0)
            || self.request_timeout_ms == Some(0)
        {
            return Err(miette!(
                "MCP timeouts must be positive milliseconds or null"
            ));
        }
        for value in [
            Some(self.connect_timeout_ms),
            self.startup_timeout_ms,
            self.request_timeout_ms,
        ] {
            crate::operation_deadline::Deadline::after_ms(value)?;
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case", deny_unknown_fields)]
pub enum Source {
    Http {
        url: String,
        #[serde(default)]
        bearer_token: Option<String>,
        #[serde(default)]
        timeouts: Timeouts,
    },
    Stdio {
        command: String,
        #[serde(default)]
        arguments: Vec<String>,
        #[serde(default)]
        environment: BTreeMap<String, String>,
        #[serde(default)]
        timeouts: Timeouts,
    },
}
impl Source {
    pub fn timeouts(&self) -> &Timeouts {
        match self {
            Self::Http { timeouts, .. } | Self::Stdio { timeouts, .. } => timeouts,
        }
    }
    pub fn validate(&self) -> Result<()> {
        self.timeouts().validate()?;
        match self {
            Self::Http {
                url, bearer_token, ..
            } => {
                crate::web_app::validate_url(url)?;
                let parsed = url::Url::parse(url).into_diagnostic()?;
                if parsed.fragment().is_some()
                    || bearer_token
                        .as_ref()
                        .is_some_and(|t| t.contains(['\r', '\n']))
                {
                    return Err(miette!("Invalid MCP connection"));
                }
                if parsed.scheme() != "https"
                    && !matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
                {
                    return Err(miette!("MCP requires HTTPS except on loopback"));
                }
            }
            Self::Stdio {
                command,
                arguments,
                environment,
                ..
            } => {
                if cfg!(target_os = "android") {
                    return Err(miette!("MCP stdio is unavailable on Android"));
                }
                if command.trim().is_empty()
                    || command.contains(['\0', '\r', '\n'])
                    || arguments.iter().any(|s| s.contains('\0'))
                {
                    return Err(miette!("Invalid MCP command"));
                }
                if environment.iter().any(|(key, value)| {
                    key.is_empty()
                        || key.to_ascii_uppercase().starts_with("DAAT_LOCUS")
                        || !key.bytes().enumerate().all(|(i, c)| {
                            c == b'_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit())
                        })
                        || value.contains('\0')
                }) {
                    return Err(miette!("Invalid MCP environment"));
                }
            }
        }
        Ok(())
    }
    pub fn public(&self) -> Value {
        match self {
            Self::Http {
                url,
                bearer_token,
                timeouts,
            } => {
                let mut url = url::Url::parse(url).expect("validated source");
                url.set_query(None);
                url.set_fragment(None);
                json!({"transport":"http","url":url.as_str(),"credential_configured":bearer_token.is_some(),"timeouts":timeouts})
            }
            Self::Stdio {
                command, timeouts, ..
            } => json!({"transport":"stdio","command":command,"timeouts":timeouts}),
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub local_name: String,
    pub description: String,
    pub input_schema: Value,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Installation {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub revision: u64,
    pub enabled: bool,
    pub source: Source,
    pub tools: Vec<Tool>,
}
impl Installation {
    pub fn public(&self, details: bool) -> Value {
        let mut value = json!({"id":self.id,"name":self.name,"description":self.description,"revision":self.revision,"enabled":self.enabled,"source":self.source.public(),"tool_count":self.tools.len()});
        if details {
            value["tools"] = json!(self.tools);
        }
        value
    }
}
#[derive(Clone, Default, Serialize, Deserialize)]
struct State {
    revision: u64,
    installations: BTreeMap<String, Installation>,
}
pub struct Registry {
    connecting: parking_lot::Mutex<
        BTreeMap<String, (std::time::Instant, tokio::sync::watch::Sender<bool>)>,
    >,
    loaded: parking_lot::Mutex<BTreeMap<String, BTreeMap<String, u64>>>,
    path: PathBuf,
    state: RwLock<State>,
    mutation: tokio::sync::Mutex<()>,
    refresh: parking_lot::Mutex<HashSet<String>>,
}
impl Registry {
    pub fn open(path: PathBuf) -> Result<Arc<Self>> {
        let state: State = if path.exists() {
            serde_json::from_slice(&std::fs::read(&path).into_diagnostic()?).into_diagnostic()?
        } else {
            State::default()
        };
        for entry in state.installations.values() {
            if !is_mcp_id(&entry.id) || entry.name.trim().is_empty() {
                return Err(miette!("Invalid MCP installation identity"));
            }
            entry.source.validate()?;
            validate_tools(&entry.tools)?;
        }
        Ok(Arc::new(Self {
            connecting: Default::default(),
            loaded: Default::default(),
            path,
            state: RwLock::new(state),
            mutation: tokio::sync::Mutex::new(()),
            refresh: Default::default(),
        }))
    }
    pub fn revision(&self) -> u64 {
        self.state.read().revision
    }
    pub fn record_loaded(&self, actor: &str, versions: BTreeMap<String, u64>) {
        self.loaded.lock().insert(actor.into(), versions);
    }
    pub fn actor_entries(&self, actor: &str) -> Vec<Value> {
        let loaded = self.loaded.lock();
        self.state
            .read()
            .installations
            .values()
            .map(|entry| entry.public(false))
            .map(|mut value| {
                value["loaded_revision"] = json!(
                    loaded
                        .get(actor)
                        .and_then(|versions| versions.get(value["id"].as_str().unwrap()))
                );
                value
            })
            .collect()
    }
    pub fn list_public(
        &self,
        after: Option<&str>,
        search: Option<&str>,
        enabled: Option<bool>,
        limit: usize,
    ) -> Vec<Value> {
        let search = search.map(str::to_lowercase);
        self.state
            .read()
            .installations
            .values()
            .filter(|entry| after.is_none_or(|id| entry.id.as_str() > id))
            .filter(|entry| enabled.is_none_or(|state| entry.enabled == state))
            .filter(|entry| {
                search.as_ref().is_none_or(|query| {
                    format!("{} {}", entry.name, entry.description)
                        .to_lowercase()
                        .contains(query)
                })
            })
            .take(limit)
            .map(|entry| entry.public(false))
            .collect()
    }
    pub fn entries(&self) -> Vec<Installation> {
        self.state.read().installations.values().cloned().collect()
    }
    pub fn get(&self, id: &str) -> Result<Installation> {
        self.state
            .read()
            .installations
            .get(id)
            .cloned()
            .ok_or_else(|| ActorError::NotFound("App not found".into()).into())
    }
    pub fn check(&self, id: &str, revision: u64) -> Result<()> {
        let state = self.state.read();
        let entry = state
            .installations
            .get(id)
            .ok_or_else(|| ActorError::NotFound("App not found".into()))?;
        if !entry.enabled || entry.revision != revision {
            return Err(
                ActorError::Conflict("App changed or was closed; refresh Apps".into()).into(),
            );
        }
        Ok(())
    }
    fn persist(&self, candidate: State) -> Result<()> {
        crate::persistence::write_bytes_atomic_sync(
            &self.path,
            &serde_json::to_vec(&candidate).into_diagnostic()?,
            crate::persistence::PersistenceFileMode::Private,
        )
        .into_diagnostic()?;
        *self.state.write() = candidate;
        Ok(())
    }
    pub async fn install(
        &self,
        name: String,
        description: String,
        source: Source,
        context: &AppToolExecutionContext,
    ) -> Result<Installation> {
        if name.trim().is_empty() {
            return Err(miette!("Invalid App name"));
        }
        source.validate()?;
        // Connecting can execute server code with OS-user access outside the sandbox.
        let mut session = transport::Session::connect(&source, context).await?;
        let tools = session.tools().await?;
        session.close().await;
        let _guard = self.mutation.lock().await;
        let mut candidate = self.state.read().clone();
        candidate.revision += 1;
        let entry = Installation {
            id: format!("mcp_{}", uuid::Uuid::new_v4().simple()),
            name: name.trim().into(),
            description: description.trim().into(),
            revision: candidate.revision,
            enabled: true,
            source,
            tools,
        };
        candidate
            .installations
            .insert(entry.id.clone(), entry.clone());
        self.persist(candidate)?;
        Ok(entry)
    }
    pub async fn install_cancellable(
        &self,
        request_id: String,
        name: String,
        description: String,
        source: Source,
        context: &AppToolExecutionContext,
    ) -> Result<Installation> {
        uuid::Uuid::parse_str(&request_id)
            .map_err(|_| miette!("Invalid installation request ID"))?;
        let (cancel, mut stopped) = tokio::sync::watch::channel(false);
        {
            let mut connecting = self.connecting.lock();
            connecting.retain(|_, (created, token)| {
                !*token.borrow() || created.elapsed() < std::time::Duration::from_secs(60)
            });
            if let Some((_, previous)) = connecting.get(&request_id) {
                if *previous.borrow() {
                    connecting.remove(&request_id);
                    return Err(miette!("MCP installation was cancelled before startup"));
                }
                return Err(miette!("Installation request is already active"));
            }
            connecting.insert(request_id.clone(), (std::time::Instant::now(), cancel));
        }
        struct Cleanup<'a>(&'a Registry, String);
        impl Drop for Cleanup<'_> {
            fn drop(&mut self) {
                self.0.connecting.lock().remove(&self.1);
            }
        }
        let _cleanup = Cleanup(self, request_id);
        tokio::select! {
            biased;
            _ = stopped.changed() => Err(miette!("MCP installation cancelled; inspect installations before retrying")),
            result = self.install(name, description, source, context) => result,
        }
    }
    pub fn cancel_installation(&self, request_id: &str) {
        let mut connecting = self.connecting.lock();
        connecting.retain(|_, (created, token)| {
            !*token.borrow() || created.elapsed() < std::time::Duration::from_secs(60)
        });
        if let Some((_, cancel)) = connecting.get(request_id) {
            let _ = cancel.send(true);
        } else {
            // Fence cancellation arriving before the corresponding HTTP POST.
            let (cancel, _) = tokio::sync::watch::channel(true);
            connecting.insert(request_id.into(), (std::time::Instant::now(), cancel));
        }
    }
    pub async fn set_enabled(
        &self,
        id: &str,
        expected: u64,
        enabled: bool,
    ) -> Result<Installation> {
        let _guard = self.mutation.lock().await;
        let mut candidate = self.state.read().clone();
        let next = candidate.revision + 1;
        let entry = candidate
            .installations
            .get_mut(id)
            .ok_or_else(|| ActorError::NotFound("App not found".into()))?;
        if entry.revision != expected {
            return Err(ActorError::Conflict("App changed; inspect it again".into()).into());
        }
        if entry.enabled == enabled {
            return Ok(entry.clone());
        }
        entry.enabled = enabled;
        entry.revision = next;
        let result = entry.clone();
        candidate.revision = next;
        self.persist(candidate)?;
        Ok(result)
    }
    pub async fn set_timeouts(
        &self,
        id: &str,
        expected: u64,
        timeouts: Timeouts,
    ) -> Result<Installation> {
        timeouts.validate()?;
        let _guard = self.mutation.lock().await;
        let mut candidate = self.state.read().clone();
        let next = candidate.revision + 1;
        let entry = candidate
            .installations
            .get_mut(id)
            .ok_or_else(|| ActorError::NotFound("App not found".into()))?;
        if entry.revision != expected {
            return Err(ActorError::Conflict("App changed; inspect it again".into()).into());
        }
        if entry.source.timeouts() == &timeouts {
            return Ok(entry.clone());
        }
        match &mut entry.source {
            Source::Http {
                timeouts: current, ..
            }
            | Source::Stdio {
                timeouts: current, ..
            } => *current = timeouts,
        }
        entry.revision = next;
        let result = entry.clone();
        candidate.revision = next;
        self.persist(candidate)?;
        Ok(result)
    }
    pub async fn update_tools(&self, id: &str, expected: u64, tools: Vec<Tool>) -> Result<()> {
        validate_tools(&tools)?;
        let _guard = self.mutation.lock().await;
        let mut candidate = self.state.read().clone();
        let old = candidate
            .installations
            .get(id)
            .ok_or_else(|| miette!("App missing"))?;
        if !old.enabled || old.revision != expected {
            return Err(miette!("App changed during discovery"));
        }
        if serde_json::to_value(&old.tools).into_diagnostic()?
            == serde_json::to_value(&tools).into_diagnostic()?
        {
            return Ok(());
        }
        candidate.revision += 1;
        let revision = candidate.revision;
        let entry = candidate.installations.get_mut(id).unwrap();
        entry.tools = tools;
        entry.revision = revision;
        self.persist(candidate)
    }
    pub fn request_refresh(&self, actor: &str) {
        self.refresh.lock().insert(actor.into());
    }
    pub fn take_refresh(&self, actor: &str) -> bool {
        self.refresh.lock().remove(actor)
    }
}
fn validate_tools(tools: &[Tool]) -> Result<()> {
    let mut names = HashSet::new();
    for tool in tools {
        if !crate::app::AppId::is_valid_name(&tool.local_name) {
            return Err(miette!("Invalid MCP tool mapping"));
        }
        if tool.name.is_empty() || !names.insert(&tool.local_name) {
            return Err(miette!("Invalid or duplicate MCP tool name"));
        }
        jsonschema::validator_for(&tool.input_schema).map_err(|_| {
            miette!("MCP tool schema is invalid or references an unavailable external schema")
        })?;
    }
    Ok(())
}
pub fn is_mcp_id(id: &str) -> bool {
    id.strip_prefix("mcp_").is_some_and(|s| {
        s.len() == 32
            && s.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    })
}

#[cfg(test)]
pub(crate) mod tests;
