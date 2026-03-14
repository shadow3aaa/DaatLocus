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
    eval::{EvalCase, run_suite_with_tuning},
    ir::PromptIR,
    optimizer::{CandidateConfig, OptimizationResult, PromptTuningConfig},
    program::Program,
    programs::{
        action_phase::{ActionPhase, ActionPhaseProgram},
        resolve_telegram::ResolveTelegramChatProgram,
    },
    signature::Signature,
    trace::TraceOrigin,
    trace_mining::{derive_resolve_telegram_eval_cases, propose_resolve_telegram_candidates},
};

const OPTIMIZER_VERSION: &str = "reasoning-optimizer-v3";
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
    let mut resolve_cases = resolve_program.eval_cases();
    resolve_cases.extend(derive_resolve_telegram_eval_cases(&resolve_program));
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
        let action_candidates = build_action_phase_candidates(&action_program, &action_base);
        compiled.push(
            ensure_suite_compiled(
                context,
                &renderer,
                &action_program,
                &action_program.tuning_key(),
                action_program.eval_cases(),
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

    candidates
}
