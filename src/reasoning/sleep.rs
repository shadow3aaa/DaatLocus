use crate::{
    context::Context,
    daat_locus_paths::daat_locus_paths,
    reasoning::{
        compiled::{
            RUNTIME_SYSTEM_PROMPT_COMPILE_KEY, save_compiled_runtime_system_prompt_for_model,
        },
        programs::{
            runtime_error_correction_planner::{
                RuntimeErrorCorrectionPlannerOutput, RuntimeErrorCorrectionPlannerProgram,
            },
            skill_improvement_planner::{
                SkillImprovementPlannerOutput, SkillImprovementPlannerProgram,
            },
        },
        runtime_error::{RuntimeErrorCase, RuntimeErrorCaseBatch, compact_runtime_error_case_file},
        turn_compile::current_runtime_system_prompt_artifact_from_store,
    },
    skill_run_records::{SkillRunBatch, SkillRunRecord, load_skill_run_batch},
};
use async_trait::async_trait;
use miette::{IntoDiagnostic, Result};
use tracing::warn;

use super::{
    evaluation_artifacts::{
        EvaluationArtifactPromptReflection, EvaluationArtifactRuntimePromptCandidate,
        EvaluationArtifactRuntimePromptCandidateEvaluation, EvaluationArtifactsStore,
        RuntimeErrorCorrectionArtifacts,
    },
    render::openai_tools::OpenAIToolRenderer,
    runtime::{execute_program_with_ir_report, resolve_program_tuning},
    trace::TraceOrigin,
};

mod artifacts;
mod prompt_pipeline;

use artifacts::{dedupe_prompt_candidates, dedupe_vec, slugify};
use prompt_pipeline::run_runtime_error_correction_pipeline;

#[derive(Clone, Default)]
pub struct RuntimeErrorCorrectionSummary {
    pub consumed_error_cases: usize,
    pub runtime_error_cases: usize,
    pub reflections: usize,
    pub candidates: usize,
    pub candidate_evaluations: usize,
    pub applied_system_additions: usize,
    pub compiled_runtime_contract_updated: bool,
}

#[derive(Clone, Default)]
pub struct WorkflowImprovementSummary {
    pub evidence_run_records: usize,
    pub patch_applied: usize,
}

#[derive(Clone, Default)]
pub struct SleepSummary {
    pub runtime_error_correction: RuntimeErrorCorrectionSummary,
    pub workflow_improvement: WorkflowImprovementSummary,
}

pub async fn run_sleep(context: &mut Context) -> Result<SleepSummary> {
    let store = EvaluationArtifactsStore::open().await?;
    let sleep_inputs = load_sleep_inputs().await?;
    let runtime_error_correction = if sleep_inputs.runtime_error_cases.cases.is_empty() {
        tracing::info!(
            "[sleep] no runtime error cases, skipping runtime error correction pipeline"
        );
        RuntimeErrorCorrectionSummary::default()
    } else {
        match run_runtime_error_correction_pipeline(
            context,
            &LlmSleepPlannerRuntime,
            &store,
            &sleep_inputs.runtime_error_cases.cases,
            sleep_inputs.runtime_error_cases.cases.len(),
        )
        .await
        {
            Ok(summary) => summary,
            Err(err) => {
                warn!(
                    "runtime error correction pipeline failed, continuing with defaults: {err:?}"
                );
                RuntimeErrorCorrectionSummary::default()
            }
        }
    };
    compact_runtime_error_case_file(sleep_inputs.runtime_error_cases.next_offset).await?;
    let workflow_improvement =
        match run_skill_improvement_pipeline(context, &sleep_inputs.skill_run_batch).await {
            Ok(summary) => summary,
            Err(err) => {
                warn!("skill improvement pipeline failed, continuing with defaults: {err:?}");
                WorkflowImprovementSummary::default()
            }
        };
    Ok(SleepSummary {
        runtime_error_correction,
        workflow_improvement,
    })
}

struct SleepInputs {
    runtime_error_cases: RuntimeErrorCaseBatch,
    skill_run_batch: SkillRunBatch,
}

async fn load_sleep_inputs() -> Result<SleepInputs> {
    let runtime_error_cases =
        crate::reasoning::runtime_error::load_runtime_error_case_batch().await?;
    let skill_run_batch = load_skill_run_batch(MAX_SKILL_RUN_RECORDS_PER_SLEEP, 0).await?;
    Ok(SleepInputs {
        runtime_error_cases,
        skill_run_batch,
    })
}

struct LlmSleepPlannerRuntime;

#[async_trait]
trait SleepPlannerRuntime: Send + Sync {
    async fn plan_runtime_error_correction(
        &self,
        context: &mut Context,
        runtime_error_cases: &[RuntimeErrorCase],
    ) -> Result<PromptPlanningResult>;
}

#[async_trait]
impl SleepPlannerRuntime for LlmSleepPlannerRuntime {
    async fn plan_runtime_error_correction(
        &self,
        context: &mut Context,
        runtime_error_cases: &[RuntimeErrorCase],
    ) -> Result<PromptPlanningResult> {
        if runtime_error_cases.is_empty() {
            return Ok(PromptPlanningResult::default());
        }

        let renderer = OpenAIToolRenderer;
        let program = RuntimeErrorCorrectionPlannerProgram;
        let tuning = resolve_program_tuning(context, &program).await;
        let current_additions = context
            .compiled_prompts
            .runtime_system_additions()
            .join("\n");
        let runtime_error_cases_json =
            serde_json::to_string_pretty(runtime_error_cases).into_diagnostic()?;
        let outcome = execute_program_with_ir_report(
            context.judge_llm.as_ref(),
            context,
            &renderer,
            &program,
            program.dataset_ir(current_additions, runtime_error_cases_json),
            &tuning,
            TraceOrigin::Sleep,
        )
        .await?;

        Ok(runtime_error_correction_planning_result_from_output(
            &outcome.output,
            runtime_error_cases,
        ))
    }
}

// ── Skill improvement pipeline ─────────────────────────────────────────────

const MAX_SKILL_RUN_RECORDS_PER_SLEEP: usize = 200;

async fn run_skill_improvement_pipeline(
    context: &mut Context,
    batch: &SkillRunBatch,
) -> Result<WorkflowImprovementSummary> {
    if batch.records.is_empty() {
        tracing::info!("[sleep] no skill run records, skipping skill improvement pipeline");
        return Ok(WorkflowImprovementSummary {
            evidence_run_records: 0,
            ..Default::default()
        });
    }

    // Group records by skill name
    let mut by_skill: std::collections::HashMap<String, Vec<&SkillRunRecord>> =
        std::collections::HashMap::new();
    for record in &batch.records {
        by_skill
            .entry(record.skill_name.clone())
            .or_default()
            .push(record);
    }

    let skills_dir = daat_locus_paths().await.root().join("skills");
    let mut total_patches_applied = 0usize;
    let renderer = super::render::openai_tools::OpenAIToolRenderer;
    let program = SkillImprovementPlannerProgram;
    let tuning = resolve_program_tuning(context, &program).await;

    for (skill_name, records) in &by_skill {
        // Find the skill file path — look in ~/.daat-locus/skills/<name>/SKILL.md
        let skill_file = skills_dir.join(skill_name).join("SKILL.md");
        if !skill_file.exists() {
            continue;
        }
        let skill_content = match tokio::fs::read_to_string(&skill_file).await {
            Ok(content) => content,
            Err(err) => {
                warn!("failed to read skill file {}: {err}", skill_file.display());
                continue;
            }
        };
        let evidence_json = serde_json::to_string_pretty(records).into_diagnostic()?;
        let ir = program.dataset_ir(skill_name.clone(), skill_content.clone(), evidence_json);
        let outcome = match execute_program_with_ir_report(
            context.judge_llm.as_ref(),
            context,
            &renderer,
            &program,
            ir,
            &tuning,
            TraceOrigin::Sleep,
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(err) => {
                warn!("skill improvement planner failed for {skill_name}: {err:?}");
                continue;
            }
        };
        let output: SkillImprovementPlannerOutput = outcome.output;
        if !output.should_improve {
            continue;
        }
        // Apply the selected patch (if any) by appending lines to the skill file
        let Some(selected) = output
            .patches
            .iter()
            .find(|p| p.selected && !p.additions.is_empty())
        else {
            continue;
        };
        let additions_text = selected
            .additions
            .iter()
            .map(|line| format!("- {}", line.trim()))
            .collect::<Vec<_>>()
            .join("\n");
        let updated = format!(
            "{}\n\n## Sleep Improvements\n{}\n",
            skill_content.trim_end(),
            additions_text
        );
        match tokio::fs::write(&skill_file, updated).await {
            Ok(_) => {
                tracing::info!(
                    "[sleep] applied improvement to skill {skill_name}: {}",
                    selected.title
                );
                total_patches_applied += 1;
            }
            Err(err) => {
                warn!("failed to write skill improvement for {skill_name}: {err}");
            }
        }
    }

    Ok(WorkflowImprovementSummary {
        evidence_run_records: batch.records.len(),
        patch_applied: total_patches_applied,
    })
}

// ── Prompt planning types ───────────────────────────────────────────────────

#[derive(Default)]
struct PromptPlanningResult {
    reflections: Vec<EvaluationArtifactPromptReflection>,
    candidates: Vec<EvaluationArtifactRuntimePromptCandidate>,
    evaluations: Vec<EvaluationArtifactRuntimePromptCandidateEvaluation>,
}

struct PromptPatchUpdate {
    applied_system_additions: usize,
    compiled_prompt_updated: bool,
}

fn runtime_error_correction_planning_result_from_output(
    output: &RuntimeErrorCorrectionPlannerOutput,
    runtime_error_cases: &[RuntimeErrorCase],
) -> PromptPlanningResult {
    let source_case_ids = runtime_error_cases
        .iter()
        .map(|case| case.case_id.clone())
        .collect::<Vec<_>>();
    let reflections = output
        .reflections
        .iter()
        .map(|reflection| EvaluationArtifactPromptReflection {
            compile_key: RUNTIME_SYSTEM_PROMPT_COMPILE_KEY.to_string(),
            title: reflection.title.trim().to_string(),
            rationale: reflection.rationale.trim().to_string(),
            missing_instructions: dedupe_vec(reflection.missing_runtime_contracts.clone()),
            over_constraints: dedupe_vec(reflection.over_constraints.clone()),
            source_trace_ids: if reflection.source_case_ids.is_empty() {
                source_case_ids.clone()
            } else {
                dedupe_vec(reflection.source_case_ids.clone())
            },
            confidence: reflection.confidence,
        })
        .collect::<Vec<_>>();
    let candidates = output
        .candidates
        .iter()
        .map(|candidate| EvaluationArtifactRuntimePromptCandidate {
            compile_key: RUNTIME_SYSTEM_PROMPT_COMPILE_KEY.to_string(),
            title: candidate.title.trim().to_string(),
            rationale: candidate.rationale.trim().to_string(),
            prompt_patches: dedupe_vec(candidate.runtime_contract_additions.clone()),
            source_demo_titles: Vec::new(),
            source_hypotheses: dedupe_vec(candidate.source_reflection_titles.clone()),
        })
        .filter(|candidate| !candidate.title.is_empty() && !candidate.prompt_patches.is_empty())
        .collect::<Vec<_>>();
    let evaluations = output
        .evaluations
        .iter()
        .map(
            |evaluation| EvaluationArtifactRuntimePromptCandidateEvaluation {
                compile_key: RUNTIME_SYSTEM_PROMPT_COMPILE_KEY.to_string(),
                candidate_title: evaluation.candidate_title.trim().to_string(),
                rationale: evaluation.rationale.trim().to_string(),
                score: evaluation.score,
                accepted: evaluation.accepted,
                selected: evaluation.selected,
                regressions_detected: evaluation.regressions_detected,
                source_trace_ids: source_case_ids.clone(),
            },
        )
        .filter(|evaluation| !evaluation.candidate_title.is_empty())
        .collect::<Vec<_>>();

    PromptPlanningResult {
        reflections,
        candidates: dedupe_prompt_candidates(candidates),
        evaluations,
    }
}

async fn apply_selected_prompt_candidate(
    context: &mut Context,
    candidates: &[EvaluationArtifactRuntimePromptCandidate],
    evaluations: &mut [EvaluationArtifactRuntimePromptCandidateEvaluation],
) -> Result<PromptPatchUpdate> {
    let Some(selected) = evaluations
        .iter_mut()
        .filter(|evaluation| evaluation.accepted)
        .max_by(|left, right| left.score.total_cmp(&right.score))
    else {
        return Ok(PromptPatchUpdate {
            applied_system_additions: 0,
            compiled_prompt_updated: false,
        });
    };
    selected.selected = true;

    let Some(candidate) = candidates
        .iter()
        .find(|candidate| candidate.title == selected.candidate_title)
    else {
        return Ok(PromptPatchUpdate {
            applied_system_additions: 0,
            compiled_prompt_updated: false,
        });
    };

    let mut compiled = current_runtime_system_prompt_artifact_from_store(&context.compiled_prompts);
    let previous_len = compiled.system_additions.len();
    for addition in &candidate.prompt_patches {
        if !compiled
            .system_additions
            .iter()
            .any(|line| line == addition)
        {
            compiled.system_additions.push(addition.clone());
        }
    }
    let applied_system_additions = compiled.system_additions.len().saturating_sub(previous_len);
    if applied_system_additions == 0 {
        return Ok(PromptPatchUpdate {
            applied_system_additions: 0,
            compiled_prompt_updated: false,
        });
    }

    compiled.best_candidate = format!(
        "sleep_prompt_candidate_{}_{}",
        slugify(&candidate.title),
        chrono::Utc::now().timestamp()
    );
    save_compiled_runtime_system_prompt_for_model(
        &context.config.main_model_config().model_id,
        &compiled,
    )
    .await?;
    context.compiled_prompts = context
        .compiled_prompts
        .clone()
        .with_runtime_system_prompt(Some(compiled));

    Ok(PromptPatchUpdate {
        applied_system_additions,
        compiled_prompt_updated: true,
    })
}
