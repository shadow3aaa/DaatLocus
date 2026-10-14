//! Account-owned model definitions. Local OAuth files, locale and listener remain
//! device settings; signed records travel only between private account peers.
use crate::{
    actors::ActorSystem,
    config::{Config, ModelConfig, ProviderConfig},
    stage::{
        StageId,
        host::Host,
        identity::{Enrollment, SignedProof, verify_enrollment},
    },
};
use miette::{IntoDiagnostic, Result, miette};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::Notify;
use zeroize::Zeroizing;
const MAX_BYTES: usize = 48 * 1024;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub enrollment: Enrollment,
    pub proof: SignedProof,
}
impl std::fmt::Debug for Record {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccountConfiguration([private])")
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definitions {
    version: u16,
    issued_at_ms: i64,
    account: String,
    revision: u64,
    writer: StageId,
    providers: HashMap<String, ProviderConfig>,
    models: HashMap<String, ModelConfig>,
    main_model: String,
    approval_model: Option<String>,
}
impl Definitions {
    fn from_config(config: &Config, account: String, revision: u64, writer: StageId) -> Self {
        let mut providers = config.providers.clone();
        for provider in providers.values_mut() {
            if let ProviderConfig::OpenaiCodexOauth { auth_file, .. }
            | ProviderConfig::OpenCodeConsoleOauth { auth_file, .. } = provider
            {
                auth_file.clear();
            }
        }
        Self {
            version: 1,
            issued_at_ms: chrono::Utc::now().timestamp_millis(),
            account,
            revision,
            writer,
            providers,
            models: config.models.clone(),
            main_model: config.main_model.clone(),
            approval_model: config.approval_model.clone(),
        }
    }
    fn materialize(&self, base: &Config) -> Config {
        let mut next = base.clone();
        next.providers = self.providers.clone();
        next.models = self.models.clone();
        next.main_model = self.main_model.clone();
        next.approval_model = self.approval_model.clone();
        for (name, provider) in &mut next.providers {
            match (provider, base.providers.get(name)) {
                (
                    ProviderConfig::OpenaiCodexOauth { auth_file, .. },
                    Some(ProviderConfig::OpenaiCodexOauth { auth_file: old, .. }),
                )
                | (
                    ProviderConfig::OpenCodeConsoleOauth { auth_file, .. },
                    Some(ProviderConfig::OpenCodeConsoleOauth { auth_file: old, .. }),
                ) => auth_file.clone_from(old),
                _ => {}
            }
        }
        next
    }
    fn key(&self) -> (u64, StageId) {
        (self.revision, self.writer)
    }
}
#[derive(Clone)]
pub struct ConfigurationStore {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
    changed: Arc<Notify>,
}
impl ConfigurationStore {
    pub fn open(directory: &Path) -> Result<Self> {
        std::fs::create_dir_all(directory).into_diagnostic()?;
        crate::actors::permissions::protect_private_path(directory)?;
        Ok(Self {
            path: directory.join("models.json"),
            lock: Arc::new(Mutex::new(())),
            changed: Arc::new(Notify::new()),
        })
    }
    pub fn changed(&self) -> &Notify {
        &self.changed
    }
    pub fn current(&self) -> Result<Option<Record>> {
        use std::io::Read;
        let file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).into_diagnostic(),
        };
        let mut bytes = Zeroizing::new(vec![]);
        file.take(MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .into_diagnostic()?;
        if bytes.len() > MAX_BYTES {
            return Err(miette!("Account configuration budget exceeded"));
        }
        Ok(Some(serde_json::from_slice(&bytes).map_err(|_| {
            miette!("Invalid private account configuration")
        })?))
    }
    fn verify(host: &Host, record: &Record, fresh: bool) -> Result<Definitions> {
        use base64::Engine;
        if record.enrollment.chain.len() > 16
            || host.account_id().as_deref() != Some(&record.enrollment.root.group_id)
        {
            return Err(miette!("Configuration does not belong to this account"));
        }
        let leaf = record
            .enrollment
            .chain
            .last()
            .ok_or_else(|| miette!("Configuration signer is missing"))?;
        if leaf.payload.len() > 65536 {
            return Err(miette!("Configuration certificate exceeds its budget"));
        }
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&leaf.payload)
            .map_err(|_| miette!("Invalid configuration certificate"))?;
        let leaf: crate::stage::identity::Membership = serde_json::from_slice(&bytes)
            .map_err(|_| miette!("Invalid configuration certificate"))?;
        let data: Definitions = record
            .proof
            .verify("account-models", &leaf.device.public_key)
            .map_err(|_| miette!("Invalid signed account configuration"))?;
        let now = chrono::Utc::now().timestamp_millis();
        if data.issued_at_ms < 0 || data.issued_at_ms > now + 60000 {
            return Err(miette!("Invalid configuration issue time"));
        }
        let removed = if fresh {
            crate::stage::revocation::removed(host.system.store())
                .map_err(|_| miette!("Cannot verify configuration authority"))?
        } else {
            Default::default()
        };
        let member = verify_enrollment(
            &record.enrollment.root,
            &record.enrollment.chain,
            if fresh { now } else { data.issued_at_ms },
            &removed,
        )
        .map_err(|_| miette!("Invalid configuration authority"))?;
        let own_account = host
            .account_id()
            .ok_or_else(|| miette!("Account identity unavailable"))?;
        if record.enrollment.root.group_id != own_account
            || !member.can_enroll
            || data.version != 1
            || data.revision == 0
            || data.revision == u64::MAX
            || data.account != own_account
            || data.writer != member.device.stage_id
            || data.models.len() > 256
            || data.providers.len() > 64
        {
            return Err(miette!("Configuration does not belong to this account"));
        }
        data.materialize(&host.system.config())
            .validate_settings()
            .map_err(|_| miette!("Account model definitions are incomplete"))?;
        Ok(data)
    }
    pub fn binding(record: &Record) -> String {
        crate::stage::digest(record.proof.payload.as_bytes())
    }
    /// Public change identity; never derive a client-visible revision from
    /// credential bytes or expose the signed private configuration payload.
    pub fn revision(&self, host: &Host) -> Result<Option<String>> {
        self.current()?
            .map(|record| {
                Self::verify(host, &record, false)
                    .map(|data| format!("{}:{}", data.revision, data.writer))
            })
            .transpose()
    }
    pub fn candidate(host: &Host, record: &Record) -> Result<Config> {
        Ok(Self::verify(host, record, false)?.materialize(&host.system.config()))
    }
    fn persist(&self, record: &Record) -> Result<()> {
        let bytes = Zeroizing::new(
            serde_json::to_vec(record)
                .map_err(|_| miette!("Cannot encode private account configuration"))?,
        );
        if bytes.len() > MAX_BYTES {
            return Err(miette!("Account configuration budget exceeded"));
        }
        crate::persistence::write_bytes_atomic_sync(
            &self.path,
            &bytes,
            crate::persistence::PersistenceFileMode::Private,
        )
        .into_diagnostic()?;
        self.changed.notify_waiters();
        Ok(())
    }
    pub fn publish(&self, host: &Host, config: &Config) -> Result<()> {
        let _lock = self.lock.lock();
        let old = self
            .current()?
            .map(|record| Self::verify(host, &record, false))
            .transpose()?;
        let revision = old.map_or(1, |old| old.revision.saturating_add(1));
        let own = host
            .enrollment()
            .map_err(|_| miette!("Account membership unavailable"))?;
        let data =
            Definitions::from_config(config, own.root.group_id.clone(), revision, host.local);
        let record = Record {
            enrollment: own,
            proof: host
                .identity()
                .map_err(|_| miette!("Device identity unavailable"))?
                .sign("account-models", &data)
                .map_err(|_| miette!("Cannot sign account configuration"))?,
        };
        Self::verify(host, &record, false)?;
        self.persist(&record)
    }
    pub fn needs_import(&self, host: &Host, record: &Record) -> Result<Option<Config>> {
        let next = Self::verify(host, record, true)?;
        if let Some(current) = self.current()? {
            let previous = Self::verify(host, &current, false)?;
            if next.key() < previous.key() {
                return Ok(None);
            }
            if next.key() == previous.key() {
                if Self::binding(record) == Self::binding(&current) {
                    return Ok(None);
                }
                return Err(miette!("Conflicting account configuration revision"));
            }
        }
        Ok(Some(next.materialize(&host.system.config())))
    }
    pub async fn import(&self, host: &Host, record: Record) -> Result<bool> {
        let Some(candidate) = self.needs_import(host, &record)? else {
            return Ok(false);
        };
        if serde_json::to_value(&candidate)
            .map_err(|_| miette!("Cannot compare model definitions"))?
            == serde_json::to_value(host.system.config())
                .map_err(|_| miette!("Cannot compare model definitions"))?
        {
            let _lock = self.lock.lock();
            if self.needs_import(host, &record)?.is_some() {
                self.persist(&record)?;
                return Ok(true);
            }
            return Ok(false);
        }
        let store = self.clone();
        let expected = self.current()?.map(|record| Self::binding(&record));
        let persistent = candidate.clone();
        host.system
            .update_config_with(candidate, async move {
                let _lock = store.lock.lock();
                if store.current()?.map(|record| Self::binding(&record)) != expected {
                    return Err(miette!("Account configuration changed during import"));
                }
                persist_device_config(&host.system, &persistent)?;
                store.persist(&record)
            })
            .await?;
        Ok(true)
    }
}
pub fn persist_device_config(system: &ActorSystem, config: &Config) -> Result<()> {
    let directory = system
        .infrastructure_dir()
        .parent()
        .ok_or_else(|| miette!("Profile path unavailable"))?
        .join("config");
    std::fs::create_dir_all(&directory).into_diagnostic()?;
    crate::actors::permissions::protect_private_path(&directory)?;
    let bytes = Zeroizing::new(
        toml::to_string_pretty(config)
            .map_err(|_| miette!("Cannot encode device configuration"))?
            .into_bytes(),
    );
    crate::persistence::write_bytes_atomic_sync(
        &directory.join("config.toml"),
        &bytes,
        crate::persistence::PersistenceFileMode::Private,
    )
    .into_diagnostic()?;
    crate::persistence::write_bytes_atomic_sync(
        &directory.join("config.toml.bak"),
        &bytes,
        crate::persistence::PersistenceFileMode::Private,
    )
    .into_diagnostic()
}
