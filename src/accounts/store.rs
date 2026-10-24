use crate::persistence::{PersistenceFileMode, write_bytes_atomic_sync};
use daat_locus_auth::{PasswordFile, Setup};
use miette::{IntoDiagnostic, Result, miette};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::Notify;
use zeroize::{Zeroize, Zeroizing};

const MAX_RECORD: usize = 16 * 1024;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialBundle {
    pub version: u16,
    pub account_id: String,
    pub name: String,
    pub revision: u64,
    pub previous_binding: Option<String>,
    setup: Vec<u8>,
    record: Vec<u8>,
}
impl std::fmt::Debug for CredentialBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CredentialBundle([private])")
    }
}
impl Drop for CredentialBundle {
    fn drop(&mut self) {
        self.setup.zeroize();
        self.record.zeroize();
    }
}
impl CredentialBundle {
    pub fn new(
        account_id: String,
        name: String,
        setup: &Setup,
        record: &PasswordFile,
        previous: Option<&Self>,
    ) -> Result<Self> {
        let value = Self {
            version: 1,
            account_id,
            name: normalize_name(&name)?,
            revision: previous.map_or(1, |old| old.revision.saturating_add(1)),
            previous_binding: previous.map(Self::binding),
            setup: setup.serialize().to_vec(),
            record: record.serialize().to_vec(),
        };
        value.validate()?;
        Ok(value)
    }
    fn validate(&self) -> Result<()> {
        if self.version != 1
            || self.account_id.len() != 64
            || !self
                .account_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            || self.name != normalize_name(&self.name)?
            || self.revision == 0
            || self.revision == u64::MAX
            || self.setup.len() > 1024
            || self.record.len() > 2048
            || self
                .previous_binding
                .as_ref()
                .is_some_and(|value| value.len() != 64 || hex::decode(value).is_err())
        {
            return Err(miette!("Invalid account credential record"));
        }
        self.setup()?;
        self.password_file()?;
        Ok(())
    }
    pub fn setup(&self) -> Result<Setup> {
        Setup::deserialize(&self.setup).map_err(|_| miette!("Invalid account setup"))
    }
    pub fn password_file(&self) -> Result<PasswordFile> {
        PasswordFile::deserialize(&self.record)
            .map_err(|_| miette!("Invalid account password record"))
    }
    pub fn identifier(&self) -> Vec<u8> {
        format!("DaatLocus.Account.v1/{}/{}", self.account_id, self.name).into_bytes()
    }
    pub fn binding(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(b"DaatLocus.AccountCredential.v1\0");
        hash.update(&self.account_id);
        hash.update([0]);
        hash.update(&self.name);
        hash.update(self.revision.to_le_bytes());
        hash.update(&self.setup);
        hash.update(&self.record);
        hex::encode(hash.finalize())
    }
    pub fn metadata(&self) -> Metadata {
        Metadata {
            account_id: self.account_id.clone(),
            name: self.name.clone(),
            revision: self.revision,
            binding: self.binding(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Metadata {
    pub account_id: String,
    pub name: String,
    pub revision: u64,
    pub binding: String,
}
pub fn normalize_name(value: &str) -> Result<String> {
    let value = value.trim().to_lowercase();
    if value.is_empty()
        || value.len() > 128
        || !value
            .chars()
            .all(|ch| ch.is_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err(miette!("Invalid account name"));
    }
    Ok(value)
}
#[derive(Clone)]
pub struct AccountStore {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
    changed: Arc<Notify>,
}
impl AccountStore {
    pub fn open(directory: &Path) -> Result<Self> {
        std::fs::create_dir_all(directory).into_diagnostic()?;
        crate::actors::permissions::protect_private_path(directory)?;
        let store = Self {
            path: directory.join("credentials.json"),
            lock: Arc::new(Mutex::new(())),
            changed: Arc::new(Notify::new()),
        };
        store.current()?;
        Ok(store)
    }
    pub fn changed(&self) -> &Notify {
        &self.changed
    }
    pub fn current(&self) -> Result<Option<CredentialBundle>> {
        let file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).into_diagnostic(),
        };
        let mut bytes = Zeroizing::new(Vec::new());
        file.take(MAX_RECORD as u64 + 1)
            .read_to_end(&mut bytes)
            .into_diagnostic()?;
        if bytes.len() > MAX_RECORD {
            return Err(miette!("Account credential budget exceeded"));
        }
        let record: CredentialBundle = serde_json::from_slice(&bytes).into_diagnostic()?;
        record.validate()?;
        Ok(Some(record))
    }
    /// Only a caller with verified account/device authority may supply a record.
    pub fn install(&self, record: CredentialBundle, expected_account: &str) -> Result<bool> {
        record.validate()?;
        if record.account_id != expected_account {
            return Err(miette!("Account namespace mismatch"));
        }
        let _lock = self.lock.lock();
        if let Some(previous) = self.current()? {
            if previous.account_id != record.account_id || previous.name != record.name {
                return Err(miette!(
                    "Account identity cannot change in an existing profile"
                ));
            }
            if previous.binding() == record.binding() {
                return Ok(false);
            }
            if record.revision <= previous.revision {
                return Err(miette!("Account credential rollback rejected"));
            }
            if record.revision == previous.revision + 1
                && record.previous_binding.as_deref() != Some(previous.binding().as_str())
            {
                return Err(miette!("Conflicting account credential revision"));
            }
        }
        let bytes = Zeroizing::new(serde_json::to_vec(&record).into_diagnostic()?);
        if bytes.len() > MAX_RECORD {
            return Err(miette!("Account credential budget exceeded"));
        }
        write_bytes_atomic_sync(&self.path, &bytes, PersistenceFileMode::Private)
            .into_diagnostic()?;
        self.changed.notify_waiters();
        Ok(true)
    }
    pub fn authorizes(&self, account_id: &str, binding: &str) -> bool {
        self.current()
            .ok()
            .flatten()
            .is_some_and(|record| record.account_id == account_id && record.binding() == binding)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn credential(group: &str, previous: Option<&CredentialBundle>) -> CredentialBundle {
        let setup = daat_locus_auth::new_setup();
        let client =
            daat_locus_auth::RegistrationClient::start(b"isolated account password".to_vec())
                .unwrap();
        let response = daat_locus_auth::registration_response(
            &setup,
            format!("DaatLocus.Account.v1/{group}/alice").as_bytes(),
            &client.request(),
        )
        .unwrap();
        let record = daat_locus_auth::password_file(&client.finish(&response).unwrap()).unwrap();
        CredentialBundle::new(group.into(), "Alice".into(), &setup, &record, previous).unwrap()
    }
    #[test]
    fn namespaces_revision_and_private_record_never_become_another_profiles_authority() {
        let temp = tempfile::tempdir().unwrap();
        let store = AccountStore::open(temp.path()).unwrap();
        let first = credential(&"a".repeat(64), None);
        let binding = first.binding();
        assert!(store.install(first.clone(), &"b".repeat(64)).is_err());
        store.install(first.clone(), &first.account_id).unwrap();
        assert!(store.authorizes(&first.account_id, &binding));
        assert!(!store.authorizes(&"b".repeat(64), &binding));
        let next = credential(&first.account_id, Some(&first));
        store.install(next.clone(), &first.account_id).unwrap();
        assert!(!store.authorizes(&first.account_id, &binding));
        assert!(store.install(first.clone(), &first.account_id).is_err());
        assert_eq!(store.current().unwrap().unwrap().binding(), next.binding());
        assert!(!format!("{next:?}").contains(&format!("{:?}", next.setup)));
    }
}
