//! A private snapshot of local Codex rollout locations; HTTP only receives opaque identities.
use super::{Transcript, parse_codex};
use crate::{actors::ActorError, sandbox::RuntimeSandboxPolicy};
use miette::{IntoDiagnostic, Result, miette};
use parking_lot::Mutex;
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    io::Read,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

#[derive(Clone, Serialize)]
pub(crate) struct Conversation {
    pub id: String,
    pub title: String,
    pub updated_at_ms: u128,
    pub archived: bool,
    #[serde(skip)]
    path: PathBuf,
    #[serde(skip)]
    root: PathBuf,
    #[serde(skip)]
    session_id: String,
    #[serde(skip)]
    secondary_session_id: Option<String>,
    #[serde(skip)]
    modified_ns: u128,
    #[serde(skip)]
    size: u64,
}
pub(crate) struct CodexSources {
    home: Option<PathBuf>,
    snapshot: Mutex<Option<Vec<Conversation>>>,
}
fn policy(root: &Path) -> RuntimeSandboxPolicy {
    let mut policy = RuntimeSandboxPolicy::disabled();
    policy.filesystem.full_disk_read = false;
    policy.filesystem.full_disk_write = false;
    policy.filesystem.readable_roots = vec![root.into()];
    policy.filesystem.bound_roots = vec![root.into()];
    policy
}
fn stamp(path: &Path) -> Result<(u64, u128)> {
    let metadata = std::fs::metadata(path).into_diagnostic()?;
    Ok((
        metadata.len(),
        metadata
            .modified()
            .into_diagnostic()?
            .duration_since(UNIX_EPOCH)
            .into_diagnostic()?
            .as_nanos(),
    ))
}

/// Leading bytes inspected for a fallback title when the index has no name.
const HEAD_INSPECTION_BYTES: u64 = 64 * 1024;
/// Bounded compressed prefix read before decompressing a rollout header.
const HEAD_COMPRESSED_BYTES: u64 = 1024 * 1024;
/// Upper bound for a decompressed rollout; larger archives are refused.
const MAX_DECOMPRESSED_BYTES: u64 = 1 << 30;

struct SessionMetaHead {
    session_id: String,
    thread_source: Option<String>,
}

/// Resolve `rollout-…-<uuid>.jsonl[.zst]` and `rollout-…-<parent>_<child>`
/// names into the parent identity used by Codex, plus the continued child.
fn rollout_identity(name: &str) -> Option<(String, Option<String>)> {
    let stem = name
        .strip_suffix(".jsonl.zst")
        .or_else(|| name.strip_suffix(".jsonl"))?;
    let (parent, child) = match stem.split_once('_') {
        Some((parent, child)) => (parent, Some(child)),
        None => (stem, None),
    };
    match (uuid_tail(parent), child.and_then(uuid_tail)) {
        (Some(parent), child) => Some((parent, child)),
        (None, _) => Some((uuid_tail(stem)?, None)),
    }
}

fn uuid_tail(part: &str) -> Option<String> {
    let tail = part.get(part.len().saturating_sub(36)..)?;
    uuid::Uuid::parse_str(tail).ok().map(|_| tail.to_owned())
}

/// `rollout-2026-10-23T21-08-08-<uuid>` names yield a `MM-DD HH:mm` label.
fn filename_timestamp(name: &str) -> Option<String> {
    let stamp = name.strip_prefix("rollout-")?.get(..19)?;
    let bytes = stamp.as_bytes();
    if bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b'-'
        || bytes[16] != b'-'
    {
        return None;
    }
    if !bytes
        .iter()
        .enumerate()
        .all(|(index, byte)| matches!(index, 4 | 7 | 10 | 13 | 16) || byte.is_ascii_digit())
    {
        return None;
    }
    Some(format!(
        "{}-{} {}:{}",
        &stamp[5..7],
        &stamp[8..10],
        &stamp[11..13],
        &stamp[14..16]
    ))
}

fn read_session_meta_head(path: &Path, root: &Path, compressed: bool) -> Option<SessionMetaHead> {
    let scope = policy(root);
    let head = if compressed {
        let prefix = crate::file_io::read_prefix(path, HEAD_COMPRESSED_BYTES, &scope).ok()?;
        decompress_prefix(&prefix, HEAD_INSPECTION_BYTES)?
    } else {
        crate::file_io::read_prefix(path, HEAD_INSPECTION_BYTES, &scope).ok()?
    };
    let first = head.split(|byte| *byte == b'\n').next()?;
    let first = first.strip_prefix(b"\xef\xbb\xbf").unwrap_or(first);
    let first = first.strip_suffix(b"\r").unwrap_or(first);
    let record = serde_json::from_slice::<serde_json::Value>(first).ok()?;
    if record["type"].as_str() != Some("session_meta") {
        return None;
    }
    let payload = &record["payload"];
    let session_id = payload["session_id"]
        .as_str()
        .or_else(|| payload["id"].as_str())
        .filter(|id| !id.is_empty())?;
    Some(SessionMetaHead {
        session_id: session_id.to_owned(),
        thread_source: payload["thread_source"].as_str().map(str::to_owned),
    })
}

/// Title used when neither the index nor the parent exposes a name.
fn fallback_title(
    path: &Path,
    root: &Path,
    file_name: &str,
    session_id: &str,
    names: &HashMap<String, String>,
) -> String {
    let compressed = file_name.ends_with(".jsonl.zst");
    if let Some(meta) = read_session_meta_head(path, root, compressed)
        && meta.thread_source.as_deref() == Some("subagent")
        && meta.session_id != session_id
        && let Some(parent) = names.get(&meta.session_id)
    {
        return format!("{parent} · 子代理");
    }
    match filename_timestamp(file_name) {
        Some(timestamp) => format!("未命名会话 · {timestamp}"),
        None => "Codex".into(),
    }
}

fn decompress_prefix(compressed: &[u8], limit: u64) -> Option<Vec<u8>> {
    let mut decoder = zstd::stream::read::Decoder::new(compressed).ok()?;
    let mut decoded = Vec::new();
    decoder
        .by_ref()
        .take(limit)
        .read_to_end(&mut decoded)
        .ok()?;
    Some(decoded)
}

fn decompress_rollout(compressed: &[u8]) -> Option<Vec<u8>> {
    let mut decoder = zstd::stream::read::Decoder::new(compressed).ok()?;
    let mut decoded = Vec::new();
    decoder
        .by_ref()
        .take(MAX_DECOMPRESSED_BYTES.saturating_add(1))
        .read_to_end(&mut decoded)
        .ok()?;
    (decoded.len() as u64 <= MAX_DECOMPRESSED_BYTES).then_some(decoded)
}

impl Default for CodexSources {
    fn default() -> Self {
        Self::new(
            std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::home_dir().map(|home| home.join(".codex"))),
        )
    }
}
impl CodexSources {
    pub fn new(home: Option<PathBuf>) -> Self {
        Self {
            home,
            snapshot: Mutex::new(None),
        }
    }
    pub fn supported(&self) -> bool {
        !cfg!(target_os = "android") && self.home.is_some()
    }
    fn scan(&self) -> Result<Vec<Conversation>> {
        let Some(home) = &self.home else {
            return Ok(vec![]);
        };
        if !home.exists() {
            return Ok(vec![]);
        }
        let home = std::fs::canonicalize(home).into_diagnostic()?;
        let mut names = HashMap::new();
        let index = home.join("session_index.jsonl");
        // The index only supplies names. Rollout locations and identities are
        // independently enumerated under the two conversation directories.
        if index.is_file() {
            let index_policy = policy(&index);
            if let Ok(bytes) = crate::file_io::read_bytes(&index, u64::MAX, &index_policy) {
                for line in bytes.split(|byte| *byte == b'\n') {
                    if let Ok(record) = serde_json::from_slice::<serde_json::Value>(line)
                        && let (Some(id), Some(name)) =
                            (record["id"].as_str(), record["thread_name"].as_str())
                    {
                        names.insert(id.to_owned(), name.to_owned());
                    }
                }
            }
        }
        let mut conversations = Vec::new();
        for (folder, archived) in [("sessions", false), ("archived_sessions", true)] {
            let root = home.join(folder);
            if !root.is_dir() {
                continue;
            }
            let root = std::fs::canonicalize(root).into_diagnostic()?;
            if !root.starts_with(&home) {
                continue;
            }
            let scope = policy(&root);
            let mut pending = vec![root.clone()];
            let mut visited = HashSet::new();
            while let Some(dir) = pending.pop() {
                let actual = std::fs::canonicalize(&dir).into_diagnostic()?;
                if !actual.starts_with(&root) || !visited.insert(actual) {
                    continue;
                }
                let mut cursor = None;
                loop {
                    let page = crate::file_io::list_directory(
                        &root,
                        &scope,
                        &crate::file_io::ListDirectoryArgs {
                            path: dir.to_string_lossy().into(),
                            cursor,
                            limit: Some(200),
                        },
                    )?;
                    for entry in page.entries {
                        let path = PathBuf::from(&entry.path);
                        if entry.kind == "directory" {
                            pending.push(path);
                            continue;
                        }
                        if entry.kind != "file"
                            || !entry.name.starts_with("rollout-")
                            || !(entry.name.ends_with(".jsonl")
                                || entry.name.ends_with(".jsonl.zst"))
                        {
                            continue;
                        }
                        let Some((id, secondary_id)) = rollout_identity(&entry.name) else {
                            continue;
                        };
                        let path = std::fs::canonicalize(path).into_diagnostic()?;
                        if !path.starts_with(&root) {
                            continue;
                        }
                        let (size, modified_ns) = stamp(&path)?;
                        let key = crate::stage::digest(
                            format!("{}:{size}:{modified_ns}", path.display()).as_bytes(),
                        );
                        let title = names
                            .get(&id)
                            .or_else(|| secondary_id.as_ref().and_then(|child| names.get(child)))
                            .cloned()
                            .unwrap_or_else(|| {
                                fallback_title(&path, &root, &entry.name, &id, &names)
                            });
                        conversations.push(Conversation {
                            id: key,
                            title,
                            updated_at_ms: modified_ns / 1_000_000,
                            archived,
                            path,
                            root: root.clone(),
                            session_id: id,
                            secondary_session_id: secondary_id,
                            size,
                            modified_ns,
                        });
                    }
                    cursor = page.next_cursor;
                    if cursor.is_none() {
                        break;
                    }
                }
            }
        }
        conversations.sort_by(|a, b| {
            b.updated_at_ms
                .cmp(&a.updated_at_ms)
                .then_with(|| a.id.cmp(&b.id))
        });
        conversations.dedup_by(|a, b| a.id == b.id);
        Ok(conversations)
    }
    pub fn list(
        &self,
        after: Option<&str>,
        search: &str,
        limit: usize,
        refresh: bool,
    ) -> Result<(Vec<Conversation>, Option<String>)> {
        let mut snapshot = self.snapshot.lock();
        if refresh || snapshot.is_none() {
            *snapshot = Some(self.scan()?)
        }
        let entries = snapshot.as_ref().expect("initialized snapshot");
        let start = match after {
            None => 0,
            Some(cursor) => entries
                .iter()
                .position(|entry| entry.id == cursor)
                .map(|index| index + 1)
                .ok_or_else(|| ActorError::Conflict("Conversation list changed".into()))?,
        };
        let search = search.to_lowercase();
        let mut page = entries
            .iter()
            .skip(start)
            .filter(|entry| entry.title.to_lowercase().contains(&search))
            .take(limit + 1)
            .cloned()
            .collect::<Vec<_>>();
        let next = if page.len() > limit {
            page.truncate(limit);
            page.last().map(|entry| entry.id.clone())
        } else {
            None
        };
        Ok((page, next))
    }
    pub fn load(&self, id: &str) -> Result<Transcript> {
        let source = self
            .snapshot
            .lock()
            .as_ref()
            .and_then(|entries| entries.iter().find(|entry| entry.id == id))
            .cloned()
            .ok_or_else(|| ActorError::NotFound("Conversation source".into()))?;
        let changed =
            || ActorError::Conflict("Codex conversation changed; refresh the list".into());
        if stamp(&source.path).map_err(|_| changed())? != (source.size, source.modified_ns) {
            return Err(changed().into());
        }
        let bytes = crate::file_io::read_bytes(&source.path, u64::MAX, &policy(&source.root))?;
        if stamp(&source.path).map_err(|_| changed())? != (source.size, source.modified_ns)
            || bytes.len() as u64 != source.size
        {
            return Err(changed().into());
        }
        let bytes = if source.path.to_string_lossy().ends_with(".jsonl.zst") {
            decompress_rollout(&bytes)
                .ok_or_else(|| miette!("Invalid Codex conversation archive"))?
        } else {
            bytes
        };
        let transcript = parse_codex(&bytes)?;
        let accepted = transcript.session_id == source.session_id
            || source.secondary_session_id.as_deref() == Some(transcript.session_id.as_str());
        if !accepted {
            return Err(changed().into());
        }
        Ok(transcript)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn open_sources(home: &Path) -> CodexSources {
        CodexSources::new(Some(home.to_path_buf()))
    }

    fn write_rollout(home: &Path, name: &str, records: &[serde_json::Value]) -> PathBuf {
        let path = home.join("sessions/2026/10/23").join(name);
        std::fs::create_dir_all(path.parent().expect("rollout directory")).unwrap();
        let body = records
            .iter()
            .map(|record| record.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, body).unwrap();
        path
    }

    fn write_index(home: &Path, entries: &[(&str, &str)]) {
        let body = entries
            .iter()
            .map(|(id, name)| json!({"id": id, "thread_name": name}).to_string())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(home.join("session_index.jsonl"), body).unwrap();
    }

    fn sole_entry(sources: &CodexSources) -> Conversation {
        let entries = sources.list(None, "", 10, false).unwrap().0;
        assert_eq!(entries.len(), 1);
        entries.into_iter().next().expect("single entry")
    }

    #[test]
    fn resumed_rollouts_use_the_parent_identity_for_titles_and_loads() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let parent = "01a10000-0000-7000-8000-000000000001";
        let child = "01a10000-0000-7000-8000-000000000002";
        write_rollout(
            home,
            &format!("rollout-2026-10-23T21-08-08-{parent}_{child}.jsonl"),
            &[
                json!({"type":"session_meta","payload":{"id":parent,"session_id":parent,"thread_source":"user"}}),
                json!({"type":"event_msg","payload":{"type":"user_message","message":"Resume the work."}}),
            ],
        );
        write_index(home, &[(parent, "Parent thread")]);
        let sources = open_sources(home);
        let entry = sole_entry(&sources);
        assert_eq!(entry.title, "Parent thread");
        assert_eq!(entry.session_id, parent);
        assert_eq!(entry.secondary_session_id.as_deref(), Some(child));
        let transcript = sources.load(&entry.id).unwrap();
        assert_eq!(transcript.session_id, parent);
        assert_eq!(transcript.messages[0].text, "Resume the work.");
    }

    #[test]
    fn unnamed_subagents_reuse_the_named_parent_title() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let parent = "01a20000-0000-7000-8000-000000000001";
        let own = "01a20000-0000-7000-8000-000000000002";
        write_rollout(
            home,
            &format!("rollout-2026-10-23T21-08-08-{own}.jsonl"),
            &[
                json!({"type":"session_meta","payload":{"id":own,"session_id":parent,"thread_source":"subagent"}}),
                json!({"type":"event_msg","payload":{"type":"user_message","message":"Delegated task."}}),
            ],
        );
        write_index(home, &[(parent, "Parent thread")]);
        let sources = open_sources(home);
        let entry = sole_entry(&sources);
        assert_eq!(entry.title, "Parent thread · 子代理");
        assert_eq!(sources.load(&entry.id).unwrap().session_id, own);
    }

    #[test]
    fn unnamed_rollouts_fall_back_to_the_filename_timestamp() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let own = "01a30000-0000-7000-8000-000000000001";
        write_rollout(
            home,
            &format!("rollout-2026-10-23T21-08-08-{own}.jsonl"),
            &[
                json!({"type":"session_meta","payload":{"id":own,"session_id":own,"thread_source":"user"}}),
                json!({"type":"event_msg","payload":{"type":"user_message","message":"Unnamed."}}),
            ],
        );
        let sources = open_sources(home);
        assert_eq!(sole_entry(&sources).title, "未命名会话 · 10-23 21:08");
    }

    #[test]
    fn compressed_rollouts_are_listed_titled_and_loaded() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let named = "01a40000-0000-7000-8000-000000000001";
        let unnamed = "01a40000-0000-7000-8000-000000000002";
        let directory = home.join("sessions/2026/10/10");
        std::fs::create_dir_all(&directory).unwrap();
        for (id, message) in [(named, "Named archive"), (unnamed, "Unnamed archive")] {
            let body = [
                json!({"type":"session_meta","payload":{"id":id,"session_id":id,"thread_source":"user"}}),
                json!({"type":"event_msg","payload":{"type":"user_message","message":message}}),
            ]
            .iter()
            .map(|record| record.to_string())
            .collect::<Vec<_>>()
            .join("\n");
            std::fs::write(
                directory.join(format!("rollout-2026-10-10T08-00-00-{id}.jsonl.zst")),
                zstd::stream::encode_all(body.as_bytes(), 3).unwrap(),
            )
            .unwrap();
        }
        write_index(home, &[(named, "Archived thread")]);
        let sources = open_sources(home);
        let entries = sources.list(None, "", 10, false).unwrap().0;
        let mut titles = entries
            .iter()
            .map(|entry| entry.title.clone())
            .collect::<Vec<_>>();
        titles.sort();
        assert_eq!(
            titles,
            vec![
                "Archived thread".to_owned(),
                "未命名会话 · 10-10 08:00".to_owned()
            ]
        );
        let named_entry = entries
            .iter()
            .find(|entry| entry.title == "Archived thread")
            .expect("named archive");
        assert_eq!(
            sources.load(&named_entry.id).unwrap().messages[0].text,
            "Named archive"
        );
    }

    #[test]
    fn load_accepts_a_child_identity_from_combined_file_names() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let parent = "01a50000-0000-7000-8000-000000000001";
        let child = "01a50000-0000-7000-8000-000000000002";
        write_rollout(
            home,
            &format!("rollout-2026-10-23T21-08-08-{parent}_{child}.jsonl"),
            &[
                json!({"type":"session_meta","payload":{"id":child,"session_id":child,"thread_source":"user"}}),
                json!({"type":"event_msg","payload":{"type":"user_message","message":"Child identity."}}),
            ],
        );
        write_index(home, &[(parent, "Parent thread")]);
        let sources = open_sources(home);
        let entry = sole_entry(&sources);
        assert_eq!(entry.title, "Parent thread");
        assert_eq!(entry.secondary_session_id.as_deref(), Some(child));
        assert_eq!(sources.load(&entry.id).unwrap().session_id, child);
    }
}
