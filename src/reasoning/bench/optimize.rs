use miette::{Result, miette};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    config::Config,
    context::Context,
    reasoning::{
        compiled::{
            CompiledProgram, StoredPromptTuningConfig, load_compiled_program_from_dir,
            save_compiled_program_to_dir,
        },
        eval::{EvalCase, run_suite_with_tuning},
        ir::PromptIR,
        optimizer::{CandidateConfig, OptimizationResult, PromptTuningConfig},
        program::Program,
        render::openai_tools::OpenAIToolRenderer,
        signature::Signature,
        trace::TraceOrigin,
    },
};

use super::{
    programs::{continuity_guard::ContinuityGuardProgram, memory_recall::MemoryRecallProgram},
    proposer::{propose_continuity_guard_candidates, propose_memory_recall_candidates},
};

const BENCH_COMPILED_DIR_NAME: &str = "reasoning_bench_compiled";
const BENCH_OPTIMIZER_VERSION: &str = "reasoning-bench-optimizer-v2";
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

async fn ensure_bench_memory_compiled(context: &Context) -> Result<CompiledProgram> {
    let renderer = OpenAIToolRenderer;
    let program = MemoryRecallProgram;
    let base = program.default_tuning();
    let baseline_results = run_suite_with_tuning(
        context,
        &renderer,
        &program,
        program.suite_name(),
        program.eval_cases(),
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
    candidates.extend(propose_memory_recall_candidates(&base, &baseline_results));
    ensure_suite_compiled(
        context,
        &renderer,
        &program,
        program.suite_name(),
        program.eval_cases(),
        candidates,
    )
    .await
}

async fn ensure_bench_continuity_compiled(context: &Context) -> Result<CompiledProgram> {
    let renderer = OpenAIToolRenderer;
    let program = ContinuityGuardProgram;
    let base = program.default_tuning();
    let baseline_results = run_suite_with_tuning(
        context,
        &renderer,
        &program,
        program.suite_name(),
        program.eval_cases(),
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
    candidates.extend(propose_continuity_guard_candidates(
        &base,
        &baseline_results,
    ));
    ensure_suite_compiled(
        context,
        &renderer,
        &program,
        program.suite_name(),
        program.eval_cases(),
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
