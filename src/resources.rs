//! Pathless resource descriptors. Received bytes only enter the Actor's delivery ledger.
use crate::actors::{ActorSystem, StoredResource};
use crate::stage::{Result, StageError, blob::ContentStore, protocol::BlobRef};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

// Input forwarding and background synchronization can install the same blob
// concurrently. Keep immutable ledger checks and local publication together.
static INSTALLATION: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resource {
    pub id: String,
    pub name: String,
    pub media_type: String,
    pub content: BlobRef,
}
impl Resource {
    /// An internal locator, interpreted against an Actor ledger on every read.
    /// This same URI is usable in tool inputs, Markdown and authorized HTTP views.
    pub fn uri(&self, actor: &str) -> String {
        format!("/v1/actors/{actor}/resources/{}", self.id)
    }

    pub fn validate(&self) -> Result<()> {
        self.content.validate()?;
        if self.id.is_empty()
            || matches!(self.id.as_str(), "." | "..")
            || self.id.len() > 255
            || self
                .id
                .bytes()
                .any(|b| !(b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')))
            || self.name.is_empty()
            || self.name.len() > 512
            || self.name.chars().any(char::is_control)
            || self.media_type.len() > 128
            || !self.media_type.contains('/')
            || self
                .media_type
                .bytes()
                .any(|b| !(b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'+' | b'.')))
        {
            return Err(StageError::Invalid("Invalid resource descriptor"));
        }
        Ok(())
    }
}

/// Resolve an opaque ID or an internal resource URI without granting new access.
/// Filesystem paths and external URLs are separate source types, never resource IDs.
pub fn resolve_reference(system: &ActorSystem, actor: &str, reference: &str) -> Result<Resource> {
    let id = if let Some(path) = reference.strip_prefix("/v1/actors/") {
        let (owner, id) = path
            .split_once("/resources/")
            .ok_or(StageError::Invalid("Invalid internal resource reference"))?;
        if owner != actor {
            return Err(StageError::Forbidden("Resource belongs to another Actor"));
        }
        id
    } else {
        reference
    };
    if id.is_empty()
        || matches!(id, "." | "..")
        || id.len() > 255
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(StageError::Invalid(
            "Use an existing resource ID or /v1/actors/<actor>/resources/<id> reference",
        ));
    }
    export(system, actor, id)
}

/// Resource locators accompanying model images from any App, with no storage paths.
pub(crate) fn bind_image_resources(
    system: &ActorSystem,
    actor: &str,
    message: &str,
    images: &[crate::model_protocol::AgentContentPart],
) -> Result<Vec<serde_json::Value>> {
    images
        .iter()
        .filter_map(|part| match part {
            crate::model_protocol::AgentContentPart::ResourceImage { resource, .. } => {
                Some((|| {
                    let record = system
                        .store()
                        .resource(actor, &resource.id)
                        .map_err(|_| StageError::Forbidden("Image is outside its Actor ledger"))?;
                    system
                        .store()
                        .register_resource_for_message(record, Some(message))
                        .map_err(|_| {
                            StageError::Unavailable("Cannot associate image observation")
                        })?;
                    Ok(serde_json::json!({
                        "id":resource.id,"uri":resource.uri(actor),
                        "name":resource.name,"media_type":resource.media_type
                    }))
                })())
            }
            _ => None,
        })
        .collect()
}

/// Admit bytes under one Actor, then use the same immutable descriptor locally or remotely.
pub fn publish(
    system: &ActorSystem,
    actor: &str,
    name: &str,
    media_type: &str,
    bytes: &[u8],
) -> Result<Resource> {
    let content = ContentStore::new(system.store())
        .store_source(actor, bytes)?
        .content;
    let identity = serde_json::to_vec(&(name, media_type, &content))
        .map_err(|_| StageError::Invalid("Invalid resource identity"))?;
    let resource = Resource {
        id: format!("r-{}", crate::stage::digest(&identity)),
        name: name.into(),
        media_type: media_type.into(),
        content,
    };
    install(system, actor, &resource)?;
    Ok(resource)
}

pub fn bytes(system: &ActorSystem, actor: &str, resource: &Resource) -> Result<Vec<u8>> {
    resource.validate()?;
    if export(system, actor, &resource.id)? != *resource {
        return Err(StageError::Forbidden(
            "Resource does not belong to this Actor",
        ));
    }
    let record = system
        .store()
        .resource(actor, &resource.id)
        .map_err(|_| StageError::Forbidden("Resource is outside its Actor ledger"))?;
    let base = std::fs::canonicalize(system.actor_data_dir(actor))?;
    let mut policy = crate::sandbox::RuntimeSandboxPolicy::disabled();
    policy.filesystem.full_disk_read = false;
    policy.filesystem.readable_roots = vec![base.clone()];
    policy.filesystem.bound_roots = vec![base];
    let bytes = crate::file_io::read_bytes(&record.path, resource.content.size, &policy)
        .map_err(|_| StageError::Forbidden("Resource storage boundary changed"))?;
    if bytes.len() as u64 != resource.content.size
        || crate::stage::digest(&bytes) != resource.content.hash
    {
        return Err(StageError::Conflict("Resource bytes changed"));
    }
    Ok(bytes)
}

/// Trusted local App results become pathless before persistence or native transport.
pub fn capture_images(
    system: &ActorSystem,
    actor: &str,
    parts: Vec<crate::model_protocol::AgentContentPart>,
) -> Result<Vec<crate::model_protocol::AgentContentPart>> {
    use crate::model_protocol::AgentContentPart;
    parts
        .into_iter()
        .map(|part| match part {
            AgentContentPart::Image {
                path,
                media_type,
                description,
            } => {
                let mut policy = crate::sandbox::RuntimeSandboxPolicy::disabled();
                policy.filesystem.full_disk_read = false;
                let roots = vec![
                    system.actor_state_dir(actor).join("images"),
                    system.actor_data_dir(actor),
                ];
                policy.filesystem.readable_roots = roots.clone();
                policy.filesystem.bound_roots = roots;
                let bytes = crate::file_io::read_authorized_bytes(
                    std::path::Path::new(&path),
                    crate::daemon::attachments::MAX_IMAGE_ATTACHMENT_BYTES as u64,
                    &policy,
                )
                .map_err(|_| {
                    StageError::Forbidden("App image is outside controlled Actor storage")
                })?;
                let resource = publish(system, actor, "image", &media_type, &bytes)?;
                Ok(AgentContentPart::ResourceImage {
                    resource,
                    description,
                })
            }
            AgentContentPart::ResourceImage {
                resource,
                description,
            } => {
                bytes(system, actor, &resource)?;
                Ok(AgentContentPart::ResourceImage {
                    resource,
                    description,
                })
            }
            AgentContentPart::Text { .. } | AgentContentPart::ImageData { .. } => Err(
                StageError::Invalid("App images must be local snapshots or resource references"),
            ),
        })
        .collect()
}

/// Resolve only this Actor's ledger entries; hashes never confer authority.
pub fn model_request(
    system: &ActorSystem,
    actor: &str,
    mut request: crate::model_protocol::AgentTurnRequest,
) -> Result<crate::model_protocol::AgentTurnRequest> {
    use crate::model_protocol::{AgentContent, AgentContentPart, AgentMessage};
    for message in &mut request.messages {
        if let AgentMessage::User { content } = message {
            let mut parts = Vec::new();
            for part in content.parts() {
                match part {
                    AgentContentPart::ResourceImage {
                        resource,
                        description,
                    } => {
                        if !matches!(
                            resource.media_type.as_str(),
                            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
                        ) {
                            return Err(StageError::Invalid("Unsupported image resource"));
                        }
                        use base64::Engine;
                        let bytes = bytes(system, actor, resource)?;
                        parts.push(AgentContentPart::ImageData {
                            data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
                            media_type: resource.media_type.clone(),
                            description: description.clone(),
                        });
                    }
                    AgentContentPart::Image {
                        path,
                        media_type,
                        description,
                    } => {
                        // Existing histories may contain local paths. Resolve them through
                        // this Actor's resource ledger when another device now owns the bytes.
                        let name = std::path::Path::new(path)
                            .file_name()
                            .and_then(|value| value.to_str())
                            .unwrap_or("");
                        let resource = export(system, actor, name).or_else(|_| {
                            let stem = std::path::Path::new(name).file_stem().and_then(|value| value.to_str()).unwrap_or("");
                            let hash = stem.strip_prefix("view-").unwrap_or(stem);
                            if hash.len() != 64 || hex::decode(hash).is_err() { return Err(StageError::Unavailable("Image is not in this Actor's resource ledger")); }
                            system.store().stage_transaction(|tx| {
                                initialize(tx)?;
                                let data: Option<String> = tx.query_row("SELECT record FROM stage_resources WHERE actor=?1 AND json_extract(record,'$.content.hash')=?2 AND json_extract(record,'$.media_type')=?3",params![actor,hash,media_type],|row|row.get(0)).optional()?;
                                serde_json::from_str(&data.ok_or(StageError::Unavailable("Image resource is unavailable"))?).map_err(|_| StageError::Invalid("Invalid image resource"))
                            })
                        }).or_else(|_| {
                            match capture_images(system, actor, vec![part.clone()])?.pop() {
                                Some(AgentContentPart::ResourceImage { resource, .. }) => Ok(resource),
                                _ => Err(StageError::Invalid("Invalid image resource")),
                            }
                        })?;
                        if resource.media_type != *media_type {
                            return Err(StageError::Conflict("Image resource type changed"));
                        }
                        use base64::Engine;
                        let bytes = bytes(system, actor, &resource)?;
                        parts.push(AgentContentPart::ImageData {
                            data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
                            media_type: media_type.clone(),
                            description: description.clone(),
                        });
                    }
                    AgentContentPart::Text { .. } => parts.push(part.clone()),
                    AgentContentPart::ImageData { .. } => {
                        return Err(StageError::Invalid(
                            "Durable model input must use resource references",
                        ));
                    }
                }
            }
            *content = AgentContent::multimodal(content.as_text(), parts);
        }
    }
    Ok(request)
}

pub fn bound_output(
    system: &ActorSystem,
    actor: &str,
    call_id: &str,
    text: &str,
    max_tokens: usize,
    continuation: Option<String>,
) -> Result<String> {
    use crate::tool_output_spill::{OverflowTarget, bound_tool_model_content};
    let limit = max_tokens
        .max(1)
        .saturating_mul(crate::context_budget::APPROX_BYTES_PER_TOKEN);
    let continuation = if text.len() <= limit {
        None
    } else if let Some(instruction) = continuation {
        Some(instruction)
    } else {
        let resource = publish(
            system,
            actor,
            &format!("output-{}", &crate::stage::digest(call_id.as_bytes())[..12]),
            "text/plain",
            text.as_bytes(),
        )?;
        Some(format!(
            "Full output resource: {}. Resource ID: {}.",
            resource.uri(actor),
            resource.id
        ))
    };
    Ok(bound_tool_model_content(
        text,
        max_tokens,
        continuation.map(|instruction| OverflowTarget::Continue { instruction }),
    ))
}
fn initialize(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch("CREATE TABLE IF NOT EXISTS stage_resources(actor TEXT NOT NULL,id TEXT NOT NULL,record TEXT NOT NULL,PRIMARY KEY(actor,id));CREATE TABLE IF NOT EXISTS stage_resource_files(actor TEXT NOT NULL,id TEXT NOT NULL,root TEXT NOT NULL,path TEXT NOT NULL,PRIMARY KEY(actor,id));")?;
    Ok(())
}
pub fn descriptor(system: &ActorSystem, actor: &str, id: &str) -> Result<Resource> {
    system.store().stage_transaction(|tx| {
        initialize(tx)?;
        let data: Option<String> = tx
            .query_row(
                "SELECT record FROM stage_resources WHERE actor=?1 AND id=?2",
                params![actor, id],
                |r| r.get(0),
            )
            .optional()?;
        serde_json::from_str(&data.ok_or(StageError::Unavailable("Resource is not synchronized"))?)
            .map_err(|_| StageError::Invalid("Invalid resource ledger"))
    })
}
fn remember(system: &ActorSystem, actor: &str, resource: &Resource) -> Result<()> {
    resource.validate()?;
    system.store().stage_transaction(|tx|{
        initialize(tx)?;let data=serde_json::to_string(resource).map_err(|_|StageError::Invalid("Invalid resource descriptor"))?;
        let previous:Option<String>=tx.query_row("SELECT record FROM stage_resources WHERE actor=?1 AND id=?2",params![actor,resource.id],|r|r.get(0)).optional()?;
        if previous.as_ref().is_some_and(|old|old!=&data){return Err(StageError::Conflict("Immutable resource changed"));}
        tx.execute("INSERT OR IGNORE INTO stage_resources VALUES(?1,?2,?3)",params![actor,resource.id,data])?;
        let native:String=tx.query_row("SELECT record FROM resources WHERE actor_id=?1 AND id=?2",params![actor,resource.id],|r|r.get(0))?;
        let path=serde_json::from_str::<serde_json::Value>(&native).map_err(|_|StageError::Invalid("Invalid resource storage"))?["path"].as_str().ok_or(StageError::Invalid("Invalid resource path"))?.to_owned();
        tx.execute("INSERT INTO stage_resource_files VALUES(?1,?2,?3,?4) ON CONFLICT(actor,id) DO UPDATE SET root=excluded.root,path=excluded.path",params![actor,resource.id,system.actor_data_dir(actor).to_string_lossy(),path])?;
        let core:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='core_dirty')",[],|r|r.get(0))?;
        if actor==crate::stage::shared::ACTOR_ID && core && previous.is_none(){tx.execute("INSERT INTO core_dirty VALUES('resources',?1,1) ON CONFLICT(entity,local_key) DO UPDATE SET revision=revision+1",[&resource.id])?;}
        Ok(())
    })
}
pub fn export(system: &ActorSystem, actor: &str, id: &str) -> Result<Resource> {
    if let Ok(resource) = descriptor(system, actor, id) {
        return Ok(resource);
    }
    let record = system
        .store()
        .resource(actor, id)
        .map_err(|_| StageError::Forbidden("Resource is outside its Actor ledger"))?;
    let mut policy = crate::sandbox::RuntimeSandboxPolicy::disabled();
    let base = std::fs::canonicalize(system.actor_data_dir(actor))?;
    policy.filesystem.full_disk_read = false;
    policy.filesystem.full_disk_write = false;
    policy.filesystem.readable_roots = vec![base.clone()];
    policy.filesystem.bound_roots = vec![base];
    let bytes =
        crate::file_io::read_bytes(&record.path, record.size_bytes, &policy).map_err(|_| {
            StageError::Forbidden("Resource path is outside controlled delivery storage")
        })?;
    if bytes.len() as u64 != record.size_bytes {
        return Err(StageError::Conflict("Resource size changed"));
    }
    let resource = Resource {
        id: record.id,
        name: record.name,
        media_type: record.media_type,
        content: ContentStore::new(system.store())
            .store_source(actor, &bytes)?
            .content,
    };
    remember(system, actor, &resource)?;
    Ok(resource)
}
pub fn export_all(system: &ActorSystem, actor: &str) -> Result<Vec<Resource>> {
    let ids = system.store().stage_transaction(|tx| {
        Ok(tx
            .prepare("SELECT id FROM resources WHERE actor_id=?1 ORDER BY id")?
            .query_map([actor], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    })?;
    ids.into_iter()
        .map(|id| export(system, actor, &id))
        .collect()
}
pub fn install(system: &ActorSystem, actor: &str, resource: &Resource) -> Result<()> {
    let _installation = INSTALLATION.lock();
    resource.validate()?;
    let bytes = ContentStore::new(system.store()).verified_bytes(&resource.content)?;
    ContentStore::new(system.store()).store_source(actor, &bytes)?;
    system.store().stage_transaction(|tx| {
        let used: i64 = tx.query_row(
            "SELECT COALESCE(SUM(json_extract(record,'$.size_bytes')),0) FROM resources",
            [],
            |r| r.get(0),
        )?;
        let existing: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM resources WHERE actor_id=?1 AND id=?2)",
            params![actor, resource.id],
            |r| r.get(0),
        )?;
        if !existing && (used as u64).saturating_add(resource.content.size) > 512 * 1024 * 1024 {
            return Err(StageError::Capacity(
                "Account resource storage budget reached",
            ));
        }
        Ok(())
    })?;
    if system.store().resource(actor, &resource.id).is_ok()
        && let Ok(previous) = export(system, actor, &resource.id)
        && previous != *resource
    {
        return Err(StageError::Conflict(
            "Existing native resource is immutable",
        ));
    }
    if let Ok(previous) = descriptor(system, actor, &resource.id)
        && previous != *resource
    {
        return Err(StageError::Conflict("Resource identifier changed content"));
    }
    let base = system.actor_data_dir(actor);
    let dir = base.join("synced");
    crate::file_io::ensure_directory(&crate::sandbox::RuntimeSandboxPolicy::disabled(), &dir)
        .map_err(|_| StageError::Unavailable("Cannot prepare resource storage"))?;
    crate::actors::permissions::protect_private_path(&dir)
        .map_err(|_| StageError::Forbidden("Cannot establish private resource storage"))?;
    let path = dir.join(&resource.id);
    let base = std::fs::canonicalize(&base)?;
    let mut policy = crate::sandbox::RuntimeSandboxPolicy::disabled();
    policy.filesystem.full_disk_read = false;
    policy.filesystem.full_disk_write = false;
    policy.filesystem.readable_roots = vec![base.clone()];
    policy.filesystem.writable_roots = vec![crate::sandbox::WritableRoot {
        root: base.clone(),
        read_only_subpaths: vec![],
    }];
    policy.filesystem.bound_roots = vec![base.clone()];
    if let Ok(stored) = system.store().resource(actor, &resource.id)
        && crate::file_io::read_bytes(&stored.path, resource.content.size, &policy)
            .is_ok_and(|existing| existing == bytes)
    {
        return remember(system, actor, resource);
    }
    if path.exists() {
        if crate::file_io::read_bytes(&path, resource.content.size, &policy)
            .map_err(|_| StageError::Forbidden("Resource storage changed"))?
            != bytes
        {
            return Err(StageError::Conflict("Resource bytes changed"));
        }
    } else {
        crate::file_io::write_bytes(&base, &policy, &path.to_string_lossy(), &bytes, None)
            .map_err(|_| StageError::Unavailable("Cannot persist received resource"))?;
    }
    if system
        .store()
        .register_resource(StoredResource {
            id: resource.id.clone(),
            actor_id: actor.into(),
            path: path.clone(),
            name: resource.name.clone(),
            media_type: resource.media_type.clone(),
            size_bytes: resource.content.size,
            uri: format!("/v1/actors/{actor}/resources/{}", resource.id),
        })
        .is_err()
    {
        let _ = std::fs::remove_file(&path);
        return Err(StageError::Unavailable("Cannot register received resource"));
    }
    remember(system, actor, resource)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn portable_images_resolve_after_restart_and_reject_tampering_or_other_actors() {
        use crate::model_protocol::{
            AgentContent, AgentContentPart, AgentMessage, AgentTurnRequest,
        };
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let source = ActorSystem::open(
            a.path().join("state"),
            a.path().join("workspace"),
            Default::default(),
        )
        .await
        .unwrap();
        if source.actor("root").is_err() {
            source.seed_test_actor("root");
        }
        let target = ActorSystem::open(
            b.path().join("state"),
            b.path().join("workspace"),
            Default::default(),
        )
        .await
        .unwrap();
        if target.actor("root").is_err() {
            target.seed_test_actor("root");
        }
        let image = b"\x89PNG\r\n\x1a\nportable image bytes";
        let local = source.actor_state_dir("root").join("images/image.png");
        std::fs::create_dir_all(local.parent().unwrap()).unwrap();
        std::fs::write(&local, image).unwrap();
        let parts = capture_images(
            &source,
            "root",
            vec![AgentContentPart::Image {
                path: local.to_string_lossy().into(),
                media_type: "image/png".into(),
                description: None,
            }],
        )
        .unwrap();
        let AgentContentPart::ResourceImage { resource, .. } = &parts[0] else {
            panic!("expected portable resource");
        };
        let wire = serde_json::to_string(&parts).unwrap();
        assert!(!wire.contains("path"));
        assert!(!wire.contains(&a.path().to_string_lossy().to_string()));
        ContentStore::new(target.store())
            .store_source("root", image)
            .unwrap();
        install(&target, "root", resource).unwrap();
        let request = AgentTurnRequest {
            messages: vec![AgentMessage::user_content(AgentContent::multimodal(
                "inspect",
                parts.clone(),
            ))],
            tools: vec![],
        };
        let resolve = |system: &ActorSystem| {
            let actual = model_request(system, "root", request.clone()).unwrap();
            let AgentMessage::User { content } = &actual.messages[0] else {
                panic!("expected image input");
            };
            let AgentContentPart::ImageData { data_base64, .. } = &content.parts()[0] else {
                panic!("expected materialized image");
            };
            use base64::Engine;
            assert_eq!(
                base64::engine::general_purpose::STANDARD
                    .decode(data_base64)
                    .unwrap(),
                image
            );
        };
        resolve(&target);
        target.shutdown().await;
        drop(target);
        let target = ActorSystem::open(
            b.path().join("state"),
            b.path().join("workspace"),
            Default::default(),
        )
        .await
        .unwrap();
        if target.actor("root").is_err() {
            target.seed_test_actor("root");
        }
        resolve(&target);
        assert!(model_request(&target, "another-actor", request.clone()).is_err());
        let mut changed = resource.clone();
        changed.content.hash = "0".repeat(64);
        assert!(bytes(&target, "root", &changed).is_err());
        let path = target.store().resource("root", &resource.id).unwrap().path;
        std::fs::write(path, b"replaced image").unwrap();
        assert!(model_request(&target, "root", request).is_err());
        source.shutdown().await;
        target.shutdown().await;
    }
    #[tokio::test]
    async fn resource_replication_never_imports_source_paths_or_crosses_actor_boundaries() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let source = ActorSystem::open(
            a.path().join("state"),
            a.path().join("workspace"),
            Default::default(),
        )
        .await
        .unwrap();
        if source.actor("root").is_err() {
            source.seed_test_actor("root");
        }
        let target = ActorSystem::open(
            b.path().join("state"),
            b.path().join("workspace"),
            Default::default(),
        )
        .await
        .unwrap();
        if target.actor("root").is_err() {
            target.seed_test_actor("root");
        }
        let dir = source.actor_data_dir("root");
        let file = dir.join("image.png");
        std::fs::write(&file, b"immutable attachment").unwrap();
        source
            .store()
            .register_resource(StoredResource {
                id: "upload-1".into(),
                actor_id: "root".into(),
                path: file.clone(),
                name: "image.png".into(),
                media_type: "image/png".into(),
                size_bytes: 20,
                uri: "/v1/actors/root/resources/upload-1".into(),
            })
            .unwrap();
        let record = export(&source, "root", "upload-1").unwrap();
        assert!(!serde_json::to_string(&record).unwrap().contains("path"));
        let bytes = ContentStore::new(source.store())
            .verified_bytes(&record.content)
            .unwrap();
        ContentStore::new(target.store())
            .store_source("root", &bytes)
            .unwrap();
        let start = std::sync::Barrier::new(4);
        std::thread::scope(|scope| {
            let installers: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        start.wait();
                        install(&target, "root", &record)
                    })
                })
                .collect();
            for installer in installers {
                installer.join().unwrap().unwrap();
            }
        });
        let local = target.store().resource("root", "upload-1").unwrap();
        assert!(local.path.starts_with(target.actor_data_dir("root")));
        assert_ne!(local.path, file);
        assert_eq!(std::fs::read(&local.path).unwrap(), bytes);
        assert!(export(&source, "different-actor", "upload-1").is_err());
        let mut wrong = record.clone();
        wrong.name = "changed".into();
        assert!(install(&target, "root", &wrong).is_err());
        source.shutdown().await;
        target.shutdown().await;
    }
}
