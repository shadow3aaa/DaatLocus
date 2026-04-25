use std::{path::Path, sync::Arc};

use axum::http::{HeaderMap, header::AUTHORIZATION};
use miette::{Result, miette};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{io::AsyncWriteExt, sync::Mutex};

use crate::daat_locus_paths::daat_locus_paths;

const BEARER_PREFIX: &str = "Bearer ";
const LOCAL_CLI_TOKEN_ID: &str = "local-cli";
const MIN_DAEMON_TOKEN_LEN: usize = 64;
const MAX_TOKEN_NAME_LEN: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaemonAuthToken(String);

impl DaemonAuthToken {
    fn generate() -> Self {
        let mut token = String::with_capacity(128);
        for _ in 0..4 {
            token.push_str(&uuid::Uuid::new_v4().simple().to_string());
        }
        Self(token)
    }

    fn parse(token: &str) -> Result<Self> {
        if token.len() < MIN_DAEMON_TOKEN_LEN {
            return Err(miette!("daemon auth token is too short"));
        }
        if token
            .bytes()
            .any(|byte| !byte.is_ascii_graphic() || byte.is_ascii_whitespace())
        {
            return Err(miette!("daemon auth token contains invalid characters"));
        }
        Ok(Self(token.to_string()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn bearer_value(&self) -> String {
        format!("{BEARER_PREFIX}{}", self.as_str())
    }
}

#[derive(Clone)]
pub struct DaemonTokenRegistryHandle {
    path: std::path::PathBuf,
    local_token_path: std::path::PathBuf,
    write_lock: Arc<Mutex<()>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedDaemonToken {
    pub id: String,
    pub name: String,
    pub token: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaemonTokenListEntry {
    pub id: String,
    pub name: String,
    pub created_at_ms: i64,
    pub last_used_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct DaemonTokenRegistry {
    #[serde(default)]
    tokens: Vec<DaemonTokenRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DaemonTokenRecord {
    id: String,
    name: String,
    created_at_ms: i64,
    #[serde(default)]
    last_used_at_ms: Option<i64>,
    token_hash: String,
}

impl DaemonTokenRegistryHandle {
    pub async fn load_or_create() -> Result<Self> {
        let paths = daat_locus_paths().await;
        let handle = Self {
            path: paths.daemon_token_registry_file(),
            local_token_path: paths.daemon_token_file(),
            write_lock: Arc::new(Mutex::new(())),
        };
        handle.ensure_registry().await?;
        Ok(handle)
    }

    pub async fn authorize_headers(&self, headers: &HeaderMap) -> bool {
        let Some(value) = headers.get(AUTHORIZATION) else {
            return false;
        };
        let Ok(value) = value.to_str() else {
            return false;
        };
        let Some(token) = value.strip_prefix(BEARER_PREFIX) else {
            return false;
        };

        match self.authorize_token(token).await {
            Ok(authorized) => authorized,
            Err(err) => {
                tracing::warn!("daemon token authorization failed: {err}");
                false
            }
        }
    }

    async fn authorize_token(&self, token: &str) -> Result<bool> {
        let _guard = self.write_lock.lock().await;
        let mut registry = read_registry(&self.path).await?;
        let token_hash = hash_token(token);
        let Some(record) = registry
            .tokens
            .iter_mut()
            .find(|record| constant_time_eq(&record.token_hash, &token_hash))
        else {
            return Ok(false);
        };
        record.last_used_at_ms = Some(now_ms());
        write_registry(&self.path, &registry).await?;
        Ok(true)
    }

    async fn ensure_registry(&self) -> Result<()> {
        let _guard = self.write_lock.lock().await;
        let mut registry = read_registry(&self.path).await?;
        let local_token = load_or_create_local_daemon_auth_token_at(&self.local_token_path).await?;
        let changed = ensure_local_cli_record(&mut registry, &local_token);
        if changed || !self.path.exists() {
            write_registry(&self.path, &registry).await?;
        } else {
            harden_private_file_permissions(&self.path).await?;
        }
        Ok(())
    }

    pub async fn create_token(&self, name: &str) -> Result<CreatedDaemonToken> {
        let name = normalize_token_name(name)?;
        if name == LOCAL_CLI_TOKEN_ID {
            return Err(miette!("token name `{LOCAL_CLI_TOKEN_ID}` is reserved"));
        }

        let _guard = self.write_lock.lock().await;
        let mut registry = read_registry(&self.path).await?;
        reject_duplicate_name(&registry, &name)?;

        let token = DaemonAuthToken::generate();
        let record = DaemonTokenRecord {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            created_at_ms: now_ms(),
            last_used_at_ms: None,
            token_hash: hash_token(token.as_str()),
        };
        let created = CreatedDaemonToken {
            id: record.id.clone(),
            name: record.name.clone(),
            token: token.as_str().to_string(),
        };
        registry.tokens.push(record);
        sort_registry(&mut registry);
        write_registry(&self.path, &registry).await?;
        Ok(created)
    }

    pub async fn list_tokens(&self) -> Result<Vec<DaemonTokenListEntry>> {
        let _guard = self.write_lock.lock().await;
        let registry = read_registry(&self.path).await?;
        Ok(registry.tokens.into_iter().map(Into::into).collect())
    }

    pub async fn revoke_token(&self, selector: &str) -> Result<DaemonTokenListEntry> {
        let _guard = self.write_lock.lock().await;
        let mut registry = read_registry(&self.path).await?;
        let index = find_token_index(&registry, selector)?;
        if registry.tokens[index].id == LOCAL_CLI_TOKEN_ID {
            return Err(miette!(
                "`{LOCAL_CLI_TOKEN_ID}` is required for local CLI access; rotate it instead"
            ));
        }
        let removed = registry.tokens.remove(index);
        write_registry(&self.path, &registry).await?;
        Ok(removed.into())
    }

    pub async fn rotate_token(&self, selector: &str) -> Result<CreatedDaemonToken> {
        let _guard = self.write_lock.lock().await;
        let mut registry = read_registry(&self.path).await?;
        let index = find_token_index(&registry, selector)?;
        let token = DaemonAuthToken::generate();
        registry.tokens[index].token_hash = hash_token(token.as_str());
        registry.tokens[index].last_used_at_ms = None;
        let rotated = CreatedDaemonToken {
            id: registry.tokens[index].id.clone(),
            name: registry.tokens[index].name.clone(),
            token: token.as_str().to_string(),
        };
        if registry.tokens[index].id == LOCAL_CLI_TOKEN_ID {
            write_local_daemon_auth_token_at(&self.local_token_path, &token).await?;
        }
        write_registry(&self.path, &registry).await?;
        Ok(rotated)
    }
}

impl From<DaemonTokenRecord> for DaemonTokenListEntry {
    fn from(record: DaemonTokenRecord) -> Self {
        Self {
            id: record.id,
            name: record.name,
            created_at_ms: record.created_at_ms,
            last_used_at_ms: record.last_used_at_ms,
        }
    }
}

pub async fn load_daemon_auth_token() -> Result<DaemonAuthToken> {
    let handle = DaemonTokenRegistryHandle::load_or_create().await?;
    load_local_daemon_auth_token_at(&handle.local_token_path).await
}

pub async fn load_or_create_daemon_token_registry() -> Result<DaemonTokenRegistryHandle> {
    DaemonTokenRegistryHandle::load_or_create().await
}

pub async fn create_daemon_token(name: &str) -> Result<CreatedDaemonToken> {
    DaemonTokenRegistryHandle::load_or_create()
        .await?
        .create_token(name)
        .await
}

pub async fn list_daemon_tokens() -> Result<Vec<DaemonTokenListEntry>> {
    DaemonTokenRegistryHandle::load_or_create()
        .await?
        .list_tokens()
        .await
}

pub async fn revoke_daemon_token(selector: &str) -> Result<DaemonTokenListEntry> {
    DaemonTokenRegistryHandle::load_or_create()
        .await?
        .revoke_token(selector)
        .await
}

pub async fn rotate_daemon_token(selector: &str) -> Result<CreatedDaemonToken> {
    DaemonTokenRegistryHandle::load_or_create()
        .await?
        .rotate_token(selector)
        .await
}

fn ensure_local_cli_record(registry: &mut DaemonTokenRegistry, token: &DaemonAuthToken) -> bool {
    let token_hash = hash_token(token.as_str());
    let now = now_ms();
    let mut changed = false;

    if let Some(record) = registry
        .tokens
        .iter_mut()
        .find(|record| record.id == LOCAL_CLI_TOKEN_ID)
    {
        if record.name != LOCAL_CLI_TOKEN_ID {
            record.name = LOCAL_CLI_TOKEN_ID.to_string();
            changed = true;
        }
        if record.token_hash != token_hash {
            record.token_hash = token_hash;
            record.last_used_at_ms = None;
            changed = true;
        }
    } else {
        registry.tokens.push(DaemonTokenRecord {
            id: LOCAL_CLI_TOKEN_ID.to_string(),
            name: LOCAL_CLI_TOKEN_ID.to_string(),
            created_at_ms: now,
            last_used_at_ms: None,
            token_hash,
        });
        changed = true;
    }

    sort_registry(registry);
    changed
}

async fn read_registry(path: &Path) -> Result<DaemonTokenRegistry> {
    let raw = match tokio::fs::read_to_string(path).await {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DaemonTokenRegistry::default());
        }
        Err(err) => {
            return Err(miette!(
                "read daemon token registry {} failed: {err}",
                path.display()
            ));
        }
    };
    serde_json::from_str(&raw).map_err(|err| {
        miette!(
            "parse daemon token registry {} failed: {err}",
            path.display()
        )
    })
}

async fn write_registry(path: &Path, registry: &DaemonTokenRegistry) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(registry)
        .map_err(|err| miette!("serialize daemon token registry failed: {err}"))?;
    write_private_file(path, &bytes).await
}

async fn load_or_create_local_daemon_auth_token_at(path: &Path) -> Result<DaemonAuthToken> {
    match tokio::fs::try_exists(path).await {
        Ok(true) => load_local_daemon_auth_token_at(path).await,
        Ok(false) => {
            let token = DaemonAuthToken::generate();
            write_local_daemon_auth_token_at(path, &token).await?;
            Ok(token)
        }
        Err(err) => Err(miette!(
            "check daemon auth token {} failed: {err}",
            path.display()
        )),
    }
}

async fn load_local_daemon_auth_token_at(path: &Path) -> Result<DaemonAuthToken> {
    let raw = tokio::fs::read_to_string(path)
        .await
        .map_err(|err| miette!("read daemon auth token {} failed: {err}", path.display()))?;
    harden_private_file_permissions(path).await?;
    DaemonAuthToken::parse(raw.trim())
        .map_err(|err| miette!("invalid daemon auth token at {}: {err}", path.display()))
}

async fn write_local_daemon_auth_token_at(path: &Path, token: &DaemonAuthToken) -> Result<()> {
    let mut bytes = token.as_str().as_bytes().to_vec();
    bytes.push(b'\n');
    write_private_file(path, &bytes).await
}

async fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|err| {
            miette!(
                "create daemon token directory {} failed: {err}",
                parent.display()
            )
        })?;
    }

    let mut options = tokio::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }

    let mut file = options
        .open(path)
        .await
        .map_err(|err| miette!("open daemon token file {} failed: {err}", path.display()))?;
    file.write_all(bytes)
        .await
        .map_err(|err| miette!("write daemon token file {} failed: {err}", path.display()))?;
    file.flush()
        .await
        .map_err(|err| miette!("flush daemon token file {} failed: {err}", path.display()))?;
    harden_private_file_permissions(path).await
}

#[cfg(unix)]
async fn harden_private_file_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .await
        .map_err(|err| {
            miette!(
                "set daemon token file permissions {} failed: {err}",
                path.display()
            )
        })
}

#[cfg(not(unix))]
async fn harden_private_file_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

fn normalize_token_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(miette!("daemon token name cannot be empty"));
    }
    if name.len() > MAX_TOKEN_NAME_LEN {
        return Err(miette!(
            "daemon token name cannot exceed {MAX_TOKEN_NAME_LEN} bytes"
        ));
    }
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(miette!(
            "daemon token name can only contain ASCII letters, digits, '-', '_', or '.'"
        ));
    }
    Ok(name.to_string())
}

fn reject_duplicate_name(registry: &DaemonTokenRegistry, name: &str) -> Result<()> {
    if registry.tokens.iter().any(|record| record.name == name) {
        return Err(miette!("daemon token name `{name}` already exists"));
    }
    Ok(())
}

fn find_token_index(registry: &DaemonTokenRegistry, selector: &str) -> Result<usize> {
    let selector = selector.trim();
    let matches: Vec<_> = registry
        .tokens
        .iter()
        .enumerate()
        .filter(|(_, record)| record.id == selector || record.name == selector)
        .map(|(index, _)| index)
        .collect();
    match matches.as_slice() {
        [index] => Ok(*index),
        [] => Err(miette!("daemon token `{selector}` not found")),
        _ => Err(miette!("daemon token selector `{selector}` is ambiguous")),
    }
}

fn sort_registry(registry: &mut DaemonTokenRegistry) {
    registry.tokens.sort_by(|left, right| {
        (left.id != LOCAL_CLI_TOKEN_ID)
            .cmp(&(right.id != LOCAL_CLI_TOKEN_ID))
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    });
}

fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    format!("sha256:{}", hex_encode(&hasher.finalize()))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.as_bytes()
        .iter()
        .zip(right.as_bytes())
        .fold(0_u8, |acc, (left, right)| acc | (left ^ right))
        == 0
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn registry_creates_private_local_cli_token_and_hash_record() {
        let temp = tempfile::tempdir().unwrap();
        let handle = DaemonTokenRegistryHandle {
            path: temp.path().join("runtime").join("daemon_tokens.json"),
            local_token_path: temp.path().join("runtime").join("daemon.token"),
            write_lock: Arc::new(Mutex::new(())),
        };

        handle.ensure_registry().await.unwrap();
        let token = load_local_daemon_auth_token_at(&handle.local_token_path)
            .await
            .unwrap();
        let registry = read_registry(&handle.path).await.unwrap();

        assert_eq!(registry.tokens.len(), 1);
        assert_eq!(registry.tokens[0].id, LOCAL_CLI_TOKEN_ID);
        assert_eq!(registry.tokens[0].token_hash, hash_token(token.as_str()));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let token_mode = std::fs::metadata(&handle.local_token_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            let registry_mode = std::fs::metadata(&handle.path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(token_mode, 0o600);
            assert_eq!(registry_mode, 0o600);
        }
    }

    #[tokio::test]
    async fn registry_authorizes_created_tokens_and_updates_last_used() {
        let temp = tempfile::tempdir().unwrap();
        let handle = DaemonTokenRegistryHandle {
            path: temp.path().join("runtime").join("daemon_tokens.json"),
            local_token_path: temp.path().join("runtime").join("daemon.token"),
            write_lock: Arc::new(Mutex::new(())),
        };
        handle.ensure_registry().await.unwrap();

        let created = handle.create_token("web").await.unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            axum::http::HeaderValue::from_str(&format!("Bearer {}", created.token)).unwrap(),
        );

        assert!(handle.authorize_headers(&headers).await);
        let registry = read_registry(&handle.path).await.unwrap();
        let web = registry
            .tokens
            .iter()
            .find(|record| record.id == created.id)
            .unwrap();
        assert!(web.last_used_at_ms.is_some());
        assert!(
            !registry
                .tokens
                .iter()
                .any(|record| record.token_hash == created.token)
        );
    }

    #[tokio::test]
    async fn registry_revokes_and_rotates_named_tokens() {
        let temp = tempfile::tempdir().unwrap();
        let handle = DaemonTokenRegistryHandle {
            path: temp.path().join("runtime").join("daemon_tokens.json"),
            local_token_path: temp.path().join("runtime").join("daemon.token"),
            write_lock: Arc::new(Mutex::new(())),
        };
        handle.ensure_registry().await.unwrap();

        let created = handle.create_token("web").await.unwrap();
        let rotated = handle.rotate_token("web").await.unwrap();
        assert_eq!(created.id, rotated.id);
        assert_ne!(created.token, rotated.token);

        let revoked = handle.revoke_token("web").await.unwrap();
        assert_eq!(revoked.id, created.id);
        assert!(handle.revoke_token(LOCAL_CLI_TOKEN_ID).await.is_err());
    }
}
