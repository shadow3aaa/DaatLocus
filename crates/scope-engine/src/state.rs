use crate::api::{PropagationResult, PropagationSource, Reference, ReviewEvent, SearchTarget};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadHandleTarget {
    pub label: String,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
}

impl ReadHandleTarget {
    pub fn new(
        label: impl Into<String>,
        path: impl Into<String>,
        start_line: usize,
        end_line: usize,
    ) -> Self {
        Self {
            label: label.into(),
            path: path.into(),
            start_line,
            end_line,
        }
    }
}

#[derive(Default)]
pub struct ReadHandleRegistry {
    by_handle: HashMap<String, ReadHandleTarget>,
    by_label: HashMap<String, String>,
}

impl ReadHandleRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.by_handle.clear();
        self.by_label.clear();
    }

    pub fn intern(&mut self, target: ReadHandleTarget) -> Result<SearchTarget, String> {
        if let Some(handle) = self.by_label.get(&target.label) {
            return Ok(SearchTarget {
                handle: handle.clone(),
                label: target.label,
            });
        }

        let handle = format!("{}#{}", target.start_line, target_hash4(&target.label));
        if let Some(existing) = self.by_handle.get(&handle)
            && existing != &target
        {
            return Err(format!(
                "read handle collision for {handle}: `{}` and `{}`",
                existing.label, target.label
            ));
        }

        self.by_handle.insert(handle.clone(), target.clone());
        self.by_label.insert(target.label.clone(), handle.clone());
        Ok(SearchTarget {
            handle,
            label: target.label,
        })
    }

    pub fn resolve(&self, handle: &str) -> Option<&ReadHandleTarget> {
        self.by_handle.get(handle)
    }
}

fn target_hash4(label: &str) -> String {
    const ALPHABET: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

    let mut hasher = Sha256::new();
    hasher.update(label.as_bytes());
    let digest = hasher.finalize();
    let mut value = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) as usize;
    let mut chars = [b'0'; 4];
    for slot in chars.iter_mut().rev() {
        *slot = ALPHABET[value % ALPHABET.len()];
        value /= ALPHABET.len();
    }
    String::from_utf8(chars.to_vec()).expect("base62 hash should be utf8")
}

pub struct PropagationState {
    pending: Vec<PropagationResult>,
    seen: HashSet<String>,
}

impl Default for PropagationState {
    fn default() -> Self {
        Self::new()
    }
}

impl PropagationState {
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
            seen: HashSet::new(),
        }
    }

    pub fn accumulate(&mut self, results: Vec<PropagationResult>) {
        for r in results {
            if self.seen.insert(r.selector.clone()) {
                self.pending.push(r);
            }
        }
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub fn next_review(&mut self) -> Option<ReviewEvent> {
        let r = self.pending.pop()?;
        self.seen.remove(&r.selector);
        match &r.source {
            PropagationSource::Lsp => {
                // LSP found precise references — build KnownReferences event
                let references: Vec<Reference> = r
                    .lsp_references
                    .clone()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(selector, line, context)| Reference {
                        selector,
                        line,
                        context,
                    })
                    .collect();
                Some(ReviewEvent::KnownReferences {
                    modified_symbol: r.selector,
                    change_summary: r.reason,
                    references,
                    file_snippet: r.file_snippet.clone().unwrap_or_default(),
                })
            }
            PropagationSource::OpenEnded => {
                // No LSP — build InvestigateImpact event
                Some(ReviewEvent::InvestigateImpact {
                    modified_symbol: r.selector,
                    change_summary: r.reason,
                    diff_summary: r.diff_summary.clone().unwrap_or_default(),
                    file_snippet: r.file_snippet.clone().unwrap_or_default(),
                    project_files: r.project_files.clone().unwrap_or_default(),
                })
            }
        }
    }

    pub fn next_reviews(&mut self, limit: usize) -> Vec<ReviewEvent> {
        (0..limit).filter_map(|_| self.next_review()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lsp_result(selector: &str, reason: &str) -> PropagationResult {
        PropagationResult {
            selector: selector.to_string(),
            reason: reason.to_string(),
            source: PropagationSource::Lsp,
            lsp_references: Some(vec![]),
            diff_summary: None,
            file_snippet: None,
            project_files: None,
        }
    }

    fn open_result(selector: &str, reason: &str) -> PropagationResult {
        PropagationResult {
            selector: selector.to_string(),
            reason: reason.to_string(),
            source: PropagationSource::OpenEnded,
            lsp_references: None,
            diff_summary: Some("test diff".to_string()),
            file_snippet: Some("fn foo() {}".to_string()),
            project_files: Some(vec!["src/lib.rs".to_string()]),
        }
    }

    #[test]
    fn read_handle_registry_generates_stable_start_line_handles() {
        let mut registry = ReadHandleRegistry::new();
        let target = ReadHandleTarget::new(
            "src/dashboard/mod.rs::fn run_tui_dashboard #L1268-L1320",
            "src/dashboard/mod.rs",
            1268,
            1320,
        );

        let first = registry.intern(target.clone()).unwrap();
        let second = registry.intern(target).unwrap();

        assert_eq!(first, second);
        assert!(first.handle.starts_with("1268#"));
        assert_eq!(first.handle.len(), "1268#".len() + 4);
        assert_eq!(
            registry.resolve(&first.handle).unwrap().label,
            "src/dashboard/mod.rs::fn run_tui_dashboard #L1268-L1320"
        );
    }

    #[test]
    fn accumulate_deduplicates_selectors() {
        let mut state = PropagationState::new();
        state.accumulate(vec![
            lsp_result("src/a.rs::fn foo", "modified"),
            lsp_result("src/a.rs::fn foo", "modified again"),
        ]);
        assert_eq!(state.pending.len(), 1);
    }

    #[test]
    fn accumulate_keeps_distinct_selectors() {
        let mut state = PropagationState::new();
        state.accumulate(vec![
            lsp_result("src/a.rs::fn foo", "modified"),
            lsp_result("src/b.rs::fn bar", "modified"),
        ]);
        assert_eq!(state.pending.len(), 2);
    }

    #[test]
    fn next_review_lsp_produces_known_references() {
        let mut state = PropagationState::new();
        let mut r = lsp_result("src/a.rs::fn foo", "referenced");
        r.lsp_references = Some(vec![(
            "src/b.rs::fn bar".to_string(),
            10,
            "foo();".to_string(),
        )]);
        state.accumulate(vec![r]);
        let event = state.next_review().unwrap();
        match event {
            ReviewEvent::KnownReferences {
                modified_symbol,
                references,
                ..
            } => {
                assert_eq!(modified_symbol, "src/a.rs::fn foo");
                assert_eq!(references.len(), 1);
                assert_eq!(references[0].selector, "src/b.rs::fn bar");
            }
            _ => panic!("Expected KnownReferences variant"),
        }
    }

    #[test]
    fn next_review_open_ended_produces_investigate_impact() {
        let mut state = PropagationState::new();
        state.accumulate(vec![open_result("src/a.rs::fn foo", "modified")]);
        let event = state.next_review().unwrap();
        match event {
            ReviewEvent::InvestigateImpact {
                modified_symbol,
                diff_summary,
                project_files,
                ..
            } => {
                assert_eq!(modified_symbol, "src/a.rs::fn foo");
                assert_eq!(diff_summary, "test diff");
                assert_eq!(project_files.len(), 1);
            }
            _ => panic!("Expected InvestigateImpact variant"),
        }
    }

    #[test]
    fn next_review_returns_none_when_empty() {
        let mut state = PropagationState::new();
        assert!(state.next_review().is_none());
    }

    #[test]
    fn next_reviews_respects_limit_and_reports_remaining() {
        let mut state = PropagationState::new();
        state.accumulate(vec![
            lsp_result("src/a.rs::fn foo", "first"),
            lsp_result("src/b.rs::fn bar", "second"),
            lsp_result("src/c.rs::fn baz", "third"),
        ]);

        let events = state.next_reviews(2);

        assert_eq!(events.len(), 2);
        assert_eq!(state.pending_count(), 1);
        match &events[0] {
            ReviewEvent::KnownReferences {
                modified_symbol, ..
            } => assert_eq!(modified_symbol, "src/c.rs::fn baz"),
            _ => panic!("Expected KnownReferences variant"),
        }
        match &events[1] {
            ReviewEvent::KnownReferences {
                modified_symbol, ..
            } => assert_eq!(modified_symbol, "src/b.rs::fn bar"),
            _ => panic!("Expected KnownReferences variant"),
        }
    }

    #[test]
    fn next_review_pops_in_lifo_order() {
        let mut state = PropagationState::new();
        state.accumulate(vec![
            lsp_result("src/a.rs::fn foo", "first"),
            lsp_result("src/b.rs::fn bar", "second"),
        ]);
        let e1 = state.next_review().unwrap();
        match e1 {
            ReviewEvent::KnownReferences {
                modified_symbol, ..
            } => assert_eq!(modified_symbol, "src/b.rs::fn bar"),
            _ => panic!(),
        };
        let e2 = state.next_review().unwrap();
        match e2 {
            ReviewEvent::KnownReferences {
                modified_symbol, ..
            } => assert_eq!(modified_symbol, "src/a.rs::fn foo"),
            _ => panic!(),
        };
    }

    #[test]
    fn acknowledged_selector_can_be_queued_again() {
        let mut state = PropagationState::new();

        state.accumulate(vec![lsp_result("src/a.rs::fn foo", "first")]);
        assert_eq!(state.pending_count(), 1);
        let first = state.next_review().unwrap();
        match first {
            ReviewEvent::KnownReferences {
                modified_symbol,
                change_summary,
                ..
            } => {
                assert_eq!(modified_symbol, "src/a.rs::fn foo");
                assert_eq!(change_summary, "first");
            }
            _ => panic!("Expected KnownReferences variant"),
        }

        state.accumulate(vec![lsp_result("src/a.rs::fn foo", "second")]);
        assert_eq!(state.pending_count(), 1);
        let second = state.next_review().unwrap();
        match second {
            ReviewEvent::KnownReferences {
                modified_symbol,
                change_summary,
                ..
            } => {
                assert_eq!(modified_symbol, "src/a.rs::fn foo");
                assert_eq!(change_summary, "second");
            }
            _ => panic!("Expected KnownReferences variant"),
        }
    }

    #[test]
    fn mixed_sources_generate_correct_variants() {
        let mut state = PropagationState::new();
        state.accumulate(vec![
            lsp_result("src/a.rs::fn foo", "lsp ref"),
            open_result("src/b.rs::fn bar", "open ref"),
        ]);
        assert_eq!(state.pending.len(), 2);
        // LIFO: first pop is the last pushed (OpenEnded)
        let e1 = state.next_review().unwrap();
        assert!(matches!(e1, ReviewEvent::InvestigateImpact { .. }));
        let e2 = state.next_review().unwrap();
        assert!(matches!(e2, ReviewEvent::KnownReferences { .. }));
    }
}
