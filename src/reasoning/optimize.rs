use miette::{Result, miette};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    config::Config, context::Context, reasoning::render::openai_tools::OpenAIToolRenderer,
};

use super::{
    compiled::{
        CompiledProgram, StoredPromptTuningConfig, load_compiled_program, save_compiled_program,
    },
    datasets,
    eval::{EvalCase, run_suite_with_tuning},
    ir::PromptIR,
    optimizer::{CandidateConfig, OptimizationResult, PromptTuningConfig},
    program::Program,
    programs::{
        action_phase::{ActionPhase, ActionPhaseProgram},
        resolve_telegram::ResolveTelegramChatProgram,
    },
    proposer::{ProposalSpec, propose_candidates},
    signature::Signature,
    trace::TraceOrigin,
    trace_mining::{derive_resolve_telegram_eval_cases, propose_resolve_telegram_candidates},
};

const OPTIMIZER_VERSION: &str = "reasoning-optimizer-v4";
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
    let total_suites = 5usize;

    let resolve_program = ResolveTelegramChatProgram;
    let resolve_base = resolve_program.default_tuning();
    let mut resolve_cases = resolve_program.eval_cases();
    resolve_cases.extend(derive_resolve_telegram_eval_cases(&resolve_program));
    let resolve_baseline_results = run_suite_with_tuning(
        context,
        &renderer,
        &resolve_program,
        "resolve_telegram_chat",
        clone_eval_cases(&resolve_cases),
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
            resolve_cases,
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
        let action_cases = action_program.eval_cases();
        let action_baseline_results = run_suite_with_tuning(
            context,
            &renderer,
            &action_program,
            &action_program.tuning_key(),
            clone_eval_cases(&action_cases),
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
                action_cases,
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
    cases: Vec<EvalCase<P::Output>>,
    candidates: Vec<CandidateConfig<P::Output>>,
    suite_index: usize,
    total_suites: usize,
) -> Result<CompiledProgram> {
    let compile_key = build_compile_key(&context.config, program, suite_name, &cases, &candidates)?;
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

    let total_cases = cases.len();
    let total_candidates = candidates.len();
    eprintln!(
        "[prompt-compile {}/{}] {}: cache miss, compiling {} candidates x {} cases",
        suite_index, total_suites, suite_name, total_candidates, total_cases
    );
    let mut best: Option<(String, PromptTuningConfig<P::Output>, usize, usize)> = None;

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
        let results = run_suite_with_tuning(
            context,
            renderer,
            program,
            suite_name,
            clone_eval_cases(&cases),
            &candidate.config,
            TraceOrigin::Compile,
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
            "no optimization candidates available for suite {suite_name}"
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
        optimizer_version: OPTIMIZER_VERSION,
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
