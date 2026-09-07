//! Turn-granularity hot reload for `config.toml`.
//!
//! Settings written to the shared config file are picked up by the running
//! runtime without a daemon restart. The module compares a cheap filesystem
//! fingerprint (path + mtime + size) against the last seen fingerprint at the
//! top of every runtime loop cycle, reloads and validates the config, then
//! applies the parts that can change live:
//!
//! - models/providers/main/efficient model selection: model providers and
//!   compiled prompt stores are rebuilt;
//! - sandbox policy: the runtime sandbox policy is rebuilt;
//! - telegram: the manager daemon watches the config file itself and rebuilds
//!   its Telegram transport when the telegram section changes;
//! - judge / sleep / locale: copied into `Context.config`.
//!
//! The daemon port cannot change on a live daemon; a warning is surfaced.

use std::{
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use crate::{
    config::Config,
    context::Context,
    daat_locus_paths::daat_locus_paths_sync,
    providers::build_model_provider,
    runtime::bootstrap::{
        PersistentTokenUsageRole, load_compiled_prompts_only, sandbox_policy_for_runtime,
        wrap_model_provider_with_persistent_token_usage,
    },
};

/// Identity of `config.toml` used for fingerprint comparison.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigFileEntry {
    pub path: PathBuf,
    pub modified_ms: u64,
    pub len: u64,
}

/// Compute the fingerprint of a config file path. Exposed for tests.
pub fn config_file_fingerprint_at(path: &Path) -> Option<ConfigFileEntry> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    let modified_ms = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_millis() as u64);
    Some(ConfigFileEntry {
        path: path.to_path_buf(),
        modified_ms,
        len: metadata.len(),
    })
}

/// Compute the fingerprint of the current runtime config file.
pub fn current_config_fingerprint() -> Option<ConfigFileEntry> {
    config_file_fingerprint_at(&daat_locus_paths_sync().config_file("config.toml"))
}

/// Refresh the stored fingerprint so the next loop cycle does not treat the
/// current file as a new change (used when the runtime itself wrote the config).
pub fn refresh_config_fingerprint(context: &mut Context) {
    context.config_hot_reload_fingerprint = current_config_fingerprint();
}

/// Detect which live-applicable groups changed between two configs.
/// Returns group labels plus warnings for restart-only settings.
pub fn config_change_groups(old: &Config, new: &Config) -> (Vec<&'static str>, Vec<String>) {
    let mut groups = Vec::new();
    if old.providers != new.providers
        || old.models != new.models
        || old.main_model != new.main_model
        || old.efficient_model != new.efficient_model
    {
        groups.push("models/providers");
    }
    if old.sandbox != new.sandbox {
        groups.push("sandbox");
    }
    if old.telegram != new.telegram {
        groups.push("telegram");
    }
    if old.judge != new.judge {
        groups.push("judge");
    }
    if old.sleep != new.sleep {
        groups.push("sleep");
    }
    if old.locale != new.locale {
        groups.push("locale");
    }
    let mut warnings = Vec::new();
    if old.daemon.port != new.daemon.port {
        warnings.push("daemon port change applies after a daemon restart".to_string());
    }
    (groups, warnings)
}

/// Result of one config hot-reload attempt.
#[derive(Debug, Default)]
pub struct ConfigReloadOutcome {
    pub reloaded: bool,
    pub changed_groups: Vec<&'static str>,
    pub warnings: Vec<String>,
}

/// Build the user-visible status text for a reload outcome, or `None` when
/// there is nothing worth surfacing.
pub fn config_reload_status_text(outcome: &ConfigReloadOutcome) -> Option<String> {
    if !outcome.reloaded {
        return None;
    }
    if outcome.changed_groups.is_empty() && outcome.warnings.is_empty() {
        return None;
    }
    let mut text = format!("config hot-reloaded: {}", outcome.changed_groups.join(", "));
    for warning in &outcome.warnings {
        text.push_str(&format!("; {warning}"));
    }
    Some(text)
}

/// Reload `config.toml` when it changed since the last loop cycle.
/// Called at the top of every runtime loop; a no-op otherwise.
pub async fn maybe_reload_config(context: &mut Context) -> ConfigReloadOutcome {
    let fingerprint = current_config_fingerprint();
    if context.config_hot_reload_fingerprint.as_ref() == fingerprint.as_ref() {
        return ConfigReloadOutcome::default();
    }
    let Some(fingerprint) = fingerprint else {
        // Config file is missing; retry on a later cycle without log spam.
        context.config_hot_reload_fingerprint = None;
        return ConfigReloadOutcome::default();
    };

    let fresh = match crate::config::load_config().await {
        Ok(config) => config,
        Err(err) => {
            tracing::warn!("config hot reload skipped; keeping the running config: {err}");
            // Sticky until the file changes again so a broken file does not
            // retry on every loop cycle.
            context.config_hot_reload_fingerprint = Some(fingerprint);
            return ConfigReloadOutcome {
                reloaded: false,
                changed_groups: Vec::new(),
                warnings: vec![format!("config reload failed: {err}")],
            };
        }
    };

    let (changed_groups, warnings) = config_change_groups(&context.config, &fresh);
    let reloaded = !changed_groups.is_empty() || !warnings.is_empty();

    if changed_groups
        .iter()
        .any(|group| *group == "models/providers")
    {
        match rebuild_model_providers(context, &fresh).await {
            Ok(()) => {}
            Err(err) => {
                tracing::warn!("config hot reload skipped; provider rebuild failed: {err}");
                context.config_hot_reload_fingerprint = Some(fingerprint);
                return ConfigReloadOutcome {
                    reloaded: false,
                    changed_groups: Vec::new(),
                    warnings: vec![format!("config reload rejected: {err}")],
                };
            }
        }
        match load_compiled_prompts_only(&fresh).await {
            Ok(store) => context.compiled_prompts = store,
            Err(err) => {
                tracing::warn!("config hot reload: compiled prompt store reload failed: {err}")
            }
        }
    }

    if changed_groups.iter().any(|group| *group == "sandbox") {
        context.sandbox_policy =
            sandbox_policy_for_runtime(&fresh, Some(&context.execution_cwd)).await;
    }

    // Telegram transport rebuild is handled by the manager daemon, which
    // watches the same config file fingerprint independently.

    context.config = fresh;
    context.config_hot_reload_fingerprint = Some(fingerprint);
    ConfigReloadOutcome {
        reloaded,
        changed_groups,
        warnings,
    }
}

async fn rebuild_model_providers(context: &mut Context, config: &Config) -> miette::Result<()> {
    let main = build_model_provider(&config.main_model, config)
        .map_err(|err| miette::miette!("main model provider: {err}"))?;
    let main = wrap_model_provider_with_persistent_token_usage(
        PersistentTokenUsageRole::Main,
        config.main_model_config().model_id.clone(),
        main,
        context.token_usage_store.clone(),
    );
    let efficient = build_model_provider(&config.efficient_model, config)
        .map_err(|err| miette::miette!("efficient model provider: {err}"))?;
    let efficient = wrap_model_provider_with_persistent_token_usage(
        PersistentTokenUsageRole::Efficient,
        config.efficient_model_config().model_id.clone(),
        efficient,
        context.token_usage_store.clone(),
    );
    context.model_provider = main;
    context.efficient_model_provider = std::sync::Arc::from(efficient);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ModelConfig, ProviderConfig};

    fn sample_config() -> Config {
        Config {
            providers: std::collections::HashMap::new(),
            models: std::collections::HashMap::new(),
            ..Config::default()
        }
    }

    #[test]
    fn change_groups_reports_live_groups_and_port_warning() {
        let old = sample_config();
        let mut changed_models = sample_config();
        changed_models.main_model = "other".to_string();
        let (groups, warnings) = config_change_groups(&old, &changed_models);
        assert!(groups.contains(&"models/providers"));
        assert!(warnings.is_empty());

        let mut changed_sleep = sample_config();
        changed_sleep.sleep.enabled = false;
        let (groups, warnings) = config_change_groups(&old, &changed_sleep);
        assert!(groups.contains(&"sleep"));
        assert!(warnings.is_empty());

        let mut changed_port = sample_config();
        changed_port.daemon.port = 9999;
        let (groups, warnings) = config_change_groups(&old, &changed_port);
        assert!(groups.is_empty());
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn change_groups_detects_sub_config_changes() {
        let old = sample_config();
        let mut changed = sample_config();
        changed.telegram.enabled = false;
        assert!(config_change_groups(&old, &changed).0.contains(&"telegram"));

        let mut changed = sample_config();
        changed.judge.max_pairwise_cases = 2;
        assert!(config_change_groups(&old, &changed).0.contains(&"judge"));

        let mut changed = sample_config();
        changed.sandbox.strong_filesystem = crate::sandbox::StrongFilesystemSandboxMode::Required;
        assert!(config_change_groups(&old, &changed).0.contains(&"sandbox"));

        let mut changed = sample_config();
        changed.locale = crate::i18n::Locale::ZhCn;
        assert!(config_change_groups(&old, &changed).0.contains(&"locale"));

        let mut changed = sample_config();
        changed
            .models
            .insert("m".to_string(), ModelConfig::default());
        assert!(
            config_change_groups(&old, &changed)
                .0
                .contains(&"models/providers")
        );

        let mut changed = sample_config();
        changed.providers.insert(
            "p".to_string(),
            ProviderConfig::Openai {
                api_key: "k".to_string(),
                base_url: None,
            },
        );
        assert!(
            config_change_groups(&old, &changed)
                .0
                .contains(&"models/providers")
        );
    }

    #[test]
    fn fingerprint_tracks_file_metadata_changes() {
        let dir = std::env::temp_dir().join(format!("daat-locus-fmt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "a").unwrap();
        let first = config_file_fingerprint_at(&path).expect("fingerprint");
        let second = config_file_fingerprint_at(&path).expect("fingerprint");
        assert_eq!(first, second);
        std::fs::write(&path, "longer content").unwrap();
        let third = config_file_fingerprint_at(&path).expect("fingerprint");
        assert_ne!(first, third);
        assert_eq!(third.path, first.path);
        std::fs::remove_dir_all(&dir).ok();
    }
}
