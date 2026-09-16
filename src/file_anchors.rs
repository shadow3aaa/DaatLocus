//! Session-local hash-anchor relocation for file edits.
//!
//! Read tools render source as `line#hash|text` records. Once an edit shifts
//! lines, anchors shown earlier would fail hash verification even when the
//! target text is unchanged. This table records the observed text for every
//! shown anchor so a later edit can relocate a unique target, while changed or
//! ambiguous targets still fail safely.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use miette::{Result, miette};
use scope_engine::api::{EditOp, StructuredEdit};

const MAX_TRACKED_ANCHORS: usize = 16_384;

#[derive(Debug, Clone)]
struct TrackedAnchor {
    line: usize,
    hash: String,
    text: String,
    last_used: u64,
}

#[derive(Debug, Default, Clone)]
pub struct FileAnchorTable {
    next_use: u64,
    entries: HashMap<(PathBuf, String), TrackedAnchor>,
}

pub struct AnchorMutation {
    path: PathBuf,
    start_line: usize,
    end_line: usize,
    operation: EditOp,
    replacement_lines: usize,
}

impl FileAnchorTable {
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Record every `line#hash|text` (and `path|line#hash|text`) record that a
    /// model-facing read or search result just displayed. Elided `~` records
    /// are skipped because they carry no text.
    pub fn observe_anchored_content(&mut self, path: &str, content: &str) {
        for line in content.lines() {
            let Some((path, anchor, text)) = parse_anchor_record(line, path) else {
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
            self.next_use = self.next_use.wrapping_add(1);
            let last_used = self.next_use;
            self.entries.insert(
                (PathBuf::from(path), anchor.to_string()),
                TrackedAnchor {
                    line,
                    hash: hash.to_string(),
                    text: text.to_string(),
                    last_used,
                },
            );
        }
        while self.entries.len() > MAX_TRACKED_ANCHORS {
            let Some(key) = self
                .entries
                .iter()
                .min_by_key(|(_, anchor)| anchor.last_used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.entries.remove(&key);
        }
    }

    /// Rewrite tracked anchors in `edits` to their current line numbers.
    ///
    /// An anchor that still points at its observed line is kept. A shifted
    /// anchor is relocated only when its observed line text appears exactly
    /// once in the current file. Otherwise the anchor is left untouched so the
    /// edit fails against the stale line instead of guessing.
    pub fn resolve_tracked_anchors(
        &mut self,
        root: &Path,
        edits: &mut [StructuredEdit],
    ) -> Result<Vec<AnchorMutation>> {
        let mut mutations = Vec::new();
        for edit in edits {
            let path = PathBuf::from(&edit.path);
            let Some(start) = edit.start.as_mut() else {
                continue;
            };
            let Some(mut anchor) = self.entries.get(&(path.clone(), start.clone())).cloned() else {
                continue;
            };
            let current = std::fs::read_to_string(root.join(&path))
                .map_err(|err| miette!("cannot refresh tracked anchor {}: {err}", edit.path))?;
            let lines: Vec<&str> = current.lines().collect();
            let Some(current_line) = relocate_line(&lines, &anchor) else {
                continue;
            };
            self.next_use = self.next_use.wrapping_add(1);
            anchor.last_used = self.next_use;
            self.entries
                .insert((path.clone(), start.clone()), anchor.clone());
            *start = format!("{}#{}", current_line, anchor.hash);
            if let Some(end) = edit.end.as_mut()
                && let Some(end_anchor) = self.entries.get(&(path.clone(), end.clone())).cloned()
                && let Some(end_line) = relocate_line(&lines, &end_anchor)
            {
                *end = format!("{}#{}", end_line, end_anchor.hash);
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
                operation: edit.op.clone().unwrap_or(EditOp::Replace),
                replacement_lines: edit
                    .content
                    .clone()
                    .map(scope_engine::api::EditContent::into_lines)
                    .unwrap_or_default()
                    .len(),
            });
        }
        Ok(mutations)
    }

    /// Update tracked anchors after a successful edit batch.
    ///
    /// All mutations in one batch are evaluated against the original line
    /// numbers, so multi-edit calls cannot shift an anchor twice. Anchors the
    /// edit range itself changed are dropped.
    pub fn apply_anchor_mutations(&mut self, mutations: &[AnchorMutation]) {
        self.entries.retain(|(path, _), anchor| {
            let original = anchor.line;
            let mut delta = 0isize;
            for mutation in mutations {
                if path != &mutation.path {
                    continue;
                }
                match mutation.operation {
                    EditOp::Replace => {
                        if original >= mutation.start_line && original <= mutation.end_line {
                            return false;
                        }
                        if original > mutation.end_line {
                            delta += replacement_delta(mutation);
                        }
                    }
                    EditOp::Append => {
                        if original > mutation.start_line {
                            delta += replacement_delta(mutation);
                        }
                    }
                    EditOp::Prepend => {
                        if original >= mutation.start_line {
                            delta += replacement_delta(mutation);
                        }
                    }
                }
            }
            anchor.line = usize::try_from((original as isize).saturating_add(delta))
                .unwrap_or(1)
                .max(1);
            true
        });
    }
}

fn parse_anchor_record<'a>(
    line: &'a str,
    default_path: &'a str,
) -> Option<(&'a str, &'a str, &'a str)> {
    let (first, rest) = line.split_once('|')?;
    if let Some((anchor, text)) = rest.split_once('|')
        && !first.contains('#')
        && looks_like_anchor(anchor)
    {
        return Some((first, anchor, text));
    }
    looks_like_anchor(first).then_some((default_path, first, rest))
}

fn looks_like_anchor(value: &str) -> bool {
    value
        .split_once('#')
        .is_some_and(|(line, hash)| !hash.is_empty() && line.parse::<u64>().is_ok())
}

fn replacement_delta(mutation: &AnchorMutation) -> isize {
    match mutation.operation {
        EditOp::Replace => {
            mutation.replacement_lines as isize
                - (mutation.end_line - mutation.start_line + 1) as isize
        }
        EditOp::Append | EditOp::Prepend => mutation.replacement_lines as isize,
    }
}

fn relocate_line(lines: &[&str], anchor: &TrackedAnchor) -> Option<usize> {
    if anchor.line > 0 && anchor.line <= lines.len() && lines[anchor.line - 1] == anchor.text {
        return Some(anchor.line);
    }
    let matches = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| **line == anchor.text)
        .map(|(index, _)| index + 1)
        .collect::<Vec<_>>();
    (matches.len() == 1).then_some(matches[0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use scope_engine::api::EditContent;

    fn replace_edit(path: &str, start: &str, end: &str, content: &str) -> StructuredEdit {
        StructuredEdit {
            path: path.to_string(),
            op: Some(EditOp::Replace),
            start: Some(start.to_string()),
            end: Some(end.to_string()),
            content: Some(EditContent::Text(content.to_string())),
        }
    }

    #[test]
    fn observe_records_read_and_search_lines_with_pipes() {
        let mut table = FileAnchorTable::default();
        let read_hash = scope_engine::patch::line_hash("let x = a | b;");
        let search_hash = scope_engine::patch::line_hash("path|text");
        table.observe_anchored_content(
            "src/pipe.rs",
            &format!("3#{read_hash}|let x = a | b;\nsrc/pipe.rs|7#{search_hash}|path|text"),
        );

        assert!(
            table
                .entries
                .contains_key(&(PathBuf::from("src/pipe.rs"), format!("3#{read_hash}")))
        );
        assert!(
            table
                .entries
                .contains_key(&(PathBuf::from("src/pipe.rs"), format!("7#{search_hash}")))
        );
    }

    #[test]
    fn relocation_uses_unique_observed_text_after_shift() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();

        let mut table = FileAnchorTable::default();
        table.observe_anchored_content(
            "notes.txt",
            &format!("2#{}|beta", scope_engine::patch::line_hash("beta")),
        );

        std::fs::write(&path, "alpha\ninserted\nbeta\ngamma\n").unwrap();
        let hash = scope_engine::patch::line_hash("beta");
        let mut edits = vec![replace_edit(
            "notes.txt",
            &format!("2#{hash}"),
            &format!("2#{hash}"),
            "BETA",
        )];

        let mutations = table
            .resolve_tracked_anchors(dir.path(), &mut edits)
            .unwrap();

        assert_eq!(
            edits[0].start.as_deref(),
            Some(format!("3#{hash}").as_str())
        );
        assert_eq!(mutations.len(), 1);
    }

    #[test]
    fn relocation_does_not_guess_ambiguous_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, "beta\nalpha\nbeta\n").unwrap();

        let mut table = FileAnchorTable::default();
        table.observe_anchored_content(
            "notes.txt",
            &format!("2#{}|beta", scope_engine::patch::line_hash("beta")),
        );

        std::fs::write(&path, "beta\nalpha\nbeta\nbeta\n").unwrap();
        let hash = scope_engine::patch::line_hash("beta");
        let mut edits = vec![replace_edit(
            "notes.txt",
            &format!("2#{hash}"),
            &format!("2#{hash}"),
            "BETA",
        )];

        let mutations = table
            .resolve_tracked_anchors(dir.path(), &mut edits)
            .unwrap();

        assert!(mutations.is_empty());
        assert_eq!(
            edits[0].start.as_deref(),
            Some(format!("2#{hash}").as_str())
        );
    }

    #[test]
    fn append_shifts_only_lines_after_the_anchor() {
        let mut table = FileAnchorTable::default();
        table.observe_anchored_content(
            "notes.txt",
            &format!(
                "2#{}|beta\n3#{}|gamma",
                scope_engine::patch::line_hash("beta"),
                scope_engine::patch::line_hash("gamma")
            ),
        );

        table.apply_anchor_mutations(&[AnchorMutation {
            path: PathBuf::from("notes.txt"),
            start_line: 2,
            end_line: 2,
            operation: EditOp::Append,
            replacement_lines: 2,
        }]);

        assert_eq!(
            table
                .entries
                .get(&(
                    PathBuf::from("notes.txt"),
                    format!("2#{}", scope_engine::patch::line_hash("beta"))
                ))
                .map(|anchor| anchor.line),
            Some(2)
        );
        assert_eq!(
            table
                .entries
                .get(&(
                    PathBuf::from("notes.txt"),
                    format!("3#{}", scope_engine::patch::line_hash("gamma"))
                ))
                .map(|anchor| anchor.line),
            Some(5)
        );
    }

    #[test]
    fn multi_edit_batch_shifts_anchors_once() {
        let mut table = FileAnchorTable::default();
        table.observe_anchored_content(
            "notes.txt",
            &format!("40#{}|tail", scope_engine::patch::line_hash("tail")),
        );

        table.apply_anchor_mutations(&[
            AnchorMutation {
                path: PathBuf::from("notes.txt"),
                start_line: 10,
                end_line: 12,
                operation: EditOp::Replace,
                replacement_lines: 7,
            },
            AnchorMutation {
                path: PathBuf::from("notes.txt"),
                start_line: 5,
                end_line: 7,
                operation: EditOp::Replace,
                replacement_lines: 1,
            },
        ]);

        assert_eq!(
            table
                .entries
                .get(&(
                    PathBuf::from("notes.txt"),
                    format!("40#{}", scope_engine::patch::line_hash("tail"))
                ))
                .map(|anchor| anchor.line),
            Some(42)
        );
    }
}
