use miette::{Result, miette};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::programs::{
    continuity_guard::ContinuityGuardProgram, interactive_cli_policy::InteractiveCliPolicyProgram,
    memory_encoding::MemoryEncodingProgram, memory_recall::MemoryRecallProgram,
    terminal_completion::TerminalCompletionProgram,
};
use crate::{
    config::Config,
    context::Context,
    reasoning::{
        bench::datasets,
        compiled::{
            BENCH_COMPILED_DIR_NAME, CompiledCandidateReport, CompiledFailureCaseReport,
            CompiledProgram, CompiledProgramReport, StoredPromptTuningConfig,
            load_compiled_program_from_dir, save_compiled_program_to_dir,
        },
        eval::{EvalCase, run_suite_with_tuning},
        ir::PromptIR,
        optimizer::{CandidateConfig, OptimizationResult, PromptTuningConfig},
        program::Program,
        programs::pairwise_judge::PairwiseJudgeProgram,
        proposer::{ProposalSpec, propose_candidates},
        render::openai_tools::OpenAIToolRenderer,
        runtime::execute_program_with_ir_report,
        selection::{
            CandidateCaseEvaluation, CandidateEvaluation, apply_pairwise_judge_tiebreak,
            compare_candidate_evaluations, render_case_context,
        },
        signature::Signature,
        trace::TraceOrigin,
    },
};

const BENCH_OPTIMIZER_VERSION: &str = "reasoning-bench-optimizer-v5";
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

pub async fn run_bench_optimize_memory_encoding(
    context: &Context,
) -> Result<Vec<OptimizationResult>> {
    let compiled = ensure_bench_memory_encoding_compiled(context).await?;
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

pub async fn run_bench_optimize_terminal_completion(
    context: &Context,
) -> Result<Vec<OptimizationResult>> {
    let compiled = ensure_bench_terminal_completion_compiled(context).await?;
    Ok(vec![OptimizationResult {
        suite: compiled.suite,
        best_candidate: compiled.best_candidate,
        score: compiled.score,
        total_cases: compiled.total_cases,
    }])
}

pub async fn run_bench_optimize_interactive_cli(
    context: &Context,
) -> Result<Vec<OptimizationResult>> {
    let compiled = ensure_bench_interactive_cli_compiled(context).await?;
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

pub async fn load_or_compile_bench_memory_encoding_tuning(
    context: &Context,
) -> Result<
    PromptTuningConfig<crate::reasoning::bench::programs::memory_encoding::MemoryEncodingOutput>,
> {
    let compiled = ensure_bench_memory_encoding_compiled(context).await?;
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

pub async fn load_or_compile_bench_terminal_completion_tuning(
    context: &Context,
) -> Result<
    PromptTuningConfig<
        crate::reasoning::bench::programs::terminal_completion::TerminalCompletionOutput,
    >,
> {
    let compiled = ensure_bench_terminal_completion_compiled(context).await?;
    compiled.tuning.to_typed()
}

pub async fn load_or_compile_bench_interactive_cli_tuning(
    context: &Context,
) -> Result<
    PromptTuningConfig<
        crate::reasoning::bench::programs::interactive_cli_policy::InteractiveCliPolicyOutput,
    >,
> {
    let compiled = ensure_bench_interactive_cli_compiled(context).await?;
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

async fn ensure_bench_memory_encoding_compiled(context: &Context) -> Result<CompiledProgram> {
    let renderer = OpenAIToolRenderer;
    let program = MemoryEncodingProgram;
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
            name: "anchor_first_bias".to_string(),
            config: PromptTuningConfig {
                extra_instructions: vec![
                    "先保住 URL、文件名、命令和对象引用这些关键锚点，再组织摘要。".to_string(),
                ],
                examples: base.examples.clone(),
            },
        },
        CandidateConfig {
            name: "thread_split_bias".to_string(),
            config: PromptTuningConfig {
                extra_instructions: vec![
                    "把持续主线和本轮事件明确拆开：thread_focus 负责连续目标，event_summary 只写本轮新增事实。".to_string(),
                ],
                examples: base.examples.clone(),
            },
        },
    ];
    let proposal_specs = [
        ProposalSpec {
            candidate_name: "auto_anchor_bias",
            when: memory_encoding_anchor_failure,
            instruction: "如果输入里有 URL、文件名、命令或对象引用，这些字面锚点必须进入 anchors，不要只保留抽象结论。",
            bootstrap_case_name: None,
            bootstrap_examples: datasets::memory_encoding::bootstrap_examples,
        },
        ProposalSpec {
            candidate_name: "auto_thread_effect_bias",
            when: memory_encoding_effect_failure,
            instruction: "thread_effect 要反映这轮事件对主线的真实影响；链接失效和报错通常是 blocked，进入交互式界面后主动退出更像 switched。",
            bootstrap_case_name: None,
            bootstrap_examples: datasets::memory_encoding::bootstrap_examples,
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

async fn ensure_bench_terminal_completion_compiled(context: &Context) -> Result<CompiledProgram> {
    let renderer = OpenAIToolRenderer;
    let program = TerminalCompletionProgram;
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
            name: "prompt_return_bias".to_string(),
            config: PromptTuningConfig {
                extra_instructions: vec![
                    "一旦终端底部已经回到 shell prompt，应优先判断为 finished 或 viewport_truncated，而不是 still_running。".to_string(),
                ],
                examples: base.examples.clone(),
            },
        },
        CandidateConfig {
            name: "interactive_prompt_bias".to_string(),
            config: PromptTuningConfig {
                extra_instructions: vec![
                    "如果看到问答式提示、>>>、(END) 或登录向导提问，应优先判断为 interactive_prompt。".to_string(),
                ],
                examples: base.examples.clone(),
            },
        },
    ];
    let proposal_specs = [
        ProposalSpec {
            candidate_name: "auto_prompt_return_bias",
            when: terminal_completion_prompt_failure,
            instruction: "只要底部已经回到 shell prompt，就不要误判为 still_running；如果只是窗口看不全，优先考虑 viewport_truncated。",
            bootstrap_case_name: None,
            bootstrap_examples: datasets::terminal_completion::bootstrap_examples,
        },
        ProposalSpec {
            candidate_name: "auto_interactive_prompt_bias",
            when: terminal_completion_interactive_failure,
            instruction: "看到 REPL 提示符、问答式向导或登录提问时，应优先判断为 interactive_prompt。",
            bootstrap_case_name: None,
            bootstrap_examples: datasets::terminal_completion::bootstrap_examples,
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

async fn ensure_bench_interactive_cli_compiled(context: &Context) -> Result<CompiledProgram> {
    let renderer = OpenAIToolRenderer;
    let program = InteractiveCliPolicyProgram;
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
            name: "interrupt_bias".to_string(),
            config: PromptTuningConfig {
                extra_instructions: vec![
                    "对于与当前任务无关的登录向导、REPL 和授权流程，优先选择 interrupt_and_switch_noninteractive。".to_string(),
                ],
                examples: base.examples.clone(),
            },
        },
        CandidateConfig {
            name: "safe_continue_bias".to_string(),
            config: PromptTuningConfig {
                extra_instructions: vec![
                    "只有在下一次输入是短小、确定、安全且直接服务于当前任务时，才选择 continue_interaction。".to_string(),
                ],
                examples: base.examples.clone(),
            },
        },
    ];
    let proposal_specs = [
        ProposalSpec {
            candidate_name: "auto_interrupt_bias",
            when: interactive_cli_interrupt_failure,
            instruction: "与当前任务无关的登录向导、授权向导和 REPL 应优先中断，并切回非交互方案。",
            bootstrap_case_name: None,
            bootstrap_examples: datasets::interactive_cli_policy::bootstrap_examples,
        },
        ProposalSpec {
            candidate_name: "auto_safe_continue_bias",
            when: interactive_cli_continue_failure,
            instruction: "只有在当前目标就是退出交互式工具、且下一步输入短小确定时，才继续交互。",
            bootstrap_case_name: None,
            bootstrap_examples: datasets::interactive_cli_policy::bootstrap_examples,
        },
        ProposalSpec {
            candidate_name: "auto_wait_bias",
            when: interactive_cli_wait_failure,
            instruction: "如果终端只是继续自然输出，且没有出现输入提示，不要抢着中断或输入，优先选择 wait。",
            bootstrap_case_name: None,
            bootstrap_examples: datasets::interactive_cli_policy::bootstrap_examples,
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
    let mut evaluations = Vec::new();

    for (candidate_index, candidate) in candidates.into_iter().enumerate() {
        eprintln!(
            "[bench-compile] {}: candidate {}/{} ({})",
            suite_name,
            candidate_index + 1,
            total_candidates,
            candidate.name
        );
        let mut score = 0usize;
        let mut attempts_used = 0usize;
        let mut failed_cases = Vec::new();
        let mut case_results = Vec::new();
        for case in clone_eval_cases(&cases) {
            let case_name = case.name.to_string();
            let case_context = render_case_context(&case.ir);
            match execute_program_with_ir_report(
                context.llm.as_ref(),
                context,
                renderer,
                program,
                case.ir,
                &candidate.config,
                TraceOrigin::BenchCompile,
            )
            .await
            {
                Ok(outcome) => {
                    attempts_used += outcome.attempts_used;
                    match case.check.as_ref()(&outcome.output) {
                        Ok(()) => {
                            score += 1;
                            case_results.push(CandidateCaseEvaluation {
                                case_name,
                                case_context,
                                output: Some(outcome.output),
                                passed: true,
                            });
                        }
                        Err(err) => {
                            failed_cases.push(CompiledFailureCaseReport {
                                case_name: case_name.clone(),
                                detail: format!("metric failed: {err}"),
                            });
                            case_results.push(CandidateCaseEvaluation {
                                case_name,
                                case_context,
                                output: Some(outcome.output),
                                passed: false,
                            });
                        }
                    }
                }
                Err(err) => {
                    attempts_used += 2;
                    failed_cases.push(CompiledFailureCaseReport {
                        case_name: case_name.clone(),
                        detail: format!("program failed: {err}"),
                    });
                    case_results.push(CandidateCaseEvaluation {
                        case_name,
                        case_context,
                        output: None,
                        passed: false,
                    });
                }
            }
        }
        evaluations.push(CandidateEvaluation {
            candidate,
            acceptance_score: None,
            acceptance_total_cases: None,
            acceptance_attempts_used: None,
            score,
            attempts_used,
            judge_wins: 0,
            judge_losses: 0,
            judge_ties: 0,
            failed_cases,
            case_results,
        });
    }

    apply_pairwise_judge_tiebreak(context, renderer, program, suite_name, &mut evaluations).await?;
    evaluations.sort_by(compare_candidate_evaluations);

    let Some(best) = evaluations.first() else {
        return Err(miette!(
            "no optimization candidates available for bench suite {suite_name}"
        ));
    };

    let candidate_reports = evaluations
        .iter()
        .map(|evaluation| CompiledCandidateReport {
            name: evaluation.candidate.name.clone(),
            acceptance_score: None,
            acceptance_total_cases: None,
            acceptance_attempts_used: None,
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
        })
        .collect::<Vec<_>>();

    let compiled = CompiledProgram {
        suite: suite_name.to_string(),
        compile_key,
        best_candidate: best.candidate.name.clone(),
        score: best.score,
        total_cases,
        tuning: StoredPromptTuningConfig::from_typed(&best.candidate.config),
        report: Some(CompiledProgramReport {
            train_score: 0,
            train_total_cases: 0,
            train_attempts_used: 0,
            acceptance_score: None,
            acceptance_total_cases: None,
            acceptance_attempts_used: None,
            dev_score: best.score,
            dev_total_cases: total_cases,
            dev_attempts_used: best.attempts_used,
            ranking_label: Some("dev".to_string()),
            selected_extra_instructions: best.candidate.config.extra_instructions.clone(),
            selected_example_titles: best
                .candidate
                .config
                .examples
                .iter()
                .map(|example| example.title.clone())
                .collect(),
            candidates: candidate_reports,
        }),
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
        cases: Vec<EvalCaseFingerprint<'a>>,
        candidates: Vec<CandidateFingerprint<'a, O>>,
    }

    let resolved_judge_model = config.judge.resolved_model(&config.main_model);
    let pairwise_judge = PairwiseJudgeProgram;

    let payload = CompileFingerprint {
        optimizer_version: BENCH_OPTIMIZER_VERSION,
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

fn terminal_completion_prompt_failure(result: &crate::reasoning::eval::EvalCaseResult) -> bool {
    !result.passed
        && (result.case_name.contains("prompt")
            || result.detail.contains("expected status ViewportTruncated"))
}

fn terminal_completion_interactive_failure(
    result: &crate::reasoning::eval::EvalCaseResult,
) -> bool {
    !result.passed && result.case_name.contains("interactive")
}

fn memory_encoding_anchor_failure(result: &crate::reasoning::eval::EvalCaseResult) -> bool {
    !result.passed && result.detail.contains("expected anchors")
}

fn memory_encoding_effect_failure(result: &crate::reasoning::eval::EvalCaseResult) -> bool {
    !result.passed && result.detail.contains("expected thread_effect")
}

fn interactive_cli_interrupt_failure(result: &crate::reasoning::eval::EvalCaseResult) -> bool {
    !result.passed
        && (result.case_name.contains("interrupt")
            || result
                .detail
                .contains("expected policy InterruptAndSwitchNoninteractive"))
}

fn interactive_cli_continue_failure(result: &crate::reasoning::eval::EvalCaseResult) -> bool {
    !result.passed
        && (result.case_name.contains("continue") || result.detail.contains("expected next_input"))
}

fn interactive_cli_wait_failure(result: &crate::reasoning::eval::EvalCaseResult) -> bool {
    !result.passed
        && (result.case_name.contains("wait") || result.detail.contains("expected policy Wait"))
}
