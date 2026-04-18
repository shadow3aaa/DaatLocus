//! Hindsight retain job preprocessing utilities.
//!
//! This module handles the preprocessing of hindsight retain jobs, including:
//! - Retain job source rendering
//! - Focus extraction
//! - Tag collection and merging
//! - Preprocessed split item construction
//! - Digest building

use crate::{
    context::Context,
    context_budget::{approx_token_count, truncate_text_to_token_budget},
    reasoning::{
        programs::runtime_retain_preprocessor::{
            RuntimeRetainPreprocessorOutput, RuntimeRetainPreprocessorProgram,
        },
        runtime::execute_program_with_ir_report,
        trace::TraceOrigin,
    },
};

use super::{HindsightRetainItem, HindsightRetainJob};

use crate::reasoning::render::openai_tools::OpenAIToolRenderer;
use miette::Result;

/// Maximum token budget for recent messages in hindsight context.
pub const HINDSIGHT_RECENT_MESSAGES_MAX_TOKENS: usize = 160;
/// Minimum number of recent message entries to keep.
pub const HINDSIGHT_RECENT_MESSAGES_MIN_ENTRIES: usize = 2;
/// Maximum token budget for retain preprocessing input.
pub const HINDSIGHT_RETAIN_PREPROCESS_INPUT_MAX_TOKENS: usize = 3200;
/// Maximum token budget for retain preprocessing output.
pub const HINDSIGHT_RETAIN_PREPROCESS_OUTPUT_MAX_TOKENS: usize = 900;
/// Maximum number of split items from preprocessing.
pub const HINDSIGHT_RETAIN_PREPROCESS_MAX_SPLIT_ITEMS: usize = 4;

/// Plan for preprocessed retain jobs.
pub struct RetainPreprocessPlan {
    pub jobs: Vec<HindsightRetainJob>,
    pub skipped_document_ids: Vec<String>,
}

/// Select recent trail lines for hindsight context within token budget.
pub fn select_recent_trail_lines_for_hindsight(context: &Context) -> Vec<String> {
    select_recent_items_by_token_budget(
        context.memory.trail(),
        HINDSIGHT_RECENT_MESSAGES_MAX_TOKENS,
        HINDSIGHT_RECENT_MESSAGES_MIN_ENTRIES,
        |line| approx_token_count(line),
    )
}

/// Select recent items by token budget.
///
/// Returns items that fit within the token budget, ensuring at least `min_items` are kept.
pub fn select_recent_items_by_token_budget<T, F>(
    items: Vec<T>,
    max_tokens: usize,
    min_items: usize,
    mut token_cost: F,
) -> Vec<T>
where
    F: FnMut(&T) -> usize,
{
    let mut selected = Vec::new();
    let mut total_tokens = 0usize;
    for item in items.into_iter().rev() {
        let cost = token_cost(&item);
        let can_fit = total_tokens.saturating_add(cost) <= max_tokens;
        if selected.len() < min_items || can_fit {
            total_tokens = total_tokens.saturating_add(cost);
            selected.push(item);
        } else {
            break;
        }
    }
    selected.reverse();
    selected
}

/// Preprocess a batch of hindsight retain jobs.
pub async fn preprocess_hindsight_retain_jobs(
    context: &Context,
    jobs: Vec<HindsightRetainJob>,
) -> RetainPreprocessPlan {
    use crate::reasoning::runtime::resolve_program_tuning;

    if jobs.is_empty() {
        return RetainPreprocessPlan {
            jobs: Vec::new(),
            skipped_document_ids: Vec::new(),
        };
    }

    let renderer = OpenAIToolRenderer;
    let program = RuntimeRetainPreprocessorProgram;
    let tuning = resolve_program_tuning(context, &program).await;

    let mut retained_jobs = Vec::new();
    let mut skipped_document_ids = Vec::new();

    for job in jobs {
        match preprocess_hindsight_retain_job(context, &renderer, &program, &tuning, job.clone())
            .await
        {
            Ok(Some(job)) => retained_jobs.push(job),
            Ok(None) => {
                if let Some(document_id) = job.document_id.clone() {
                    skipped_document_ids.push(document_id);
                }
            }
            Err(err) => {
                tracing::warn!(
                    "runtime retain preprocessing failed; falling back to raw retain job: {err:?}"
                );
                retained_jobs.push(job);
            }
        }
    }

    RetainPreprocessPlan {
        jobs: retained_jobs,
        skipped_document_ids,
    }
}

/// Preprocess a single hindsight retain job.
pub async fn preprocess_hindsight_retain_job(
    context: &Context,
    renderer: &OpenAIToolRenderer,
    program: &RuntimeRetainPreprocessorProgram,
    tuning: &crate::reasoning::optimizer::PromptTuningConfig<RuntimeRetainPreprocessorOutput>,
    job: HindsightRetainJob,
) -> Result<Option<HindsightRetainJob>> {
    let raw_retain_content = render_retain_job_source(&job);
    let input = truncate_text_to_token_budget(
        &raw_retain_content,
        HINDSIGHT_RETAIN_PREPROCESS_INPUT_MAX_TOKENS,
    );
    let current_doing = extract_retain_focus(&input).unwrap_or_else(|| "未知".to_string());
    let document_id = job
        .document_id
        .clone()
        .or_else(|| job.items.first().and_then(|item| item.document_id.clone()))
        .unwrap_or_else(|| "unknown".to_string());
    let existing_tags = collect_retain_job_tags(&job);
    let ir = program.dataset_ir(
        current_doing,
        document_id.clone(),
        existing_tags.clone(),
        input,
    );
    let outcome = execute_program_with_ir_report(
        context.llm.as_ref(),
        context,
        renderer,
        program,
        ir,
        tuning,
        TraceOrigin::Runtime,
    )
    .await?;
    let output = outcome.output;

    if !output.should_retain {
        tracing::info!(
            "runtime retain preprocessing skipped document_id={} reason={}",
            document_id,
            if output.reason.trim().is_empty() {
                "<none>"
            } else {
                output.reason.trim()
            }
        );
        return Ok(None);
    }

    let tags = merge_tags(existing_tags, output.tags.clone());
    let split_items = build_preprocessed_split_items(&job, &output, &tags);
    if !split_items.is_empty() {
        return Ok(Some(HindsightRetainJob {
            items: split_items,
            document_id: job.document_id,
        }));
    }

    let digest = build_preprocessed_retain_digest(&output);
    let content =
        truncate_text_to_token_budget(&digest, HINDSIGHT_RETAIN_PREPROCESS_OUTPUT_MAX_TOKENS);
    if content.trim().is_empty() {
        tracing::warn!(
            "runtime retain preprocessing produced empty digest; falling back to raw retain document_id={}",
            document_id
        );
        return Ok(Some(job));
    }

    Ok(Some(HindsightRetainJob {
        items: vec![HindsightRetainItem {
            content,
            timestamp: job.items.first().and_then(|item| item.timestamp.clone()),
            context: Some("runtime retain digest".to_string()),
            metadata: job.items.first().and_then(|item| item.metadata.clone()),
            document_id: job
                .items
                .first()
                .and_then(|item| item.document_id.clone())
                .or_else(|| job.document_id.clone()),
            tags: Some(tags),
        }],
        document_id: job.document_id,
    }))
}

/// Render retain job source as a single string.
pub fn render_retain_job_source(job: &HindsightRetainJob) -> String {
    job.items
        .iter()
        .map(|item| item.content.trim())
        .filter(|content| !content.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n---\n\n")
}

/// Extract focus from retain content.
pub fn extract_retain_focus(content: &str) -> Option<String> {
    content
        .lines()
        .find_map(|line| {
            line.strip_prefix("focus: ")
                .map(|value| value.trim().to_string())
        })
        .filter(|value| !value.is_empty())
}

/// Collect tags from a retain job.
pub fn collect_retain_job_tags(job: &HindsightRetainJob) -> Vec<String> {
    let mut tags = Vec::new();
    for item in &job.items {
        if let Some(item_tags) = &item.tags {
            tags.extend(item_tags.iter().cloned());
        }
    }
    tags
}

/// Merge existing tags with extra tags, deduplicating.
pub fn merge_tags(existing: Vec<String>, extra: Vec<String>) -> Vec<String> {
    let mut merged = Vec::new();
    for tag in existing.into_iter().chain(extra) {
        let normalized = tag.trim();
        if normalized.is_empty() || merged.iter().any(|current| current == normalized) {
            continue;
        }
        merged.push(normalized.to_string());
    }
    merged
}

/// Build preprocessed split items from retain job and preprocessor output.
pub fn build_preprocessed_split_items(
    job: &HindsightRetainJob,
    output: &RuntimeRetainPreprocessorOutput,
    shared_tags: &[String],
) -> Vec<HindsightRetainItem> {
    output
        .split_items
        .iter()
        .filter_map(|item| {
            let mut content = String::new();
            if !item.title.trim().is_empty() {
                content.push_str("topic: ");
                content.push_str(item.title.trim());
                content.push('\n');
            }
            content.push_str(item.content.trim());
            let content = truncate_text_to_token_budget(
                &content,
                HINDSIGHT_RETAIN_PREPROCESS_OUTPUT_MAX_TOKENS,
            );
            if content.trim().is_empty() {
                return None;
            }
            let tags = merge_tags(shared_tags.to_vec(), item.tags.clone());
            let base_document_id = job
                .items
                .first()
                .and_then(|source| source.document_id.clone())
                .or_else(|| job.document_id.clone())
                .unwrap_or_else(|| "runtime-retain".to_string());
            Some(HindsightRetainItem {
                content,
                timestamp: job
                    .items
                    .first()
                    .and_then(|source| source.timestamp.clone()),
                context: Some(if item.context.trim().is_empty() {
                    "runtime retain digest split".to_string()
                } else {
                    item.context.trim().to_string()
                }),
                metadata: job.items.first().and_then(|source| source.metadata.clone()),
                document_id: Some(format!(
                    "{}:part:{}",
                    base_document_id,
                    item.title
                        .trim()
                        .replace(char::is_whitespace, "-")
                        .to_ascii_lowercase()
                )),
                tags: Some(tags),
            })
        })
        .take(HINDSIGHT_RETAIN_PREPROCESS_MAX_SPLIT_ITEMS)
        .collect()
}

/// Build preprocessed retain digest from preprocessor output.
pub fn build_preprocessed_retain_digest(output: &RuntimeRetainPreprocessorOutput) -> String {
    let mut lines = Vec::new();
    if !output.summary.trim().is_empty() {
        lines.push(format!("summary: {}", output.summary.trim()));
    }
    append_digest_section(&mut lines, "facts", &output.facts);
    append_digest_section(&mut lines, "preferences", &output.preferences);
    append_digest_section(&mut lines, "failures", &output.failures);
    append_digest_section(&mut lines, "lessons", &output.lessons);
    if !output.reason.trim().is_empty() {
        lines.push(format!("retain_reason: {}", output.reason.trim()));
    }
    lines.join("\n")
}

/// Append a digest section to lines.
fn append_digest_section(lines: &mut Vec<String>, title: &str, items: &[String]) {
    let entries = items
        .iter()
        .map(|item| item.trim())
        .filter(|item| !item.is_empty())
        .collect::<Vec<_>>();
    if entries.is_empty() {
        return;
    }
    lines.push(format!("{title}:"));
    lines.extend(entries.into_iter().map(|item| format!("- {item}")));
}
