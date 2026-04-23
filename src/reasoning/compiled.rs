use std::{collections::HashMap, path::PathBuf, time::SystemTime};

use miette::{Result, miette};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use tokio::fs;

use crate::daat_locus_paths::daat_locus_paths;

use super::{
    examples::{ExampleField, ProgramExample},
    optimizer::PromptTuningConfig,
    program::Program,
};

pub const COMPILED_DIR_NAME: &str = "reasoning_compiled";
pub const RUNTIME_SYSTEM_PROMPT_COMPILE_KEY: &str = "runtime_agent_system";
pub const RUNTIME_SYSTEM_PROMPT_PREVIOUS_COMPILE_KEY: &str = "runtime_agent_system_previous";

fn model_scoped_runtime_compile_key(base: &str, model_name: &str) -> String {
    let normalized = model_name
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if normalized.is_empty() {
        base.to_string()
    } else {
        format!("{base}--{normalized}")
    }
}

fn model_scope_suffix(model_name: &str) -> String {
    model_scoped_runtime_compile_key("", model_name)
}

fn stem_has_model_scope(stem: &str, model_name: &str) -> bool {
    let suffix = model_scope_suffix(model_name);
    !suffix.is_empty() && stem.ends_with(&suffix)
}

#[derive(Clone, Serialize, Deserialize)]
pub struct StoredProgramExample {
    pub title: String,
    pub inputs: Vec<ExampleField>,
    pub output: Value,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct StoredPromptTuningConfig {
    pub extra_instructions: Vec<String>,
    pub examples: Vec<StoredProgramExample>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CompiledProgram {
    pub suite: String,
    pub compile_key: String,
    pub best_candidate: String,
    pub score: usize,
    pub total_cases: usize,
    pub tuning: StoredPromptTuningConfig,
    #[serde(default)]
    pub report: Option<CompiledProgramReport>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CompiledProgramReport {
    pub train_score: usize,
    pub train_total_cases: usize,
    pub train_attempts_used: usize,
    #[serde(default)]
    pub acceptance_score: Option<usize>,
    #[serde(default)]
    pub acceptance_total_cases: Option<usize>,
    #[serde(default)]
    pub acceptance_attempts_used: Option<usize>,
    pub dev_score: usize,
    pub dev_total_cases: usize,
    pub dev_attempts_used: usize,
    #[serde(default)]
    pub ranking_label: Option<String>,
    pub selected_extra_instructions: Vec<String>,
    pub selected_example_titles: Vec<String>,
    pub candidates: Vec<CompiledCandidateReport>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CompiledCandidateReport {
    pub name: String,
    #[serde(default)]
    pub acceptance_score: Option<usize>,
    #[serde(default)]
    pub acceptance_total_cases: Option<usize>,
    #[serde(default)]
    pub acceptance_attempts_used: Option<usize>,
    pub score: usize,
    pub total_cases: usize,
    pub attempts_used: usize,
    #[serde(default)]
    pub judge_wins: usize,
    #[serde(default)]
    pub judge_losses: usize,
    #[serde(default)]
    pub judge_ties: usize,
    pub extra_instructions: Vec<String>,
    pub example_titles: Vec<String>,
    pub failed_cases: Vec<CompiledFailureCaseReport>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CompiledFailureCaseReport {
    pub case_name: String,
    pub detail: String,
}

#[derive(Clone, Default)]
pub struct CompiledPromptStore {
    entries: HashMap<String, CompiledProgram>,
    runtime_system_prompt: Option<CompiledRuntimeSystemPrompt>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CompiledRuntimeSystemPrompt {
    pub compile_key: String,
    pub best_candidate: String,
    #[serde(default)]
    pub system_additions: Vec<String>,
    #[serde(default)]
    pub selected_demo_titles: Vec<String>,
    #[serde(default)]
    pub report: Option<CompiledRuntimeSystemPromptReport>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CompiledRuntimeSystemPromptReport {
    pub score: usize,
    pub total_cases: usize,
    #[serde(default)]
    pub judge_summary: Option<String>,
}

impl CompiledRuntimeSystemPrompt {
    pub fn with_compile_key(mut self, compile_key: impl Into<String>) -> Self {
        self.compile_key = compile_key.into();
        self
    }
}

impl CompiledPromptStore {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn from_entries(entries: Vec<CompiledProgram>) -> Self {
        let entries = entries
            .into_iter()
            .map(|entry| (entry.suite.clone(), entry))
            .collect();
        Self {
            entries,
            runtime_system_prompt: None,
        }
    }

    pub fn with_runtime_system_prompt(
        mut self,
        runtime_system_prompt: Option<CompiledRuntimeSystemPrompt>,
    ) -> Self {
        self.runtime_system_prompt = runtime_system_prompt;
        self
    }

    pub fn get_tuning<P: Program>(&self, program: &P) -> Option<PromptTuningConfig<P::Output>> {
        self.entries
            .get(&program.tuning_key())
            .and_then(|entry| entry.tuning.to_typed::<P::Output>().ok())
    }

    pub fn runtime_system_additions(&self) -> &[String] {
        self.runtime_system_prompt
            .as_ref()
            .map(|prompt| prompt.system_additions.as_slice())
            .unwrap_or(&[])
    }
}

impl StoredPromptTuningConfig {
    pub fn from_typed<O: Serialize + Clone + DeserializeOwned>(
        tuning: &PromptTuningConfig<O>,
    ) -> Self {
        Self {
            extra_instructions: tuning.extra_instructions.clone(),
            examples: tuning
                .examples
                .iter()
                .map(|example| StoredProgramExample {
                    title: example.title.clone(),
                    inputs: example.inputs.clone(),
                    output: serde_json::to_value(&example.output).unwrap(),
                })
                .collect(),
        }
    }

    pub fn to_typed<O: Serialize + Clone + DeserializeOwned>(
        &self,
    ) -> Result<PromptTuningConfig<O>> {
        let mut examples = Vec::with_capacity(self.examples.len());
        for example in &self.examples {
            let output = serde_json::from_value::<O>(example.output.clone())
                .map_err(|err| miette!("failed to deserialize stored example output: {err}"))?;
            examples.push(ProgramExample {
                title: example.title.clone(),
                inputs: example.inputs.clone(),
                output,
            });
        }

        Ok(PromptTuningConfig {
            extra_instructions: self.extra_instructions.clone(),
            examples,
        })
    }
}

pub async fn load_all_compiled_programs_for_model(
    model_name: &str,
) -> Result<Vec<CompiledProgram>> {
    load_all_compiled_programs_from_dir(COMPILED_DIR_NAME, model_name).await
}

pub async fn load_all_compiled_programs_from_dir(
    dir_name: &str,
    model_name: &str,
) -> Result<Vec<CompiledProgram>> {
    let dir = compiled_dir_named(dir_name).await;
    let Ok(mut entries) = fs::read_dir(&dir).await else {
        return Ok(Vec::new());
    };

    let mut newest_by_suite: HashMap<String, (SystemTime, CompiledProgram)> = HashMap::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|err| miette!("failed to iterate compiled dir {}: {err}", dir.display()))?
    {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("");
        if stem == RUNTIME_SYSTEM_PROMPT_COMPILE_KEY
            || stem == RUNTIME_SYSTEM_PROMPT_PREVIOUS_COMPILE_KEY
            || stem.starts_with(&format!("{RUNTIME_SYSTEM_PROMPT_COMPILE_KEY}--"))
            || stem.starts_with(&format!("{RUNTIME_SYSTEM_PROMPT_PREVIOUS_COMPILE_KEY}--"))
        {
            continue;
        }
        if !stem_has_model_scope(stem, model_name) {
            continue;
        }
        let bytes = fs::read(&path).await.map_err(|err| {
            miette!(
                "failed to read compiled prompt config {}: {err}",
                path.display()
            )
        })?;
        let program = serde_json::from_slice::<CompiledProgram>(&bytes).map_err(|err| {
            miette!(
                "failed to decode compiled prompt config {}: {err}",
                path.display()
            )
        })?;
        let modified = entry
            .metadata()
            .await
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        match newest_by_suite.get(&program.suite) {
            Some((existing_modified, _)) if *existing_modified >= modified => {}
            _ => {
                newest_by_suite.insert(program.suite.clone(), (modified, program));
            }
        }
    }

    Ok(newest_by_suite
        .into_values()
        .map(|(_, program)| program)
        .collect())
}

pub async fn save_compiled_program_for_model(
    model_name: &str,
    compiled: &CompiledProgram,
) -> Result<()> {
    let dir = compiled_dir_named(COMPILED_DIR_NAME).await;
    if !dir.exists() {
        fs::create_dir_all(&dir)
            .await
            .map_err(|err| miette!("failed to create compiled prompt dir: {err}"))?;
    }
    let compile_key = model_scoped_runtime_compile_key(&compiled.compile_key, model_name);
    let path = dir.join(format!("{compile_key}.json"));
    let bytes = serde_json::to_vec_pretty(compiled)
        .map_err(|err| miette!("failed to serialize compiled prompt config: {err}"))?;
    fs::write(path, bytes)
        .await
        .map_err(|err| miette!("failed to write compiled prompt config: {err}"))?;
    Ok(())
}

pub async fn seed_compiled_program_from_tuning_for_model<P: Program>(
    model_name: &str,
    program: &P,
    tuning: &PromptTuningConfig<P::Output>,
) -> Result<()> {
    let compiled = CompiledProgram {
        suite: program.tuning_key(),
        compile_key: program.tuning_key(),
        best_candidate: "default_seed".to_string(),
        score: 0,
        total_cases: 0,
        tuning: StoredPromptTuningConfig::from_typed(tuning),
        report: None,
    };
    save_compiled_program_for_model(model_name, &compiled).await
}

pub async fn load_compiled_runtime_system_prompt_for_model(
    model_name: &str,
) -> Result<Option<CompiledRuntimeSystemPrompt>> {
    let scoped_key =
        model_scoped_runtime_compile_key(RUNTIME_SYSTEM_PROMPT_COMPILE_KEY, model_name);
    load_compiled_runtime_system_prompt_by_key(&scoped_key).await
}

pub async fn save_compiled_runtime_system_prompt_for_model(
    model_name: &str,
    compiled: &CompiledRuntimeSystemPrompt,
) -> Result<()> {
    let scoped_key =
        model_scoped_runtime_compile_key(RUNTIME_SYSTEM_PROMPT_COMPILE_KEY, model_name);
    save_compiled_runtime_system_prompt_by_key(&scoped_key, compiled).await
}

async fn load_compiled_runtime_system_prompt_by_key(
    compile_key: &str,
) -> Result<Option<CompiledRuntimeSystemPrompt>> {
    let path = compiled_dir_named(COMPILED_DIR_NAME)
        .await
        .join(format!("{compile_key}.json"));
    let Ok(bytes) = fs::read(path).await else {
        return Ok(None);
    };

    let compiled = serde_json::from_slice::<CompiledRuntimeSystemPrompt>(&bytes)
        .map_err(|err| miette!("failed to decode runtime system prompt config: {err}"))?;
    Ok(Some(compiled))
}

async fn save_compiled_runtime_system_prompt_by_key(
    compile_key: &str,
    compiled: &CompiledRuntimeSystemPrompt,
) -> Result<()> {
    let dir = compiled_dir_named(COMPILED_DIR_NAME).await;
    if !dir.exists() {
        fs::create_dir_all(&dir)
            .await
            .map_err(|err| miette!("failed to create compiled prompt dir: {err}"))?;
    }
    let path = dir.join(format!("{compile_key}.json"));
    let bytes = serde_json::to_vec_pretty(&compiled.clone().with_compile_key(compile_key))
        .map_err(|err| miette!("failed to serialize runtime system prompt config: {err}"))?;
    fs::write(path, bytes)
        .await
        .map_err(|err| miette!("failed to write runtime system prompt config: {err}"))?;
    Ok(())
}

async fn compiled_dir_named(dir_name: &str) -> PathBuf {
    daat_locus_paths().await.artifact_dir(dir_name)
}
