//! Account-owned instruction files and unordered role membership. No Actor lifecycle effects.
use crate::{
    accounts::configuration::{Record, verify_account_payload},
    stage::{host::Host, protocol::BlobRef},
};
use crate::{
    actors::ActorError,
    persistence::{PersistenceFileMode, write_bytes_atomic_sync},
    stage::StageId,
};
use miette::{IntoDiagnostic, Result};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use tokio::sync::Notify;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Offer {
    pub record: Record,
    pub export: Uuid,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedLibrary {
    account: String,
    issued_at_ms: i64,
    revision: u64,
    writer: StageId,
    content: BlobRef,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    library: Library,
    signed: Option<Record>,
    #[serde(default)]
    sequence: u64,
}
fn next_sequence(sequence: u64) -> Result<u64> {
    sequence
        .checked_add(1)
        .filter(|value| *value < (1u64 << 53) - 1)
        .ok_or_else(|| ActorError::Invalid("Instruction cursor exhausted".into()).into())
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    #[default]
    Core,
    Others,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    pub id: String,
    pub name: String,
    pub content: String,
}
impl Layer {
    pub fn revision(&self) -> String {
        crate::stage::digest(&serde_json::to_vec(self).expect("instruction strings serialize"))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Library {
    pub version: u16,
    pub revision: u64,
    pub writer: StageId,
    pub layers: Vec<Layer>,
    pub core: BTreeSet<String>,
    pub others: BTreeSet<String>,
}
impl Library {
    pub fn ids(&self, role: Role) -> &BTreeSet<String> {
        match role {
            Role::Core => &self.core,
            Role::Others => &self.others,
        }
    }
    pub fn enabled(&self, role: Role) -> Vec<&Layer> {
        let ids = self.ids(role);
        let mut files: Vec<_> = self
            .layers
            .iter()
            .filter(|layer| ids.contains(&layer.id))
            .collect();
        files.sort_by(|a, b| {
            b.content
                .len()
                .cmp(&a.content.len())
                .then_with(|| a.id.cmp(&b.id))
        });
        files
    }
    pub fn validate(&self) -> Result<()> {
        if self.version != 1 || self.revision == u64::MAX || self.writer.0.is_nil() {
            return Err(ActorError::Invalid("Invalid instruction library".into()).into());
        }
        let mut ids = BTreeSet::new();
        let mut names = BTreeSet::new();
        for layer in &self.layers {
            if Uuid::parse_str(&layer.id).is_err()
                || !ids.insert(&layer.id)
                || !names.insert(layer.name.to_lowercase())
                || Self::name(&layer.name)? != layer.name
                || layer.content.trim().is_empty()
            {
                return Err(ActorError::Invalid("Invalid instruction file".into()).into());
            }
        }
        if self
            .core
            .iter()
            .chain(&self.others)
            .any(|id| !ids.contains(id))
        {
            return Err(ActorError::Invalid("Unknown instruction file".into()).into());
        }
        Ok(())
    }
    fn name(name: &str) -> Result<String> {
        let name = name.trim();
        if name.is_empty()
            || name.len() > 240
            || name
                .chars()
                .any(|c| c.is_control() || "/\\:*?\"<>|".contains(c))
            || matches!(name, "." | ".." | ".md")
        {
            return Err(ActorError::Invalid("Invalid instruction file name".into()).into());
        }
        Ok(if name.ends_with(".md") {
            name.to_owned()
        } else {
            format!("{name}.md")
        })
    }
}

pub struct InstructionStore {
    path: PathBuf,
    writer: StageId,
    data: RwLock<Stored>,
    changed: Notify,
}
impl InstructionStore {
    pub fn open(directory: &Path, writer: StageId) -> Result<Self> {
        std::fs::create_dir_all(directory).into_diagnostic()?;
        crate::actors::permissions::protect_private_path(directory)?;
        let path = directory.join("instructions.json");
        let data = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).into_diagnostic()?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Stored {
                library: Library {
                    version: 1,
                    revision: 0,
                    writer,
                    layers: Vec::new(),
                    core: BTreeSet::new(),
                    others: BTreeSet::new(),
                },
                signed: None,
                sequence: 0,
            },
            Err(error) => return Err(error).into_diagnostic(),
        };
        data.library.validate()?;
        next_sequence(data.sequence)?;
        Ok(Self {
            path,
            writer,
            data: RwLock::new(data),
            changed: Notify::new(),
        })
    }
    pub fn snapshot(&self) -> Library {
        self.data.read().library.clone()
    }
    pub fn binding(&self) -> (u64, StageId) {
        let data = self.data.read();
        (data.library.revision, data.library.writer)
    }
    pub fn changed(&self) -> &Notify {
        &self.changed
    }
    /// Device-local monotonic invalidation cursor, independent of replicated writer clocks.
    pub fn event_cursor(&self) -> u64 {
        self.data.read().sequence + 1
    }
    fn persist(&self, next: &Stored) -> Result<()> {
        write_bytes_atomic_sync(
            &self.path,
            &serde_json::to_vec(next).into_diagnostic()?,
            PersistenceFileMode::Private,
        )
        .into_diagnostic()
    }
    fn update<T>(&self, change: impl FnOnce(&mut Library) -> Result<T>) -> Result<T> {
        let mut current = self.data.write();
        let mut next = current.library.clone();
        let value = change(&mut next)?;
        if next != current.library {
            next.writer = self.writer;
            next.revision = next
                .revision
                .checked_add(1)
                .ok_or_else(|| ActorError::Invalid("Instruction revision exhausted".into()))?;
            next.validate()?;
            let next = Stored {
                library: next,
                signed: None,
                sequence: next_sequence(current.sequence)?,
            };
            self.persist(&next)?;
            *current = next;
            self.changed.notify_waiters();
        }
        Ok(value)
    }
    pub(crate) fn sync_binding(&self) -> Option<String> {
        let (revision, writer) = self.binding();
        (revision > 0).then(|| format!("{revision}:{writer}"))
    }
    fn verify(host: &Host, record: &Record, fresh: bool) -> Result<SignedLibrary> {
        let (data, writer) = verify_account_payload(
            host,
            record,
            "account-instructions",
            fresh,
            |data: &SignedLibrary| data.issued_at_ms,
        )?;
        data.content.validate().into_diagnostic()?;
        if host.account_id().as_deref() != Some(&data.account)
            || data.writer != writer
            || data.revision == 0
            || data.revision == u64::MAX
        {
            return Err(ActorError::Invalid("Invalid signed instruction library".into()).into());
        }
        Ok(data)
    }
    /// Sign only local edits; imported signatures survive forwarding and restarts.
    pub(crate) fn offer(&self, host: &Host, peer: StageId) -> Result<Option<Offer>> {
        let mut current = self.data.write();
        if current.library.revision == 0 {
            return Ok(None);
        }
        let bytes = serde_json::to_vec(&current.library).into_diagnostic()?;
        let content = host
            .system
            .store()
            .stage_transaction(|tx| crate::stage::blob::put_control(tx, &bytes))
            .into_diagnostic()?
            .content;
        let record = if let Some(record) = &current.signed {
            let data = Self::verify(host, record, false)?;
            if data.content != content
                || (data.revision, data.writer)
                    != (current.library.revision, current.library.writer)
            {
                return Err(ActorError::Invalid(
                    "Instruction signature does not match its files".into(),
                )
                .into());
            }
            record.clone()
        } else {
            if current.library.writer != host.local {
                return Err(ActorError::Invalid("Instruction signer unavailable".into()).into());
            }
            let enrollment = host.enrollment().into_diagnostic()?;
            let data = SignedLibrary {
                account: enrollment.root.group_id.clone(),
                issued_at_ms: chrono::Utc::now().timestamp_millis(),
                revision: current.library.revision,
                writer: host.local,
                content: content.clone(),
            };
            let record = Record {
                enrollment,
                proof: host
                    .identity()
                    .into_diagnostic()?
                    .sign("account-instructions", &data)
                    .into_diagnostic()?,
            };
            let next = Stored {
                library: current.library.clone(),
                signed: Some(record.clone()),
                sequence: current.sequence,
            };
            self.persist(&next)?;
            *current = next;
            record
        };
        let export = Uuid::new_v4();
        crate::stage::ledger::StageLedger::attach(host.system.store(), host.local)
            .export_control_content(
                peer,
                export,
                &content,
                chrono::Utc::now().timestamp_millis(),
            )
            .into_diagnostic()?;
        Ok(Some(Offer { record, export }))
    }
    pub(crate) fn offered_content(&self, host: &Host, offer: &Offer) -> Result<Option<BlobRef>> {
        if offer.export.is_nil() {
            return Err(ActorError::Invalid("Invalid instruction export".into()).into());
        }
        let data = Self::verify(host, &offer.record, true)?;
        let binding = self.binding();
        Ok(((data.revision, data.writer) >= binding).then_some(data.content))
    }
    pub(crate) fn import(&self, host: &Host, record: Record, bytes: &[u8]) -> Result<bool> {
        let signed = Self::verify(host, &record, true)?;
        if signed.content.size != bytes.len() as u64
            || signed.content.hash != crate::stage::digest(bytes)
        {
            return Err(
                ActorError::Invalid("Instruction content signature mismatch".into()).into(),
            );
        }
        let library: Library = serde_json::from_slice(bytes).into_diagnostic()?;
        library.validate()?;
        if (library.revision, library.writer) != (signed.revision, signed.writer) {
            return Err(
                ActorError::Invalid("Instruction revision signature mismatch".into()).into(),
            );
        }
        let mut current = self.data.write();
        let previous = (current.library.revision, current.library.writer);
        let next = (library.revision, library.writer);
        if next < previous {
            return Ok(false);
        }
        if next == previous {
            if library != current.library {
                return Err(ActorError::Conflict("Conflicting instruction revision".into()).into());
            }
            return Ok(false);
        }
        let next = Stored {
            library,
            signed: Some(record),
            sequence: next_sequence(current.sequence)?,
        };
        self.persist(&next)?;
        *current = next;
        self.changed.notify_waiters();
        Ok(true)
    }
    pub fn save(
        &self,
        id: Option<&str>,
        revision: Option<&str>,
        name: &str,
        content: String,
    ) -> Result<Layer> {
        let name = Library::name(name)?;
        if content.trim().is_empty() {
            return Err(ActorError::Invalid("Empty instruction file".into()).into());
        }
        self.update(|library| {
            let existing = id
                .map(|id| {
                    library
                        .layers
                        .iter()
                        .position(|layer| layer.id == id)
                        .ok_or_else(|| ActorError::NotFound("Instruction file not found".into()))
                })
                .transpose()?;
            if let Some(index) = existing
                && revision != Some(library.layers[index].revision().as_str())
            {
                return Err(ActorError::Conflict("Instruction file changed".into()).into());
            }
            if library.layers.iter().any(|layer| {
                Some(layer.id.as_str()) != id && layer.name.to_lowercase() == name.to_lowercase()
            }) {
                return Err(
                    ActorError::Conflict("Instruction file name already exists".into()).into(),
                );
            }
            let file = Layer {
                id: id
                    .map(str::to_owned)
                    .unwrap_or_else(|| Uuid::new_v4().to_string()),
                name,
                content,
            };
            if let Some(index) = existing {
                library.layers[index] = file.clone();
            } else {
                library.layers.push(file.clone());
            }
            Ok(file)
        })
    }
    pub fn replace(
        &self,
        role: Role,
        ids: Vec<String>,
        check: impl FnOnce(&Library) -> Result<()>,
    ) -> Result<()> {
        self.update(|library| {
            check(library)?;
            let count = ids.len();
            let ids: BTreeSet<_> = ids.into_iter().collect();
            if ids.len() != count
                || ids
                    .iter()
                    .any(|id| !library.layers.iter().any(|layer| &layer.id == id))
            {
                return Err(ActorError::Invalid("Invalid instruction membership".into()).into());
            }
            match role {
                Role::Core => library.core = ids,
                Role::Others => library.others = ids,
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn host(directory: &Path) -> std::sync::Arc<Host> {
        let system = crate::actors::ActorSystem::open_account_gated(
            directory.join("data"),
            directory.join("workspace"),
            crate::config::Config::default(),
        )
        .await
        .unwrap();
        Host::load(system).unwrap()
    }

    #[tokio::test]
    async fn concurrent_device_edits_converge_by_revision_and_writer() {
        let directory = tempfile::tempdir().unwrap();
        let a = host(&directory.path().join("a")).await;
        let b = host(&directory.path().join("b")).await;
        a.initialize(true).unwrap();
        let enrollment = a
            .enroll_authenticated_device(&b.initialize(false).unwrap())
            .unwrap();
        b.join(enrollment, &a.account_id().unwrap()).unwrap();
        a.system
            .instructions()
            .save(None, None, "a", "First offline edit".into())
            .unwrap();
        b.system
            .instructions()
            .save(None, None, "b", "Second offline edit".into())
            .unwrap();
        let snapshots: Vec<_> = [(&a, &b), (&b, &a)]
            .into_iter()
            .map(|(source, target)| {
                let offer = source
                    .system
                    .instructions()
                    .offer(source, target.local)
                    .unwrap()
                    .unwrap();
                let content = target.system.instructions().offered_content(target, &offer);
                let bytes = serde_json::to_vec(&source.system.instructions().snapshot()).unwrap();
                (offer.record, bytes, content)
            })
            .collect();
        assert_eq!(
            snapshots
                .iter()
                .filter(|(_, _, content)| content.as_ref().unwrap().is_some())
                .count(),
            1
        );
        let expected = if a.local > b.local {
            a.system.instructions().snapshot()
        } else {
            b.system.instructions().snapshot()
        };
        for (target, (record, bytes, _)) in [&b, &a].into_iter().zip(snapshots) {
            target
                .system
                .instructions()
                .import(target, record, &bytes)
                .unwrap();
        }
        assert_eq!(a.system.instructions().snapshot(), expected);
        assert_eq!(b.system.instructions().snapshot(), expected);
        for host in [a, b] {
            host.system.shutdown().await;
        }
    }

    #[tokio::test]
    async fn signed_libraries_are_private_forwardable_and_reject_tampering_stale_and_revoked_writers()
     {
        use crate::stage::{blob::ContentStore, ledger::StageLedger};
        let directory = tempfile::tempdir().unwrap();
        let a = host(&directory.path().join("a")).await;
        let b = host(&directory.path().join("b")).await;
        a.initialize(true).unwrap();
        let enrollment = a
            .enroll_authenticated_device(&b.initialize(false).unwrap())
            .unwrap();
        b.join(enrollment, &a.account_id().unwrap()).unwrap();
        let file = a
            .system
            .instructions()
            .save(
                None,
                None,
                "shared",
                "Shared private instructions 中文\n".repeat(4000),
            )
            .unwrap();
        a.system
            .instructions()
            .replace(Role::Others, vec![file.id.clone()], |_| Ok(()))
            .unwrap();
        let offer = a.system.instructions().offer(&a, b.local).unwrap().unwrap();
        let reference = b
            .system
            .instructions()
            .offered_content(&b, &offer)
            .unwrap()
            .unwrap();
        assert!(
            reference.size > 64 * 1024,
            "files must not be limited by a control-frame budget"
        );
        let ledger = StageLedger::attach(a.system.store(), a.local);
        assert!(
            ledger
                .exported_manifest(
                    b.local,
                    offer.export,
                    &reference,
                    chrono::Utc::now().timestamp_millis()
                )
                .is_ok()
        );
        assert!(
            ledger
                .exported_manifest(
                    b.local,
                    Uuid::new_v4(),
                    &reference,
                    chrono::Utc::now().timestamp_millis()
                )
                .is_err()
        );
        let bytes = ContentStore::new(a.system.store())
            .verified_bytes(&reference)
            .unwrap();
        let mut tampered = bytes.clone();
        tampered[10] ^= 1;
        assert!(
            b.system
                .instructions()
                .import(&b, offer.record.clone(), &tampered)
                .is_err()
        );
        assert!(
            b.system
                .instructions()
                .import(&b, offer.record.clone(), &bytes)
                .unwrap()
        );
        assert_eq!(
            b.system.instructions().snapshot(),
            a.system.instructions().snapshot()
        );
        assert!(
            !b.system
                .instructions()
                .import(&b, offer.record.clone(), &bytes)
                .unwrap()
        );
        let forwarded = b.system.instructions().offer(&b, a.local).unwrap().unwrap();
        assert_eq!(
            forwarded.record.proof.payload, offer.record.proof.payload,
            "forwarding must retain the original signer"
        );
        let reopened =
            InstructionStore::open(&b.system.account_credentials_dir(), b.local).unwrap();
        assert_eq!(reopened.snapshot(), a.system.instructions().snapshot());
        assert_eq!(
            reopened
                .offer(&b, a.local)
                .unwrap()
                .unwrap()
                .record
                .proof
                .payload,
            offer.record.proof.payload
        );

        let foreign = host(&directory.path().join("foreign")).await;
        foreign.initialize(true).unwrap();
        assert!(
            foreign
                .system
                .instructions()
                .offered_content(&foreign, &offer)
                .is_err()
        );
        b.system
            .instructions()
            .save(
                Some(&file.id),
                Some(&file.revision()),
                "shared",
                "A newer local edit".into(),
            )
            .unwrap();
        assert!(
            !b.system
                .instructions()
                .import(&b, offer.record.clone(), &bytes)
                .unwrap()
        );
        StageLedger::attach(b.system.store(), b.local)
            .revoke(a.local)
            .unwrap();
        assert!(
            b.system
                .instructions()
                .import(&b, offer.record, &bytes)
                .is_err(),
            "revocation must be checked again after download"
        );
        for host in [a, b, foreign] {
            host.system.shutdown().await;
        }
    }

    #[test]
    fn files_and_unordered_membership_survive_restart_without_removing_library_files() {
        let directory = tempfile::tempdir().unwrap();
        let writer = StageId::new();
        let store = InstructionStore::open(directory.path(), writer).unwrap();
        let small = store
            .save(None, None, "small", "small text".into())
            .unwrap();
        let large = store
            .save(
                None,
                None,
                "large.md",
                "large content repeated several times".into(),
            )
            .unwrap();
        store
            .replace(Role::Core, vec![small.id.clone(), large.id.clone()], |_| {
                Ok(())
            })
            .unwrap();
        let before = store.snapshot();
        assert_eq!(
            before
                .enabled(Role::Core)
                .iter()
                .map(|file| file.id.as_str())
                .collect::<Vec<_>>(),
            [large.id.as_str(), small.id.as_str()]
        );
        store
            .replace(Role::Core, vec![large.id.clone(), small.id.clone()], |_| {
                Ok(())
            })
            .unwrap();
        assert_eq!(
            store.binding(),
            (before.revision, before.writer),
            "reordering membership must not rewrite the account state"
        );
        store
            .replace(Role::Core, vec![small.id.clone()], |_| Ok(()))
            .unwrap();
        let loaded = InstructionStore::open(directory.path(), writer).unwrap();
        assert_eq!(
            loaded.event_cursor(),
            store.event_cursor(),
            "restarting must preserve stream continuation"
        );
        assert_eq!(loaded.snapshot().layers.len(), 2);
        assert_eq!(
            loaded.snapshot().ids(Role::Core),
            &BTreeSet::from([small.id])
        );
        assert!(loaded.snapshot().ids(Role::Others).is_empty());
    }

    #[test]
    fn stale_edits_and_membership_checks_never_publish_partial_state() {
        let directory = tempfile::tempdir().unwrap();
        let store = InstructionStore::open(directory.path(), StageId::new()).unwrap();
        let file = store
            .save(None, None, "instruction.md", "Original text".into())
            .unwrap();
        let saved = store
            .save(
                Some(&file.id),
                Some(&file.revision()),
                "renamed",
                "Updated text".into(),
            )
            .unwrap();
        assert_ne!(saved.revision(), file.revision());
        let before = store.snapshot();
        assert!(
            store
                .save(
                    Some(&file.id),
                    Some(&file.revision()),
                    "stale",
                    "Lost edit".into()
                )
                .is_err()
        );
        assert!(
            store
                .replace(Role::Core, vec![file.id.clone()], |_| Err(
                    ActorError::Conflict("changed".into()).into()
                ))
                .is_err()
        );
        assert!(
            store
                .replace(Role::Core, vec!["core.md".into()], |_| Ok(()))
                .is_err()
        );
        assert!(
            store
                .replace(Role::Core, vec![file.id.clone(), file.id.clone()], |_| Ok(
                    ()
                ))
                .is_err()
        );
        assert!(
            store
                .save(None, None, "../outside.md", "text".into())
                .is_err()
        );
        assert!(
            store
                .save(None, None, "RENAMED.md", "duplicate".into())
                .is_err()
        );
        assert_eq!(store.snapshot(), before);
        assert_eq!(
            InstructionStore::open(directory.path(), before.writer)
                .unwrap()
                .snapshot(),
            before
        );
    }
}
