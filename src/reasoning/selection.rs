use miette::Result;
use serde::Serialize;

use crate::{
    context::Context,
    reasoning::{
        compiled::CompiledFailureCaseReport, judge::judge_pairwise_outputs,
        optimizer::CandidateConfig, program::Program, render::Renderer,
    },
};

#[derive(Clone)]
pub struct CandidateCaseEvaluation<O: Clone> {
    pub case_name: String,
    pub case_context: String,
    pub output: Option<O>,
    pub passed: bool,
}

#[derive(Clone)]
pub struct CandidateEvaluation<O: Clone> {
    pub candidate: CandidateConfig<O>,
    pub acceptance_score: Option<usize>,
    pub acceptance_total_cases: Option<usize>,
    pub acceptance_attempts_used: Option<usize>,
    pub score: usize,
    pub attempts_used: usize,
    pub episode_wins: usize,
    pub episode_losses: usize,
    pub episode_ties: usize,
    pub judge_wins: usize,
    pub judge_losses: usize,
    pub judge_ties: usize,
    pub failed_cases: Vec<CompiledFailureCaseReport>,
    pub case_results: Vec<CandidateCaseEvaluation<O>>,
}

impl<O: Clone> CandidateEvaluation<O> {
    pub fn acceptance_is_full(&self) -> bool {
        match (self.acceptance_score, self.acceptance_total_cases) {
            (Some(score), Some(total)) => score == total,
            _ => true,
        }
    }
}

pub async fn apply_pairwise_judge_tiebreak<P, R>(
    context: &Context,
    renderer: &R,
    program: &P,
    suite_name: &str,
    evaluations: &mut [CandidateEvaluation<P::Output>],
) -> Result<()>
where
    P: Program,
    P::Output: Serialize,
    R: Renderer,
{
    if !context.config.judge.enabled || evaluations.len() < 2 {
        return Ok(());
    }

    let best_score = evaluations
        .iter()
        .map(|evaluation| evaluation.score)
        .max()
        .unwrap_or(0);
    let tied_indices = evaluations
        .iter()
        .enumerate()
        .filter(|(_, evaluation)| evaluation.acceptance_is_full() && evaluation.score == best_score)
        .map(|(index, _)| index)
        .take(context.config.judge.max_pairwise_candidates)
        .collect::<Vec<_>>();

    for left_position in 0..tied_indices.len() {
        for right_position in (left_position + 1)..tied_indices.len() {
            let left_index = tied_indices[left_position];
            let right_index = tied_indices[right_position];
            let judgeable_cases =
                collect_judgeable_cases(&evaluations[left_index], &evaluations[right_index]);
            for (case_name, case_context, left_output, right_output) in judgeable_cases
                .into_iter()
                .take(context.config.judge.max_pairwise_cases)
            {
                let ab = judge_pairwise_outputs(
                    context,
                    renderer,
                    program,
                    suite_name,
                    &case_name,
                    &case_context,
                    &left_output,
                    &right_output,
                )
                .await?;
                let ba = judge_pairwise_outputs(
                    context,
                    renderer,
                    program,
                    suite_name,
                    &case_name,
                    &case_context,
                    &right_output,
                    &left_output,
                )
                .await?;

                use crate::reasoning::programs::pairwise_judge::PairwiseWinner;
                let (left_eval, right_eval) = if left_index < right_index {
                    let (left_slice, right_slice) = evaluations.split_at_mut(right_index);
                    (&mut left_slice[left_index], &mut right_slice[0])
                } else {
                    let (left_slice, right_slice) = evaluations.split_at_mut(left_index);
                    (&mut right_slice[0], &mut left_slice[right_index])
                };
                match (ab.winner, ba.winner) {
                    (PairwiseWinner::A, PairwiseWinner::B) => {
                        left_eval.judge_wins += 1;
                        right_eval.judge_losses += 1;
                    }
                    (PairwiseWinner::B, PairwiseWinner::A) => {
                        right_eval.judge_wins += 1;
                        left_eval.judge_losses += 1;
                    }
                    _ => {
                        left_eval.judge_ties += 1;
                        right_eval.judge_ties += 1;
                    }
                }
            }
        }
    }

    Ok(())
}

pub fn collect_judgeable_cases<O: Clone + Serialize>(
    left: &CandidateEvaluation<O>,
    right: &CandidateEvaluation<O>,
) -> Vec<(String, String, O, O)> {
    let mut cases = Vec::new();

    for (left_case, right_case) in left.case_results.iter().zip(right.case_results.iter()) {
        if !left_case.passed || !right_case.passed {
            continue;
        }

        let (Some(left_output), Some(right_output)) = (&left_case.output, &right_case.output)
        else {
            continue;
        };
        if left_case.case_name != right_case.case_name {
            continue;
        }

        let Ok(left_json) = serde_json::to_string(left_output) else {
            continue;
        };
        let Ok(right_json) = serde_json::to_string(right_output) else {
            continue;
        };
        if left_json == right_json {
            continue;
        }

        cases.push((
            left_case.case_name.clone(),
            left_case.case_context.clone(),
            left_output.clone(),
            right_output.clone(),
        ));
    }

    cases
}

pub fn compare_candidate_evaluations<O: Clone>(
    left: &CandidateEvaluation<O>,
    right: &CandidateEvaluation<O>,
) -> std::cmp::Ordering {
    right
        .acceptance_is_full()
        .cmp(&left.acceptance_is_full())
        .then_with(|| right.score.cmp(&left.score))
        .then_with(|| right.episode_wins.cmp(&left.episode_wins))
        .then_with(|| left.episode_losses.cmp(&right.episode_losses))
        .then_with(|| right.judge_wins.cmp(&left.judge_wins))
        .then_with(|| left.judge_losses.cmp(&right.judge_losses))
        .then_with(|| left.attempts_used.cmp(&right.attempts_used))
}

pub fn render_case_context(ir: &crate::reasoning::ir::PromptIR) -> String {
    let mut sections = Vec::new();
    if !ir.instructions.is_empty() {
        sections.push(format!("任务说明：\n{}", ir.instructions.join("\n")));
    }
    for section in &ir.sections {
        sections.push(format!("## {}\n{}", section.title, section.body));
    }
    sections.join("\n\n")
}
