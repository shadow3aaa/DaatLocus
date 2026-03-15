use miette::Result;

use crate::{context::Context, reasoning::render::openai_tools::OpenAIToolRenderer};

use super::programs::{
    continuity_guard::ContinuityGuardProgram, memory_recall::MemoryRecallProgram,
};

use crate::reasoning::program::Program;

pub use crate::reasoning::eval::EvalCaseResult;

pub async fn run_bench_eval_memory(context: &Context) -> Result<Vec<EvalCaseResult>> {
    let renderer = OpenAIToolRenderer;
    let program = MemoryRecallProgram;
    Ok(crate::reasoning::eval::run_suite_with_tuning(
        context,
        &renderer,
        &program,
        program.suite_name(),
        program.eval_cases(),
        &program.default_tuning(),
        crate::reasoning::trace::TraceOrigin::BenchEval,
    )
    .await)
}

pub async fn run_bench_eval_continuity(context: &Context) -> Result<Vec<EvalCaseResult>> {
    let renderer = OpenAIToolRenderer;
    let program = ContinuityGuardProgram;
    Ok(crate::reasoning::eval::run_suite_with_tuning(
        context,
        &renderer,
        &program,
        program.suite_name(),
        program.eval_cases(),
        &program.default_tuning(),
        crate::reasoning::trace::TraceOrigin::BenchEval,
    )
    .await)
}
