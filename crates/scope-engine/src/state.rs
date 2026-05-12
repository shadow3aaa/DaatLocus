use crate::api::{PropagationResult, PropagationSource, ReviewEvent};
use std::collections::HashSet;

pub struct PropagationState {
    pending: Vec<PropagationResult>,
    seen: HashSet<String>,
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

    pub fn next_review(&mut self) -> Option<ReviewEvent> {
        let r = self.pending.pop()?;
        let suggested_action = match &r.source {
            PropagationSource::Lsp => "read_and_verify".to_string(),
            PropagationSource::OpenEnded => "investigate_impact".to_string(),
        };
        Some(ReviewEvent {
            selector: r.selector,
            reason: r.reason,
            suggested_action,
            source: r.source,
        })
    }
}
