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
    time::UNIX_EPOCH,
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

/// Compute the fingerprint for the current runtime's skill roots and workflow directory.
pub fn compute_catalogs_fingerprint(execution_cwd: &Path) -> Vec<CatalogFileEntry> {
    let roots = crate::openskills::skill_roots(execution_cwd)
        .into_iter()
        .map(|root| root.path)
        .collect::<Vec<_>>();
    let workflows_dir = crate::daat_locus_paths::daat_locus_paths_sync().workflows_dir();
    compute_fingerprint_for_roots(&roots, &workflows_dir)
}

/// Reload the skills and workflow catalogs when their input files changed since
/// the last turn. Called at the start of every runtime turn; a no-op otherwise.
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
    // Recompute after reload so catalog writes (e.g. builtin skill materialization)
    // do not trigger a second reload on the next turn.
    context.catalog_hot_reload_fingerprint =
        Some(compute_catalogs_fingerprint(&context.execution_cwd));
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
        } else if file_name == SKILL_FILE_NAME {
            if let Some(entry) = file_entry(&path) {
                entries.push(entry);
            }
        }
    }
}

fn collect_lua_files(dir: &Path, entries: &mut Vec<CatalogFileEntry>) {
    let Ok(read_dir) = fs::read_dir(dir) else {
        return;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) == Some("lua") {
            if let Some(entry) = file_entry(&path) {
                entries.push(entry);
            }
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
        let first = compute_fingerprint_for_roots(&[skills.clone()], &workflows);
        let second = compute_fingerprint_for_roots(&[skills.clone()], &workflows);
        assert_eq!(first, second);
        assert_eq!(first.len(), 2);
    }

    #[test]
    fn fingerprint_detects_new_skill() {
        let home = tempfile::tempdir().expect("tempdir");
        let skills = home.path().join("skills");
        write(&skills.join("alpha").join("SKILL.md"), "# Alpha\n");
        let workflows = home.path().join("workflows");
        let before = compute_fingerprint_for_roots(&[skills.clone()], &workflows);
        write(&skills.join("beta").join("SKILL.md"), "# Beta\n");
        let after = compute_fingerprint_for_roots(&[skills.clone()], &workflows);
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
        let before = compute_fingerprint_for_roots(&[skills.clone()], &workflows);
        fs::remove_file(&path).expect("remove workflow");
        let after = compute_fingerprint_for_roots(&[skills.clone()], &workflows);
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
        let before = compute_fingerprint_for_roots(&[skills.clone()], &workflows);
        write(&path, "return { extra = true }\n");
        let after = compute_fingerprint_for_roots(&[skills.clone()], &workflows);
        assert_ne!(before, after);
    }

    #[test]
    fn fingerprint_ignores_non_lua_and_dot_entries() {
        let home = tempfile::tempdir().expect("tempdir");
        let skills = home.path().join("skills");
        let workflows = home.path().join("workflows");
        write(&skills.join(".hidden").join("SKILL.md"), "# Hidden\n");
        write(&workflows.join("notes.txt"), "not a workflow\n");
        let fingerprint = compute_fingerprint_for_roots(&[skills.clone()], &workflows);
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
        let fingerprint = compute_fingerprint_for_roots(&[skills.clone()], &workflows);
        assert_eq!(fingerprint.len(), 1);
        assert!(fingerprint[0].path.ends_with("SKILL.md"));
    }
}
