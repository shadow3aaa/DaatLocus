//! One account trust namespace and one Actor store per local Stage. Membership
//! never confers another account's access.
use super::{
    Result, StageError, StageId,
    identity::{DeviceIdentity, DeviceProfile, Enrollment, JoinRequest, verify_enrollment},
    ledger::StageLedger,
    transport::ssh::SshTarget,
};
use crate::actors::ActorSystem;
use parking_lot::RwLock;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    sync::Arc,
};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default = "super::device_name::device_name")]
    pub label: String,
    pub listen: String,
    pub interfaces: Vec<String>,
    pub signaling_origin: Option<String>,
    pub stun: Vec<String>,
    pub ssh: BTreeMap<StageId, SshTarget>,
    #[serde(default = "default_priority")]
    pub compute_priority: u16,
}
fn default_priority() -> u16 {
    100
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            label: super::device_name::device_name(),
            listen: "0.0.0.0:0".into(),
            interfaces: vec![],
            signaling_origin: None,
            stun: vec![],
            ssh: BTreeMap::new(),
            compute_priority: 100,
        }
    }
}
impl Settings {
    /// Public rendezvous needs reflexive ICE candidates across NAT. This is
    /// enabled only with an explicit public connection origin; LAN/SSH stay
    /// independent of external services. Preserve operator-provided servers.
    pub(crate) fn public_stun_urls(&self) -> Vec<String> {
        if self.stun.is_empty()
            && self
                .signaling_origin
                .as_deref()
                .is_some_and(|origin| origin.starts_with("wss://"))
        {
            vec!["stun:stun.cloudflare.com:3478".into()]
        } else {
            self.stun.clone()
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.label.trim().is_empty()
            || self.label.len() > 100
            || self.listen.parse::<std::net::SocketAddr>().is_err()
            || self.interfaces.len() > 16
            || self
                .interfaces
                .iter()
                .any(|name| name.is_empty() || name.len() > 100)
            || self.ssh.len() > 64
            || self.stun.len() > 4
        {
            return Err(StageError::Invalid("Invalid device settings"));
        }
        for target in self.ssh.values() {
            target.arguments()?;
        }
        if self
            .stun
            .iter()
            .any(|url| !url.starts_with("stun:") || url.contains('@') || url.len() > 200)
        {
            return Err(StageError::Invalid("Invalid STUN configuration"));
        }
        if let Some(value) = self.signaling_origin.as_deref() {
            let url = url::Url::parse(value)
                .map_err(|_| StageError::Invalid("Invalid signaling origin"))?;
            if url.scheme() != "wss"
                && !(cfg!(test) && url.scheme() == "ws" && url.host_str() == Some("127.0.0.1"))
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
                || url.path() != "/"
            {
                return Err(StageError::Invalid(
                    "Signaling requires an explicit WSS origin",
                ));
            }
        }
        Ok(())
    }
}

pub struct Host {
    pub accounts: crate::accounts::AccountStore,
    pub configurations: crate::accounts::configuration::ConfigurationStore,
    pub(crate) coordination: RwLock<Option<std::sync::Weak<super::coordination::Coordinator>>>,
    pub(crate) core: RwLock<Option<std::sync::Weak<super::shared_runtime::Controller>>>,
    pub system: ActorSystem,
    pub local: StageId,
    directory: PathBuf,
    identity: RwLock<Option<Arc<DeviceIdentity>>>,
    enrollment: RwLock<Option<Enrollment>>,
    settings: RwLock<Settings>,
    pub lifecycle: tokio::sync::Mutex<()>,
    pub(crate) work_reset: super::reset::Pending,
}
impl Host {
    /// Loading preserves the device identity; login/discovery prepares missing keys.
    pub fn load(system: ActorSystem) -> Result<Arc<Self>> {
        let local = system.store().public_stage_id()?;
        let directory = system.infrastructure_dir().join("stage");
        let identity = if directory.join("identity.json").exists() {
            Some(Arc::new(DeviceIdentity::load(
                &directory.join("identity.json"),
            )?))
        } else {
            None
        };
        if identity
            .as_ref()
            .is_some_and(|identity| identity.profile().stage_id != local)
        {
            return Err(StageError::Conflict(
                "Device identity does not match its Actor store",
            ));
        }
        let metadata=system.store().stage_transaction(|tx|{
            tx.execute_batch("CREATE TABLE IF NOT EXISTS stage_host_config(key TEXT PRIMARY KEY,value TEXT NOT NULL);")?;
            let settings:Option<String>=tx.query_row("SELECT value FROM stage_host_config WHERE key='settings'",[],|r|r.get(0)).optional()?;
            let enrollment:Option<String>=tx.query_row("SELECT value FROM stage_host_config WHERE key='enrollment'",[],|r|r.get(0)).optional()?;Ok((settings,enrollment))
        })?;
        let settings = metadata
            .0
            .map(|value| {
                let mut persisted: serde_json::Value = serde_json::from_str(&value)
                    .map_err(|_| StageError::Invalid("Invalid persisted device settings"))?;
                if let Some(fields) = persisted.as_object_mut() {
                    for key in ["enabled", "lan", "public_discovery", "shared_actor"] {
                        fields.remove(key);
                    }
                    if fields.get("label").and_then(|value| value.as_str()) == Some("This device") {
                        fields.insert(
                            "label".into(),
                            serde_json::Value::String(super::device_name::device_name()),
                        );
                    }
                }
                serde_json::from_value::<Settings>(persisted)
                    .map_err(|_| StageError::Invalid("Invalid persisted device settings"))
            })
            .transpose()?
            .unwrap_or_default();
        settings.validate()?;
        let enrollment = metadata
            .1
            .map(|value| {
                serde_json::from_str::<Enrollment>(&value)
                    .map_err(|_| StageError::Invalid("Invalid persisted membership"))
            })
            .transpose()?;
        if let Some(own) = &enrollment {
            let identity = identity
                .as_ref()
                .ok_or(StageError::Invalid("Membership has no device identity"))?;
            // An expired credential remains inspectable/recoverable by Owner.
            // Only historical signature validation is used here; live operations
            // revalidate it against the current time in enrollment()/peer().
            let expires = own
                .chain
                .iter()
                .map(|proof| {
                    use base64::Engine;
                    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .decode(&proof.payload)
                        .map_err(|_| StageError::Invalid("Invalid membership bytes"))?;
                    let member: super::identity::Membership = serde_json::from_slice(&bytes)
                        .map_err(|_| StageError::Invalid("Invalid membership"))?;
                    Ok(member.expires_at_ms)
                })
                .collect::<Result<Vec<_>>>()?;
            let at = chrono::Utc::now().timestamp_millis().min(
                expires
                    .into_iter()
                    .min()
                    .ok_or(StageError::Invalid("Membership chain is empty"))?
                    .saturating_sub(1),
            );
            let member = verify_enrollment(&own.root, &own.chain, at, &HashSet::new())?;
            if member.device != identity.profile() {
                return Err(StageError::Forbidden(
                    "Membership does not match this device",
                ));
            }
            StageLedger::initialize(system.store(), local, &own.root)?;
        }
        let host = Arc::new(Self {
            configurations: crate::accounts::configuration::ConfigurationStore::open(
                &system.account_credentials_dir(),
            )
            .map_err(|_| StageError::Invalid("Cannot load private account configuration"))?,
            coordination: RwLock::new(None),
            accounts: crate::accounts::AccountStore::open(&system.account_credentials_dir())
                .map_err(|_| StageError::Invalid("Cannot load private account credentials"))?,
            core: RwLock::new(None),
            system,
            local,
            directory,
            identity: RwLock::new(identity),
            enrollment: RwLock::new(enrollment),
            settings: RwLock::new(settings),
            lifecycle: tokio::sync::Mutex::new(()),
            work_reset: Default::default(),
        });
        super::reset::initialize(&host)?;
        Ok(host)
    }
    pub fn identity(&self) -> Result<Arc<DeviceIdentity>> {
        self.identity
            .read()
            .clone()
            .ok_or(StageError::Unavailable("Device has not been initialized"))
    }
    pub fn enrollment(&self) -> Result<Enrollment> {
        let own = self
            .enrollment
            .read()
            .clone()
            .ok_or(StageError::Unavailable("Account has not been joined"))?;
        verify_enrollment(
            &own.root,
            &own.chain,
            chrono::Utc::now().timestamp_millis(),
            &super::revocation::removed(self.system.store())?,
        )?;
        Ok(own)
    }
    pub fn profile(&self) -> Option<DeviceProfile> {
        self.identity
            .read()
            .as_ref()
            .map(|identity| identity.profile())
    }
    pub fn settings(&self) -> Settings {
        self.settings.read().clone()
    }
    pub fn account_id(&self) -> Option<String> {
        self.enrollment
            .read()
            .as_ref()
            .map(|membership| membership.root.group_id.clone())
    }
    fn persist<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let value = serde_json::to_string(value)
            .map_err(|_| StageError::Invalid("Cannot encode device configuration"))?;
        self.system.store().stage_transaction(|tx|{tx.execute("INSERT INTO stage_host_config VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key,value])?;Ok(())})
    }
    pub fn initialize(&self, create_account: bool) -> Result<JoinRequest> {
        if self.profile().is_some() {
            if create_account && self.account_id().is_none() {
                let path = self.directory.join("identity.json");
                let mut identity = DeviceIdentity::load(&path)?;
                let now = chrono::Utc::now().timestamp_millis();
                let own = identity.initialize_group(now, now + 365 * 24 * 60 * 60 * 1000)?;
                identity.save_existing(&path)?;
                StageLedger::initialize(self.system.store(), self.local, &own.root)?;
                self.persist("enrollment", &own)?;
                *self.identity.write() = Some(Arc::new(identity));
                *self.enrollment.write() = Some(own);
            }
            return self
                .identity()?
                .join_request(chrono::Utc::now().timestamp_millis());
        }
        let now = chrono::Utc::now().timestamp_millis();
        let mut identity = DeviceIdentity::generate_for_stage(self.local)?;
        let own = if create_account {
            Some(identity.initialize_group(now, now + 365 * 24 * 60 * 60 * 1000)?)
        } else {
            None
        };
        identity.save_new(&self.directory.join("identity.json"))?;
        let identity = Arc::new(identity);
        let request = identity.join_request(now)?;
        *self.identity.write() = Some(identity);
        if let Some(own) = own {
            StageLedger::initialize(self.system.store(), self.local, &own.root)?;
            self.persist("enrollment", &own)?;
            *self.enrollment.write() = Some(own);
        }
        Ok(request)
    }
    pub fn join(&self, own: Enrollment, pinned_account: &str) -> Result<()> {
        if own.root.group_id != pinned_account {
            return Err(StageError::Forbidden(
                "Account trust must be explicitly confirmed",
            ));
        }
        if self
            .account_id()
            .as_deref()
            .is_some_and(|id| id != pinned_account)
        {
            return Err(StageError::Forbidden(
                "A different account requires a separate local Stage and store",
            ));
        }
        let member = verify_enrollment(
            &own.root,
            &own.chain,
            chrono::Utc::now().timestamp_millis(),
            &HashSet::new(),
        )?;
        if member.device != self.identity()?.profile() {
            return Err(StageError::Forbidden(
                "Enrollment belongs to another device",
            ));
        }
        let ledger = if self.account_id().is_none() {
            StageLedger::initialize(self.system.store(), self.local, &own.root)?
        } else {
            StageLedger::attach(self.system.store(), self.local)
        };
        self.persist("enrollment", &own)?;
        *self.enrollment.write() = Some(own.clone());
        // Issuer membership provides connection authentication, never Actor grants.
        for length in 1..own.chain.len() {
            let enrollment = Enrollment {
                root: own.root.clone(),
                chain: own.chain[..length].to_vec(),
            };
            ledger.import_peer(enrollment, vec![], chrono::Utc::now().timestamp_millis())?;
        }
        Ok(())
    }
    pub(crate) fn enroll_authenticated_device(&self, request: &JoinRequest) -> Result<Enrollment> {
        let now = chrono::Utc::now().timestamp_millis();
        let own = self.enrollment()?;
        let issuer = verify_enrollment(&own.root, &own.chain, now, &HashSet::new())?;
        let enrollment = self.identity()?.authorize(
            &own,
            request,
            now,
            issuer.expires_at_ms.min(now + 180 * 24 * 60 * 60 * 1000),
            true,
        )?;
        StageLedger::attach(self.system.store(), self.local).import_peer(
            enrollment.clone(),
            vec![],
            now,
        )?;
        Ok(enrollment)
    }
    pub fn renew_account_authority(&self) -> Result<Enrollment> {
        if super::revocation::removed(self.system.store())?.contains(&self.local) {
            return Err(StageError::Forbidden("Revoked identity cannot be renewed"));
        }
        let path = self.directory.join("identity.json");
        let mut identity = DeviceIdentity::load(&path)?;
        if !identity.has_group_authority() {
            return Err(StageError::Forbidden(
                "Renewal requires the account authority device",
            ));
        }
        let now = chrono::Utc::now().timestamp_millis();
        let own = identity.initialize_group(now, now + 365 * 24 * 60 * 60 * 1000)?;
        if self.account_id().as_deref() != Some(&own.root.group_id) {
            return Err(StageError::Forbidden(
                "Renewal cannot change account identity",
            ));
        }
        self.persist("enrollment", &own)?;
        *self.enrollment.write() = Some(own.clone());
        Ok(own)
    }
    pub fn revoke_account_device(&self, target: StageId) -> Result<super::revocation::Change> {
        let now = chrono::Utc::now().timestamp_millis();
        let enrollment = self.enrollment()?;
        let proof = self.identity()?.sign(
            "account-revocation",
            &super::revocation::Record {
                version: 1,
                group_id: enrollment.root.group_id.clone(),
                issuer: self.local,
                target,
                issued_at_ms: now,
                nonce: uuid::Uuid::new_v4(),
            },
        )?;
        let change = super::revocation::Change { enrollment, proof };
        super::revocation::apply(self.system.store(), self.local, &change, now)?;
        Ok(change)
    }
    pub fn discover_member(&self, enrollment: Enrollment) -> Result<()> {
        let own = self.enrollment()?;
        if own.root != enrollment.root {
            return Err(StageError::Forbidden("Member belongs to another account"));
        }
        let now = chrono::Utc::now().timestamp_millis();
        let member = verify_enrollment(&own.root, &enrollment.chain, now, &HashSet::new())?;
        if member.device.stage_id == self.local {
            return Ok(());
        }
        let ledger = StageLedger::attach(self.system.store(), self.local);
        let grants = ledger
            .stored_peer(member.device.stage_id)?
            .map(|p| p.grants)
            .unwrap_or_default();
        ledger.import_peer(enrollment, grants, now)?;
        Ok(())
    }
    pub fn save_settings(&self, candidate: Settings) -> Result<()> {
        candidate.validate()?;
        self.persist("settings", &candidate)?;
        *self.settings.write() = candidate;
        Ok(())
    }
    pub fn device_label(&self, peer: StageId) -> Result<Option<String>> {
        self.system.store().coordination_transaction(|tx|{
            tx.execute_batch("CREATE TABLE IF NOT EXISTS stage_device_labels(stage TEXT PRIMARY KEY,label TEXT NOT NULL);")?;
            tx.query_row("SELECT label FROM stage_device_labels WHERE stage=?1",[peer.to_string()],|row|row.get(0)).optional().map_err(StageError::from)
        })
    }
    pub fn record_device_label(&self, peer: StageId, label: &str) -> Result<()> {
        if label.trim().is_empty() || label.len() > 100 {
            return Err(StageError::Invalid("Invalid device name"));
        }
        self.system.store().coordination_transaction(|tx|{
            tx.execute_batch("CREATE TABLE IF NOT EXISTS stage_device_labels(stage TEXT PRIMARY KEY,label TEXT NOT NULL);")?;
            tx.execute("INSERT INTO stage_device_labels VALUES(?1,?2) ON CONFLICT(stage) DO UPDATE SET label=excluded.label",params![peer.to_string(),label])?;Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_origin_enables_stun_without_an_external_dependency_for_lan() {
        let mut settings = Settings::default();
        assert!(settings.public_stun_urls().is_empty());
        settings.signaling_origin = Some("wss://connection.example".into());
        assert_eq!(
            settings.public_stun_urls(),
            ["stun:stun.cloudflare.com:3478"]
        );
        settings.stun = vec!["stun:operator.example:3478".into()];
        assert_eq!(settings.public_stun_urls(), settings.stun);
        settings.stun.clear();
        settings.signaling_origin = Some("ws://127.0.0.1:45827".into());
        assert!(settings.public_stun_urls().is_empty());
    }
}
