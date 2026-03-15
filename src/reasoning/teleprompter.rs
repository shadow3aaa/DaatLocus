use crate::reasoning::{
    examples::ProgramExample,
    optimizer::{CandidateConfig, PromptTuningConfig},
};

pub fn build_teleprompter_candidates<O: Clone>(
    base: &PromptTuningConfig<O>,
    instruction_candidate_name: &str,
    instructions: &[&str],
) -> Vec<CandidateConfig<O>> {
    let mut candidates = Vec::new();
    let merged_instructions = merge_instructions(base, instructions);

    if !merged_instructions.is_empty() {
        candidates.push(CandidateConfig {
            name: instruction_candidate_name.to_string(),
            config: PromptTuningConfig {
                extra_instructions: merged_instructions.clone(),
                examples: base.examples.clone(),
            },
        });
    }

    candidates
}

pub fn build_bootstrap_demo_candidates<O: Clone>(
    base: &PromptTuningConfig<O>,
    demos_candidate_name: &str,
    combo_candidate_name: &str,
    instructions: &[&str],
    bootstrap_examples: Vec<ProgramExample<O>>,
) -> Vec<CandidateConfig<O>> {
    let mut candidates = Vec::new();
    let merged_instructions = merge_instructions(base, instructions);

    if !bootstrap_examples.is_empty() {
        candidates.push(CandidateConfig {
            name: demos_candidate_name.to_string(),
            config: PromptTuningConfig {
                extra_instructions: base.extra_instructions.clone(),
                examples: bootstrap_examples.clone(),
            },
        });

        if !merged_instructions.is_empty() {
            candidates.push(CandidateConfig {
                name: combo_candidate_name.to_string(),
                config: PromptTuningConfig {
                    extra_instructions: merged_instructions,
                    examples: bootstrap_examples,
                },
            });
        }
    }

    candidates
}

fn merge_instructions<O>(base: &PromptTuningConfig<O>, additions: &[&str]) -> Vec<String> {
    let mut merged = base.extra_instructions.clone();
    for addition in additions {
        let addition = addition.to_string();
        if !merged.iter().any(|existing| existing == &addition) {
            merged.push(addition);
        }
    }
    merged
}
