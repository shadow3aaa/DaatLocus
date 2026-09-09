//! In-process SCOPE engine handle.
//!
//! Provides direct access to scope-engine functionality (tree-sitter parsing,
//! symbol lookup, code editing, propagation analysis) without spawning a
//! separate helper process. The scope-engine crate is linked directly as a
//! library dependency.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use miette::{Result, miette};
use scope_engine::analyzer::Analyzer;
use scope_engine::api;
use scope_engine::engine;
use scope_engine::state::PropagationState;

pub struct ScopeEditCodeResult {
    pub propagation_results: Vec<api::PropagationResult>,
    pub applied_summary: api::AppliedStructuredEditSummary,
}

/// In-process SCOPE engine handle.
///
/// Wraps the scope-engine library to provide:
/// - Path plus line-hash code search and reading
/// - Hash-anchored code editing and deletion
/// - Propagation review events
/// - Config hints for language servers
pub struct ScopeEngineHandle {
    project_root: Option<PathBuf>,
    propagation_state: Mutex<PropagationState>,
    lsp_analyzer: Mutex<Option<Box<dyn Analyzer + Send>>>,
    anchors: Mutex<AnchorTable>,
}

const MAX_TRACKED_ANCHORS: usize = 16_384;

#[derive(Debug, Clone)]
struct TrackedAnchor {
    line: usize,
    hash: String,
    text: String,
    last_used: u64,
}

#[derive(Debug, Default)]
struct AnchorTable {
    next_use: u64,
    entries: HashMap<(PathBuf, String), TrackedAnchor>,
}

#[derive(Debug)]
struct AnchorMutation {
    path: PathBuf,
    start_line: usize,
    end_line: usize,
    operation: api::EditOp,
    replacement_lines: usize,
}

impl ScopeEngineHandle {
    pub fn new() -> Self {
        Self {
            project_root: None,
            propagation_state: Mutex::new(PropagationState::new()),
            lsp_analyzer: Mutex::new(None),
            anchors: Mutex::new(AnchorTable::default()),
        }
    }

    /// Open a project, setting the root directory for subsequent operations.
    pub fn open_project(
        &mut self,
        project_root: impl Into<PathBuf>,
    ) -> Result<api::OpenProjectOutput> {
        let project_root = project_root.into();
        let previous_project_root = self.project_root.clone();
        let output = engine::open_project(
            &project_root,
            previous_project_root.as_deref(),
            &self.lsp_analyzer,
        )
        .map_err(|err| miette!("{err}"))?;
        if previous_project_root.as_deref() != Some(project_root.as_path()) {
            let mut state = self
                .propagation_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *state = PropagationState::new();
            self.anchors
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entries
                .clear();
        }
        self.project_root = Some(project_root);
        Ok(output)
    }

    fn require_project_root(&self) -> Result<&Path> {
        self.project_root
            .as_deref()
            .ok_or_else(|| miette!("no project opened"))
    }

    /// Accumulate propagation results and get the next review event, if any.
    #[cfg(test)]
    pub fn next_review_event(
        &self,
        results: Vec<api::PropagationResult>,
    ) -> Option<api::ReviewEvent> {
        let mut state = self
            .propagation_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.accumulate(results);
        state.next_review()
    }

    /// Read code using a path plus line-hash anchor.
    pub fn read_code(&self, input: &api::ReadCodeInput) -> Result<api::ReadCodeOutput> {
        let root = self.require_project_root()?;
        let output = engine::read_code(root, input)
            .map_err(|err| miette!("scope-engine read_code failed: {err}"))?;
        self.observe_anchored_content(&input.path, &output.content);
        Ok(output)
    }

    /// Search code and return matched line-hash hits.
    pub fn search_code(&self, input: &api::SearchCodeInput) -> Result<api::SearchCodeOutput> {
        let root = self.require_project_root()?;
        let output = engine::search_code(root, input)
            .map_err(|err| miette!("scope-engine search_code failed: {err}"))?;
        for hit in &output.matches {
            self.observe_anchored_content(&hit.path, &hit.hit);
        }
        Ok(output)
    }

    /// Apply structured edits via scope-engine.
    pub fn edit_code(&self, edits: &[api::StructuredEdit]) -> Result<ScopeEditCodeResult> {
        let root = self.require_project_root()?;
        let mut resolved_edits = edits.to_vec();
        let mutations = self.resolve_tracked_anchors(root, &mut resolved_edits)?;
        let output = engine::edit_code(
            root,
            &api::EditCodeInput {
                edits: resolved_edits,
            },
            &self.propagation_state,
            &self.lsp_analyzer,
        )
        .map_err(|err| miette!("scope-engine edit_code failed: {err}"))?;
        self.apply_anchor_mutations(&mutations);
        Ok(ScopeEditCodeResult {
            propagation_results: output.propagation_results,
            applied_summary: output.applied_summary,
        })
    }

    fn observe_anchored_content(&self, path: &str, content: &str) {
        let Ok(mut table) = self.anchors.lock() else {
            return;
        };
        for line in content.lines() {
            let (path, anchor, text) = if let Some((first, rest)) = line.split_once('|')
                && let Some((anchor, text)) = rest.split_once('|')
            {
                (first, anchor, text)
            } else if let Some((anchor, text)) = line.split_once('|') {
                (path, anchor, text)
            } else {
                continue;
            };
            let Some((line_number, hash)) = anchor.rsplit_once('#') else {
                continue;
            };
            let Ok(line) = line_number.parse::<usize>() else {
                continue;
            };
            if line == 0 || hash.is_empty() || hash.ends_with('~') {
                continue;
            }
            table.next_use = table.next_use.wrapping_add(1);
            let last_used = table.next_use;
            table.entries.insert(
                (PathBuf::from(path), anchor.to_string()),
                TrackedAnchor {
                    line,
                    hash: hash.to_string(),
                    text: text.to_string(),
                    last_used,
                },
            );
        }
        while table.entries.len() > MAX_TRACKED_ANCHORS {
            let Some(key) = table
                .entries
                .iter()
                .min_by_key(|(_, anchor)| anchor.last_used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            table.entries.remove(&key);
        }
    }

    fn resolve_tracked_anchors(
        &self,
        root: &Path,
        edits: &mut [api::StructuredEdit],
    ) -> Result<Vec<AnchorMutation>> {
        let mut table = self
            .anchors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut mutations = Vec::new();
        for edit in edits {
            let path = PathBuf::from(&edit.path);
            let Some(start) = edit.start.as_mut() else {
                continue;
            };
            let Some(mut anchor) = table.entries.get(&(path.clone(), start.clone())).cloned()
            else {
                continue;
            };
            let current = std::fs::read_to_string(root.join(&path))
                .map_err(|err| miette!("cannot refresh tracked anchor {}: {err}", edit.path))?;
            let lines: Vec<&str> = current.lines().collect();
            let current_line = if anchor.line > 0
                && anchor.line <= lines.len()
                && lines[anchor.line - 1] == anchor.text
            {
                Some(anchor.line)
            } else {
                let matches = lines
                    .iter()
                    .enumerate()
                    .filter(|(_, line)| **line == anchor.text)
                    .map(|(index, _)| index + 1)
                    .collect::<Vec<_>>();
                (matches.len() == 1).then_some(matches[0])
            };
            let Some(current_line) = current_line else {
                continue;
            };
            table.next_use = table.next_use.wrapping_add(1);
            anchor.last_used = table.next_use;
            table
                .entries
                .insert((path.clone(), start.clone()), anchor.clone());
            let new_start = format!("{}#{}", current_line, anchor.hash);
            *start = new_start;
            if let Some(end) = edit.end.as_mut()
                && let Some(end_anchor) = table.entries.get(&(path.clone(), end.clone())).cloned()
            {
                let end_line = if end_anchor.line > 0
                    && end_anchor.line <= lines.len()
                    && lines[end_anchor.line - 1] == end_anchor.text
                {
                    Some(end_anchor.line)
                } else {
                    let matches = lines
                        .iter()
                        .enumerate()
                        .filter(|(_, line)| **line == end_anchor.text)
                        .map(|(index, _)| index + 1)
                        .collect::<Vec<_>>();
                    (matches.len() == 1).then_some(matches[0])
                };
                if let Some(end_line) = end_line {
                    *end = format!("{}#{}", end_line, end_anchor.hash);
                }
            }
            let start_line = current_line;
            let end_line = edit
                .end
                .as_deref()
                .and_then(|value| value.split_once('#'))
                .and_then(|(line, _)| line.parse::<usize>().ok())
                .unwrap_or(start_line);
            mutations.push(AnchorMutation {
                path,
                start_line,
                end_line,
                operation: edit.op.clone().unwrap_or(api::EditOp::Replace),
                replacement_lines: edit
                    .content
                    .clone()
                    .map(api::EditContent::into_lines)
                    .unwrap_or_default()
                    .len(),
            });
        }
        Ok(mutations)
    }

    fn apply_anchor_mutations(&self, mutations: &[AnchorMutation]) {
        let mut table = self
            .anchors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for mutation in mutations {
            let delta = mutation.replacement_lines as isize
                - match &mutation.operation {
                    api::EditOp::Replace => (mutation.end_line - mutation.start_line + 1) as isize,
                    api::EditOp::Append | api::EditOp::Prepend => 0,
                };
            for ((path, _), anchor) in &mut table.entries {
                if *path != mutation.path {
                    continue;
                }
                let insertion_line = match &mutation.operation {
                    api::EditOp::Append => mutation.start_line,
                    api::EditOp::Prepend => mutation.start_line,
                    api::EditOp::Replace => mutation.start_line,
                };
                let affected = match &mutation.operation {
                    api::EditOp::Replace => {
                        anchor.line >= mutation.start_line && anchor.line <= mutation.end_line
                    }
                    api::EditOp::Append | api::EditOp::Prepend => false,
                };
                if affected {
                    anchor.line = 0;
                } else if anchor.line >= insertion_line {
                    anchor.line = anchor.line.saturating_add_signed(delta);
                }
            }
        }
        table.entries.retain(|_, anchor| anchor.line != 0);
    }

    /// Return whether SCOPE owns semantic source operations for a path.
    pub fn is_responsible_source(&self, path: &Path) -> Result<api::SourceResponsibility> {
        let root = self.require_project_root()?;
        engine::is_responsible_source(
            root,
            &api::SourceResponsibilityInput {
                path: path.to_string_lossy().into_owned(),
            },
        )
        .map_err(|err| miette!("scope-engine is_responsible_source failed: {err}"))
    }

    /// Count accumulated propagation review events.
    pub fn pending_review_count(&self) -> usize {
        let state = self
            .propagation_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.pending_count()
    }

    /// Acknowledge and return accumulated propagation review events.
    pub fn ack_next_events(&self, limit: Option<usize>) -> api::ReviewBatch {
        engine::ack_next_events(&self.propagation_state, limit)
            .unwrap_or_else(|err| panic!("scope-engine ack_next_events failed: {err}"))
    }

    /// Get config hints for language servers and tree-sitter languages.
    pub fn get_config_hints() -> serde_json::Value {
        engine::config_hints()
    }
}

impl Default for ScopeEngineHandle {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_result(selector: &str) -> api::PropagationResult {
        api::PropagationResult {
            selector: selector.to_string(),
            reason: "changed".to_string(),
            source: api::PropagationSource::OpenEnded,
            lsp_references: None,
            diff_summary: Some("diff".to_string()),
            file_snippet: Some("fn main() {}".to_string()),
            project_files: Some(vec!["src/main.rs".to_string()]),
        }
    }

    #[test]
    fn open_project_preserves_pending_review_for_same_project_and_resets_on_project_change() {
        let temp_dir = tempfile::tempdir().unwrap();
        let other_temp_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            temp_dir.path().join("Cargo.toml"),
            "[package]\nname = \"tmp\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::write(
            other_temp_dir.path().join("Cargo.toml"),
            "[package]\nname = \"other\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();

        let mut handle = ScopeEngineHandle::new();
        assert!(
            handle
                .next_review_event(vec![open_result("src/main.rs::fn main")])
                .is_some()
        );
        assert_eq!(handle.pending_review_count(), 0);
        assert!(
            handle
                .next_review_event(vec![open_result("src/main.rs::fn main")])
                .is_some()
        );
        assert_eq!(handle.pending_review_count(), 0);

        handle
            .propagation_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .accumulate(vec![open_result("src/main.rs::fn main")]);
        assert_eq!(handle.pending_review_count(), 1);

        handle.open_project(temp_dir.path()).expect("open project");
        assert_eq!(handle.pending_review_count(), 0);

        handle
            .propagation_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .accumulate(vec![open_result("src/lib.rs::fn lib")]);
        assert_eq!(handle.pending_review_count(), 1);

        let output = handle
            .open_project(temp_dir.path())
            .expect("reopen same project");
        assert_eq!(output.status, "already_open");
        assert_eq!(handle.pending_review_count(), 1);

        handle
            .open_project(other_temp_dir.path())
            .expect("open other project");
        assert_eq!(handle.pending_review_count(), 0);
        assert!(
            handle
                .next_review_event(vec![open_result("src/main.rs::fn main")])
                .is_some()
        );
    }

    #[test]
    fn ack_next_events_returns_batch_and_remaining_count() {
        let handle = ScopeEngineHandle::new();
        handle
            .propagation_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .accumulate(vec![
                open_result("src/a.rs::fn first"),
                open_result("src/b.rs::fn second"),
                open_result("src/c.rs::fn third"),
            ]);

        let output = handle.ack_next_events(Some(2));

        assert_eq!(output.returned, 2);
        assert_eq!(output.reviews.len(), 2);
        assert_eq!(output.remaining, 1);
        match output.review.unwrap() {
            api::ReviewEvent::InvestigateImpact {
                modified_symbol, ..
            } => assert_eq!(modified_symbol, "src/c.rs::fn third"),
            api::ReviewEvent::KnownReferences { .. } => panic!("expected InvestigateImpact review"),
        }
    }

    #[test]
    fn edit_code_relocates_unique_observed_anchor_after_prior_insertion() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file = temp_dir.path().join("src").join("lib.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "fn first() {}\nfn target() {}\n").unwrap();

        let mut handle = ScopeEngineHandle::new();
        handle.open_project(temp_dir.path()).unwrap();
        let target_hash = scope_engine::patch::line_hash("fn target() {}");
        handle
            .read_code(&api::ReadCodeInput {
                path: "src/lib.rs".to_string(),
                anchor: format!("2#{target_hash}"),
                mode: api::ReadCodeMode::Around,
            })
            .unwrap();

        std::fs::write(&file, "fn inserted() {}\nfn first() {}\nfn target() {}\n").unwrap();
        handle
            .edit_code(&[api::StructuredEdit {
                path: "src/lib.rs".to_string(),
                op: Some(api::EditOp::Replace),
                start: Some(format!("2#{target_hash}")),
                end: Some(format!("2#{target_hash}")),
                content: Some(api::EditContent::Text("fn changed() {}".to_string())),
            }])
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(file).unwrap(),
            "fn inserted() {}\nfn first() {}\nfn changed() {}\n"
        );
    }

    #[test]
    fn edit_code_does_not_guess_when_observed_hash_is_ambiguous() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file = temp_dir.path().join("src").join("lib.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "fn first() {}\nfn target() {}\n").unwrap();

        let mut handle = ScopeEngineHandle::new();
        handle.open_project(temp_dir.path()).unwrap();
        let target_hash = scope_engine::patch::line_hash("fn target() {}");
        handle
            .read_code(&api::ReadCodeInput {
                path: "src/lib.rs".to_string(),
                anchor: format!("2#{target_hash}"),
                mode: api::ReadCodeMode::Around,
            })
            .unwrap();

        std::fs::write(&file, "fn target() {}\nfn first() {}\nfn target() {}\n").unwrap();
        let result = handle.edit_code(&[api::StructuredEdit {
            path: "src/lib.rs".to_string(),
            op: Some(api::EditOp::Replace),
            start: Some(format!("2#{target_hash}")),
            end: Some(format!("2#{target_hash}")),
            content: Some(api::EditContent::Text("fn changed() {}".to_string())),
        }]);

        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(file).unwrap(),
            "fn target() {}\nfn first() {}\nfn target() {}\n"
        );
    }
}
