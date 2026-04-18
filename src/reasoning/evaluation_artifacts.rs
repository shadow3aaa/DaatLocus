use std::path::{Path, PathBuf};

use miette::{Result, miette};
use serde::{Deserialize, Serialize};
use tokio::fs;
use uuid::Uuid;

use crate::daat_locus_paths::daat_locus_paths;
use crate::reasoning::examples::ExampleField;

const EVALUATIONS_DIR_NAME: &str = "evaluations";
const FAILURE_PATTERNS_DIR: &str = "failure_patterns";
const BOOTSTRAP_DEMOS_DIR: &str = "bootstrap_demos";
const STRESS_CASES_DIR: &str = "stress_cases";
const INSTRUCTION_HYPOTHESES_DIR: &str = "instruction_hypotheses";
const RUNTIME_DEMOS_DIR: &str = "runtime_demos";
const TURN_DEMOS_DIR: &str = "turn_demos";
const TURN_DEMO_EVALUATIONS_DIR: &str = "turn_demo_evaluations";
const RUNTIME_PROMPT_CANDIDATES_DIR: &str = "runtime_prompt_candidates";
const RUNTIME_PROMPT_EVOLUTION_REPORTS_DIR: &str = "runtime_prompt_evolution_reports";
const WORKFLOW_PATCHES_DIR: &str = "workflow_patches";
const WORKFLOW_MERGES_DIR: &str = "workflow_merges";
const MAX_ARTIFACT_FILE_STEM_LEN: usize = 96;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationArtifactSuggestedFixKind {
    Demo,
    Instruction,
    StressCase,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvaluationArtifactFailurePattern {
    pub suite: String,
    pub pattern_id: String,
    pub description: String,
    #[serde(default)]
    pub supporting_trace_ids: Vec<String>,
    pub frequency: usize,
    pub severity: u8,
    pub suggested_fix_kind: EvaluationArtifactSuggestedFixKind,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EvaluationArtifactBootstrapDemo {
    pub suite: String,
    pub title: String,
    pub input_summary: String,
    #[serde(default)]
    pub inputs: Vec<ExampleField>,
    pub expected_output: serde_json::Value,
    #[serde(default)]
    pub reference_case_names: Vec<String>,
    #[serde(default)]
    pub source_trace_ids: Vec<String>,
    pub confidence: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EvaluationArtifactStressCase {
    pub suite: String,
    pub name: String,
    pub input_ir: serde_json::Value,
    #[serde(default)]
    pub expected_constraints: Vec<String>,
    #[serde(default)]
    pub reference_case_names: Vec<String>,
    pub source_pattern_id: String,
    pub repeat: usize,
    pub weight: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvaluationArtifactInstructionHypothesis {
    pub suite: String,
    pub text: String,
    pub justification: String,
    #[serde(default)]
    pub source_pattern_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EvaluationArtifactRuntimeDemo {
    pub compile_key: String,
    pub title: String,
    pub scenario_summary: String,
    #[serde(default)]
    pub inputs: Vec<ExampleField>,
    pub expected_behavior: String,
    #[serde(default)]
    pub judge_focus: Vec<String>,
    #[serde(default)]
    pub source_trace_ids: Vec<String>,
    pub confidence: f32,
}

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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvaluationArtifactRuntimePromptEvolutionRound {
    pub round: usize,
    pub candidate: String,
    pub passed: usize,
    pub total_demos: usize,
    pub regressions: usize,
    pub rolled_back: bool,
    pub accepted: bool,
    #[serde(default)]
    pub suggestion_titles: Vec<String>,
    #[serde(default)]
    pub candidate_titles: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvaluationArtifactRuntimePromptEvolutionReport {
    pub compile_key: String,
    pub rounds: usize,
    pub accepted: bool,
    pub rolled_back: bool,
    pub passed: usize,
    pub total_demos: usize,
    pub regressions: usize,
    pub selected_candidate: String,
    #[serde(default)]
    pub selected_demo_titles: Vec<String>,
    #[serde(default)]
    pub final_system_additions: Vec<String>,
    #[serde(default)]
    pub round_history: Vec<EvaluationArtifactRuntimePromptEvolutionRound>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EvaluationArtifactWorkflowPatch {
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
pub struct EvaluationArtifactWorkflowMerge {
    pub target_workflow_id: String,
    #[serde(default)]
    pub source_workflow_ids: Vec<String>,
    pub rationale: String,
    pub confidence: f64,
    pub applied: bool,
}

pub struct PromptImprovementArtifacts<'a> {
    pub failure_patterns: &'a [EvaluationArtifactFailurePattern],
    pub bootstrap_demos: &'a [EvaluationArtifactBootstrapDemo],
    pub stress_cases: &'a [EvaluationArtifactStressCase],
    pub instruction_hypotheses: &'a [EvaluationArtifactInstructionHypothesis],
    pub runtime_demos: &'a [EvaluationArtifactRuntimeDemo],
    pub turn_demos: &'a [EvaluationArtifactTurnDemo],
}

pub struct WorkflowImprovementArtifacts<'a> {
    pub workflow_patches: &'a [EvaluationArtifactWorkflowPatch],
    pub workflow_merges: &'a [EvaluationArtifactWorkflowMerge],
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
        ensure_dir(&root.join(FAILURE_PATTERNS_DIR)).await?;
        ensure_dir(&root.join(BOOTSTRAP_DEMOS_DIR)).await?;
        ensure_dir(&root.join(STRESS_CASES_DIR)).await?;
        ensure_dir(&root.join(INSTRUCTION_HYPOTHESES_DIR)).await?;
        ensure_dir(&root.join(RUNTIME_DEMOS_DIR)).await?;
        ensure_dir(&root.join(TURN_DEMOS_DIR)).await?;
        ensure_dir(&root.join(TURN_DEMO_EVALUATIONS_DIR)).await?;
        ensure_dir(&root.join(RUNTIME_PROMPT_CANDIDATES_DIR)).await?;
        ensure_dir(&root.join(RUNTIME_PROMPT_EVOLUTION_REPORTS_DIR)).await?;
        ensure_dir(&root.join(WORKFLOW_PATCHES_DIR)).await?;
        ensure_dir(&root.join(WORKFLOW_MERGES_DIR)).await?;
        Ok(Self { root })
    }

    pub async fn replace_failure_patterns(
        &self,
        artifacts: &[EvaluationArtifactFailurePattern],
    ) -> Result<Vec<PathBuf>> {
        let artifacts = artifacts
            .iter()
            .cloned()
            .map(|artifact| (artifact.pattern_id.clone(), artifact))
            .collect::<Vec<_>>();
        replace_artifacts(&self.root.join(FAILURE_PATTERNS_DIR), artifacts).await
    }

    pub async fn replace_bootstrap_demos(
        &self,
        artifacts: &[EvaluationArtifactBootstrapDemo],
    ) -> Result<Vec<PathBuf>> {
        let artifacts = artifacts
            .iter()
            .cloned()
            .map(|artifact| {
                let slug = slugify(&artifact.title);
                (format!("{}-{}", artifact.suite, slug), artifact)
            })
            .collect::<Vec<_>>();
        replace_artifacts(&self.root.join(BOOTSTRAP_DEMOS_DIR), artifacts).await
    }

    pub async fn replace_stress_cases(
        &self,
        artifacts: &[EvaluationArtifactStressCase],
    ) -> Result<Vec<PathBuf>> {
        let artifacts = artifacts
            .iter()
            .cloned()
            .map(|artifact| (format!("{}-{}", artifact.suite, artifact.name), artifact))
            .collect::<Vec<_>>();
        replace_artifacts(&self.root.join(STRESS_CASES_DIR), artifacts).await
    }

    pub async fn replace_instruction_hypotheses(
        &self,
        artifacts: &[EvaluationArtifactInstructionHypothesis],
    ) -> Result<Vec<PathBuf>> {
        let artifacts = artifacts
            .iter()
            .cloned()
            .map(|artifact| {
                let slug = slugify(&artifact.text);
                (format!("{}-{}", artifact.suite, slug), artifact)
            })
            .collect::<Vec<_>>();
        replace_artifacts(&self.root.join(INSTRUCTION_HYPOTHESES_DIR), artifacts).await
    }

    pub async fn replace_runtime_demos(
        &self,
        artifacts: &[EvaluationArtifactRuntimeDemo],
    ) -> Result<Vec<PathBuf>> {
        let artifacts = artifacts
            .iter()
            .cloned()
            .map(|artifact| {
                let slug = slugify(&artifact.title);
                (format!("{}-{}", artifact.compile_key, slug), artifact)
            })
            .collect::<Vec<_>>();
        replace_artifacts(&self.root.join(RUNTIME_DEMOS_DIR), artifacts).await
    }

    pub async fn replace_turn_demos(
        &self,
        artifacts: &[EvaluationArtifactTurnDemo],
    ) -> Result<Vec<PathBuf>> {
        let artifacts = artifacts
            .iter()
            .cloned()
            .map(|artifact| {
                let slug = slugify(&artifact.title);
                (format!("{}-{}", artifact.compile_key, slug), artifact)
            })
            .collect::<Vec<_>>();
        replace_artifacts(&self.root.join(TURN_DEMOS_DIR), artifacts).await
    }

    pub async fn replace_turn_demo_evaluations(
        &self,
        artifacts: &[EvaluationArtifactTurnDemoEvaluation],
    ) -> Result<Vec<PathBuf>> {
        let artifacts = artifacts
            .iter()
            .cloned()
            .map(|artifact| {
                let slug = slugify(&artifact.demo_title);
                (format!("{}-{}", artifact.compile_key, slug), artifact)
            })
            .collect::<Vec<_>>();
        replace_artifacts(&self.root.join(TURN_DEMO_EVALUATIONS_DIR), artifacts).await
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

    pub async fn replace_runtime_prompt_evolution_reports(
        &self,
        artifacts: &[EvaluationArtifactRuntimePromptEvolutionReport],
    ) -> Result<Vec<PathBuf>> {
        let artifacts = artifacts
            .iter()
            .cloned()
            .map(|artifact| {
                (
                    format!("{}-rounds-{}", artifact.compile_key, artifact.rounds),
                    artifact,
                )
            })
            .collect::<Vec<_>>();
        replace_artifacts(
            &self.root.join(RUNTIME_PROMPT_EVOLUTION_REPORTS_DIR),
            artifacts,
        )
        .await
    }

    pub async fn replace_workflow_patches(
        &self,
        artifacts: &[EvaluationArtifactWorkflowPatch],
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

    pub async fn replace_prompt_improvement_artifacts(
        &self,
        artifacts: PromptImprovementArtifacts<'_>,
    ) -> Result<()> {
        self.replace_failure_patterns(artifacts.failure_patterns)
            .await?;
        self.replace_bootstrap_demos(artifacts.bootstrap_demos)
            .await?;
        self.replace_stress_cases(artifacts.stress_cases).await?;
        self.replace_instruction_hypotheses(artifacts.instruction_hypotheses)
            .await?;
        self.replace_runtime_demos(artifacts.runtime_demos).await?;
        self.replace_turn_demos(artifacts.turn_demos).await?;
        Ok(())
    }

    pub async fn replace_workflow_improvement_artifacts(
        &self,
        artifacts: WorkflowImprovementArtifacts<'_>,
    ) -> Result<()> {
        self.replace_workflow_patches(artifacts.workflow_patches)
            .await?;
        self.replace_workflow_merges(artifacts.workflow_merges)
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

async fn save_artifact<T>(dir: &Path, stem: &str, artifact: &T) -> Result<PathBuf>
where
    T: Serialize,
{
    let file_name = format!("{}-{}.json", artifact_file_stem(stem), Uuid::new_v4());
    let path = dir.join(file_name);
    let bytes = serde_json::to_vec_pretty(artifact)
        .map_err(|err| miette!("failed to serialize evaluation artifact: {err}"))?;
    fs::write(&path, bytes).await.map_err(|err| {
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
    fn runtime_prompt_evolution_report_roundtrip_preserves_round_history() {
        let report = EvaluationArtifactRuntimePromptEvolutionReport {
            compile_key: "runtime_agent_system".to_string(),
            rounds: 2,
            accepted: false,
            rolled_back: true,
            passed: 1,
            total_demos: 2,
            regressions: 1,
            selected_candidate: "candidate-b".to_string(),
            selected_demo_titles: vec!["demo-a".to_string()],
            final_system_additions: vec!["rule a".to_string(), "rule b".to_string()],
            round_history: vec![
                EvaluationArtifactRuntimePromptEvolutionRound {
                    round: 1,
                    candidate: "current".to_string(),
                    passed: 1,
                    total_demos: 2,
                    regressions: 0,
                    rolled_back: false,
                    accepted: false,
                    suggestion_titles: vec!["suggestion-1".to_string()],
                    candidate_titles: vec!["candidate-a".to_string()],
                },
                EvaluationArtifactRuntimePromptEvolutionRound {
                    round: 2,
                    candidate: "candidate-a".to_string(),
                    passed: 1,
                    total_demos: 2,
                    regressions: 1,
                    rolled_back: true,
                    accepted: false,
                    suggestion_titles: vec!["suggestion-2".to_string()],
                    candidate_titles: vec!["candidate-b".to_string()],
                },
            ],
        };

        let json = serde_json::to_string(&report).unwrap();
        let decoded: EvaluationArtifactRuntimePromptEvolutionReport =
            serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, report);
    }

    #[test]
    fn artifact_file_stem_is_bounded_and_non_empty() {
        let stem = artifact_file_stem(
            "tool call when a app is already in foreground state do not call focus app with unknown app id parameter",
        );
        assert!(!stem.is_empty());
        assert!(stem.len() <= MAX_ARTIFACT_FILE_STEM_LEN);
    }
}
