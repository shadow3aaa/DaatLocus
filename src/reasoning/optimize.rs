use miette::{Result, miette};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use crate::{
    config::Config, context::Context, reasoning::render::openai_tools::OpenAIToolRenderer,
};

use super::{
    compiled::{
        CompiledCandidateReport, CompiledFailureCaseReport, CompiledProgram, CompiledProgramReport,
        StoredPromptTuningConfig, load_compiled_program, save_compiled_program,
    },
    datasets,
    eval::{EvalCase, run_suite_with_tuning},
    ir::PromptIR,
    optimizer::{CandidateConfig, OptimizationResult, PromptTuningConfig},
    program::Program,
    programs::{
        pairwise_judge::PairwiseJudgeProgram, resolve_telegram::ResolveTelegramChatProgram,
    },
    runtime::execute_program_with_ir_report,
    selection::{
        CandidateCaseEvaluation, CandidateEvaluation, apply_pairwise_judge_tiebreak,
        compare_candidate_evaluations, render_case_context,
    },
    signature::Signature,
    sleep_artifacts::{SleepArtifactsSnapshot, SleepArtifactsStore},
    trace::TraceOrigin,
    trace_mining::derive_resolve_telegram_eval_cases,
};

const OPTIMIZER_VERSION: &str = "reasoning-optimizer-v12";
const RENDERER_NAME: &str = "openai_tools";

pub async fn run_reasoning_optimize(context: &Context) -> Result<Vec<OptimizationResult>> {
    let compiled = ensure_reasoning_compiled(context).await?;
    Ok(compiled
        .into_iter()
        .map(|entry| OptimizationResult {
            suite: entry.suite,
            best_candidate: entry.best_candidate,
            score: entry.score,
            total_cases: entry.total_cases,
        })
        .collect())
}

pub async fn ensure_reasoning_compiled(context: &Context) -> Result<Vec<CompiledProgram>> {
    let renderer = OpenAIToolRenderer;
    let mut compiled = Vec::new();
    let total_suites = 1usize;
    let sleep_snapshot = load_sleep_artifacts_snapshot().await?;

    let resolve_program = ResolveTelegramChatProgram;
    let resolve_base = resolve_program.default_tuning();
    let resolve_sleep = sleep_snapshot.filter_suite("resolve_telegram_chat");
    let mut resolve_train_cases = resolve_program.train_eval_cases();
    resolve_train_cases.extend(derive_resolve_telegram_eval_cases(&resolve_program));
    let resolve_acceptance_cases = resolve_program.acceptance_eval_cases();
    let mut resolve_stress_cases = resolve_program.stress_eval_cases();
    resolve_stress_cases.extend(datasets::resolve_telegram::stress_eval_cases_by_names(
        &resolve_program,
        &sleep_reference_case_names(&resolve_sleep),
    ));
    let mut resolve_candidates = vec![CandidateConfig {
        name: "baseline".to_string(),
        config: resolve_base.clone(),
    }];
    resolve_candidates.extend(build_sleep_artifact_candidates(
        &resolve_base,
        &resolve_sleep,
        sleep_examples_to_program_examples::<
            crate::reasoning::programs::resolve_telegram::ResolveTelegramProgramOutput,
        >(&resolve_sleep),
    ));
    compiled.push(
        ensure_suite_compiled(
            context,
            &renderer,
            &resolve_program,
            "resolve_telegram_chat",
            resolve_train_cases,
            resolve_acceptance_cases,
            resolve_stress_cases,
            "stress",
            resolve_candidates,
            1,
            total_suites,
        )
        .await?,
    );

    Ok(compiled)
}

async fn load_sleep_artifacts_snapshot() -> Result<SleepArtifactsSnapshot> {
    let store = SleepArtifactsStore::open().await?;
    store.load_snapshot().await
}

fn sleep_reference_case_names(snapshot: &SleepArtifactsSnapshot) -> Vec<String> {
    let mut names = std::collections::BTreeSet::new();
    for demo in &snapshot.bootstrap_demos {
        for name in &demo.reference_case_names {
            names.insert(name.clone());
        }
    }
    for case in &snapshot.stress_cases {
        for name in &case.reference_case_names {
            names.insert(name.clone());
        }
    }
    names.into_iter().collect()
}

fn build_sleep_artifact_candidates<O: Clone>(
    base: &PromptTuningConfig<O>,
    snapshot: &SleepArtifactsSnapshot,
    direct_sleep_examples: Vec<crate::reasoning::examples::ProgramExample<O>>,
) -> Vec<CandidateConfig<O>> {
    let mut candidates = Vec::new();
    let sleep_instructions = snapshot
        .instruction_hypotheses
        .iter()
        .map(|item| item.text.clone())
        .collect::<Vec<_>>();
    if !sleep_instructions.is_empty() {
        candidates.push(CandidateConfig {
            name: "sleep_instruction_hypotheses".to_string(),
            config: PromptTuningConfig {
                extra_instructions: sleep_instructions,
                examples: base.examples.clone(),
            },
        });
    }
    if !direct_sleep_examples.is_empty() {
        candidates.push(CandidateConfig {
            name: "sleep_bootstrap_demos".to_string(),
            config: PromptTuningConfig {
                extra_instructions: base.extra_instructions.clone(),
                examples: direct_sleep_examples,
            },
        });
    }
    candidates
}

fn sleep_examples_to_program_examples<O: Clone + Serialize + DeserializeOwned>(
    snapshot: &SleepArtifactsSnapshot,
) -> Vec<crate::reasoning::examples::ProgramExample<O>> {
    snapshot
        .bootstrap_demos
        .iter()
        .filter_map(|artifact| {
            let output = serde_json::from_value::<O>(artifact.expected_output.clone()).ok()?;
            Some(crate::reasoning::examples::ProgramExample {
                title: artifact.title.clone(),
                inputs: artifact.inputs.clone(),
                output,
            })
        })
        .collect()
}

async fn ensure_suite_compiled<P: Program>(
    context: &Context,
    renderer: &OpenAIToolRenderer,
    program: &P,
    suite_name: &str,
    train_cases: Vec<EvalCase<P::Output>>,
    acceptance_cases: Vec<EvalCase<P::Output>>,
    ranking_cases: Vec<EvalCase<P::Output>>,
    ranking_label: &str,
    candidates: Vec<CandidateConfig<P::Output>>,
    suite_index: usize,
    total_suites: usize,
) -> Result<CompiledProgram> {
    let compile_key = build_compile_key(
        &context.config,
        program,
        suite_name,
        &train_cases,
        &acceptance_cases,
        ranking_label,
        &ranking_cases,
        &candidates,
    )?;
    if let Some(compiled) = load_compiled_program(&compile_key).await? {
        eprintln!(
            "[prompt-compile {}/{}] {}: cache hit ({}/{}) using {}",
            suite_index,
            total_suites,
            suite_name,
            compiled.score,
            compiled.total_cases,
            compiled.best_candidate
        );
        return Ok(compiled);
    }

    let total_cases = ranking_cases.len();
    let total_candidates = candidates.len();
    eprintln!(
        "[prompt-compile {}/{}] {}: cache miss, compiling {} candidates x {} acceptance + {} {} cases",
        suite_index,
        total_suites,
        suite_name,
        total_candidates,
        acceptance_cases.len(),
        total_cases,
        ranking_label
    );
    let mut evaluations = evaluate_candidates(
        context,
        renderer,
        program,
        suite_name,
        &acceptance_cases,
        &ranking_cases,
        candidates,
        suite_index,
        total_suites,
        total_candidates,
    )
    .await;
    apply_pairwise_judge_tiebreak(context, renderer, program, suite_name, &mut evaluations).await?;
    evaluations.sort_by(compare_candidate_evaluations);

    let mut best: Option<(
        String,
        PromptTuningConfig<P::Output>,
        usize,
        usize,
        usize,
        usize,
        usize,
    )> = None;
    let mut candidate_reports = Vec::new();
    for evaluation in &evaluations {
        candidate_reports.push(CompiledCandidateReport {
            name: evaluation.candidate.name.clone(),
            acceptance_score: evaluation.acceptance_score,
            acceptance_total_cases: evaluation.acceptance_total_cases,
            acceptance_attempts_used: evaluation.acceptance_attempts_used,
            score: evaluation.score,
            total_cases,
            attempts_used: evaluation.attempts_used,
            judge_wins: evaluation.judge_wins,
            judge_losses: evaluation.judge_losses,
            judge_ties: evaluation.judge_ties,
            extra_instructions: evaluation.candidate.config.extra_instructions.clone(),
            example_titles: evaluation
                .candidate
                .config
                .examples
                .iter()
                .map(|example| example.title.clone())
                .collect(),
            failed_cases: evaluation.failed_cases.clone(),
        });
        if evaluation.acceptance_score != evaluation.acceptance_total_cases {
            continue;
        }
        if best.as_ref().is_none_or(
            |(_, _, best_score, best_attempts, best_judge_wins, best_judge_losses, _)| {
                evaluation.score > *best_score
                    || (evaluation.score == *best_score
                        && (evaluation.judge_wins > *best_judge_wins
                            || (evaluation.judge_wins == *best_judge_wins
                                && (evaluation.judge_losses < *best_judge_losses
                                    || (evaluation.judge_losses == *best_judge_losses
                                        && evaluation.attempts_used < *best_attempts)))))
            },
        ) {
            best = Some((
                evaluation.candidate.name.clone(),
                evaluation.candidate.config.clone(),
                evaluation.score,
                evaluation.attempts_used,
                evaluation.judge_wins,
                evaluation.judge_losses,
                evaluation.judge_ties,
            ));
        }
    }

    let Some((
        best_candidate,
        best_tuning,
        score,
        _attempts_used,
        _judge_wins,
        _judge_losses,
        _judge_ties,
    )) = best
    else {
        return Err(miette!(
            "no optimization candidates available for suite {suite_name}"
        ));
    };

    let selected_dev_results = run_suite_with_tuning(
        context,
        renderer,
        program,
        suite_name,
        clone_eval_cases(&ranking_cases),
        &best_tuning,
        TraceOrigin::Compile,
    )
    .await;
    let selected_acceptance_results = run_suite_with_tuning(
        context,
        renderer,
        program,
        &format!("{suite_name}.acceptance"),
        clone_eval_cases(&acceptance_cases),
        &best_tuning,
        TraceOrigin::Compile,
    )
    .await;
    let selected_train_results = run_suite_with_tuning(
        context,
        renderer,
        program,
        &format!("{suite_name}.train"),
        clone_eval_cases(&train_cases),
        &best_tuning,
        TraceOrigin::Compile,
    )
    .await;
    let dev_attempts_used = selected_dev_results
        .iter()
        .map(|result| result.attempts_used)
        .sum();
    let acceptance_attempts_used = selected_acceptance_results
        .iter()
        .map(|result| result.attempts_used)
        .sum();
    let train_attempts_used = selected_train_results
        .iter()
        .map(|result| result.attempts_used)
        .sum();

    let compiled = CompiledProgram {
        suite: suite_name.to_string(),
        compile_key,
        best_candidate,
        score,
        total_cases,
        tuning: StoredPromptTuningConfig::from_typed(&best_tuning),
        report: Some(CompiledProgramReport {
            train_score: selected_train_results
                .iter()
                .filter(|result| result.passed)
                .count(),
            train_total_cases: train_cases.len(),
            train_attempts_used,
            acceptance_score: Some(
                selected_acceptance_results
                    .iter()
                    .filter(|result| result.passed)
                    .count(),
            ),
            acceptance_total_cases: Some(acceptance_cases.len()),
            acceptance_attempts_used: Some(acceptance_attempts_used),
            dev_score: selected_dev_results
                .iter()
                .filter(|result| result.passed)
                .count(),
            dev_total_cases: ranking_cases.len(),
            dev_attempts_used,
            ranking_label: Some(ranking_label.to_string()),
            selected_extra_instructions: best_tuning.extra_instructions.clone(),
            selected_example_titles: best_tuning
                .examples
                .iter()
                .map(|example| example.title.clone())
                .collect(),
            candidates: candidate_reports,
        }),
    };
    save_compiled_program(&compiled).await?;
    eprintln!(
        "[prompt-compile {}/{}] {}: selected {} ({}/{})",
        suite_index,
        total_suites,
        suite_name,
        compiled.best_candidate,
        compiled.score,
        compiled.total_cases
    );
    Ok(compiled)
}

async fn evaluate_candidates<P: Program>(
    context: &Context,
    renderer: &OpenAIToolRenderer,
    program: &P,
    suite_name: &str,
    acceptance_cases: &[EvalCase<P::Output>],
    ranking_cases: &[EvalCase<P::Output>],
    candidates: Vec<CandidateConfig<P::Output>>,
    suite_index: usize,
    total_suites: usize,
    total_candidates: usize,
) -> Vec<CandidateEvaluation<P::Output>> {
    let mut evaluations = Vec::new();
    for (candidate_index, candidate) in candidates.into_iter().enumerate() {
        eprintln!(
            "[prompt-compile {}/{}] {}: candidate {}/{} ({})",
            suite_index,
            total_suites,
            suite_name,
            candidate_index + 1,
            total_candidates,
            candidate.name
        );
        let mut acceptance_score = 0usize;
        let mut acceptance_attempts_used = 0usize;
        let mut score = 0usize;
        let mut attempts_used = 0usize;
        let mut failed_cases = Vec::new();
        let mut case_results = Vec::new();

        for case in clone_eval_cases(acceptance_cases) {
            let case_name = case.name.to_string();
            let result = match execute_program_with_ir_report(
                context.llm.as_ref(),
                context,
                renderer,
                program,
                case.ir,
                &candidate.config,
                TraceOrigin::Compile,
            )
            .await
            {
                Ok(outcome) => {
                    acceptance_attempts_used += outcome.attempts_used;
                    match case.check.as_ref()(&outcome.output) {
                        Ok(()) => {
                            acceptance_score += 1;
                            true
                        }
                        Err(err) => {
                            failed_cases.push(CompiledFailureCaseReport {
                                case_name,
                                detail: format!("acceptance metric failed: {err}"),
                            });
                            false
                        }
                    }
                }
                Err(err) => {
                    acceptance_attempts_used += 2;
                    failed_cases.push(CompiledFailureCaseReport {
                        case_name,
                        detail: format!("acceptance program failed: {err}"),
                    });
                    false
                }
            };
            if !result {
                break;
            }
        }

        if acceptance_score == acceptance_cases.len() {
            for case in clone_eval_cases(ranking_cases) {
                let case_name = case.name.to_string();
                let case_context = render_case_context(&case.ir);
                let result = match execute_program_with_ir_report(
                    context.llm.as_ref(),
                    context,
                    renderer,
                    program,
                    case.ir,
                    &candidate.config,
                    TraceOrigin::Compile,
                )
                .await
                {
                    Ok(outcome) => {
                        attempts_used += outcome.attempts_used;
                        match case.check.as_ref()(&outcome.output) {
                            Ok(()) => {
                                score += 1;
                                CandidateCaseEvaluation {
                                    case_name,
                                    case_context,
                                    output: Some(outcome.output),
                                    passed: true,
                                }
                            }
                            Err(err) => {
                                let detail = format!("stress metric failed: {err}");
                                failed_cases.push(CompiledFailureCaseReport {
                                    case_name: case_name.clone(),
                                    detail,
                                });
                                CandidateCaseEvaluation {
                                    case_name,
                                    case_context,
                                    output: Some(outcome.output),
                                    passed: false,
                                }
                            }
                        }
                    }
                    Err(err) => {
                        let detail = format!("stress program failed: {err}");
                        attempts_used += 2;
                        failed_cases.push(CompiledFailureCaseReport {
                            case_name: case_name.clone(),
                            detail,
                        });
                        CandidateCaseEvaluation {
                            case_name,
                            case_context,
                            output: None,
                            passed: false,
                        }
                    }
                };
                case_results.push(result);
            }
        }
        evaluations.push(CandidateEvaluation {
            candidate,
            acceptance_score: Some(acceptance_score),
            acceptance_total_cases: Some(acceptance_cases.len()),
            acceptance_attempts_used: Some(acceptance_attempts_used),
            score,
            attempts_used: acceptance_attempts_used + attempts_used,
            judge_wins: 0,
            judge_losses: 0,
            judge_ties: 0,
            failed_cases,
            case_results,
        });
    }
    evaluations
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
    train_cases: &[EvalCase<P::Output>],
    acceptance_cases: &[EvalCase<P::Output>],
    ranking_label: &str,
    ranking_cases: &[EvalCase<P::Output>],
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
    struct JudgeFingerprint<'a> {
        enabled: bool,
        model_base_url: &'a str,
        model_name: &'a str,
        temperature: f64,
        max_pairwise_candidates: usize,
        max_pairwise_cases: usize,
        signature: Signature,
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
        judge: JudgeFingerprint<'a>,
        base_tuning: PromptTuningConfig<O>,
        train_cases: Vec<EvalCaseFingerprint<'a>>,
        acceptance_cases: Vec<EvalCaseFingerprint<'a>>,
        ranking_label: &'a str,
        ranking_cases: Vec<EvalCaseFingerprint<'a>>,
        candidates: Vec<CandidateFingerprint<'a, O>>,
    }

    let resolved_judge_model = config.judge.resolved_model(&config.main_model);
    let pairwise_judge = PairwiseJudgeProgram;

    let payload = CompileFingerprint {
        optimizer_version: OPTIMIZER_VERSION,
        renderer: RENDERER_NAME,
        suite: suite_name,
        program_name: program.name(),
        model_base_url: &config.main_model.base_url,
        model_name: &config.main_model.model_name,
        temperature: config.main_model.temperature,
        signature: program.signature(),
        judge: JudgeFingerprint {
            enabled: config.judge.enabled,
            model_base_url: &resolved_judge_model.base_url,
            model_name: &resolved_judge_model.model_name,
            temperature: resolved_judge_model.temperature,
            max_pairwise_candidates: config.judge.max_pairwise_candidates,
            max_pairwise_cases: config.judge.max_pairwise_cases,
            signature: pairwise_judge.signature(),
        },
        base_tuning: program.default_tuning(),
        train_cases: train_cases
            .iter()
            .map(|case| EvalCaseFingerprint {
                name: case.name,
                ir: &case.ir,
            })
            .collect(),
        acceptance_cases: acceptance_cases
            .iter()
            .map(|case| EvalCaseFingerprint {
                name: case.name,
                ir: &case.ir,
            })
            .collect(),
        ranking_label,
        ranking_cases: ranking_cases
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
        .map_err(|err| miette!("failed to serialize compile fingerprint: {err}"))?;
    let digest = Sha256::digest(bytes);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}
