use std::path::{Path, PathBuf};

use miette::{Result, miette};
use serde::{Deserialize, Serialize};
use tokio::fs;
use uuid::Uuid;

use crate::get_spinova_home;
use crate::reasoning::examples::ExampleField;

const SLEEP_ARTIFACTS_DIR_NAME: &str = "sleep_artifacts";
const FAILURE_PATTERNS_DIR: &str = "failure_patterns";
const BOOTSTRAP_DEMOS_DIR: &str = "bootstrap_demos";
const STRESS_CASES_DIR: &str = "stress_cases";
const INSTRUCTION_HYPOTHESES_DIR: &str = "instruction_hypotheses";
const RUNTIME_DEMOS_DIR: &str = "runtime_demos";
const RUNTIME_PROMPT_SUGGESTIONS_DIR: &str = "runtime_prompt_suggestions";
const RUNTIME_DEMO_EVALUATIONS_DIR: &str = "runtime_demo_evaluations";
const RUNTIME_PROMPT_CANDIDATES_DIR: &str = "runtime_prompt_candidates";
const RUNTIME_PROMPT_EVOLUTION_REPORTS_DIR: &str = "runtime_prompt_evolution_reports";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SleepArtifactSuggestedFixKind {
    Demo,
    Instruction,
    StressCase,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SleepArtifactFailurePattern {
    pub suite: String,
    pub pattern_id: String,
    pub description: String,
    #[serde(default)]
    pub supporting_trace_ids: Vec<String>,
    pub frequency: usize,
    pub severity: u8,
    pub suggested_fix_kind: SleepArtifactSuggestedFixKind,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SleepArtifactBootstrapDemo {
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
pub struct SleepArtifactStressCase {
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
pub struct SleepArtifactInstructionHypothesis {
    pub suite: String,
    pub text: String,
    pub justification: String,
    #[serde(default)]
    pub source_pattern_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SleepArtifactRuntimeDemo {
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SleepArtifactRuntimePromptSuggestion {
    pub compile_key: String,
    pub title: String,
    pub rationale: String,
    #[serde(default)]
    pub suggested_additions: Vec<String>,
    #[serde(default)]
    pub source_demo_titles: Vec<String>,
    #[serde(default)]
    pub source_pattern_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SleepArtifactRuntimeDemoEvaluation {
    pub compile_key: String,
    pub demo_title: String,
    pub passed: bool,
    pub regression_detected: bool,
    pub confidence: f64,
    #[serde(default)]
    pub needed_changes: Vec<String>,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SleepArtifactRuntimePromptCandidate {
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
pub struct SleepArtifactRuntimePromptEvolutionRound {
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
pub struct SleepArtifactRuntimePromptEvolutionReport {
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
    pub round_history: Vec<SleepArtifactRuntimePromptEvolutionRound>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SleepArtifactsSnapshot {
    pub failure_patterns: Vec<SleepArtifactFailurePattern>,
    pub bootstrap_demos: Vec<SleepArtifactBootstrapDemo>,
    pub stress_cases: Vec<SleepArtifactStressCase>,
    pub instruction_hypotheses: Vec<SleepArtifactInstructionHypothesis>,
    pub runtime_demos: Vec<SleepArtifactRuntimeDemo>,
    pub runtime_prompt_suggestions: Vec<SleepArtifactRuntimePromptSuggestion>,
    pub runtime_demo_evaluations: Vec<SleepArtifactRuntimeDemoEvaluation>,
    pub runtime_prompt_candidates: Vec<SleepArtifactRuntimePromptCandidate>,
    pub runtime_prompt_evolution_reports: Vec<SleepArtifactRuntimePromptEvolutionReport>,
}

impl SleepArtifactsSnapshot {
    pub fn filter_suite(&self, suite: &str) -> Self {
        Self {
            failure_patterns: self
                .failure_patterns
                .iter()
                .filter(|item| item.suite == suite)
                .cloned()
                .collect(),
            bootstrap_demos: self
                .bootstrap_demos
                .iter()
                .filter(|item| item.suite == suite)
                .cloned()
                .collect(),
            stress_cases: self
                .stress_cases
                .iter()
                .filter(|item| item.suite == suite)
                .cloned()
                .collect(),
            instruction_hypotheses: self
                .instruction_hypotheses
                .iter()
                .filter(|item| item.suite == suite)
                .cloned()
                .collect(),
            runtime_demos: self
                .runtime_demos
                .iter()
                .filter(|item| item.compile_key == suite)
                .cloned()
                .collect(),
            runtime_prompt_suggestions: self
                .runtime_prompt_suggestions
                .iter()
                .filter(|item| item.compile_key == suite)
                .cloned()
                .collect(),
            runtime_demo_evaluations: self
                .runtime_demo_evaluations
                .iter()
                .filter(|item| item.compile_key == suite)
                .cloned()
                .collect(),
            runtime_prompt_candidates: self
                .runtime_prompt_candidates
                .iter()
                .filter(|item| item.compile_key == suite)
                .cloned()
                .collect(),
            runtime_prompt_evolution_reports: self
                .runtime_prompt_evolution_reports
                .iter()
                .filter(|item| item.compile_key == suite)
                .cloned()
                .collect(),
        }
    }
}

pub struct SleepArtifactsStore {
    root: PathBuf,
}

impl SleepArtifactsStore {
    pub async fn open() -> Result<Self> {
        let root = get_spinova_home().await.join(SLEEP_ARTIFACTS_DIR_NAME);
        ensure_dir(&root).await?;
        ensure_dir(&root.join(FAILURE_PATTERNS_DIR)).await?;
        ensure_dir(&root.join(BOOTSTRAP_DEMOS_DIR)).await?;
        ensure_dir(&root.join(STRESS_CASES_DIR)).await?;
        ensure_dir(&root.join(INSTRUCTION_HYPOTHESES_DIR)).await?;
        ensure_dir(&root.join(RUNTIME_DEMOS_DIR)).await?;
        ensure_dir(&root.join(RUNTIME_PROMPT_SUGGESTIONS_DIR)).await?;
        ensure_dir(&root.join(RUNTIME_DEMO_EVALUATIONS_DIR)).await?;
        ensure_dir(&root.join(RUNTIME_PROMPT_CANDIDATES_DIR)).await?;
        ensure_dir(&root.join(RUNTIME_PROMPT_EVOLUTION_REPORTS_DIR)).await?;
        Ok(Self { root })
    }

    pub async fn load_snapshot(&self) -> Result<SleepArtifactsSnapshot> {
        Ok(SleepArtifactsSnapshot {
            failure_patterns: load_all::<SleepArtifactFailurePattern>(
                &self.root.join(FAILURE_PATTERNS_DIR),
            )
            .await?,
            bootstrap_demos: load_all::<SleepArtifactBootstrapDemo>(
                &self.root.join(BOOTSTRAP_DEMOS_DIR),
            )
            .await?,
            stress_cases: load_all::<SleepArtifactStressCase>(&self.root.join(STRESS_CASES_DIR))
                .await?,
            instruction_hypotheses: load_all::<SleepArtifactInstructionHypothesis>(
                &self.root.join(INSTRUCTION_HYPOTHESES_DIR),
            )
            .await?,
            runtime_demos: load_all::<SleepArtifactRuntimeDemo>(&self.root.join(RUNTIME_DEMOS_DIR))
                .await?,
            runtime_prompt_suggestions: load_all::<SleepArtifactRuntimePromptSuggestion>(
                &self.root.join(RUNTIME_PROMPT_SUGGESTIONS_DIR),
            )
            .await?,
            runtime_demo_evaluations: load_all::<SleepArtifactRuntimeDemoEvaluation>(
                &self.root.join(RUNTIME_DEMO_EVALUATIONS_DIR),
            )
            .await?,
            runtime_prompt_candidates: load_all::<SleepArtifactRuntimePromptCandidate>(
                &self.root.join(RUNTIME_PROMPT_CANDIDATES_DIR),
            )
            .await?,
            runtime_prompt_evolution_reports: load_all::<SleepArtifactRuntimePromptEvolutionReport>(
                &self.root.join(RUNTIME_PROMPT_EVOLUTION_REPORTS_DIR),
            )
            .await?,
        })
    }

    pub async fn replace_failure_patterns(
        &self,
        artifacts: &[SleepArtifactFailurePattern],
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
        artifacts: &[SleepArtifactBootstrapDemo],
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
        artifacts: &[SleepArtifactStressCase],
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
        artifacts: &[SleepArtifactInstructionHypothesis],
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
        artifacts: &[SleepArtifactRuntimeDemo],
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

    pub async fn replace_runtime_prompt_suggestions(
        &self,
        artifacts: &[SleepArtifactRuntimePromptSuggestion],
    ) -> Result<Vec<PathBuf>> {
        let artifacts = artifacts
            .iter()
            .cloned()
            .map(|artifact| {
                let slug = slugify(&artifact.title);
                (format!("{}-{}", artifact.compile_key, slug), artifact)
            })
            .collect::<Vec<_>>();
        replace_artifacts(&self.root.join(RUNTIME_PROMPT_SUGGESTIONS_DIR), artifacts).await
    }

    pub async fn replace_runtime_demo_evaluations(
        &self,
        artifacts: &[SleepArtifactRuntimeDemoEvaluation],
    ) -> Result<Vec<PathBuf>> {
        let artifacts = artifacts
            .iter()
            .cloned()
            .map(|artifact| {
                let slug = slugify(&artifact.demo_title);
                (format!("{}-{}", artifact.compile_key, slug), artifact)
            })
            .collect::<Vec<_>>();
        replace_artifacts(&self.root.join(RUNTIME_DEMO_EVALUATIONS_DIR), artifacts).await
    }

    pub async fn replace_runtime_prompt_candidates(
        &self,
        artifacts: &[SleepArtifactRuntimePromptCandidate],
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
        artifacts: &[SleepArtifactRuntimePromptEvolutionReport],
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
        replace_artifacts(&self.root.join(RUNTIME_PROMPT_EVOLUTION_REPORTS_DIR), artifacts).await
    }
}

async fn ensure_dir(path: &Path) -> Result<()> {
    if !path.exists() {
        fs::create_dir_all(path).await.map_err(|err| {
            miette!(
                "failed to create sleep artifacts dir {}: {err}",
                path.display()
            )
        })?;
    }
    Ok(())
}

async fn load_all<T>(dir: &Path) -> Result<Vec<T>>
where
    T: for<'de> Deserialize<'de>,
{
    let mut entries = fs::read_dir(dir).await.map_err(|err| {
        miette!(
            "failed to read sleep artifacts dir {}: {err}",
            dir.display()
        )
    })?;
    let mut items = Vec::new();

    while let Some(entry) = entries.next_entry().await.map_err(|err| {
        miette!(
            "failed to iterate sleep artifacts dir {}: {err}",
            dir.display()
        )
    })? {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let bytes = fs::read(&path)
            .await
            .map_err(|err| miette!("failed to read sleep artifact {}: {err}", path.display()))?;
        let item = serde_json::from_slice::<T>(&bytes)
            .map_err(|err| miette!("failed to decode sleep artifact {}: {err}", path.display()))?;
        items.push(item);
    }

    Ok(items)
}

async fn save_artifact<T>(dir: &Path, stem: &str, artifact: &T) -> Result<PathBuf>
where
    T: Serialize,
{
    let file_name = format!("{}-{}.json", slugify(stem), Uuid::new_v4());
    let path = dir.join(file_name);
    let bytes = serde_json::to_vec_pretty(artifact)
        .map_err(|err| miette!("failed to serialize sleep artifact: {err}"))?;
    fs::write(&path, bytes)
        .await
        .map_err(|err| miette!("failed to write sleep artifact {}: {err}", path.display()))?;
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
                "failed to reset sleep artifacts dir {}: {err}",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_prompt_evolution_report_roundtrip_preserves_round_history() {
        let report = SleepArtifactRuntimePromptEvolutionReport {
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
                SleepArtifactRuntimePromptEvolutionRound {
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
                SleepArtifactRuntimePromptEvolutionRound {
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
        let decoded: SleepArtifactRuntimePromptEvolutionReport =
            serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, report);
    }
}
