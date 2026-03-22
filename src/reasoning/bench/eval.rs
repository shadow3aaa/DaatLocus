use miette::Result;

use crate::{context::Context, reasoning::render::openai_tools::OpenAIToolRenderer};

use super::optimize::{
    load_or_compile_bench_continuity_tuning, load_or_compile_bench_interactive_cli_tuning,
};
use super::programs::{
    continuity_guard::ContinuityGuardProgram, interactive_cli_policy::InteractiveCliPolicyProgram,
};

pub use crate::reasoning::eval::EvalCaseResult;

pub async fn run_bench_eval_continuity(context: &Context) -> Result<Vec<EvalCaseResult>> {
    let renderer = OpenAIToolRenderer;
    let program = ContinuityGuardProgram;
    let tuning = load_or_compile_bench_continuity_tuning(context).await?;
    Ok(crate::reasoning::eval::run_suite_with_tuning(
        context,
        &renderer,
        &program,
        program.suite_name(),
        program.dev_eval_cases(),
        &tuning,
        crate::reasoning::trace::TraceOrigin::BenchEval,
    )
    .await)
}

pub async fn run_bench_eval_interactive_cli(context: &Context) -> Result<Vec<EvalCaseResult>> {
    let renderer = OpenAIToolRenderer;
    let program = InteractiveCliPolicyProgram;
    let tuning = load_or_compile_bench_interactive_cli_tuning(context).await?;
    Ok(crate::reasoning::eval::run_suite_with_tuning(
        context,
        &renderer,
        &program,
        program.suite_name(),
        program.dev_eval_cases(),
        &tuning,
        crate::reasoning::trace::TraceOrigin::BenchEval,
    )
    .await)
}
