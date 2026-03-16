use miette::{Result, miette};
use serde::Serialize;

use crate::{
    context::Context,
    reasoning::{
        program::Program,
        programs::pairwise_judge::{PairwiseJudgeOutput, PairwiseJudgeProgram},
        render::Renderer,
        runtime::execute_program_with_ir_report,
        trace::TraceOrigin,
    },
};

pub async fn judge_pairwise_outputs<P, R>(
    context: &Context,
    renderer: &R,
    program: &P,
    suite_name: &str,
    case_name: &str,
    case_context: &str,
    candidate_a_output: &P::Output,
    candidate_b_output: &P::Output,
) -> Result<PairwiseJudgeOutput>
where
    P: Program,
    P::Output: Serialize,
    R: Renderer,
{
    let judge = PairwiseJudgeProgram;
    let rubric = build_case_rubric(program, suite_name);
    let candidate_a = format!(
        "```json\n{}\n```",
        serde_json::to_string_pretty(candidate_a_output)
            .map_err(|err| miette!("failed to serialize candidate A for judge: {err}"))?
    );
    let candidate_b = format!(
        "```json\n{}\n```",
        serde_json::to_string_pretty(candidate_b_output)
            .map_err(|err| miette!("failed to serialize candidate B for judge: {err}"))?
    );
    let ir = judge.dataset_ir(
        format!("{suite_name} / {case_name}"),
        case_context.to_string(),
        rubric,
        candidate_a,
        candidate_b,
    );
    let tuning = judge.default_tuning();
    let outcome = execute_program_with_ir_report(
        context.judge_llm.as_ref(),
        context,
        renderer,
        &judge,
        ir,
        &tuning,
        TraceOrigin::Compile,
    )
    .await?;
    Ok(outcome.output)
}

fn build_case_rubric<P: Program>(program: &P, suite_name: &str) -> String {
    if program.name() == "resolve_telegram_chat" {
        return String::from(
            "选择更符合 IM 语义边界的候选：该短答时应直接短答，该追问时应追问，该拒绝敏感请求时应明确拒绝，该接成项目时才接成项目；并且应避免不必要的额外动作或过度升级。",
        );
    }
    if suite_name.contains("execute_task") {
        return String::from(
            "选择更能直接推进当前下一步动作、且不引入额外绕路或错误设备切换的候选。",
        );
    }
    if suite_name.contains("attend_notifications") {
        return String::from("选择更能优先处理提醒来源、且更少无关切换或拖延的候选。");
    }
    if suite_name.contains("plan_from_project") {
        return String::from("选择更能围绕当前项目补出具体下一步动作，而不是偏离项目主题的候选。");
    }
    if suite_name.contains("explore_new_tasks") {
        return String::from("选择更符合探索阶段边界、且更少无效等待或不必要切换的候选。");
    }
    String::from("选择更符合 case 意图、边界和直接性的候选；若本质等价则 tie。")
}
