use miette::{Result, miette};
use serde::Serialize;
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
        action_phase::{ActionPhase, ActionPhaseProgram},
        pairwise_judge::PairwiseJudgeProgram,
        resolve_telegram::ResolveTelegramChatProgram,
    },
    proposer::{ProposalSpec, propose_candidates},
    runtime::execute_program_with_ir_report,
    selection::{
        CandidateCaseEvaluation, CandidateEvaluation, apply_pairwise_judge_tiebreak,
        compare_candidate_evaluations, render_case_context,
    },
    signature::Signature,
    teleprompter::{build_bootstrap_demo_candidates, build_teleprompter_candidates},
    trace::TraceOrigin,
    trace_mining::{derive_resolve_telegram_eval_cases, propose_resolve_telegram_candidates},
};

const OPTIMIZER_VERSION: &str = "reasoning-optimizer-v10";
const RENDERER_NAME: &str = "openai_tools";
const SEARCH_SEED_LIMIT: usize = 4;
const SEARCH_PAIR_LIMIT: usize = 6;

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
    let total_suites = 5usize;

    let resolve_program = ResolveTelegramChatProgram;
    let resolve_base = resolve_program.default_tuning();
    let mut resolve_train_cases = resolve_program.train_eval_cases();
    resolve_train_cases.extend(derive_resolve_telegram_eval_cases(&resolve_program));
    let resolve_acceptance_cases = resolve_program.acceptance_eval_cases();
    let resolve_stress_cases = resolve_program.stress_eval_cases();
    let resolve_baseline_results = run_suite_with_tuning(
        context,
        &renderer,
        &resolve_program,
        "resolve_telegram_chat.train",
        clone_eval_cases(&resolve_train_cases),
        &resolve_base,
        TraceOrigin::Compile,
    )
    .await;
    let mut resolve_candidates = vec![
        CandidateConfig {
            name: "baseline".to_string(),
            config: resolve_base.clone(),
        },
        CandidateConfig {
            name: "reply_project_bias".to_string(),
            config: PromptTuningConfig {
                extra_instructions: vec![
                    "如果消息要求持续推进的工作，优先输出 AcceptAsProject，而不是只做礼貌确认。"
                        .to_string(),
                ],
                examples: resolve_base.examples.clone(),
            },
        },
        CandidateConfig {
            name: "minimal_examples".to_string(),
            config: PromptTuningConfig {
                extra_instructions: resolve_base.extra_instructions.clone(),
                examples: resolve_base.examples.iter().take(2).cloned().collect(),
            },
        },
    ];
    resolve_candidates.extend(propose_resolve_telegram_candidates(&resolve_base));
    resolve_candidates.extend(build_teleprompter_candidates(
        &resolve_base,
        "teleprompt_instruction",
        &[
            "优先按训练边界处理 Telegram：先聚焦，再打开会话，再区分 ReplyOnly、AcceptAsProject、AskClarification、Decline。",
            "如果当前只剩待回复，不要重新语义判定；如果请求需要长期推进，优先识别为项目而不是礼貌确认。",
        ],
    ));
    resolve_candidates.extend(build_bootstrap_demo_candidates(
        &resolve_base,
        "bootstrap_train_demos",
        "bootstrap_train_combo",
        &[
            "优先按训练边界处理 Telegram：先聚焦，再打开会话，再区分 ReplyOnly、AcceptAsProject、AskClarification、Decline。",
            "如果当前只剩待回复，不要重新语义判定；如果请求需要长期推进，优先识别为项目而不是礼貌确认。",
        ],
        datasets::resolve_telegram::all_bootstrap_examples(),
    ));
    let resolve_proposal_specs = [
        ProposalSpec {
            candidate_name: "auto_focus_first",
            when: resolve_focus_failure,
            instruction: "当 Telegram 消息仍待判断且 Telegram 不在前景时，优先切到 Telegram，不要继续留在 Terminal。",
            bootstrap_case_name: Some("resolve_telegram_focuses_app_before_reading"),
            bootstrap_examples: datasets::resolve_telegram::bootstrap_examples,
        },
        ProposalSpec {
            candidate_name: "auto_open_chat_first",
            when: resolve_open_chat_failure,
            instruction: "如果 Telegram 已在前景但当前还停留在列表页，应先打开相关会话，再做语义判断。",
            bootstrap_case_name: Some("resolve_telegram_opens_chat_from_list_page"),
            bootstrap_examples: datasets::resolve_telegram::bootstrap_examples,
        },
        ProposalSpec {
            candidate_name: "auto_accept_project",
            when: resolve_accept_project_failure,
            instruction: "明确要求持续推进的工作应优先接受为项目，而不是只做礼貌确认。",
            bootstrap_case_name: Some("resolve_telegram_accepts_project_request"),
            bootstrap_examples: datasets::resolve_telegram::bootstrap_examples,
        },
        ProposalSpec {
            candidate_name: "auto_reply_pending",
            when: resolve_reply_pending_failure,
            instruction: "如果当前会话已经待判断：否且待回复：是，说明只差补发消息，应直接回复，不要重复语义分类。",
            bootstrap_case_name: Some("resolve_telegram_replies_when_only_reply_pending"),
            bootstrap_examples: datasets::resolve_telegram::bootstrap_examples,
        },
        ProposalSpec {
            candidate_name: "auto_reply_only",
            when: resolve_reply_only_failure,
            instruction: "对于简单状态询问、寒暄或无需持续推进的短消息，应使用 ReplyOnly，而不是误接成项目。",
            bootstrap_case_name: Some("resolve_telegram_uses_reply_only_for_status_question"),
            bootstrap_examples: datasets::resolve_telegram::bootstrap_examples,
        },
        ProposalSpec {
            candidate_name: "auto_ask_clarification",
            when: resolve_ask_clarification_failure,
            instruction: "如果请求缺少项目名称、链接或具体目标，信息不足时应先 AskClarification，而不是直接 AcceptAsProject。",
            bootstrap_case_name: Some(
                "resolve_telegram_asks_clarification_when_request_is_underspecified",
            ),
            bootstrap_examples: datasets::resolve_telegram::bootstrap_examples,
        },
        ProposalSpec {
            candidate_name: "auto_decline_sensitive",
            when: resolve_decline_failure,
            instruction: "如果消息要求提供 token、密码或其他敏感凭据，应明确 Decline，并保持安全边界。",
            bootstrap_case_name: Some("resolve_telegram_declines_credential_request"),
            bootstrap_examples: datasets::resolve_telegram::bootstrap_examples,
        },
    ];
    resolve_candidates.extend(propose_candidates(
        &resolve_base,
        &resolve_baseline_results,
        &resolve_proposal_specs,
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

    for phase in [
        ActionPhase::AttendNotifications,
        ActionPhase::ExecuteTask,
        ActionPhase::PlanFromProject,
        ActionPhase::ExploreNewTasks,
    ] {
        let action_program = ActionPhaseProgram::new(phase);
        let action_base = action_program.default_tuning();
        let action_train_cases = action_program.train_eval_cases();
        let action_acceptance_cases = action_program.acceptance_eval_cases();
        let action_stress_cases = action_program.stress_eval_cases();
        let action_baseline_results = run_suite_with_tuning(
            context,
            &renderer,
            &action_program,
            &format!("{}.train", action_program.tuning_key()),
            clone_eval_cases(&action_train_cases),
            &action_base,
            TraceOrigin::Compile,
        )
        .await;
        let action_candidates =
            build_action_phase_candidates(&action_program, &action_base, &action_baseline_results);
        compiled.push(
            ensure_suite_compiled(
                context,
                &renderer,
                &action_program,
                &action_program.tuning_key(),
                action_train_cases,
                action_acceptance_cases,
                action_stress_cases,
                "stress",
                action_candidates,
                compiled.len() + 1,
                total_suites,
            )
            .await?,
        );
    }

    Ok(compiled)
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

    let search_candidates = build_search_candidates(&evaluations)?;
    if !search_candidates.is_empty() {
        let search_total = search_candidates.len();
        evaluations.extend(
            evaluate_candidates(
                context,
                renderer,
                program,
                suite_name,
                &acceptance_cases,
                &ranking_cases,
                search_candidates,
                suite_index,
                total_suites,
                search_total,
            )
            .await,
        );
    }

    apply_pairwise_judge_tiebreak(context, renderer, program, suite_name, &mut evaluations)
        .await?;
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

fn build_search_candidates<O: Clone + Serialize>(
    evaluations: &[CandidateEvaluation<O>],
) -> Result<Vec<CandidateConfig<O>>> {
    let mut ranked = evaluations.to_vec();
    ranked.sort_by(compare_candidate_evaluations);
    let seeds = ranked
        .into_iter()
        .filter(|evaluation| evaluation.acceptance_is_full())
        .filter(|evaluation| evaluation.candidate.name != "baseline")
        .take(SEARCH_SEED_LIMIT)
        .collect::<Vec<_>>();

    let mut seen = std::collections::BTreeSet::new();
    let mut combos = Vec::new();
    for i in 0..seeds.len() {
        for j in (i + 1)..seeds.len() {
            if combos.len() >= SEARCH_PAIR_LIMIT {
                return Ok(combos);
            }
            let merged =
                merge_tuning_configs(&seeds[i].candidate.config, &seeds[j].candidate.config)?;
            let signature = serialize_tuning_signature(&merged)?;
            if !seen.insert(signature) {
                continue;
            }
            combos.push(CandidateConfig {
                name: format!(
                    "search_combo({}+{})",
                    seeds[i].candidate.name, seeds[j].candidate.name
                ),
                config: merged,
            });
        }
    }
    Ok(combos)
}

fn merge_tuning_configs<O: Clone + Serialize>(
    left: &PromptTuningConfig<O>,
    right: &PromptTuningConfig<O>,
) -> Result<PromptTuningConfig<O>> {
    let mut extra_instructions = left.extra_instructions.clone();
    for instruction in &right.extra_instructions {
        if !extra_instructions
            .iter()
            .any(|existing| existing == instruction)
        {
            extra_instructions.push(instruction.clone());
        }
    }

    let mut examples = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for example in left.examples.iter().chain(right.examples.iter()) {
        let fingerprint = serde_json::to_string(example)
            .map_err(|err| miette!("failed to serialize example fingerprint: {err}"))?;
        if seen.insert(fingerprint) {
            examples.push(example.clone());
        }
    }

    Ok(PromptTuningConfig {
        extra_instructions,
        examples,
    })
}

fn serialize_tuning_signature<O: Clone + Serialize>(
    tuning: &PromptTuningConfig<O>,
) -> Result<String> {
    serde_json::to_string(tuning)
        .map_err(|err| miette!("failed to serialize tuning signature: {err}"))
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

fn build_action_phase_candidates(
    program: &ActionPhaseProgram,
    base: &PromptTuningConfig<crate::core::Output>,
    baseline_results: &[crate::reasoning::eval::EvalCaseResult],
) -> Vec<CandidateConfig<crate::core::Output>> {
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
    ];

    let phase_bias = match program.eval_suite_name() {
        "action_phase.attend_notifications" => {
            Some("当 Telegram 后台有提醒时，优先切去 Telegram，而不是继续终端工作。")
        }
        "action_phase.execute_task" => {
            Some("执行阶段优先推进当前已存在的下一步动作，不要绕回探索。")
        }
        "action_phase.plan_from_project" => {
            Some("项目规划阶段必须产出挂到项目上的下一步动作，除非项目已完成。")
        }
        "action_phase.explore_new_tasks" => {
            Some("探索阶段若当前没有前景设备，优先切到 Terminal 获取可操作环境。")
        }
        _ => None,
    };

    if let Some(instruction) = phase_bias {
        candidates.push(CandidateConfig {
            name: "phase_bias".to_string(),
            config: PromptTuningConfig {
                extra_instructions: vec![instruction.to_string()],
                examples: base.examples.clone(),
            },
        });
    }

    candidates.extend(build_teleprompter_candidates(
        base,
        "teleprompt_instruction",
        action_phase_teleprompter_instructions(program.phase()),
    ));
    candidates.extend(build_bootstrap_demo_candidates(
        base,
        "bootstrap_train_demos",
        "bootstrap_train_combo",
        action_phase_teleprompter_instructions(program.phase()),
        datasets::action_phase::all_bootstrap_examples(program.phase()),
    ));

    let proposal_specs = match program.phase() {
        ActionPhase::AttendNotifications => vec![ProposalSpec {
            candidate_name: "auto_focus_telegram",
            when: action_phase_focus_telegram_failure,
            instruction: "提醒处理阶段只要 Telegram 在后台有待处理消息，就应先切到 Telegram，而不是继续终端工作。",
            bootstrap_case_name: Some("attend_notifications_focuses_telegram_first"),
            bootstrap_examples: bootstrap_attend_notifications_examples,
        }],
        ActionPhase::ExecuteTask => vec![
            ProposalSpec {
                candidate_name: "auto_select_task",
                when: action_phase_select_task_failure,
                instruction: "执行阶段如果还没有选中下一步动作，应先 TaskSelect，再开始真正执行。",
                bootstrap_case_name: Some("execute_task_selects_existing_task_before_running"),
                bootstrap_examples: bootstrap_execute_task_examples,
            },
            ProposalSpec {
                candidate_name: "auto_cancel_interactive",
                when: action_phase_cancel_interactive_failure,
                instruction: "如果终端误入交互式认证或登录向导，应先用 Ctrl+C 中断，再改用非交互方案。",
                bootstrap_case_name: Some("execute_task_cancels_interactive_auth_prompt"),
                bootstrap_examples: bootstrap_execute_task_examples,
            },
            ProposalSpec {
                candidate_name: "auto_quit_pager",
                when: action_phase_quit_pager_failure,
                instruction: "如果终端停在 less、man 等分页器，而当前目标只是回到 shell，应优先发送安全、短小、确定的输入 `q` 退出。",
                bootstrap_case_name: Some("execute_task_quits_less_pager_to_return_to_shell"),
                bootstrap_examples: bootstrap_execute_task_examples,
            },
            ProposalSpec {
                candidate_name: "auto_wait_streaming_output",
                when: action_phase_wait_streaming_failure,
                instruction: "如果终端只是持续输出普通命令结果，且没有出现输入提示，不要误判成交互式界面；此时应优先 Wait。",
                bootstrap_case_name: Some("execute_task_waits_for_streaming_test_output"),
                bootstrap_examples: bootstrap_execute_task_examples,
            },
        ],
        ActionPhase::PlanFromProject => vec![ProposalSpec {
            candidate_name: "auto_add_project_task",
            when: action_phase_add_project_task_failure,
            instruction: "项目规划阶段应优先补出挂到该项目上的下一步动作，而不是转去探索别的方向。",
            bootstrap_case_name: Some("plan_from_project_creates_project_scoped_task"),
            bootstrap_examples: bootstrap_plan_from_project_examples,
        }],
        ActionPhase::ExploreNewTasks => vec![
            ProposalSpec {
                candidate_name: "auto_focus_terminal",
                when: action_phase_focus_terminal_failure,
                instruction: "探索阶段在完全空闲且没有前景设备时，应先切到 Terminal 获取可操作环境。",
                bootstrap_case_name: Some("explore_focuses_terminal_when_idle"),
                bootstrap_examples: bootstrap_explore_examples,
            },
            ProposalSpec {
                candidate_name: "auto_silent_wait",
                when: action_phase_silent_wait_failure,
                instruction: "如果只是空闲等待新的外部输入，应使用 SilentWait，而不是把空转等待写进普通 Wait。",
                bootstrap_case_name: Some("explore_uses_silent_wait_when_completely_idle"),
                bootstrap_examples: bootstrap_explore_examples,
            },
        ],
    };

    candidates.extend(propose_candidates(base, baseline_results, &proposal_specs));

    candidates
}

fn action_phase_teleprompter_instructions(phase: ActionPhase) -> &'static [&'static str] {
    match phase {
        ActionPhase::AttendNotifications => &[
            "提醒处理阶段优先按照训练边界行动：先处理 Telegram 与 Pending 义务，再考虑其他设备或探索。",
        ],
        ActionPhase::ExecuteTask => &[
            "执行阶段优先按照训练边界行动：先选中已有动作、保持正确设备前景、误入交互式认证时先中断。",
        ],
        ActionPhase::PlanFromProject => &[
            "项目规划阶段优先按照训练边界行动：为 Active 项目补出 project-scoped 的具体下一步动作，而不是偏离项目。",
        ],
        ActionPhase::ExploreNewTasks => &[
            "探索阶段优先按照训练边界行动：无前景设备时先 FocusTerminal，完全空闲时用 SilentWait。",
        ],
    }
}

fn bootstrap_attend_notifications_examples(
    case_names: &[&str],
) -> Vec<crate::reasoning::examples::ProgramExample<crate::core::Output>> {
    datasets::action_phase::bootstrap_examples(ActionPhase::AttendNotifications, case_names)
}

fn bootstrap_execute_task_examples(
    case_names: &[&str],
) -> Vec<crate::reasoning::examples::ProgramExample<crate::core::Output>> {
    datasets::action_phase::bootstrap_examples(ActionPhase::ExecuteTask, case_names)
}

fn bootstrap_plan_from_project_examples(
    case_names: &[&str],
) -> Vec<crate::reasoning::examples::ProgramExample<crate::core::Output>> {
    datasets::action_phase::bootstrap_examples(ActionPhase::PlanFromProject, case_names)
}

fn bootstrap_explore_examples(
    case_names: &[&str],
) -> Vec<crate::reasoning::examples::ProgramExample<crate::core::Output>> {
    datasets::action_phase::bootstrap_examples(ActionPhase::ExploreNewTasks, case_names)
}

fn resolve_focus_failure(result: &crate::reasoning::eval::EvalCaseResult) -> bool {
    !result.passed
        && (result.case_name.contains("focuses_app")
            || result.detail.contains("expected FocusTelegram"))
}

fn resolve_open_chat_failure(result: &crate::reasoning::eval::EvalCaseResult) -> bool {
    !result.passed
        && (result.case_name.contains("opens_chat") || result.detail.contains("expected OpenChat"))
}

fn resolve_accept_project_failure(result: &crate::reasoning::eval::EvalCaseResult) -> bool {
    !result.passed
        && (result.case_name.contains("accepts_project")
            || result.detail.contains("AcceptAsProject"))
}

fn resolve_reply_pending_failure(result: &crate::reasoning::eval::EvalCaseResult) -> bool {
    !result.passed
        && (result.case_name.contains("reply")
            || result.detail.contains("expected ReplyInCurrentChat"))
}

fn resolve_reply_only_failure(result: &crate::reasoning::eval::EvalCaseResult) -> bool {
    !result.passed
        && (result.case_name.contains("reply_only") || result.detail.contains("ReplyOnly"))
}

fn resolve_ask_clarification_failure(result: &crate::reasoning::eval::EvalCaseResult) -> bool {
    !result.passed
        && (result.case_name.contains("clarification")
            || result.detail.contains("AskClarification"))
}

fn resolve_decline_failure(result: &crate::reasoning::eval::EvalCaseResult) -> bool {
    !result.passed && (result.case_name.contains("decline") || result.detail.contains("Decline"))
}

fn action_phase_focus_telegram_failure(result: &crate::reasoning::eval::EvalCaseResult) -> bool {
    !result.passed
        && (result.case_name.contains("focuses_telegram")
            || result.detail.contains("FocusDevice(Telegram)"))
}

fn action_phase_select_task_failure(result: &crate::reasoning::eval::EvalCaseResult) -> bool {
    !result.passed
        && (result.case_name.contains("selects_existing_task")
            || result.detail.contains("TaskSelect"))
}

fn action_phase_cancel_interactive_failure(
    result: &crate::reasoning::eval::EvalCaseResult,
) -> bool {
    !result.passed
        && (result.case_name.contains("cancels_interactive") || result.detail.contains("Ctrl+C"))
}

fn action_phase_quit_pager_failure(result: &crate::reasoning::eval::EvalCaseResult) -> bool {
    !result.passed
        && (result.case_name.contains("quits_less")
            || result.case_name.contains("quits_manual_pager")
            || result.detail.contains("TerminalInput containing \"q\"")
            || result.detail.contains("TerminalInput containing 'q'"))
}

fn action_phase_wait_streaming_failure(result: &crate::reasoning::eval::EvalCaseResult) -> bool {
    !result.passed
        && (result.case_name.contains("waits_for_streaming")
            || result.detail.contains("expected Wait"))
}

fn action_phase_add_project_task_failure(result: &crate::reasoning::eval::EvalCaseResult) -> bool {
    !result.passed
        && (result.case_name.contains("creates_project_scoped_task")
            || result.detail.contains("TaskAdd with project_id"))
}

fn action_phase_focus_terminal_failure(result: &crate::reasoning::eval::EvalCaseResult) -> bool {
    !result.passed
        && (result.case_name.contains("focuses_terminal")
            || result.detail.contains("FocusDevice(Terminal)"))
}

fn action_phase_silent_wait_failure(result: &crate::reasoning::eval::EvalCaseResult) -> bool {
    !result.passed
        && (result.case_name.contains("silent_wait") || result.detail.contains("SilentWait"))
}
