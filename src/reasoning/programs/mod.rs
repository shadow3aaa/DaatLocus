pub mod runtime_error_correction_planner;
pub mod skill_improvement_planner;

#[cfg(test)]
mod tests {
    use crate::{
        reasoning::{
            program::Program,
            programs::{
                runtime_error_correction_planner::RuntimeErrorCorrectionPlannerProgram,
                skill_improvement_planner::SkillImprovementPlannerProgram,
            },
        },
        schema_utils::validate_model_facing_schema,
    };

    #[test]
    fn program_output_schemas_follow_model_facing_dialect() {
        let schemas = [
            RuntimeErrorCorrectionPlannerProgram.output_schema(),
            SkillImprovementPlannerProgram.output_schema(),
        ];

        for schema in schemas {
            validate_model_facing_schema(&schema).unwrap();
        }
    }
}
