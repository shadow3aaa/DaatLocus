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

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SleepArtifactsSnapshot {
    pub failure_patterns: Vec<SleepArtifactFailurePattern>,
    pub bootstrap_demos: Vec<SleepArtifactBootstrapDemo>,
    pub stress_cases: Vec<SleepArtifactStressCase>,
    pub instruction_hypotheses: Vec<SleepArtifactInstructionHypothesis>,
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
        })
    }

    pub async fn replace_failure_patterns(
        &self,
        artifacts: &[SleepArtifactFailurePattern],
    ) -> Result<Vec<PathBuf>> {
        replace_artifacts(
            &self.root.join(FAILURE_PATTERNS_DIR),
            artifacts
                .iter()
                .map(|artifact| (artifact.pattern_id.as_str(), artifact)),
        )
        .await
    }

    pub async fn replace_bootstrap_demos(
        &self,
        artifacts: &[SleepArtifactBootstrapDemo],
    ) -> Result<Vec<PathBuf>> {
        replace_artifacts(
            &self.root.join(BOOTSTRAP_DEMOS_DIR),
            artifacts.iter().map(|artifact| {
                let slug = slugify(&artifact.title);
                (format!("{}-{}", artifact.suite, slug), artifact)
            }),
        )
        .await
    }

    pub async fn replace_stress_cases(
        &self,
        artifacts: &[SleepArtifactStressCase],
    ) -> Result<Vec<PathBuf>> {
        replace_artifacts(
            &self.root.join(STRESS_CASES_DIR),
            artifacts
                .iter()
                .map(|artifact| (format!("{}-{}", artifact.suite, artifact.name), artifact)),
        )
        .await
    }

    pub async fn replace_instruction_hypotheses(
        &self,
        artifacts: &[SleepArtifactInstructionHypothesis],
    ) -> Result<Vec<PathBuf>> {
        replace_artifacts(
            &self.root.join(INSTRUCTION_HYPOTHESES_DIR),
            artifacts.iter().map(|artifact| {
                let slug = slugify(&artifact.text);
                (format!("{}-{}", artifact.suite, slug), artifact)
            }),
        )
        .await
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

async fn replace_artifacts<'a, S, T, I>(dir: &Path, artifacts: I) -> Result<Vec<PathBuf>>
where
    T: Serialize + 'a,
    S: AsRef<str>,
    I: IntoIterator<Item = (S, &'a T)>,
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
        let path = save_artifact(dir, stem.as_ref(), artifact).await?;
        paths.push(path);
    }

    Ok(paths)
}

fn slugify(value: &str) -> String {
    let mut slug = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if matches!(ch, ' ' | '-' | '_' | '.') {
            if !slug.ends_with('-') {
                slug.push('-');
            }
        }
    }
    slug.trim_matches('-').to_string()
}
