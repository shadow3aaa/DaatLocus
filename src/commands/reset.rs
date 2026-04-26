use std::path::{Path, PathBuf};

use miette::{Result, miette};

use crate::{
    config::load_config,
    daat_locus_paths::{DaatLocusPaths, daat_locus_paths},
    daemon::connect_daemon_status,
    hindsight::{HindsightClient, llm_proxy::HindsightLlmProxy, managed::HindsightManagedServer},
    reasoning::compiled::COMPILED_DIR_NAME,
};

async fn get_daat_locus_home() -> PathBuf {
    daat_locus_paths().await.root().to_path_buf()
}

async fn reject_if_daemon_running(reset_name: &str) -> Result<()> {
    if connect_daemon_status().await.is_ok() {
        return Err(miette!(
            "{reset_name} refused while DaatLocus daemon is running; run `daat-locus daemon stop` first"
        ));
    }
    Ok(())
}

async fn clear_hindsight_bank(config: &crate::config::Config) -> Result<()> {
    let llm_proxy = HindsightLlmProxy::start(config)
        .await
        .map_err(|err| miette!("failed to start hindsight LLM proxy for memory-reset: {err}"))?;
    let llm_env_vars = llm_proxy.env_vars();
    let server = HindsightManagedServer::new(config.hindsight.clone(), llm_env_vars.clone());
    let was_running = server.check_health().await;
    if !was_running {
        server.start().await?;
    }

    let delete_result = async {
        let hindsight = HindsightClient::connect(&config.hindsight)
            .await?
            .with_restart_support(llm_env_vars)
            .with_llm_proxy(llm_proxy);
        hindsight.delete_bank().await
    }
    .await;

    if !was_running && let Err(err) = server.stop().await {
        tracing::warn!("[memory-reset] failed to stop temporary hindsight daemon: {err}");
    }

    delete_result
}

pub async fn run_memory_reset() -> Result<()> {
    let home = get_daat_locus_home().await;
    reject_if_daemon_running("memory reset").await?;
    clear_memory_state(&home).await?;

    println!(
        "[memory-reset] reset memory persistence under {}",
        home.display()
    );
    println!("[memory-reset] cleared: runtime_conversation, hindsight_queue");
    println!("[memory-reset] cleared: reasoning_traces.jsonl");
    println!("[memory-reset] cleared: hindsight bank, observations");
    println!("[memory-reset] cleared: current plan");
    println!("[memory-reset] preserved: config/, state/, artifacts/, logs/");

    Ok(())
}

async fn clear_memory_state(home: &Path) -> Result<()> {
    let config = load_config()
        .await
        .map_err(|err| miette!("failed to load config for memory-reset: {err}"))?;
    clear_hindsight_bank(&config).await?;
    let paths = DaatLocusPaths::from_root(home.to_path_buf());
    clear_files(&[
        paths.memory_file("runtime_conversation"),
        paths.memory_file("hindsight_queue"),
        paths.memory_file("plan"),
        paths.journal_file("reasoning_traces.jsonl"),
    ])
    .await?;

    Ok(())
}

pub async fn run_state_reset() -> Result<()> {
    let home = get_daat_locus_home().await;
    reject_if_daemon_running("state reset").await?;
    let cleared = clear_state_files(&home).await?;

    println!("[state-reset] reset runtime state under {}", home.display());
    if cleared.is_empty() {
        println!("[state-reset] nothing to remove");
    } else {
        println!("[state-reset] cleared: {}", cleared.join(", "));
    }
    println!("[state-reset] preserved: config/, memory/, artifacts/, logs/");

    Ok(())
}

async fn clear_state_files(home: &Path) -> Result<Vec<String>> {
    let paths = DaatLocusPaths::from_root(home.to_path_buf());
    let files = ["events", "pending_work_queue", "telegram_transport_state"];
    clear_named_files(paths.state_dir(), &files).await
}

pub async fn run_complite_reset() -> Result<()> {
    let home = get_daat_locus_home().await;
    reject_if_daemon_running("complite reset").await?;
    let cleared = clear_compiled_artifacts(&home).await?;

    println!(
        "[complite-reset] cleared compile/evaluation artifacts under {}",
        home.display()
    );
    if cleared.is_empty() {
        println!("[complite-reset] nothing to remove");
    } else {
        println!("[complite-reset] cleared: {}", cleared.join(", "));
    }
    println!("[complite-reset] preserved: config/, state/, memory, logs/");

    Ok(())
}

async fn clear_compiled_artifacts(home: &Path) -> Result<Vec<String>> {
    let mut cleared = Vec::new();
    let paths = DaatLocusPaths::from_root(home.to_path_buf());

    for dir_name in [COMPILED_DIR_NAME, "evaluations"] {
        let path = paths.artifact_dir(dir_name);
        if path.exists() {
            tokio::fs::remove_dir_all(&path)
                .await
                .map_err(|err| miette!("failed to remove {}: {err}", path.display()))?;
            cleared.push(dir_name.to_string());
        }
    }

    Ok(cleared)
}

pub async fn run_reset_all() -> Result<()> {
    let home = get_daat_locus_home().await;
    reject_if_daemon_running("reset all").await?;
    let memory_cleared = clear_memory_state(&home).await;
    let state_cleared = clear_state_files(&home).await?;
    let artifact_cleared = clear_compiled_artifacts(&home).await?;
    let log_cleared = clear_log_dirs(&home).await?;
    memory_cleared?;

    println!("[reset] reset all state under {}", home.display());
    if state_cleared.is_empty() {
        println!("[reset] cleared state: none");
    } else {
        println!("[reset] cleared state: {}", state_cleared.join(", "));
    }
    println!(
        "[reset] cleared memory: runtime_conversation, hindsight_queue, reasoning_traces.jsonl, hindsight bank, observations"
    );
    if artifact_cleared.is_empty() {
        println!("[reset] cleared complite artifacts: none");
    } else {
        println!(
            "[reset] cleared complite artifacts: {}",
            artifact_cleared.join(", ")
        );
    }
    if log_cleared.is_empty() {
        println!("[reset] cleared logs: none");
    } else {
        println!("[reset] cleared logs: {}", log_cleared.join(", "));
    }
    println!("[reset] preserved: config.toml, telegram_acl.json");

    Ok(())
}

async fn clear_log_dirs(home: &Path) -> Result<Vec<String>> {
    let mut cleared = Vec::new();
    let paths = DaatLocusPaths::from_root(home.to_path_buf());
    let path = paths.logs_dir();
    if path.exists() {
        tokio::fs::remove_dir_all(&path)
            .await
            .map_err(|err| miette!("failed to remove {}: {err}", path.display()))?;
        cleared.push("logs".to_string());
    }

    Ok(cleared)
}

async fn clear_named_files(dir: PathBuf, file_names: &[&str]) -> Result<Vec<String>> {
    let mut cleared = Vec::new();
    for file_name in file_names {
        let path = dir.join(file_name);
        if path.exists() {
            tokio::fs::remove_file(&path)
                .await
                .map_err(|err| miette!("failed to remove {}: {err}", path.display()))?;
            cleared.push((*file_name).to_string());
        }
    }
    Ok(cleared)
}

async fn clear_files(paths: &[PathBuf]) -> Result<()> {
    for path in paths {
        if path.exists() {
            tokio::fs::remove_file(path)
                .await
                .map_err(|err| miette!("failed to remove {}: {err}", path.display()))?;
        }
    }
    Ok(())
}
