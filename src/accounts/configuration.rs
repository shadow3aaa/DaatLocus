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
/// Shared signature and account authority checks for private replicated records.
pub(crate) fn verify_account_payload<T: serde::de::DeserializeOwned>(
    host: &Host,
    record: &Record,
    domain: &str,
    fresh: bool,
    issued_at: impl FnOnce(&T) -> i64,
) -> Result<(T, StageId)> {
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
    let leaf: crate::stage::identity::Membership =
        serde_json::from_slice(&bytes).map_err(|_| miette!("Invalid configuration certificate"))?;
    let data: T = record
        .proof
        .verify(domain, &leaf.device.public_key)
        .map_err(|_| miette!("Invalid signed account configuration"))?;
    let issued_at_ms = issued_at(&data);
    let now = chrono::Utc::now().timestamp_millis();
    if issued_at_ms < 0 || issued_at_ms > now + 60000 {
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
        if fresh { now } else { issued_at_ms },
        &removed,
    )
    .map_err(|_| miette!("Invalid configuration authority"))?;
    let own_account = host
        .account_id()
        .ok_or_else(|| miette!("Account identity unavailable"))?;
    if record.enrollment.root.group_id != own_account || !member.can_enroll {
        return Err(miette!("Record does not belong to this account"));
    }
    Ok((data, member.device.stage_id))
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
            if let ProviderConfig::Codex { auth_file, .. }
            | ProviderConfig::OpenCodeConsoleOauth { auth_file, .. }
            | ProviderConfig::OpenCodeGoOauth { auth_file, .. } = provider
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
                    ProviderConfig::Codex {
                        api_key: None,
                        auth_file,
                        ..
                    },
                    Some(ProviderConfig::Codex {
                        api_key: None,
                        auth_file: old,
                        ..
                    }),
                )
                | (
                    ProviderConfig::OpenCodeConsoleOauth { auth_file, .. }
                    | ProviderConfig::OpenCodeGoOauth { auth_file, .. },
                    Some(
                        ProviderConfig::OpenCodeConsoleOauth { auth_file: old, .. }
                        | ProviderConfig::OpenCodeGoOauth { auth_file: old, .. },
                    ),
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
        let mut file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).into_diagnostic(),
        };
        let mut bytes = Zeroizing::new(vec![]);
        file.read_to_end(&mut bytes).into_diagnostic()?;
        Ok(Some(serde_json::from_slice(&bytes).map_err(|_| {
            miette!("Invalid private account configuration")
        })?))
    }
    fn verify(host: &Host, record: &Record, fresh: bool) -> Result<Definitions> {
        let (data, writer) = verify_account_payload(
            host,
            record,
            "account-models",
            fresh,
            |data: &Definitions| data.issued_at_ms,
        )?;
        if data.version != 1
            || data.revision == 0
            || data.revision == u64::MAX
            || host.account_id().as_deref() != Some(&data.account)
            || data.writer != writer
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_api_key_sync_does_not_restore_an_oauth_file() {
        let mut sender = Config::default();
        sender.providers.insert(
            "codex".into(),
            ProviderConfig::Codex {
                base_url: Some("https://gateway.invalid/v1".into()),
                auth_file: String::new(),
                api_key: Some("test-key".into()),
            },
        );
        let mut receiver = Config::default();
        receiver.providers.insert(
            "codex".into(),
            ProviderConfig::Codex {
                base_url: None,
                auth_file: "receiver-login.json".into(),
                api_key: None,
            },
        );
        let definitions = Definitions::from_config(&sender, "account".into(), 1, StageId::new());
        let imported = definitions.materialize(&receiver);
        let ProviderConfig::Codex {
            auth_file, api_key, ..
        } = &imported.providers["codex"]
        else {
            panic!("Codex");
        };
        assert!(auth_file.is_empty());
        assert_eq!(api_key.as_deref(), Some("test-key"));
        let definitions = Definitions::from_config(&receiver, "account".into(), 2, StageId::new());
        assert!(
            !serde_json::to_string(&definitions)
                .unwrap()
                .contains("receiver-login.json")
        );
        assert!(
            definitions.materialize(&receiver).providers["codex"] == receiver.providers["codex"]
        );
    }

    #[test]
    fn model_affinities_sync_with_core_and_lightweight_selections() {
        let mut sender = Config {
            main_model: "reasoner".into(),
            approval_model: Some("small".into()),
            ..Default::default()
        };
        for (name, capabilities) in [
            ("reasoner", "Complex coordination"),
            ("small", "Low-cost short judgments"),
        ] {
            sender.models.insert(
                name.into(),
                ModelConfig {
                    capabilities: capabilities.into(),
                    ..Default::default()
                },
            );
        }
        let definitions = Definitions::from_config(&sender, "account".into(), 1, StageId::new());
        let wire = serde_json::to_vec(&definitions).unwrap();
        let restored: Definitions = serde_json::from_slice(&wire).unwrap();
        let imported = restored.materialize(&Config::default());
        assert_eq!(imported.main_model, "reasoner");
        assert_eq!(imported.approval_model.as_deref(), Some("small"));
        assert_eq!(
            imported.models["reasoner"].capabilities,
            "Complex coordination"
        );
        assert_eq!(
            imported.models["small"].capabilities,
            "Low-cost short judgments"
        );
    }

    #[test]
    fn opencode_channel_sync_omits_sender_auth_and_retains_receiver_login() {
        for (source, target) in [
            ("open-code-console-oauth", "open-code-go-oauth"),
            ("open-code-go-oauth", "open-code-console-oauth"),
        ] {
            let mut sender = Config::default();
            sender.providers.insert(
                "console".into(),
                serde_json::from_value(serde_json::json!({
                    "type":source, "auth_file":"sender-private.json"
                }))
                .unwrap(),
            );
            let definitions =
                Definitions::from_config(&sender, "account".into(), 1, StageId::new());
            assert!(
                !serde_json::to_string(&definitions)
                    .unwrap()
                    .contains("sender-private.json")
            );
            let mut receiver = Config::default();
            receiver.providers.insert(
                "console".into(),
                serde_json::from_value(serde_json::json!({
                    "type":target, "auth_file":"receiver-private.json"
                }))
                .unwrap(),
            );
            let imported = definitions.materialize(&receiver);
            let provider = serde_json::to_value(&imported.providers["console"]).unwrap();
            assert_eq!(provider["type"], source);
            assert_eq!(provider["auth_file"], "receiver-private.json");
        }
    }
}
