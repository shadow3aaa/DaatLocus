//! A private snapshot of local Codex rollout locations; HTTP only receives opaque identities.
use super::{Transcript, parse_codex};
use crate::{actors::ActorError, sandbox::RuntimeSandboxPolicy};
use miette::{IntoDiagnostic, Result};
use parking_lot::Mutex;
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
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
                            || !entry.name.ends_with(".jsonl")
                        {
                            continue;
                        }
                        let stem = entry.name.trim_end_matches(".jsonl");
                        let Some(id) = stem
                            .get(stem.len().saturating_sub(36)..)
                            .filter(|id| uuid::Uuid::parse_str(id).is_ok())
                        else {
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
                        conversations.push(Conversation {
                            id: key,
                            title: names.get(id).cloned().unwrap_or_else(|| "Codex".into()),
                            updated_at_ms: modified_ns / 1_000_000,
                            archived,
                            path,
                            root: root.clone(),
                            session_id: id.into(),
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
        let transcript = parse_codex(&bytes)?;
        if transcript.session_id != source.session_id {
            return Err(changed().into());
        }
        Ok(transcript)
    }
}
