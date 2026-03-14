use std::{collections::HashMap, path::PathBuf};

use miette::{Result, miette};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use tokio::fs;

use crate::get_spinova_home;

use super::{
    examples::{ExampleField, ProgramExample},
    optimizer::PromptTuningConfig,
    program::Program,
};

const COMPILED_DIR_NAME: &str = "reasoning_compiled";

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
}

#[derive(Clone, Default)]
pub struct CompiledPromptStore {
    entries: HashMap<String, CompiledProgram>,
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
        Self { entries }
    }

    pub fn insert(&mut self, entry: CompiledProgram) {
        self.entries.insert(entry.suite.clone(), entry);
    }

    pub fn get_tuning<P: Program>(&self, program: &P) -> Option<PromptTuningConfig<P::Output>> {
        self.entries
            .get(&program.tuning_key())
            .and_then(|entry| entry.tuning.to_typed::<P::Output>().ok())
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

pub async fn load_compiled_program(compile_key: &str) -> Result<Option<CompiledProgram>> {
    let path = compiled_dir().await.join(format!("{compile_key}.json"));
    let Ok(bytes) = fs::read(path).await else {
        return Ok(None);
    };

    let compiled = serde_json::from_slice::<CompiledProgram>(&bytes)
        .map_err(|err| miette!("failed to decode compiled prompt config: {err}"))?;
    Ok(Some(compiled))
}

pub async fn save_compiled_program(compiled: &CompiledProgram) -> Result<()> {
    let dir = compiled_dir().await;
    if !dir.exists() {
        fs::create_dir_all(&dir)
            .await
            .map_err(|err| miette!("failed to create compiled prompt dir: {err}"))?;
    }
    let path = dir.join(format!("{}.json", compiled.compile_key));
    let bytes = serde_json::to_vec_pretty(compiled)
        .map_err(|err| miette!("failed to serialize compiled prompt config: {err}"))?;
    fs::write(path, bytes)
        .await
        .map_err(|err| miette!("failed to write compiled prompt config: {err}"))?;
    Ok(())
}

async fn compiled_dir() -> PathBuf {
    get_spinova_home().await.join(COMPILED_DIR_NAME)
}
