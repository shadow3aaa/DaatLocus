use crate::api::{PropagationResult, PropagationSource, Reference, ReviewEvent};
use std::collections::HashSet;

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
