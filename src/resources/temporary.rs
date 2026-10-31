//! Time-limited resource storage. Ordinary Actor-resource authorization still applies.
use super::*;
use crate::{actors::StoredResource, file_io::guarded_directory::GuardedDirectory};
use std::{ffi::OsStr, path::PathBuf};

pub(crate) const OUTPUT_LIFETIME_MS: i64 = 24 * 60 * 60 * 1000;

pub(crate) fn output_root(system: &ActorSystem) -> PathBuf {
    system
        .infrastructure_dir()
        .with_file_name("runtime")
        .join("tool-outputs")
}

pub(crate) fn storage_root(
    system: &ActorSystem,
    actor: &str,
    expires_at_ms: Option<i64>,
) -> PathBuf {
    if expires_at_ms.is_some() {
        output_root(system).join(crate::stage::digest(actor.as_bytes()))
    } else {
        system.actor_data_dir(actor)
    }
}

pub(super) fn ensure_reference_available(id: &str) -> Result<()> {
    if id
        .strip_prefix("temporary-")
        .and_then(|value| value.split_once('-'))
        .and_then(|(expiry, _)| expiry.parse::<i64>().ok())
        .is_some_and(|expiry| expiry <= chrono::Utc::now().timestamp_millis())
    {
        return Err(StageError::Unavailable(
            "Temporary resource expired after 24 hours; do not replay the original operation",
        ));
    }
    Ok(())
}

pub(super) fn publish_output(
    system: &ActorSystem,
    actor: &str,
    call: &str,
    text: &str,
    now: i64,
) -> Result<Resource> {
    let expires_at_ms = now.saturating_add(OUTPUT_LIFETIME_MS);
    let content = BlobRef {
        hash: crate::stage::digest(text.as_bytes()),
        size: text.len() as u64,
        version: 1,
    };
    let identity = crate::stage::digest(
        &serde_json::to_vec(&(actor, call, &content, expires_at_ms))
            .map_err(|_| StageError::Invalid("Invalid temporary output"))?,
    );
    let resource = Resource {
        id: format!("temporary-{expires_at_ms}-{identity}"),
        name: format!("output-{}", &crate::stage::digest(call.as_bytes())[..12]),
        media_type: "text/plain".into(),
        content,
        expires_at_ms: Some(expires_at_ms),
    };
    let _installation = INSTALLATION.lock();
    let root = storage_root(system, actor, resource.expires_at_ms);
    let directory = GuardedDirectory::open(&root.join("synced"), true)?;
    directory.protect_private()?;
    use std::io::{Read, Write};
    match directory.create_private_file(OsStr::new(&resource.id)) {
        Ok(mut file) => {
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let mut file = directory.open_read(OsStr::new(&resource.id))?;
            if file.metadata()?.len() != text.len() as u64 {
                return Err(StageError::Conflict("Temporary output bytes changed"));
            }
            let mut buffer = [0u8; 65536];
            for expected in text.as_bytes().chunks(buffer.len()) {
                file.read_exact(&mut buffer[..expected.len()])?;
                if &buffer[..expected.len()] != expected {
                    return Err(StageError::Conflict("Temporary output bytes changed"));
                }
            }
        }
        Err(error) => return Err(error.into()),
    }
    system
        .store()
        .register_resource(StoredResource {
            expires_at_ms: resource.expires_at_ms,
            id: resource.id.clone(),
            actor_id: actor.into(),
            path: root.join("synced").join(&resource.id),
            name: resource.name.clone(),
            media_type: resource.media_type.clone(),
            size_bytes: resource.content.size,
            uri: resource.uri(actor),
        })
        .map_err(|_| StageError::Unavailable("Cannot register temporary output"))?;
    remember(system, actor, &resource)?;
    ContentStore::new(system.store()).register_file_source(actor, text.as_bytes())?;
    Ok(resource)
}

/// Only expired, framework-created temporary files are removed. The pinned
/// directory broker rejects symlinks and reparse ancestors during deletion.
pub(crate) fn cleanup_expired(system: &ActorSystem, now: i64) -> Result<usize> {
    let _installation = INSTALLATION.lock();
    let expired: Vec<StoredResource> = system.store().coordination_transaction(|tx| {
        let records = tx
            .prepare(
                "SELECT record FROM resources WHERE json_extract(record,'$.expires_at_ms')<=?1",
            )?
            .query_map([now], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        records
            .into_iter()
            .map(|record| {
                serde_json::from_str(&record)
                    .map_err(|_| StageError::Invalid("Invalid temporary resource record"))
            })
            .collect()
    })?;
    let mut removed = 0;
    for resource in expired {
        let result = (|| -> Result<()> {
            let root = storage_root(system, &resource.actor_id, resource.expires_at_ms);
            let directory = root.join("synced");
            if resource.path != directory.join(&resource.id) {
                return Err(StageError::Forbidden("Temporary resource path changed"));
            }
            match GuardedDirectory::open(&directory, false) {
                Ok(directory) => match directory.remove(OsStr::new(&resource.id)) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            system.store().stage_transaction(|tx| {
                initialize(tx)?;
                tx.execute("DELETE FROM resources WHERE actor_id=?1 AND id=?2 AND json_extract(record,'$.expires_at_ms')<=?3",
                    params![resource.actor_id, resource.id, now])?;
                tx.execute("DELETE FROM stage_resources WHERE actor=?1 AND id=?2", params![resource.actor_id, resource.id])?;
                tx.execute("DELETE FROM stage_resource_files WHERE actor=?1 AND id=?2", params![resource.actor_id, resource.id])?;
                if resource.actor_id == crate::stage::shared::ACTOR_ID
                    && tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='core_dirty')", [], |row| row.get::<_, bool>(0))?
                {
                    tx.execute("DELETE FROM core_dirty WHERE entity='resources' AND local_key=?1", [&resource.id])?;
                }
                Ok(())
            })?;
            Ok(())
        })();
        match result {
            Ok(()) => removed += 1,
            Err(error) => tracing::warn!(%error, "Expired temporary output cleanup deferred"),
        }
    }
    // A crash before ledger registration or a work reset can leave a file
    // without a row. The filename's creation-time deadline covers those too.
    match GuardedDirectory::open(&output_root(system), false) {
        Ok(root) => {
            for actor in root.names()? {
                let Some(actor_name) = actor.to_str() else {
                    continue;
                };
                if actor_name.len() != 64
                    || !actor_name.bytes().all(|byte| byte.is_ascii_hexdigit())
                {
                    continue;
                }
                let directory = match root
                    .child(&actor, false)
                    .and_then(|actor| actor.child(OsStr::new("synced"), false))
                {
                    Ok(directory) => directory,
                    Err(_) => continue,
                };
                for name in directory.names()? {
                    let Some((expiry, hash)) = name
                        .to_str()
                        .and_then(|name| name.strip_prefix("temporary-"))
                        .and_then(|name| name.split_once('-'))
                    else {
                        continue;
                    };
                    if hash.len() != 64
                        || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                        || !expiry.parse::<i64>().is_ok_and(|expiry| expiry <= now)
                    {
                        continue;
                    }
                    if directory.is_regular_file(&name)? {
                        directory.remove(&name)?;
                        removed += 1;
                    }
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn fixture() -> (tempfile::TempDir, ActorSystem) {
        let directory = crate::sandbox::test_support::scoped_directory();
        let system = ActorSystem::open(
            directory.path().join("state"),
            directory.path().join("workspace"),
            Default::default(),
        )
        .await
        .unwrap();
        system.seed_test_actor("root");
        system.seed_test_actor("other");
        (directory, system)
    }

    #[tokio::test]
    async fn storage_beyond_the_former_quota_accepts_durable_resources_and_complete_temporary_output()
     {
        let (_directory, system) = fixture().await;
        let path = system.actor_data_dir("root").join("large.bin");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(600 * 1024 * 1024).unwrap();
        system
            .store()
            .register_resource(StoredResource {
                expires_at_ms: None,
                actor_id: "root".into(),
                id: "large".into(),
                path,
                name: "large.bin".into(),
                media_type: "application/octet-stream".into(),
                size_bytes: 600 * 1024 * 1024,
                uri: "/v1/actors/root/resources/large".into(),
            })
            .unwrap();
        let durable = publish(&system, "root", "note", "text/plain", b"durable contents").unwrap();
        assert_eq!(
            bytes(&system, "root", &durable).unwrap(),
            b"durable contents"
        );
        let original = "完整输出🔵\n".repeat(2000) + "TAIL-MARKER";
        let bounded =
            super::super::bound_output(&system, "root", "large-call", &original, 256, None)
                .unwrap();
        assert!(bounded.len() <= 1024);
        let id: String = system.store().stage_transaction(|tx|Ok(tx.query_row(
            "SELECT id FROM resources WHERE actor_id='root' AND json_extract(record,'$.expires_at_ms') IS NOT NULL",[],|row|row.get(0))?)).unwrap();
        let resource = export(&system, "root", &id).unwrap();
        assert!(bounded.contains(&resource.uri("root")));
        let cached: i64 = system
            .store()
            .stage_transaction(|tx| {
                Ok(tx.query_row(
                    "SELECT length(bytes) FROM stage_blobs WHERE hash=?1",
                    [&resource.content.hash],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(
            cached, 0,
            "long outputs must not duplicate their contents into the persistent database"
        );
        assert_eq!(
            bytes(&system, "root", &resource).unwrap(),
            original.as_bytes()
        );
        let stored = system.store().resource("root", &resource.id).unwrap();
        assert!(
            stored
                .path
                .starts_with(system.infrastructure_dir().with_file_name("runtime"))
        );
        assert!(!stored.path.starts_with(system.actor_data_dir("root")));
        assert!(resolve_reference(&system, "other", &resource.id).is_err());
        assert!(system.store().resource("other", &resource.id).is_err());
        system.shutdown().await;
    }

    #[tokio::test]
    async fn temporary_outputs_survive_restart_and_expire_after_24_hours_without_removing_durable_data()
     {
        let (directory, system) = fixture().await;
        let created = chrono::Utc::now().timestamp_millis();
        let text = "preserve the complete output".repeat(500);
        let temporary = publish_output(&system, "root", "expiry", &text, created).unwrap();
        assert_eq!(temporary.expires_at_ms, Some(created + 86_400_000));
        install(&system, "root", &temporary).unwrap();
        assert_eq!(
            publish_output(&system, "root", "expiry", &text, created).unwrap(),
            temporary
        );
        let path = system.store().resource("root", &temporary.id).unwrap().path;
        let durable = publish(&system, "root", "retained", "text/plain", b"permanent").unwrap();
        system.shutdown().await;
        assert!(path.is_file());
        let reopened = ActorSystem::open(
            directory.path().join("state"),
            directory.path().join("workspace"),
            Default::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            bytes(&reopened, "root", &temporary).unwrap(),
            text.as_bytes()
        );
        assert_eq!(
            cleanup_expired(&reopened, created + 86_400_000 - 1).unwrap(),
            0
        );
        assert!(path.is_file());
        assert_eq!(cleanup_expired(&reopened, created + 86_400_000).unwrap(), 1);
        assert!(!path.exists());
        assert!(reopened.store().resource("root", &temporary.id).is_err());
        assert!(descriptor(&reopened, "root", &temporary.id).is_err());
        assert_eq!(bytes(&reopened, "root", &durable).unwrap(), b"permanent");
        reopened.shutdown().await;
    }

    #[tokio::test]
    async fn native_temporary_resource_install_preserves_expiration_and_account_ownership() {
        let (_a, source) = fixture().await;
        let (_b, target) = fixture().await;
        let created = chrono::Utc::now().timestamp_millis();
        let resource =
            publish_output(&source, "root", "native", "native complete text", created).unwrap();
        let original = bytes(&source, "root", &resource).unwrap();
        source
            .store()
            .stage_transaction(|tx| crate::stage::blob::materialize(tx, "root", &resource.content))
            .unwrap();
        assert_eq!(
            ContentStore::new(source.store())
                .verified_bytes(&resource.content)
                .unwrap(),
            original
        );
        ContentStore::new(target.store())
            .store_source("root", &original)
            .unwrap();
        install(&target, "root", &resource).unwrap();
        let received = export(&target, "root", &resource.id).unwrap();
        assert_eq!(received, resource);
        assert_eq!(bytes(&target, "root", &received).unwrap(), original);
        assert!(export(&target, "other", &resource.id).is_err());
        assert_eq!(cleanup_expired(&target, created + 86_400_000).unwrap(), 1);
        let mut expired = resource.clone();
        expired.expires_at_ms = Some(created - 1);
        assert!(expired.ensure_available().is_err());
        install(&target, "root", &expired).unwrap();
        assert!(
            target.store().resource("root", &expired.id).is_err(),
            "expired snapshot metadata cannot restore bytes"
        );
        source.shutdown().await;
        target.shutdown().await;
    }

    #[tokio::test]
    async fn expiration_before_checkpoint_capture_does_not_leave_a_missing_dirty_record() {
        let (_directory, system) = fixture().await;
        system.ensure_core_actor().unwrap();
        crate::stage::shared_store::initialize(system.store(), "expiry-account").unwrap();
        let created = chrono::Utc::now().timestamp_millis();
        let resource = publish_output(
            &system,
            "core",
            "pending-checkpoint",
            "complete output",
            created,
        )
        .unwrap();
        assert_eq!(cleanup_expired(&system, created + 86_400_000).unwrap(), 1);
        let pending = crate::stage::shared_store::capture(
            system.store(),
            "expiry-account",
            crate::stage::StageId::new(),
            1,
        )
        .unwrap();
        assert!(
            pending
                .is_none_or(|batch| batch.changes.iter().all(|change| change.key != resource.id))
        );
        system.shutdown().await;
    }

    #[tokio::test]
    async fn orphaned_temporary_files_are_cleaned_after_their_creation_time_deadline() {
        let (_directory, system) = fixture().await;
        let created = chrono::Utc::now().timestamp_millis();
        let resource =
            publish_output(&system, "root", "orphan", "complete output", created).unwrap();
        let path = system.store().resource("root", &resource.id).unwrap().path;
        system
            .store()
            .stage_transaction(|tx| {
                tx.execute(
                    "DELETE FROM resources WHERE actor_id='root' AND id=?1",
                    [&resource.id],
                )?;
                tx.execute(
                    "DELETE FROM stage_resources WHERE actor='root' AND id=?1",
                    [&resource.id],
                )?;
                tx.execute(
                    "DELETE FROM stage_resource_files WHERE actor='root' AND id=?1",
                    [&resource.id],
                )?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            cleanup_expired(&system, created + 86_400_000 - 1).unwrap(),
            0
        );
        assert!(path.exists());
        assert_eq!(cleanup_expired(&system, created + 86_400_000).unwrap(), 1);
        assert!(!path.exists());
        system.shutdown().await;
    }

    #[tokio::test]
    async fn expiration_cleanup_does_not_follow_a_changed_resource_path() {
        let (directory, system) = fixture().await;
        let created = chrono::Utc::now().timestamp_millis();
        let resource = publish_output(&system, "root", "tamper", "original", created).unwrap();
        let unrelated = directory.path().join("owner-file.txt");
        std::fs::write(&unrelated, "owner data").unwrap();
        system.store().stage_transaction(|tx|{
            tx.execute("UPDATE resources SET record=json_set(record,'$.path',?3) WHERE actor_id=?1 AND id=?2",params!["root",resource.id,unrelated.to_string_lossy()])?;
            Ok(())
        }).unwrap();
        cleanup_expired(&system, created + 86_400_000).unwrap();
        assert_eq!(std::fs::read_to_string(unrelated).unwrap(), "owner data");
        system.shutdown().await;
    }
}
