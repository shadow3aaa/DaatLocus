use miette::{Result, miette};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::programs::{
    continuity_guard::ContinuityGuardProgram, memory_recall::MemoryRecallProgram,
};
use crate::{
    config::Config,
    context::Context,
    reasoning::{
        bench::datasets,
        compiled::{
            BENCH_COMPILED_DIR_NAME, CompiledProgram, StoredPromptTuningConfig,
            load_compiled_program_from_dir, save_compiled_program_to_dir,
        },
        eval::{EvalCase, run_suite_with_tuning},
        ir::PromptIR,
        optimizer::{CandidateConfig, OptimizationResult, PromptTuningConfig},
        program::Program,
        proposer::{ProposalSpec, propose_candidates},
        render::openai_tools::OpenAIToolRenderer,
        signature::Signature,
        trace::TraceOrigin,
    },
};

const BENCH_OPTIMIZER_VERSION: &str = "reasoning-bench-optimizer-v3";
const RENDERER_NAME: &str = "openai_tools";

pub async fn run_bench_optimize_memory(context: &Context) -> Result<Vec<OptimizationResult>> {
    let compiled = ensure_bench_memory_compiled(context).await?;
    Ok(vec![OptimizationResult {
        suite: compiled.suite,
        best_candidate: compiled.best_candidate,
        score: compiled.score,
        total_cases: compiled.total_cases,
    }])
}

pub async fn run_bench_optimize_continuity(context: &Context) -> Result<Vec<OptimizationResult>> {
    let compiled = ensure_bench_continuity_compiled(context).await?;
    Ok(vec![OptimizationResult {
        suite: compiled.suite,
        best_candidate: compiled.best_candidate,
        score: compiled.score,
        total_cases: compiled.total_cases,
    }])
}

pub async fn load_or_compile_bench_memory_tuning(
    context: &Context,
) -> Result<PromptTuningConfig<crate::reasoning::bench::programs::memory_recall::MemoryRecallOutput>>
{
    let compiled = ensure_bench_memory_compiled(context).await?;
    compiled.tuning.to_typed()
}

pub async fn load_or_compile_bench_continuity_tuning(
    context: &Context,
) -> Result<
    PromptTuningConfig<crate::reasoning::bench::programs::continuity_guard::ContinuityGuardOutput>,
> {
    let compiled = ensure_bench_continuity_compiled(context).await?;
    compiled.tuning.to_typed()
}

async fn ensure_bench_memory_compiled(context: &Context) -> Result<CompiledProgram> {
    let renderer = OpenAIToolRenderer;
    let program = MemoryRecallProgram;
    let base = program.default_tuning();
    let train_cases = program.train_eval_cases();
    let dev_cases = program.dev_eval_cases();
    let baseline_results = run_suite_with_tuning(
        context,
        &renderer,
        &program,
        program.suite_name(),
        clone_eval_cases(&train_cases),
        &base,
        TraceOrigin::BenchCompile,
    )
    .await;
    let mut candidates = vec![
        CandidateConfig {
            name: "baseline".to_string(),
            config: base.clone(),
        },
        CandidateConfig {
            name: "minimal_examples".to_string(),
            config: PromptTuningConfig {
                extra_instructions: base.extra_instructions.clone(),
                examples: base.examples.iter().take(1).cloned().collect(),
            },
        },
        CandidateConfig {
            name: "continuity_bias".to_string(),
            config: PromptTuningConfig {
                extra_instructions: vec![
                    "优先保留长期承诺、项目连续性和明确阻塞信息；把纯等待和寒暄降为噪声。"
                        .to_string(),
                ],
                examples: base.examples.clone(),
            },
        },
        CandidateConfig {
            name: "id_first_bias".to_string(),
            config: PromptTuningConfig {
                extra_instructions: vec![
                    "先挑出最相关的记忆 id，再用这些 id 组织简洁结论。".to_string(),
                ],
                examples: base.examples.clone(),
            },
        },
    ];
    let proposal_specs = [
        ProposalSpec {
            candidate_name: "auto_blocker_continuity",
            when: memory_recall_blocker_failure,
            instruction: "如果当前关键事实是阻塞，至少同时保留三类记忆：阻塞事件本身、阻塞原因、仍可继续推进该项目的替代路径或后续线索。",
            bootstrap_case_name: Some("remember_blocker_not_idle_waits"),
            bootstrap_examples: datasets::memory_recall::bootstrap_examples,
        },
        ProposalSpec {
            candidate_name: "auto_noise_suppression",
            when: memory_recall_noise_failure,
            instruction: "纯等待、寒暄和与当前问题无关的聊天只算噪声；除非它们直接改变项目状态，否则不要把它们选进关键记忆。",
            bootstrap_case_name: Some("prefer_owner_reply_over_small_talk"),
            bootstrap_examples: datasets::memory_recall::bootstrap_examples,
        },
        ProposalSpec {
            candidate_name: "auto_supporting_recall",
            when: memory_recall_supporting_failure,
            instruction: "如果你已经选中了事件性记忆(T*)，还要补上支撑它的联想回忆(M*)，尤其是能解释后续推进路径的那条。",
            bootstrap_case_name: Some("remember_blocker_not_idle_waits"),
            bootstrap_examples: datasets::memory_recall::bootstrap_examples,
        },
    ];
    candidates.extend(propose_candidates(
        &base,
        &baseline_results,
        &proposal_specs,
    ));
    ensure_suite_compiled(
        context,
        &renderer,
        &program,
        program.suite_name(),
        dev_cases,
        candidates,
    )
    .await
}

async fn ensure_bench_continuity_compiled(context: &Context) -> Result<CompiledProgram> {
    let renderer = OpenAIToolRenderer;
    let program = ContinuityGuardProgram;
    let base = program.default_tuning();
    let train_cases = program.train_eval_cases();
    let dev_cases = program.dev_eval_cases();
    let baseline_results = run_suite_with_tuning(
        context,
        &renderer,
        &program,
        program.suite_name(),
        clone_eval_cases(&train_cases),
        &base,
        TraceOrigin::BenchCompile,
    )
    .await;
    let mut candidates = vec![
        CandidateConfig {
            name: "baseline".to_string(),
            config: base.clone(),
        },
        CandidateConfig {
            name: "minimal_examples".to_string(),
            config: PromptTuningConfig {
                extra_instructions: base.extra_instructions.clone(),
                examples: base.examples.iter().take(1).cloned().collect(),
            },
        },
        CandidateConfig {
            name: "commitment_bias".to_string(),
            config: PromptTuningConfig {
                extra_instructions: vec![
                    "如果输入里出现明确承诺、活跃项目或未完成调查，应优先维持项目连续性。"
                        .to_string(),
                ],
                examples: base.examples.clone(),
            },
        },
        CandidateConfig {
            name: "blocker_bias".to_string(),
            config: PromptTuningConfig {
                extra_instructions: vec![
                    "如果当前真正的问题是阻塞信息，应继续原项目并指出阻塞，而不是切换目标。"
                        .to_string(),
                ],
                examples: base.examples.clone(),
            },
        },
    ];
    let proposal_specs = [
        ProposalSpec {
            candidate_name: "auto_commitment_guard",
            when: continuity_commitment_failure,
            instruction: "如果输入里出现 owner 承诺、活跃项目或明确未完成调查，近期寒暄和等待噪声不应改变主目标。",
            bootstrap_case_name: Some("continue_owner_commitment_despite_small_talk"),
            bootstrap_examples: datasets::continuity_guard::bootstrap_examples,
        },
        ProposalSpec {
            candidate_name: "auto_blocker_guard",
            when: continuity_blocker_failure,
            instruction: "阻塞不等于换项目；如果当前问题是阻塞，应继续原项目，并把阻塞与替代推进方式一起说清楚。",
            bootstrap_case_name: Some("remember_blocker_instead_of_switching_goal"),
            bootstrap_examples: datasets::continuity_guard::bootstrap_examples,
        },
        ProposalSpec {
            candidate_name: "auto_no_forced_continuity",
            when: continuity_no_project_failure,
            instruction: "如果没有活跃项目、长期承诺或未完成调查，不要因为等待和轻量聊天而虚构连续性。",
            bootstrap_case_name: Some("no_project_no_forced_continuity"),
            bootstrap_examples: datasets::continuity_guard::bootstrap_examples,
        },
    ];
    candidates.extend(propose_candidates(
        &base,
        &baseline_results,
        &proposal_specs,
    ));
    ensure_suite_compiled(
        context,
        &renderer,
        &program,
        program.suite_name(),
        dev_cases,
        candidates,
    )
    .await
}

async fn ensure_suite_compiled<P: Program>(
    context: &Context,
    renderer: &OpenAIToolRenderer,
    program: &P,
    suite_name: &str,
    cases: Vec<EvalCase<P::Output>>,
    candidates: Vec<CandidateConfig<P::Output>>,
) -> Result<CompiledProgram> {
    let compile_key = build_compile_key(&context.config, program, suite_name, &cases, &candidates)?;
    if let Some(compiled) =
        load_compiled_program_from_dir(BENCH_COMPILED_DIR_NAME, &compile_key).await?
    {
        eprintln!(
            "[bench-compile] {}: cache hit ({}/{}) using {}",
            suite_name, compiled.score, compiled.total_cases, compiled.best_candidate
        );
        return Ok(compiled);
    }

    let total_cases = cases.len();
    let total_candidates = candidates.len();
    eprintln!(
        "[bench-compile] {}: cache miss, compiling {} candidates x {} cases",
        suite_name, total_candidates, total_cases
    );
    let mut best: Option<(String, PromptTuningConfig<P::Output>, usize, usize)> = None;

    for (candidate_index, candidate) in candidates.into_iter().enumerate() {
        eprintln!(
            "[bench-compile] {}: candidate {}/{} ({})",
            suite_name,
            candidate_index + 1,
            total_candidates,
            candidate.name
        );
        let results = run_suite_with_tuning(
            context,
            renderer,
            program,
            suite_name,
            clone_eval_cases(&cases),
            &candidate.config,
            TraceOrigin::BenchCompile,
        )
        .await;
        let score = results.iter().filter(|result| result.passed).count();
        let attempts_used = results.iter().map(|result| result.attempts_used).sum();
        if best
            .as_ref()
            .is_none_or(|(_, _, best_score, best_attempts)| {
                score > *best_score || (score == *best_score && attempts_used < *best_attempts)
            })
        {
            best = Some((candidate.name, candidate.config, score, attempts_used));
        }
    }

    let Some((best_candidate, best_tuning, score, _attempts_used)) = best else {
        return Err(miette!(
            "no optimization candidates available for bench suite {suite_name}"
        ));
    };

    let compiled = CompiledProgram {
        suite: suite_name.to_string(),
        compile_key,
        best_candidate,
        score,
        total_cases,
        tuning: StoredPromptTuningConfig::from_typed(&best_tuning),
    };
    save_compiled_program_to_dir(BENCH_COMPILED_DIR_NAME, &compiled).await?;
    eprintln!(
        "[bench-compile] {}: selected {} ({}/{})",
        suite_name, compiled.best_candidate, compiled.score, compiled.total_cases
    );
    Ok(compiled)
}

fn clone_eval_cases<O>(cases: &[EvalCase<O>]) -> Vec<EvalCase<O>> {
    cases
        .iter()
        .map(|case| EvalCase {
            name: case.name,
            ir: case.ir.clone(),
            check: case.check.clone(),
        })
        .collect()
}

fn build_compile_key<P: Program>(
    config: &Config,
    program: &P,
    suite_name: &str,
    cases: &[EvalCase<P::Output>],
    candidates: &[CandidateConfig<P::Output>],
) -> Result<String> {
    #[derive(Serialize)]
    struct EvalCaseFingerprint<'a> {
        name: &'a str,
        ir: &'a PromptIR,
    }

    #[derive(Serialize)]
    struct CandidateFingerprint<'a, O> {
        name: &'a str,
        config: &'a PromptTuningConfig<O>,
    }

    #[derive(Serialize)]
    struct CompileFingerprint<'a, O> {
        optimizer_version: &'static str,
        renderer: &'static str,
        suite: &'a str,
        program_name: &'a str,
        model_base_url: &'a str,
        model_name: &'a str,
        temperature: f64,
        signature: Signature,
        base_tuning: PromptTuningConfig<O>,
        cases: Vec<EvalCaseFingerprint<'a>>,
        candidates: Vec<CandidateFingerprint<'a, O>>,
    }

    let payload = CompileFingerprint {
        optimizer_version: BENCH_OPTIMIZER_VERSION,
        renderer: RENDERER_NAME,
        suite: suite_name,
        program_name: program.name(),
        model_base_url: &config.main_model.base_url,
        model_name: &config.main_model.model_name,
        temperature: config.main_model.temperature,
        signature: program.signature(),
        base_tuning: program.default_tuning(),
        cases: cases
            .iter()
            .map(|case| EvalCaseFingerprint {
                name: case.name,
                ir: &case.ir,
            })
            .collect(),
        candidates: candidates
            .iter()
            .map(|candidate| CandidateFingerprint {
                name: &candidate.name,
                config: &candidate.config,
            })
            .collect(),
    };

    let bytes = serde_json::to_vec(&payload)
        .map_err(|err| miette!("failed to serialize bench compile fingerprint: {err}"))?;
    let digest = Sha256::digest(bytes);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn memory_recall_blocker_failure(result: &crate::reasoning::eval::EvalCaseResult) -> bool {
    !result.passed && result.case_name.contains("blocker") && result.detail.contains("contain M")
}

fn memory_recall_noise_failure(result: &crate::reasoning::eval::EvalCaseResult) -> bool {
    !result.passed && result.detail.contains("avoid noise")
}

fn memory_recall_supporting_failure(result: &crate::reasoning::eval::EvalCaseResult) -> bool {
    !result.passed && result.case_name.contains("blocker") && result.detail.contains("contain M")
}

fn continuity_commitment_failure(result: &crate::reasoning::eval::EvalCaseResult) -> bool {
    !result.passed
        && result.case_name.contains("commitment")
        && result.detail.contains("should_continue_project")
}

fn continuity_blocker_failure(result: &crate::reasoning::eval::EvalCaseResult) -> bool {
    !result.passed && result.case_name.contains("blocker") && result.detail.contains("contain T")
}

fn continuity_no_project_failure(result: &crate::reasoning::eval::EvalCaseResult) -> bool {
    !result.passed
        && result.case_name.contains("no_project")
        && result.detail.contains("expected empty project_title")
}
