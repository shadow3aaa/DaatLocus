//! Bound model-visible tool output with continuations to its resource ledger.

use crate::context_budget::{APPROX_BYTES_PER_TOKEN, utf8_prefix};

const PLAIN_TRUNCATION_NOTICE: &str = "[tool output too long; model content truncated]";

#[derive(Clone, Debug)]
pub enum OverflowTarget {
    Continue { instruction: String },
}

pub fn bound_tool_model_content(
    text: &str,
    max_tokens: usize,
    target: Option<OverflowTarget>,
) -> String {
    let max_chars = max_tokens.max(1).saturating_mul(APPROX_BYTES_PER_TOKEN);
    if text.len() <= max_chars {
        return text.to_owned();
    }
    let total_chars = text.chars().count();
    let bounded = match target {
        None => append_plain_truncation(utf8_prefix(text, max_chars), max_chars, total_chars),
        Some(OverflowTarget::Continue { instruction }) => {
            let marker = format!(
                "[tool output truncated: showing part of {} chars]\n{instruction}",
                total_chars
            );
            let kept = utf8_prefix(text, max_chars.saturating_sub(marker.len() + 1));
            format!("{kept}\n{marker}")
        }
    };
    utf8_prefix(&bounded, max_chars).to_owned()
}

fn append_plain_truncation(kept: &str, max_chars: usize, total_chars: usize) -> String {
    let marker = format!(
        "{PLAIN_TRUNCATION_NOTICE} ({} chars omitted)",
        total_chars.saturating_sub(kept.chars().count())
    );
    let head = utf8_prefix(kept, max_chars.saturating_sub(marker.len() + 1));
    let marker = format!(
        "{PLAIN_TRUNCATION_NOTICE} ({} chars omitted)",
        total_chars.saturating_sub(head.chars().count())
    );
    utf8_prefix(&format!("{head}\n{marker}"), max_chars).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multilingual_tool_observations_are_bounded_in_bytes() {
        let text = "中文🙂".repeat(1000);
        let bounded = bound_tool_model_content(&text, 100, None);
        assert!(crate::context_budget::approx_token_count(&bounded) <= 100);
        assert!(bounded.contains(PLAIN_TRUNCATION_NOTICE));
    }

    #[test]
    fn content_within_budget_is_untouched() {
        let text = "short output";
        assert_eq!(
            bound_tool_model_content(text, 2_000, None),
            text,
            "content inside the budget must not be rewritten"
        );
    }

    #[test]
    fn oversized_content_without_spill_degrades_to_prefix_truncation() {
        let text = "a".repeat(50_000);
        let bounded = bound_tool_model_content(&text, 100, None);

        assert!(bounded.contains(PLAIN_TRUNCATION_NOTICE));
        assert!(bounded.starts_with('a'));
        assert!(
            bounded.len() <= 400,
            "100-token output must fit its byte budget, including the notice"
        );
    }

    #[test]
    fn continue_target_points_back_at_the_source_instead_of_copying_it() {
        let body = "42#ab|some source line\n".repeat(4_000);
        let bounded = bound_tool_model_content(
            &body,
            200,
            Some(OverflowTarget::Continue {
                instruction: "Source file: src/lib.rs. Start line: 2001.".to_string(),
            }),
        );

        assert!(
            bounded.contains("src/lib.rs") && bounded.contains("2001"),
            "marker must retain the source locator"
        );
        assert!(bounded.len() <= 800);
    }

    #[test]
    fn spill_is_left_readable_but_not_model_writable() {
        let temp = tempfile::tempdir().expect("temp dir");
        let pool = temp.path().join("pool");
        std::fs::create_dir_all(&pool).expect("create pool");

        let mut policy = crate::sandbox::RuntimeSandboxPolicy::disabled();
        policy.filesystem.deny_write_paths.push(pool.clone());

        let inside = pool.join("spill.txt");
        assert!(
            policy.is_path_readable(&inside),
            "the model must be able to read_file its own spilled output"
        );
        assert!(
            !policy.is_path_writable(&inside),
            "the model must not be able to overwrite a spill a pending result points at"
        );
    }
}
