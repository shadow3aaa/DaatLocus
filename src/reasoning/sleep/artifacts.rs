use std::collections::HashSet;

use crate::reasoning::evaluation_artifacts::EvaluationArtifactRuntimePromptCandidate;

pub(super) fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }
    slug.trim_matches('-').to_string()
}

pub(super) fn dedupe_vec(items: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for item in items {
        let trimmed = item.trim().to_string();
        if trimmed.is_empty() || !seen.insert(trimmed.clone()) {
            continue;
        }
        deduped.push(trimmed);
    }
    deduped
}

pub(super) fn dedupe_prompt_candidates(
    candidates: Vec<EvaluationArtifactRuntimePromptCandidate>,
) -> Vec<EvaluationArtifactRuntimePromptCandidate> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for candidate in candidates {
        if !seen.insert(candidate.title.clone()) {
            continue;
        }
        deduped.push(candidate);
    }
    deduped
}
