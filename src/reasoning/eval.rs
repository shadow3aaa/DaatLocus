use std::sync::Arc;

use miette::Result;

use crate::{context::Context, reasoning::render::openai_tools::OpenAIToolRenderer};

use super::{
    ir::PromptIR,
    optimizer::PromptTuningConfig,
    program::Program,
    programs::{
        action_phase::{ActionPhase, ActionPhaseProgram},
        resolve_telegram::ResolveTelegramChatProgram,
    },
    runtime::execute_program_with_ir_report,
    trace_mining::derive_resolve_telegram_eval_cases,
};

pub struct EvalCase<O> {
    pub name: &'static str,
    pub ir: PromptIR,
    pub check: Arc<dyn Fn(&O) -> Result<()> + Send + Sync>,
}

pub struct EvalCaseResult {
    pub suite: String,
    pub case_name: &'static str,
    pub passed: bool,
    pub detail: String,
    pub attempts_used: usize,
}

pub async fn run_reasoning_eval(context: &Context) -> Result<Vec<EvalCaseResult>> {
    let renderer = OpenAIToolRenderer;
    let mut results = Vec::new();

    let resolve_program = ResolveTelegramChatProgram;
    let mut resolve_cases = resolve_program.eval_cases();
    resolve_cases.extend(derive_resolve_telegram_eval_cases(&resolve_program));
    results.extend(
        run_suite(
            context,
            &renderer,
            &resolve_program,
            "resolve_telegram_chat",
            resolve_cases,
        )
        .await,
    );

    for phase in [
        ActionPhase::AttendNotifications,
        ActionPhase::ExecuteTask,
        ActionPhase::PlanFromProject,
        ActionPhase::ExploreNewTasks,
    ] {
        let program = ActionPhaseProgram::new(phase);
        let suite_name = program.eval_suite_name();
        let cases = program.eval_cases();
        results.extend(run_suite(context, &renderer, &program, suite_name, cases).await);
    }

    Ok(results)
}

async fn run_suite<P: Program>(
    context: &Context,
    renderer: &OpenAIToolRenderer,
    program: &P,
    suite_name: &str,
    cases: Vec<EvalCase<P::Output>>,
) -> Vec<EvalCaseResult> {
    let mut results = Vec::new();

    for case in cases {
        let result = match execute_program_with_ir_report(
            context.llm.as_ref(),
            context,
            renderer,
            program,
            case.ir,
            &program.default_tuning(),
        )
        .await
        {
            Ok(outcome) => match case.check.as_ref()(&outcome.output) {
                Ok(()) => EvalCaseResult {
                    suite: suite_name.to_string(),
                    case_name: case.name,
                    passed: true,
                    detail: "passed".to_string(),
                    attempts_used: outcome.attempts_used,
                },
                Err(err) => EvalCaseResult {
                    suite: suite_name.to_string(),
                    case_name: case.name,
                    passed: false,
                    detail: format!("metric failed: {err}"),
                    attempts_used: outcome.attempts_used,
                },
            },
            Err(err) => EvalCaseResult {
                suite: suite_name.to_string(),
                case_name: case.name,
                passed: false,
                detail: format!("program failed: {err}"),
                attempts_used: 2,
            },
        };

        results.push(result);
    }

    results
}

pub async fn run_suite_with_tuning<P: Program>(
    context: &Context,
    renderer: &OpenAIToolRenderer,
    program: &P,
    suite_name: &str,
    cases: Vec<EvalCase<P::Output>>,
    tuning: &PromptTuningConfig<P::Output>,
) -> Vec<EvalCaseResult> {
    let mut results = Vec::new();

    for case in cases {
        let result = match execute_program_with_ir_report(
            context.llm.as_ref(),
            context,
            renderer,
            program,
            case.ir,
            tuning,
        )
        .await
        {
            Ok(outcome) => match case.check.as_ref()(&outcome.output) {
                Ok(()) => EvalCaseResult {
                    suite: suite_name.to_string(),
                    case_name: case.name,
                    passed: true,
                    detail: "passed".to_string(),
                    attempts_used: outcome.attempts_used,
                },
                Err(err) => EvalCaseResult {
                    suite: suite_name.to_string(),
                    case_name: case.name,
                    passed: false,
                    detail: format!("metric failed: {err}"),
                    attempts_used: outcome.attempts_used,
                },
            },
            Err(err) => EvalCaseResult {
                suite: suite_name.to_string(),
                case_name: case.name,
                passed: false,
                detail: format!("program failed: {err}"),
                attempts_used: 2,
            },
        };

        results.push(result);
    }

    results
}
