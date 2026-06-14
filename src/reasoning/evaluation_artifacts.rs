//! Evaluation artifact types used across offline evaluation and training runs.
//! Many items here exist for evaluation pipelines not linked into the main binary.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use miette::{Result, miette};
use serde::{Deserialize, Serialize};
use tokio::fs;
use uuid::Uuid;

use crate::reasoning::{examples::ExampleField, runtime_error::RuntimeErrorCase};
use crate::{
    daat_locus_paths::daat_locus_paths,
    persistence::{PersistenceFileMode, write_bytes_atomic},
};

const EVALUATIONS_DIR_NAME: &str = "evaluations";
const RUNTIME_ERROR_CASES_DIR: &str = "runtime_error_cases";
const PROMPT_REFLECTIONS_DIR: &str = "prompt_reflections";
const RUNTIME_PROMPT_CANDIDATES_DIR: &str = "runtime_prompt_candidates";
const RUNTIME_PROMPT_CANDIDATE_EVALUATIONS_DIR: &str = "runtime_prompt_candidate_evaluations";
const WORKFLOW_REFLECTIONS_DIR: &str = "workflow_reflections";
const WORKFLOW_PATCHES_DIR: &str = "workflow_patches";
const WORKFLOW_MERGES_DIR: &str = "workflow_merges";
const WORKFLOW_CANDIDATE_EVALUATIONS_DIR: &str = "workflow_candidate_evaluations";
const MAX_ARTIFACT_FILE_STEM_LEN: usize = 96;

const LEGACY_RUNTIME_ERROR_CORRECTION_DIRS: &[&str] = &[
    "failure_patterns",
    "bootstrap_demos",
    "stress_cases",
    "instruction_hypotheses",
    "runtime_demos",
    "turn_demos",
];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EvaluationArtifactTurnDemo {
    pub compile_key: String,
    pub title: String,
    pub scenario_summary: String,
    #[serde(default)]
    pub initial_inputs: Vec<ExampleField>,
    pub expected_behavior: String,
    #[serde(default)]
    pub judge_focus: Vec<String>,
    #[serde(default)]
    pub covered_tests: Vec<String>,
    pub must_use_tools: bool,
    #[serde(default)]
    pub must_not_final_answer_patterns: Vec<String>,
    pub must_end_with_terminal_answer: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EvaluationArtifactTurnDemoEvaluation {
    pub compile_key: String,
    pub demo_title: String,
    pub passed: bool,
    pub regression_detected: bool,
    pub confidence: f64,
    #[serde(default)]
    pub needed_changes: Vec<String>,
    pub reason: String,
    pub trace_summary: String,
    #[serde(default)]
    pub incoming_text: String,
    #[serde(default)]
    pub expected_behavior: String,
    #[serde(default)]
    pub judge_focus: Vec<String>,
    #[serde(default)]
    pub must_use_tools: bool,
    #[serde(default)]
    pub must_not_final_answer_patterns: Vec<String>,
    #[serde(default)]
    pub trace_rendered: String,
    #[serde(default)]
    pub final_assistant_message: String,
    #[serde(default)]
    pub final_reply_message: String,
    #[serde(default)]
    pub actions_rendered: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvaluationArtifactRuntimePromptCandidate {
    pub compile_key: String,
    pub title: String,
    pub rationale: String,
    #[serde(default)]
    pub prompt_patches: Vec<String>,
    #[serde(default)]
    pub source_demo_titles: Vec<String>,
    #[serde(default)]
    pub source_hypotheses: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EvaluationArtifactPromptReflection {
    pub compile_key: String,
    pub title: String,
    pub rationale: String,
    #[serde(default)]
    pub missing_instructions: Vec<String>,
    #[serde(default)]
    pub over_constraints: Vec<String>,
    #[serde(default)]
    pub source_trace_ids: Vec<String>,
    pub confidence: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EvaluationArtifactRuntimePromptCandidateEvaluation {
    pub compile_key: String,
    pub candidate_title: String,
    pub rationale: String,
    pub score: f64,
    pub accepted: bool,
    pub selected: bool,
    pub regressions_detected: usize,
    #[serde(default)]
    pub source_trace_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EvaluationArtifactPrimitiveSpecPatch {
    pub workflow_id: String,
    pub title: String,
    pub rationale: String,
    #[serde(default)]
    pub when_to_use_additions: Vec<String>,
    #[serde(default)]
    pub precondition_additions: Vec<String>,
    #[serde(default)]
    pub workflow_step_additions: Vec<String>,
    #[serde(default)]
    pub done_criteria_additions: Vec<String>,
    #[serde(default)]
    pub recovery_additions: Vec<String>,
    #[serde(default)]
    pub source_run_ids: Vec<String>,
    pub confidence: f64,
    pub applied: bool,
    pub rolled_back: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EvaluationArtifactWorkflowReflection {
    pub workflow_id: String,
    pub rationale: String,
    #[serde(default)]
    pub missing_preconditions: Vec<String>,
    #[serde(default)]
    pub weak_primitive_steps: Vec<String>,
    #[serde(default)]
    pub weak_done_criteria: Vec<String>,
    #[serde(default)]
    pub weak_recovery: Vec<String>,
    #[serde(default)]
    pub recurring_failure_patterns: Vec<String>,
    #[serde(default)]
    pub source_run_ids: Vec<String>,
    pub confidence: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EvaluationArtifactWorkflowMerge {
    pub target_workflow_id: String,
    #[serde(default)]
    pub source_workflow_ids: Vec<String>,
    pub rationale: String,
    pub confidence: f64,
    pub applied: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EvaluationArtifactWorkflowCandidateEvaluation {
    pub workflow_id: String,
    pub candidate_kind: String,
    pub candidate_title: String,
    pub rationale: String,
    pub score: f64,
    pub accepted: bool,
    pub selected: bool,
    #[serde(default)]
    pub source_run_ids: Vec<String>,
}

pub struct RuntimeErrorCorrectionArtifacts<'a> {
    pub runtime_error_cases: &'a [RuntimeErrorCase],
    pub prompt_reflections: &'a [EvaluationArtifactPromptReflection],
    pub runtime_prompt_candidates: &'a [EvaluationArtifactRuntimePromptCandidate],
    pub runtime_prompt_candidate_evaluations:
        &'a [EvaluationArtifactRuntimePromptCandidateEvaluation],
}

pub struct WorkflowImprovementArtifacts<'a> {
    pub workflow_reflections: &'a [EvaluationArtifactWorkflowReflection],
    pub workflow_patches: &'a [EvaluationArtifactPrimitiveSpecPatch],
    pub workflow_merges: &'a [EvaluationArtifactWorkflowMerge],
    pub workflow_candidate_evaluations: &'a [EvaluationArtifactWorkflowCandidateEvaluation],
}

pub struct EvaluationArtifactsStore {
    root: PathBuf,
}

impl EvaluationArtifactsStore {
    pub async fn open() -> Result<Self> {
        Self::open_scoped(None).await
    }

    pub async fn open_scoped(scope: Option<&str>) -> Result<Self> {
        let mut root = daat_locus_paths().await.artifact_dir(EVALUATIONS_DIR_NAME);
        if let Some(scope) = scope {
            root = root.join(artifact_file_stem(scope));
        }
        ensure_dir(&root).await?;
        ensure_dir(&root.join(RUNTIME_ERROR_CASES_DIR)).await?;
        ensure_dir(&root.join(PROMPT_REFLECTIONS_DIR)).await?;
        ensure_dir(&root.join(RUNTIME_PROMPT_CANDIDATES_DIR)).await?;
        ensure_dir(&root.join(RUNTIME_PROMPT_CANDIDATE_EVALUATIONS_DIR)).await?;
        ensure_dir(&root.join(WORKFLOW_REFLECTIONS_DIR)).await?;
        ensure_dir(&root.join(WORKFLOW_PATCHES_DIR)).await?;
        ensure_dir(&root.join(WORKFLOW_MERGES_DIR)).await?;
        ensure_dir(&root.join(WORKFLOW_CANDIDATE_EVALUATIONS_DIR)).await?;
        Ok(Self { root })
    }

    pub async fn replace_runtime_error_cases(
        &self,
        artifacts: &[RuntimeErrorCase],
    ) -> Result<Vec<PathBuf>> {
        let artifacts = artifacts
            .iter()
            .cloned()
            .map(|artifact| (artifact.case_id.clone(), artifact))
            .collect::<Vec<_>>();
        replace_artifacts(&self.root.join(RUNTIME_ERROR_CASES_DIR), artifacts).await
    }

    pub async fn replace_runtime_prompt_candidates(
        &self,
        artifacts: &[EvaluationArtifactRuntimePromptCandidate],
    ) -> Result<Vec<PathBuf>> {
        let artifacts = artifacts
            .iter()
            .cloned()
            .map(|artifact| {
                let slug = slugify(&artifact.title);
                (format!("{}-{}", artifact.compile_key, slug), artifact)
            })
            .collect::<Vec<_>>();
        replace_artifacts(&self.root.join(RUNTIME_PROMPT_CANDIDATES_DIR), artifacts).await
    }

    pub async fn replace_prompt_reflections(
        &self,
        artifacts: &[EvaluationArtifactPromptReflection],
    ) -> Result<Vec<PathBuf>> {
        let artifacts = artifacts
            .iter()
            .cloned()
            .map(|artifact| {
                let slug = slugify(&artifact.title);
                (format!("{}-{}", artifact.compile_key, slug), artifact)
            })
            .collect::<Vec<_>>();
        replace_artifacts(&self.root.join(PROMPT_REFLECTIONS_DIR), artifacts).await
    }

    pub async fn replace_runtime_prompt_candidate_evaluations(
        &self,
        artifacts: &[EvaluationArtifactRuntimePromptCandidateEvaluation],
    ) -> Result<Vec<PathBuf>> {
        let artifacts = artifacts
            .iter()
            .cloned()
            .map(|artifact| {
                let slug = slugify(&artifact.candidate_title);
                (format!("{}-{}", artifact.compile_key, slug), artifact)
            })
            .collect::<Vec<_>>();
        replace_artifacts(
            &self.root.join(RUNTIME_PROMPT_CANDIDATE_EVALUATIONS_DIR),
            artifacts,
        )
        .await
    }

    pub async fn replace_workflow_patches(
        &self,
        artifacts: &[EvaluationArtifactPrimitiveSpecPatch],
    ) -> Result<Vec<PathBuf>> {
        let artifacts = artifacts
            .iter()
            .cloned()
            .map(|artifact| {
                (
                    format!("{}-{}", artifact.workflow_id, slugify(&artifact.title)),
                    artifact,
                )
            })
            .collect::<Vec<_>>();
        replace_artifacts(&self.root.join(WORKFLOW_PATCHES_DIR), artifacts).await
    }

    pub async fn replace_workflow_reflections(
        &self,
        artifacts: &[EvaluationArtifactWorkflowReflection],
    ) -> Result<Vec<PathBuf>> {
        let artifacts = artifacts
            .iter()
            .cloned()
            .map(|artifact| (artifact.workflow_id.clone(), artifact))
            .collect::<Vec<_>>();
        replace_artifacts(&self.root.join(WORKFLOW_REFLECTIONS_DIR), artifacts).await
    }

    pub async fn replace_workflow_merges(
        &self,
        artifacts: &[EvaluationArtifactWorkflowMerge],
    ) -> Result<Vec<PathBuf>> {
        let artifacts = artifacts
            .iter()
            .cloned()
            .map(|artifact| (format!("{}-merge", artifact.target_workflow_id), artifact))
            .collect::<Vec<_>>();
        replace_artifacts(&self.root.join(WORKFLOW_MERGES_DIR), artifacts).await
    }

    pub async fn replace_workflow_candidate_evaluations(
        &self,
        artifacts: &[EvaluationArtifactWorkflowCandidateEvaluation],
    ) -> Result<Vec<PathBuf>> {
        let artifacts = artifacts
            .iter()
            .cloned()
            .map(|artifact| {
                let slug = slugify(&artifact.candidate_title);
                (
                    format!(
                        "{}-{}-{}",
                        artifact.workflow_id, artifact.candidate_kind, slug
                    ),
                    artifact,
                )
            })
            .collect::<Vec<_>>();
        replace_artifacts(
            &self.root.join(WORKFLOW_CANDIDATE_EVALUATIONS_DIR),
            artifacts,
        )
        .await
    }

    pub async fn replace_runtime_error_correction_artifacts(
        &self,
        artifacts: RuntimeErrorCorrectionArtifacts<'_>,
    ) -> Result<()> {
        self.replace_runtime_error_cases(artifacts.runtime_error_cases)
            .await?;
        for dir_name in LEGACY_RUNTIME_ERROR_CORRECTION_DIRS {
            reset_artifact_dir(&self.root.join(dir_name)).await?;
        }
        self.replace_prompt_reflections(artifacts.prompt_reflections)
            .await?;
        self.replace_runtime_prompt_candidates(artifacts.runtime_prompt_candidates)
            .await?;
        self.replace_runtime_prompt_candidate_evaluations(
            artifacts.runtime_prompt_candidate_evaluations,
        )
        .await?;
        Ok(())
    }

    pub async fn replace_workflow_improvement_artifacts(
        &self,
        artifacts: WorkflowImprovementArtifacts<'_>,
    ) -> Result<()> {
        self.replace_workflow_reflections(artifacts.workflow_reflections)
            .await?;
        self.replace_workflow_patches(artifacts.workflow_patches)
            .await?;
        self.replace_workflow_merges(artifacts.workflow_merges)
            .await?;
        self.replace_workflow_candidate_evaluations(artifacts.workflow_candidate_evaluations)
            .await?;
        Ok(())
    }
}

async fn ensure_dir(path: &Path) -> Result<()> {
    if !path.exists() {
        fs::create_dir_all(path).await.map_err(|err| {
            miette!(
                "failed to create evaluation artifacts dir {}: {err}",
                path.display()
            )
        })?;
    }
    Ok(())
}

async fn reset_artifact_dir(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_dir_all(path).await.map_err(|err| {
            miette!(
                "failed to reset evaluation artifacts dir {}: {err}",
                path.display()
            )
        })?;
    }
    ensure_dir(path).await
}

async fn save_artifact<T>(dir: &Path, stem: &str, artifact: &T) -> Result<PathBuf>
where
    T: Serialize,
{
    let file_name = format!("{}-{}.json", artifact_file_stem(stem), Uuid::new_v4());
    let path = dir.join(file_name);
    let bytes = serde_json::to_vec_pretty(artifact)
        .map_err(|err| miette!("failed to serialize evaluation artifact: {err}"))?;
    write_bytes_atomic(path.clone(), bytes, PersistenceFileMode::Default)
        .await
        .map_err(|err| {
            miette!(
                "failed to write evaluation artifact {}: {err}",
                path.display()
            )
        })?;
    Ok(path)
}

async fn replace_artifacts<S, T, I>(dir: &Path, artifacts: I) -> Result<Vec<PathBuf>>
where
    T: Serialize,
    S: AsRef<str>,
    I: IntoIterator<Item = (S, T)>,
{
    if dir.exists() {
        fs::remove_dir_all(dir).await.map_err(|err| {
            miette!(
                "failed to reset evaluation artifacts dir {}: {err}",
                dir.display()
            )
        })?;
    }
    ensure_dir(dir).await?;

    let mut paths = Vec::new();
    for (stem, artifact) in artifacts {
        let path = save_artifact(dir, stem.as_ref(), &artifact).await?;
        paths.push(path);
    }

    Ok(paths)
}

fn slugify(value: &str) -> String {
    let mut slug = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if matches!(ch, ' ' | '-' | '_' | '.') && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_string()
}

fn artifact_file_stem(value: &str) -> String {
    let slug = slugify(value);
    let slug = if slug.is_empty() {
        "artifact"
    } else {
        slug.as_str()
    };
    slug.chars().take(MAX_ARTIFACT_FILE_STEM_LEN).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_file_stem_is_bounded_and_non_empty() {
        let stem = artifact_file_stem(
            "tool call uses an unknown app id parameter and should be reported clearly",
        );
        assert!(!stem.is_empty());
        assert!(stem.len() <= MAX_ARTIFACT_FILE_STEM_LEN);
    }
}
