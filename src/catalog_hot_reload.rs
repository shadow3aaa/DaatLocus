//! Turn-granularity hot reload for the skills and workflow catalogs.
//!
//! The runtime renders the system prompt skills section and the workflow tool
//! specs from the in-memory catalogs on every turn, but the catalogs themselves
//! are only refreshed on session start or via `/skills reload`. This module
//! compares a cheap filesystem fingerprint (path + mtime + size) of every
//! catalog input file against the last seen fingerprint at each turn boundary
//! and reloads both catalogs when it changes.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::context::Context;

/// Identity of one catalog input file (a skill `SKILL.md` or a workflow `.lua`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogFileEntry {
    pub path: PathBuf,
    pub modified_ms: u64,
    pub len: u64,
}

const SKILL_FILE_NAME: &str = "SKILL.md";

/// Compute the catalog fingerprint from explicit roots. Exposed for tests.
pub fn compute_fingerprint_for_roots(
    skill_roots: &[PathBuf],
    workflows_dir: &Path,
) -> Vec<CatalogFileEntry> {
    let mut entries = Vec::new();
    for root in skill_roots {
        collect_skill_files(root, &mut entries);
    }
    collect_lua_files(workflows_dir, &mut entries);
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    entries.dedup_by(|a, b| a.path == b.path);
    entries
}

/// Last observed root-directory mtimes plus the fingerprint computed from them.
///
/// When every watched root's mtime is unchanged, the full recursive scan is skipped.
/// A fingerprint change is recorded from that same scan; the tree is not walked again.
struct CatalogScanCache {
    root_mtimes: Vec<(PathBuf, Option<SystemTime>)>,
    fingerprint: Vec<CatalogFileEntry>,
}

fn catalog_scan_cache() -> &'static Mutex<Option<CatalogScanCache>> {
    static CACHE: Mutex<Option<CatalogScanCache>> = Mutex::new(None);
    &CACHE
}

fn root_mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
}

fn watched_root_mtimes(
    skill_roots: &[PathBuf],
    workflows_dir: &Path,
) -> Vec<(PathBuf, Option<SystemTime>)> {
    let mut roots = skill_roots.to_vec();
    roots.push(workflows_dir.to_path_buf());
    roots
        .into_iter()
        .map(|path| {
            let mtime = root_mtime(&path);
            (path, mtime)
        })
        .collect()
}

/// Compute the fingerprint for the current runtime's skill roots and workflow directory.
///
/// Unchanged root-directory mtimes reuse the previous fingerprint. A changed root triggers
/// one full scan, which is cached immediately so a later identical check does not rescan.
pub fn compute_catalogs_fingerprint(execution_cwd: &Path) -> Vec<CatalogFileEntry> {
    let roots = crate::openskills::skill_roots(execution_cwd)
        .into_iter()
        .map(|root| root.path)
        .collect::<Vec<_>>();
    let workflows_dir = crate::daat_locus_paths::daat_locus_paths_sync().workflows_dir();
    let root_mtimes = watched_root_mtimes(&roots, &workflows_dir);

    let mut cache = catalog_scan_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(cached) = cache.as_ref()
        && cached.root_mtimes == root_mtimes
    {
        return cached.fingerprint.clone();
    }

    let fingerprint = compute_fingerprint_for_roots(&roots, &workflows_dir);
    *cache = Some(CatalogScanCache {
        root_mtimes,
        fingerprint: fingerprint.clone(),
    });
    fingerprint
}

/// Reload the skills and workflow catalogs when their input files changed since
/// the last turn. Called at the start of every runtime turn; a no-op otherwise.
///
/// The fingerprint comes from [`compute_catalogs_fingerprint`], which skips the
/// recursive directory walk when watched root mtimes are unchanged. After a real
/// change the post-reload fingerprint is taken from that same cached scan — it is
/// not computed by walking the tree a second time.
pub fn maybe_hot_reload_catalogs(context: &mut Context) {
    let fingerprint = compute_catalogs_fingerprint(&context.execution_cwd);
    if context.catalog_hot_reload_fingerprint.as_deref() == Some(&fingerprint) {
        return;
    }
    context.openskills = crate::openskills::reload_openskills_for_runtime(&context.execution_cwd);
    context.workflows.reload();
    tracing::info!(
        "skills and workflows catalogs hot-reloaded after file change; fingerprint entries: {}",
        fingerprint.len()
    );
    // Keep the fingerprint from the scan that decided to reload. Catalog writes
    // during reload bump root mtimes, but we do not walk the tree again here.
    context.catalog_hot_reload_fingerprint = Some(fingerprint);
}

fn file_entry(path: &Path) -> Option<CatalogFileEntry> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    let modified_ms = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis() as u64;
    Some(CatalogFileEntry {
        path: path.to_path_buf(),
        modified_ms,
        len: metadata.len(),
    })
}

fn collect_skill_files(dir: &Path, entries: &mut Vec<CatalogFileEntry>) {
    let Ok(read_dir) = fs::read_dir(dir) else {
        return;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if file_name.starts_with('.') {
            continue;
        }
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if metadata.is_dir() {
            collect_skill_files(&path, entries);
        } else if file_name == SKILL_FILE_NAME
            && let Some(entry) = file_entry(&path)
        {
            entries.push(entry);
        }
    }
}

fn collect_lua_files(dir: &Path, entries: &mut Vec<CatalogFileEntry>) {
    let Ok(read_dir) = fs::read_dir(dir) else {
        return;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) == Some("lua")
            && let Some(entry) = file_entry(&path)
        {
            entries.push(entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::write(path, content).expect("write file");
    }

    #[test]
    fn fingerprint_is_empty_without_inputs() {
        let home = tempfile::tempdir().expect("tempdir");
        let fingerprint = compute_fingerprint_for_roots(&[], &home.path().join("workflows"));
        assert!(fingerprint.is_empty());
    }

    #[test]
    fn fingerprint_is_stable_across_recomputation() {
        let home = tempfile::tempdir().expect("tempdir");
        let skills = home.path().join("skills");
        write(&skills.join("alpha").join("SKILL.md"), "# Alpha\n");
        let workflows = home.path().join("workflows");
        write(&workflows.join("goal.lua"), "return {}\n");
        let first = compute_fingerprint_for_roots(std::slice::from_ref(&skills), &workflows);
        let second = compute_fingerprint_for_roots(std::slice::from_ref(&skills), &workflows);
        assert_eq!(first, second);
        assert_eq!(first.len(), 2);
    }

    #[test]
    fn fingerprint_detects_new_skill() {
        let home = tempfile::tempdir().expect("tempdir");
        let skills = home.path().join("skills");
        write(&skills.join("alpha").join("SKILL.md"), "# Alpha\n");
        let workflows = home.path().join("workflows");
        let before = compute_fingerprint_for_roots(std::slice::from_ref(&skills), &workflows);
        write(&skills.join("beta").join("SKILL.md"), "# Beta\n");
        let after = compute_fingerprint_for_roots(std::slice::from_ref(&skills), &workflows);
        assert_ne!(before, after);
        assert_eq!(after.len(), 2);
    }

    #[test]
    fn fingerprint_detects_removed_workflow() {
        let home = tempfile::tempdir().expect("tempdir");
        let skills = home.path().join("skills");
        let workflows = home.path().join("workflows");
        let path = workflows.join("goal.lua");
        write(&path, "return {}\n");
        let before = compute_fingerprint_for_roots(std::slice::from_ref(&skills), &workflows);
        fs::remove_file(&path).expect("remove workflow");
        let after = compute_fingerprint_for_roots(std::slice::from_ref(&skills), &workflows);
        assert_ne!(before, after);
        assert!(after.is_empty());
    }

    #[test]
    fn fingerprint_detects_content_change() {
        let home = tempfile::tempdir().expect("tempdir");
        let skills = home.path().join("skills");
        let workflows = home.path().join("workflows");
        let path = workflows.join("goal.lua");
        write(&path, "return {}\n");
        let before = compute_fingerprint_for_roots(std::slice::from_ref(&skills), &workflows);
        write(&path, "return { extra = true }\n");
        let after = compute_fingerprint_for_roots(std::slice::from_ref(&skills), &workflows);
        assert_ne!(before, after);
    }

    #[test]
    fn fingerprint_ignores_non_lua_and_dot_entries() {
        let home = tempfile::tempdir().expect("tempdir");
        let skills = home.path().join("skills");
        let workflows = home.path().join("workflows");
        write(&skills.join(".hidden").join("SKILL.md"), "# Hidden\n");
        write(&workflows.join("notes.txt"), "not a workflow\n");
        let fingerprint = compute_fingerprint_for_roots(std::slice::from_ref(&skills), &workflows);
        assert!(fingerprint.is_empty());
    }

    #[test]
    fn fingerprint_finds_nested_skill_files() {
        let home = tempfile::tempdir().expect("tempdir");
        let skills = home.path().join("skills");
        write(
            &skills.join("nested").join("deep").join("SKILL.md"),
            "# Deep\n",
        );
        let workflows = home.path().join("workflows");
        let fingerprint = compute_fingerprint_for_roots(std::slice::from_ref(&skills), &workflows);
        assert_eq!(fingerprint.len(), 1);
        assert!(fingerprint[0].path.ends_with("SKILL.md"));
    }
}
