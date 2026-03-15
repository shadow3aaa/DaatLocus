use std::collections::BTreeMap;

use crate::reasoning::{
    bench::datasets::continuity_guard as continuity_guard_dataset,
    eval::EvalCaseResult,
    examples::ProgramExample,
    optimizer::{CandidateConfig, PromptTuningConfig},
};

use super::programs::{continuity_guard::ContinuityGuardOutput, memory_recall::MemoryRecallOutput};

pub fn propose_memory_recall_candidates(
    base: &PromptTuningConfig<MemoryRecallOutput>,
    baseline_results: &[EvalCaseResult],
) -> Vec<CandidateConfig<MemoryRecallOutput>> {
    let mut proposals = BTreeMap::<String, Vec<String>>::new();

    for failure in baseline_results.iter().filter(|result| !result.passed) {
        if failure.case_name.contains("blocker")
            || failure.detail.contains("gh auth login")
            || failure
                .detail
                .contains("expected relevant_memory_ids to contain M")
        {
            proposals
                .entry("auto_blocker_continuity".to_string())
                .or_default()
                .push("如果当前关键事实是阻塞，至少同时保留三类记忆：阻塞事件本身、阻塞原因、仍可继续推进该项目的替代路径或后续线索。".to_string());
        }

        if failure.case_name.contains("small_talk")
            || failure.case_name.contains("wait_noise")
            || failure.detail.contains("forbidden")
        {
            proposals
                .entry("auto_noise_suppression".to_string())
                .or_default()
                .push("纯等待、寒暄和与当前问题无关的聊天只算噪声；除非它们直接改变项目状态，否则不要把它们选进关键记忆。".to_string());
        }

        if failure
            .detail
            .contains("expected relevant_memory_ids to contain M")
        {
            proposals
                .entry("auto_supporting_recall".to_string())
                .or_default()
                .push("如果你已经选中了事件性记忆(T*)，还要补上支撑它的联想回忆(M*)，尤其是能解释后续推进路径的那条。".to_string());
        }
    }

    proposals
        .into_iter()
        .map(|(name, instructions)| CandidateConfig {
            name,
            config: PromptTuningConfig {
                extra_instructions: dedupe_instructions(base, instructions),
                examples: base.examples.clone(),
            },
        })
        .collect()
}

pub fn propose_continuity_guard_candidates(
    base: &PromptTuningConfig<ContinuityGuardOutput>,
    baseline_results: &[EvalCaseResult],
) -> Vec<CandidateConfig<ContinuityGuardOutput>> {
    let mut proposals = BTreeMap::<String, Vec<String>>::new();
    let mut bootstrap_case_names = Vec::<&str>::new();

    for failure in baseline_results.iter().filter(|result| !result.passed) {
        if failure.case_name.contains("small_talk")
            || failure.case_name.contains("commitment")
            || failure
                .detail
                .contains("expected should_continue_project=true")
        {
            proposals
                .entry("auto_commitment_guard".to_string())
                .or_default()
                .push("如果输入里出现 owner 承诺、活跃项目或明确未完成调查，近期寒暄和等待噪声不应改变主目标。".to_string());
            bootstrap_case_names.push(failure.case_name);
        }

        if failure.case_name.contains("blocker")
            || failure.detail.contains("gh auth login")
            || failure.detail.contains("非交互")
        {
            proposals
                .entry("auto_blocker_guard".to_string())
                .or_default()
                .push("阻塞不等于换项目；如果当前问题是阻塞，应继续原项目，并把阻塞与替代推进方式一起说清楚。".to_string());
            bootstrap_case_names.push(failure.case_name);
        }

        if failure.case_name.contains("no_project")
            || failure
                .detail
                .contains("expected should_continue_project=false")
        {
            proposals
                .entry("auto_no_forced_continuity".to_string())
                .or_default()
                .push(
                    "如果没有活跃项目、长期承诺或未完成调查，不要因为等待和轻量聊天而虚构连续性。"
                        .to_string(),
                );
            bootstrap_case_names.push(failure.case_name);
        }
    }

    let mut candidates = proposals
        .into_iter()
        .map(|(name, instructions)| CandidateConfig {
            name,
            config: PromptTuningConfig {
                extra_instructions: dedupe_instructions(base, instructions),
                examples: base.examples.clone(),
            },
        })
        .collect::<Vec<_>>();

    let bootstrap_examples = continuity_guard_dataset::bootstrap_examples(&bootstrap_case_names);
    if !bootstrap_examples.is_empty() {
        candidates.push(CandidateConfig {
            name: "auto_bootstrap_demo".to_string(),
            config: PromptTuningConfig {
                extra_instructions: base.extra_instructions.clone(),
                examples: append_examples(&base.examples, &bootstrap_examples),
            },
        });

        let mut combo_instructions = Vec::new();
        for candidate in &candidates {
            if candidate.name.starts_with("auto_") && candidate.name != "auto_bootstrap_demo" {
                combo_instructions.extend(candidate.config.extra_instructions.clone());
            }
        }
        candidates.push(CandidateConfig {
            name: "auto_bootstrap_combo".to_string(),
            config: PromptTuningConfig {
                extra_instructions: dedupe_instructions(base, combo_instructions),
                examples: append_examples(&base.examples, &bootstrap_examples),
            },
        });
    }

    candidates
}

fn dedupe_instructions<O>(
    base: &PromptTuningConfig<O>,
    new_instructions: Vec<String>,
) -> Vec<String> {
    let mut combined = base.extra_instructions.clone();
    for instruction in new_instructions {
        if !combined.iter().any(|existing| existing == &instruction) {
            combined.push(instruction);
        }
    }
    combined
}

fn append_examples<O: Clone>(
    base_examples: &[ProgramExample<O>],
    extra_examples: &[ProgramExample<O>],
) -> Vec<ProgramExample<O>> {
    let mut examples = base_examples.to_vec();
    examples.extend_from_slice(extra_examples);
    examples
}
