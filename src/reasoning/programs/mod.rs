pub mod runtime_error_correction_planner;
pub mod runtime_turn_trace_judge;
pub mod workflow_candidate_rollout_evaluator;
pub mod workflow_evolution_planner;
pub mod workflow_merge_planner;

#[cfg(test)]
mod tests {
    use crate::{
        reasoning::{
            program::Program,
            programs::{
                runtime_error_correction_planner::RuntimeErrorCorrectionPlannerProgram,
                runtime_turn_trace_judge::RuntimeTurnTraceJudgeProgram,
                workflow_candidate_rollout_evaluator::WorkflowCandidateRolloutEvaluatorProgram,
                workflow_evolution_planner::WorkflowEvolutionPlannerProgram,
                workflow_merge_planner::WorkflowMergePlannerProgram,
            },
        },
        schema_utils::validate_model_facing_schema,
    };

    #[test]
    fn program_output_schemas_follow_model_facing_dialect() {
        let schemas = [
            RuntimeErrorCorrectionPlannerProgram.output_schema(),
            RuntimeTurnTraceJudgeProgram.output_schema(),
            WorkflowCandidateRolloutEvaluatorProgram.output_schema(),
            WorkflowEvolutionPlannerProgram.output_schema(),
            WorkflowMergePlannerProgram.output_schema(),
        ];

        for schema in schemas {
            validate_model_facing_schema(&schema).unwrap();
        }
    }
}
