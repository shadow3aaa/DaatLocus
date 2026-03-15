use std::collections::{BTreeMap, BTreeSet};

use crate::reasoning::{
    eval::EvalCaseResult,
    examples::ProgramExample,
    optimizer::{CandidateConfig, PromptTuningConfig},
};

pub struct ProposalSpec<O> {
    pub candidate_name: &'static str,
    pub when: fn(&EvalCaseResult) -> bool,
    pub instruction: &'static str,
    pub bootstrap_case_name: Option<&'static str>,
    pub bootstrap_examples: fn(&[&str]) -> Vec<ProgramExample<O>>,
}

pub fn propose_candidates<O: Clone>(
    base: &PromptTuningConfig<O>,
    baseline_results: &[EvalCaseResult],
    specs: &[ProposalSpec<O>],
) -> Vec<CandidateConfig<O>> {
    let mut instructions_by_candidate = BTreeMap::<String, Vec<String>>::new();
    let mut bootstrap_case_names = BTreeSet::<&str>::new();
    let bootstrap_loader = specs.first().map(|spec| spec.bootstrap_examples);

    for failure in baseline_results.iter().filter(|result| !result.passed) {
        for spec in specs.iter().filter(|spec| (spec.when)(failure)) {
            instructions_by_candidate
                .entry(spec.candidate_name.to_string())
                .or_default()
                .push(spec.instruction.to_string());
            if let Some(case_name) = spec.bootstrap_case_name {
                bootstrap_case_names.insert(case_name);
            }
        }
    }

    let mut candidates = instructions_by_candidate
        .into_iter()
        .map(|(name, instructions)| CandidateConfig {
            name,
            config: PromptTuningConfig {
                extra_instructions: dedupe_instructions(base, instructions),
                examples: base.examples.clone(),
            },
        })
        .collect::<Vec<_>>();

    if let Some(loader) = bootstrap_loader {
        let bootstrap_case_names = bootstrap_case_names.into_iter().collect::<Vec<_>>();
        let bootstrap_examples = loader(&bootstrap_case_names);
        if !bootstrap_examples.is_empty() {
            candidates.push(CandidateConfig {
                name: "auto_bootstrap_demo".to_string(),
                config: PromptTuningConfig {
                    extra_instructions: base.extra_instructions.clone(),
                    examples: bootstrap_examples.clone(),
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
                    examples: bootstrap_examples,
                },
            });
        }
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
