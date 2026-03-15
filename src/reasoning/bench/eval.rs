use miette::Result;

use crate::{context::Context, reasoning::render::openai_tools::OpenAIToolRenderer};

use super::optimize::{
    load_or_compile_bench_continuity_tuning, load_or_compile_bench_memory_tuning,
};
use super::programs::{
    continuity_guard::ContinuityGuardProgram, memory_recall::MemoryRecallProgram,
};

pub use crate::reasoning::eval::EvalCaseResult;

pub async fn run_bench_eval_memory(context: &Context) -> Result<Vec<EvalCaseResult>> {
    let renderer = OpenAIToolRenderer;
    let program = MemoryRecallProgram;
    let tuning = load_or_compile_bench_memory_tuning(context).await?;
    Ok(crate::reasoning::eval::run_suite_with_tuning(
        context,
        &renderer,
        &program,
        program.suite_name(),
        program.eval_cases(),
        &tuning,
        crate::reasoning::trace::TraceOrigin::BenchEval,
    )
    .await)
}

pub async fn run_bench_eval_continuity(context: &Context) -> Result<Vec<EvalCaseResult>> {
    let renderer = OpenAIToolRenderer;
    let program = ContinuityGuardProgram;
    let tuning = load_or_compile_bench_continuity_tuning(context).await?;
    Ok(crate::reasoning::eval::run_suite_with_tuning(
        context,
        &renderer,
        &program,
        program.suite_name(),
        program.eval_cases(),
        &tuning,
        crate::reasoning::trace::TraceOrigin::BenchEval,
    )
    .await)
}
