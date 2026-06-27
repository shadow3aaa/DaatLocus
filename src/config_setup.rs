use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Duration,
};

use miette::{Result, miette};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    config::{
        Config, DaemonConfig, ModelConfig, ProviderConfig, TelegramConfig, ThinkingBudget,
        load_config, normalize_provider_base_url, write_config,
    },
    daat_locus_paths::daat_locus_paths,
    i18n::Locale,
    model_catalog::ReasoningOption,
    model_discovery::{
        DiscoveredModel, discover_model_ids, reasoning_options_for_prompt, resolve_model_capacity,
    },
    open_url::open_url,
    persistence::{PersistenceFileMode, write_bytes_atomic},
    providers::{
        CodexOAuthTokens, codex_cli_auth_file, codex_oauth_auth_file,
        import_codex_cli_oauth_tokens, write_codex_oauth_tokens,
    },
    reasoning::turn_compile::{
        PromptPersonaSpec, load_prompt_persona_spec_sync, prompt_persona_path_sync,
        render_prompt_persona_markdown,
    },
};

const CONFIG_FILE_NAME: &str = "config.toml";
const CONFIG_BACKUP_FILE_NAME: &str = "config.toml.bak";
const GITHUB_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
const GITHUB_DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const GITHUB_ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const CODEX_OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CODEX_OAUTH_ISSUER: &str = "https://auth.openai.com";
const CODEX_DEVICE_USER_CODE_PATH: &str = "/api/accounts/deviceauth/usercode";
const CODEX_DEVICE_TOKEN_PATH: &str = "/api/accounts/deviceauth/token";
const CODEX_OAUTH_TOKEN_PATH: &str = "/oauth/token";

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigReadinessKind {
    Unconfigured,
    Incomplete,
    Complete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigReadinessReport {
    pub kind: ConfigReadinessKind,
    pub config_path: String,
    pub backup_path: String,
    pub port: u16,
    pub message: String,
    pub recovery_note: Option<String>,
}

impl ConfigReadinessReport {
    pub fn is_complete(&self) -> bool {
        self.kind == ConfigReadinessKind::Complete
    }

    pub fn agent_unavailable_message(&self) -> String {
        format!("agent configuration is not ready: {}", self.message)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ManagerBootConfig {
    pub port: u16,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SetupConfigRequest {
    #[serde(default)]
    pub locale: Option<Locale>,
    #[serde(default)]
    pub persona_name: Option<String>,
    #[serde(default)]
    pub persona_language: Option<String>,
    #[serde(default)]
    pub providers: Vec<SetupProviderRequest>,
    #[serde(default)]
    pub models: Vec<SetupModelRequest>,
    #[serde(default)]
    pub main_model: Option<String>,
    #[serde(default)]
    pub efficient_model: Option<String>,
    #[serde(default)]
    pub provider_kind: Option<SetupProviderKind>,
    #[serde(default)]
    pub provider_name: Option<String>,
    #[serde(default)]
    pub main_model_name: Option<String>,
    #[serde(default)]
    pub main_model_id: Option<String>,
    #[serde(default)]
    pub efficient_model_name: Option<String>,
    #[serde(default)]
    pub efficient_model_id: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub daemon_port: Option<u16>,
    #[serde(default)]
    pub telegram_enabled: Option<bool>,
    #[serde(default)]
    pub telegram_bot_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupProviderRequest {
    pub kind: SetupProviderKind,
    pub name: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub keep_alive: Option<String>,
    #[serde(default)]
    pub codex_auth_method: Option<SetupCodexAuthMethod>,
    #[serde(default)]
    pub codex_auth_file: Option<String>,
    #[serde(default)]
    pub github_auth_method: Option<SetupGithubAuthMethod>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupModelRequest {
    pub name: String,
    pub provider_name: String,
    pub model_id: String,
    #[serde(default)]
    pub context_window_tokens: Option<usize>,
    #[serde(default)]
    pub max_completion_tokens: Option<usize>,
    #[serde(default)]
    pub supports_vision: Option<bool>,
    #[serde(default)]
    pub thinking_budget: Option<String>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub rpm: Option<u32>,
    #[serde(default)]
    pub request_timeout_secs: Option<u64>,
    #[serde(default)]
    pub stream_idle_timeout_secs: Option<u64>,
    #[serde(default)]
    pub auto_compact_token_limit: Option<usize>,
    #[serde(default)]
    pub effective_context_window_percent: Option<i64>,
    #[serde(default)]
    pub tool_output_max_tokens: Option<usize>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupProviderKind {
    Openai,
    OpenaiCompatible,
    OpenaiCodexOauth,
    GithubCopilot,
    Ollama,
    OllamaCloud,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupCodexAuthMethod {
    BrowserLogin,
    DeviceLogin,
    ImportLocalCodex,
    ImportAuthFile,
    ExistingAuthFile,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupGithubAuthMethod {
    DeviceLogin,
    ManualToken,
    EnvToken,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupDiscoverModelsRequest {
    pub provider: SetupProviderRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupDiscoverModelsResponse {
    pub models: Vec<SetupDiscoveredModel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupProviderAuthRunRequest {
    pub provider: SetupProviderRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupProviderAuthStartRequest {
    pub provider: SetupProviderRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupProviderAuthStartResponse {
    pub flow_id: String,
    pub provider_kind: SetupProviderKind,
    pub verification_url: String,
    pub user_code: String,
    pub expires_at_ms: i64,
    pub interval_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupProviderAuthCompleteRequest {
    pub provider: SetupProviderRequest,
    pub flow_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupProviderAuthResponse {
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub auth_file: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone)]
pub enum PendingSetupProviderAuthFlow {
    GithubDevice {
        flow_id: String,
        device_code: String,
        expires_at_ms: i64,
    },
    CodexDevice {
        flow_id: String,
        device_auth_id: String,
        user_code: String,
        expires_at_ms: i64,
    },
}

impl PendingSetupProviderAuthFlow {
    pub fn flow_id(&self) -> &str {
        match self {
            Self::GithubDevice { flow_id, .. } | Self::CodexDevice { flow_id, .. } => flow_id,
        }
    }

    pub fn expires_at_ms(&self) -> i64 {
        match self {
            Self::GithubDevice { expires_at_ms, .. } | Self::CodexDevice { expires_at_ms, .. } => {
                *expires_at_ms
            }
        }
    }

    pub fn is_expired(&self) -> bool {
        chrono::Utc::now().timestamp_millis() >= self.expires_at_ms()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupDiscoveredModel {
    pub id: String,
    #[serde(default)]
    pub context_window_tokens: Option<usize>,
    #[serde(default)]
    pub max_completion_tokens: Option<usize>,
    #[serde(default)]
    pub supports_vision: Option<bool>,
    #[serde(default)]
    pub thinking_budgets: Vec<String>,
}

#[derive(Debug)]
struct ReadinessParts {
    kind: ConfigReadinessKind,
    port: u16,
    message: String,
}

#[derive(Debug)]
enum ConfigReadOutcome {
    Missing,
    Damaged { error: String, port: Option<u16> },
    Parsed(ReadinessParts),
}

#[derive(Debug, Deserialize)]
struct BootToml {
    #[serde(default)]
    daemon: BootDaemonConfig,
}

#[derive(Debug, Default, Deserialize)]
struct BootDaemonConfig {
    port: Option<u16>,
}

pub async fn read_manager_boot_config() -> ManagerBootConfig {
    let paths = daat_locus_paths().await;
    let config_path = paths.config_file(CONFIG_FILE_NAME);
    let backup_path = paths.config_file(CONFIG_BACKUP_FILE_NAME);
    let default_port = DaemonConfig::default().port;

    if let Some(port) = read_boot_port_from_path(&config_path).await {
        return ManagerBootConfig { port };
    }
    if let Some(port) = read_boot_port_from_path(&backup_path).await {
        return ManagerBootConfig { port };
    }
    ManagerBootConfig { port: default_port }
}

pub async fn ensure_config_readiness() -> ConfigReadinessReport {
    let paths = daat_locus_paths().await;
    let config_path = paths.config_file(CONFIG_FILE_NAME);
    let backup_path = paths.config_file(CONFIG_BACKUP_FILE_NAME);
    let default_port = DaemonConfig::default().port;

    match read_config_readiness_from_path(&config_path).await {
        ConfigReadOutcome::Parsed(parts) => {
            if let Err(err) = update_config_backup(&config_path, &backup_path).await {
                tracing::warn!("failed to update config backup: {err}");
            }
            report(parts, config_path, backup_path, None)
        }
        ConfigReadOutcome::Missing => {
            let note = match write_setup_safe_default_config(&config_path, default_port).await {
                Ok(()) => {
                    if let Err(err) = update_config_backup(&config_path, &backup_path).await {
                        tracing::warn!("failed to update setup-safe config backup: {err}");
                    }
                    Some(format!(
                        "config.toml was missing; wrote setup-safe defaults with daemon.port={default_port}"
                    ))
                }
                Err(err) => Some(format!(
                    "config.toml is missing and setup-safe defaults could not be written: {err}"
                )),
            };
            report(
                ReadinessParts {
                    kind: ConfigReadinessKind::Unconfigured,
                    port: default_port,
                    message: "configuration has not been initialized".to_string(),
                },
                config_path,
                backup_path,
                note,
            )
        }
        ConfigReadOutcome::Damaged { error, port } => {
            recover_damaged_config(
                config_path,
                backup_path,
                port.unwrap_or(default_port),
                error,
            )
            .await
        }
    }
}

pub async fn load_complete_config() -> Result<Config> {
    let readiness = ensure_config_readiness().await;
    if !readiness.is_complete() {
        return Err(miette!(readiness.agent_unavailable_message()));
    }
    load_config()
        .await
        .map_err(|err| miette!("failed to load complete config: {err}"))
}

pub async fn write_setup_config(request: SetupConfigRequest) -> Result<ConfigReadinessReport> {
    prepare_setup_provider_credentials(&request.providers).await?;
    let base_config = load_config().await.unwrap_or_else(|_| Config::default());
    let config = config_from_setup_request_with_base(&request, base_config)?;
    config
        .validate()
        .map_err(|err| miette!("setup config is internally invalid: {err}"))?;
    write_config(&config)
        .await
        .map_err(|err| miette!("failed to write setup config: {err}"))?;
    write_setup_persona(&request)
        .await
        .map_err(|err| miette!("failed to write setup persona: {err}"))?;
    Ok(ensure_config_readiness().await)
}

pub async fn read_setup_config() -> Result<SetupConfigRequest> {
    let config = load_config()
        .await
        .map_err(|err| miette!("failed to load setup config: {err}"))?;
    Ok(setup_config_from_config(&config))
}

pub async fn preview_setup_config(request: SetupConfigRequest) -> Result<ConfigReadinessReport> {
    let config = config_from_setup_request(&request)?;
    let text = toml::to_string_pretty(&config)
        .map_err(|err| miette!("encode setup config failed: {err}"))?;
    let parts = match read_config_readiness_from_str(&text) {
        ConfigReadOutcome::Parsed(parts) => parts,
        ConfigReadOutcome::Missing => ReadinessParts {
            kind: ConfigReadinessKind::Unconfigured,
            port: config.daemon.port,
            message: "setup preview produced no config".to_string(),
        },
        ConfigReadOutcome::Damaged { error, .. } => ReadinessParts {
            kind: ConfigReadinessKind::Incomplete,
            port: config.daemon.port,
            message: error,
        },
    };
    let paths = daat_locus_paths().await;
    Ok(report(
        parts,
        paths.config_file(CONFIG_FILE_NAME),
        paths.config_file(CONFIG_BACKUP_FILE_NAME),
        None,
    ))
}

pub async fn discover_setup_models(
    request: SetupDiscoverModelsRequest,
) -> Result<SetupDiscoverModelsResponse> {
    prepare_setup_provider_credentials(std::slice::from_ref(&request.provider)).await?;
    let name = required_name(&request.provider.name, "provider.name")?;
    let provider = provider_from_setup_provider(&request.provider)?;
    let models = discover_model_ids(&name, &provider)
        .await?
        .into_iter()
        .map(|model| setup_discovered_model(&provider, model))
        .collect();
    Ok(SetupDiscoverModelsResponse { models })
}

pub async fn run_setup_provider_auth(
    request: SetupProviderAuthRunRequest,
) -> Result<SetupProviderAuthResponse> {
    match request.provider.kind {
        SetupProviderKind::OpenaiCodexOauth => {
            run_codex_setup_provider_auth(&request.provider).await
        }
        SetupProviderKind::GithubCopilot => Err(miette!(
            "GitHub Copilot device login must be started before it can be completed"
        )),
        SetupProviderKind::Openai
        | SetupProviderKind::OpenaiCompatible
        | SetupProviderKind::OllamaCloud => {
            if request
                .provider
                .api_key
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty()
            {
                Err(miette!("provider API key is required"))
            } else {
                Ok(SetupProviderAuthResponse {
                    api_key: request.provider.api_key,
                    auth_file: None,
                    message: "API key is ready".to_string(),
                })
            }
        }
        SetupProviderKind::Ollama => Ok(SetupProviderAuthResponse {
            api_key: request.provider.api_key,
            auth_file: None,
            message: "Ollama provider does not require setup authentication".to_string(),
        }),
    }
}

pub async fn start_setup_provider_auth(
    request: SetupProviderAuthStartRequest,
) -> Result<(SetupProviderAuthStartResponse, PendingSetupProviderAuthFlow)> {
    match request.provider.kind {
        SetupProviderKind::OpenaiCodexOauth => start_codex_device_auth().await,
        SetupProviderKind::GithubCopilot => start_github_device_auth().await,
        _ => Err(miette!(
            "selected provider type does not support device authentication"
        )),
    }
}

pub async fn complete_setup_provider_auth(
    request: SetupProviderAuthCompleteRequest,
    flow: PendingSetupProviderAuthFlow,
) -> Result<SetupProviderAuthResponse> {
    if request.flow_id != flow.flow_id() {
        return Err(miette!("setup authentication flow id mismatch"));
    }
    if flow.is_expired() {
        return Err(miette!("setup authentication flow expired"));
    }

    match (request.provider.kind, flow) {
        (
            SetupProviderKind::GithubCopilot,
            PendingSetupProviderAuthFlow::GithubDevice { device_code, .. },
        ) => complete_github_device_auth(device_code).await,
        (
            SetupProviderKind::OpenaiCodexOauth,
            PendingSetupProviderAuthFlow::CodexDevice {
                device_auth_id,
                user_code,
                ..
            },
        ) => complete_codex_device_auth(&request.provider, device_auth_id, user_code).await,
        _ => Err(miette!(
            "setup authentication flow does not match the selected provider"
        )),
    }
}

fn setup_config_from_config(config: &Config) -> SetupConfigRequest {
    let persona = load_prompt_persona_spec_sync();
    let mut providers = config
        .providers
        .iter()
        .map(|(name, provider)| setup_provider_from_config(name, provider))
        .collect::<Vec<_>>();
    providers.sort_by(|a, b| a.name.cmp(&b.name));

    let mut models = config
        .models
        .iter()
        .map(|(name, model)| setup_model_from_config(name, model))
        .collect::<Vec<_>>();
    models.sort_by(|a, b| a.name.cmp(&b.name));

    SetupConfigRequest {
        locale: Some(config.locale),
        persona_name: Some(persona.name),
        persona_language: Some(persona.language),
        providers,
        models,
        main_model: Some(config.main_model.clone()),
        efficient_model: Some(config.efficient_model.clone()),
        daemon_port: Some(config.daemon.port),
        telegram_enabled: Some(config.telegram.enabled),
        telegram_bot_token: Some(config.telegram.bot_token.clone()),
        ..SetupConfigRequest::default()
    }
}

fn setup_provider_from_config(name: &str, provider: &ProviderConfig) -> SetupProviderRequest {
    match provider {
        ProviderConfig::Openai { api_key, base_url } => SetupProviderRequest {
            kind: SetupProviderKind::Openai,
            name: name.to_string(),
            api_key: Some(api_key.clone()),
            base_url: base_url.clone(),
            keep_alive: None,
            codex_auth_method: None,
            codex_auth_file: None,
            github_auth_method: None,
        },
        ProviderConfig::GithubCopilot { github_token } => SetupProviderRequest {
            kind: SetupProviderKind::GithubCopilot,
            name: name.to_string(),
            api_key: Some(github_token.clone()),
            base_url: None,
            keep_alive: None,
            codex_auth_method: None,
            codex_auth_file: None,
            github_auth_method: Some(if looks_like_env_reference(github_token) {
                SetupGithubAuthMethod::EnvToken
            } else {
                SetupGithubAuthMethod::ManualToken
            }),
        },
        ProviderConfig::OpenaiCodexOauth { base_url } => SetupProviderRequest {
            kind: SetupProviderKind::OpenaiCodexOauth,
            name: name.to_string(),
            api_key: None,
            base_url: base_url.clone(),
            keep_alive: None,
            codex_auth_method: Some(SetupCodexAuthMethod::ExistingAuthFile),
            codex_auth_file: Some(codex_oauth_auth_file(name).to_string_lossy().to_string()),
            github_auth_method: None,
        },
        ProviderConfig::OpenaiCompatible {
            base_url, api_key, ..
        } => SetupProviderRequest {
            kind: SetupProviderKind::OpenaiCompatible,
            name: name.to_string(),
            api_key: Some(api_key.clone()),
            base_url: Some(base_url.clone()),
            keep_alive: None,
            codex_auth_method: None,
            codex_auth_file: None,
            github_auth_method: None,
        },
        ProviderConfig::Ollama {
            host,
            api_key,
            keep_alive,
        } => {
            let normalized_host = host.as_deref().map(normalize_provider_base_url);
            let is_cloud = normalized_host.as_deref() == Some("https://ollama.com")
                && api_key
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty());
            SetupProviderRequest {
                kind: if is_cloud {
                    SetupProviderKind::OllamaCloud
                } else {
                    SetupProviderKind::Ollama
                },
                name: name.to_string(),
                api_key: api_key.clone(),
                base_url: host.clone(),
                keep_alive: keep_alive.clone(),
                codex_auth_method: None,
                codex_auth_file: None,
                github_auth_method: None,
            }
        }
    }
}

fn setup_model_from_config(name: &str, model: &ModelConfig) -> SetupModelRequest {
    SetupModelRequest {
        name: name.to_string(),
        provider_name: model.provider.clone(),
        model_id: model.model_id.clone(),
        context_window_tokens: Some(model.context_window_tokens),
        max_completion_tokens: Some(model.max_completion_tokens),
        supports_vision: model.supports_vision,
        thinking_budget: model
            .thinking_budget()
            .map(|budget| budget.as_str().to_string()),
        temperature: Some(model.temperature),
        rpm: model.rpm,
        request_timeout_secs: Some(model.request_timeout_secs),
        stream_idle_timeout_secs: Some(model.stream_idle_timeout_secs),
        auto_compact_token_limit: model.auto_compact_token_limit,
        effective_context_window_percent: Some(model.effective_context_window_percent),
        tool_output_max_tokens: Some(model.tool_output_max_tokens),
    }
}

fn looks_like_env_reference(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.starts_with('$') || trimmed.starts_with("env:")
}

fn config_from_setup_request(request: &SetupConfigRequest) -> Result<Config> {
    config_from_setup_request_with_base(request, Config::default())
}

fn config_from_setup_request_with_base(
    request: &SetupConfigRequest,
    base: Config,
) -> Result<Config> {
    let SetupConfigRegistryParts {
        providers,
        models,
        main_model,
        efficient_model,
    } = if !request.providers.is_empty() || !request.models.is_empty() {
        config_registries_from_setup_request(request)?
    } else {
        legacy_config_registries_from_setup_request(request)?
    };
    let daemon_port = request
        .daemon_port
        .filter(|port| *port > 0)
        .unwrap_or(base.daemon.port);

    let telegram = TelegramConfig {
        enabled: request.telegram_enabled.unwrap_or(base.telegram.enabled),
        bot_token: request
            .telegram_bot_token
            .clone()
            .unwrap_or_else(|| base.telegram.bot_token.clone()),
        poll_timeout_secs: base.telegram.poll_timeout_secs,
    };

    Ok(Config {
        providers,
        models,
        locale: request.locale.unwrap_or(base.locale),
        main_model,
        efficient_model,
        daemon: DaemonConfig { port: daemon_port },
        telegram,
        ..base
    })
}

struct SetupConfigRegistryParts {
    providers: HashMap<String, ProviderConfig>,
    models: HashMap<String, ModelConfig>,
    main_model: String,
    efficient_model: String,
}

fn config_registries_from_setup_request(
    request: &SetupConfigRequest,
) -> Result<SetupConfigRegistryParts> {
    let mut providers = HashMap::new();
    for provider in &request.providers {
        let name = required_name(&provider.name, "provider.name")?;
        if providers.contains_key(&name) {
            return Err(miette!("duplicate provider name '{name}'"));
        }
        providers.insert(name, provider_from_setup_provider(provider)?);
    }
    if providers.is_empty() {
        return Err(miette!("at least one provider is required"));
    }

    let mut models = HashMap::new();
    for model in &request.models {
        let name = required_name(&model.name, "model.name")?;
        if models.contains_key(&name) {
            return Err(miette!("duplicate model name '{name}'"));
        }
        let provider_name = required_name(&model.provider_name, "model.provider_name")?;
        if !providers.contains_key(&provider_name) {
            return Err(miette!(
                "model '{name}' references missing provider '{provider_name}'"
            ));
        }
        let provider = providers
            .get(&provider_name)
            .expect("provider presence was checked above");
        models.insert(
            name,
            model_from_setup_model(&provider_name, provider, model)?,
        );
    }
    if models.is_empty() {
        return Err(miette!("at least one model is required"));
    }

    let main_model = required_name(request.main_model.as_deref().unwrap_or(""), "main_model")?;
    let efficient_model = required_name(
        request
            .efficient_model
            .as_deref()
            .filter(|model| !model.trim().is_empty())
            .unwrap_or(&main_model),
        "efficient_model",
    )?;
    Ok(SetupConfigRegistryParts {
        providers,
        models,
        main_model,
        efficient_model,
    })
}

fn legacy_config_registries_from_setup_request(
    request: &SetupConfigRequest,
) -> Result<SetupConfigRegistryParts> {
    let provider_kind = request
        .provider_kind
        .ok_or_else(|| miette!("provider_kind is required"))?;
    let provider_name = required_name(
        request.provider_name.as_deref().unwrap_or(""),
        "provider_name",
    )?;
    let main_model_name = required_name(
        request.main_model_name.as_deref().unwrap_or(""),
        "main_model_name",
    )?;
    let main_model_id = required_name(
        request.main_model_id.as_deref().unwrap_or(""),
        "main_model_id",
    )?;
    let efficient_model_name = required_name(
        request.efficient_model_name.as_deref().unwrap_or(""),
        "efficient_model_name",
    )?;
    let efficient_model_id = required_name(
        request.efficient_model_id.as_deref().unwrap_or(""),
        "efficient_model_id",
    )?;

    let provider = provider_from_setup_legacy(provider_kind, request)?;
    let mut providers = HashMap::new();
    providers.insert(provider_name.clone(), provider.clone());

    let mut models = HashMap::new();
    models.insert(
        main_model_name.clone(),
        model_from_setup(&provider_name, &provider, &main_model_id),
    );
    if efficient_model_name != main_model_name {
        models.insert(
            efficient_model_name.clone(),
            model_from_setup(&provider_name, &provider, &efficient_model_id),
        );
    }

    Ok(SetupConfigRegistryParts {
        providers,
        models,
        main_model: main_model_name,
        efficient_model: efficient_model_name,
    })
}

async fn write_setup_persona(request: &SetupConfigRequest) -> Result<()> {
    let current = load_prompt_persona_spec_sync();
    let default = PromptPersonaSpec::default();
    let name = match request.persona_name.as_deref() {
        Some(name) => required_name(name, "persona_name")?,
        None => default.name.clone(),
    };
    let language = request
        .persona_language
        .as_deref()
        .map(str::trim)
        .filter(|language| !language.is_empty())
        .unwrap_or(default.language.as_str())
        .to_string();
    let persona = PromptPersonaSpec {
        name,
        language,
        identity_summary: current.identity_summary,
    };
    let content = render_prompt_persona_markdown(&persona);
    write_bytes_atomic(
        prompt_persona_path_sync(),
        content.into_bytes(),
        PersistenceFileMode::Private,
    )
    .await
    .map_err(|err| miette!("write persona file failed: {err}"))
}

fn provider_from_setup_legacy(
    provider_kind: SetupProviderKind,
    request: &SetupConfigRequest,
) -> Result<ProviderConfig> {
    let api_key = request.api_key.as_deref().unwrap_or("").trim().to_string();
    let base_url = request.base_url.as_deref().unwrap_or("").trim();
    match provider_kind {
        SetupProviderKind::Openai => Ok(ProviderConfig::Openai {
            api_key,
            base_url: optional_normalized_url(base_url),
        }),
        SetupProviderKind::OpenaiCompatible => Ok(ProviderConfig::OpenaiCompatible {
            base_url: required_string(base_url, "base_url")?,
            api_key,
            api_style: None,
        }),
        SetupProviderKind::OpenaiCodexOauth => Ok(ProviderConfig::OpenaiCodexOauth {
            base_url: optional_normalized_url(base_url),
        }),
        SetupProviderKind::GithubCopilot => Ok(ProviderConfig::GithubCopilot {
            github_token: api_key,
        }),
        SetupProviderKind::Ollama => Ok(ProviderConfig::Ollama {
            host: optional_normalized_url(base_url),
            api_key: (!api_key.is_empty()).then_some(api_key),
            keep_alive: None,
        }),
        SetupProviderKind::OllamaCloud => Ok(ProviderConfig::Ollama {
            host: Some("https://ollama.com".to_string()),
            api_key: Some(required_string(&api_key, "api_key")?),
            keep_alive: None,
        }),
    }
}

fn provider_from_setup_provider(provider: &SetupProviderRequest) -> Result<ProviderConfig> {
    let api_key = provider.api_key.as_deref().unwrap_or("").trim().to_string();
    let base_url = provider.base_url.as_deref().unwrap_or("").trim();
    match provider.kind {
        SetupProviderKind::Openai => Ok(ProviderConfig::Openai {
            api_key: required_string(&api_key, "provider.api_key")?,
            base_url: optional_normalized_url(base_url),
        }),
        SetupProviderKind::OpenaiCompatible => Ok(ProviderConfig::OpenaiCompatible {
            base_url: required_string(base_url, "provider.base_url")?,
            api_key: required_string(&api_key, "provider.api_key")?,
            api_style: None,
        }),
        SetupProviderKind::OpenaiCodexOauth => Ok(ProviderConfig::OpenaiCodexOauth {
            base_url: optional_normalized_url(base_url),
        }),
        SetupProviderKind::GithubCopilot => Ok(ProviderConfig::GithubCopilot {
            github_token: required_string(&api_key, "provider.github_token")?,
        }),
        SetupProviderKind::Ollama => Ok(ProviderConfig::Ollama {
            host: optional_normalized_url(base_url),
            api_key: (!api_key.is_empty()).then_some(api_key),
            keep_alive: provider
                .keep_alive
                .as_deref()
                .map(str::trim)
                .filter(|keep_alive| !keep_alive.is_empty())
                .map(str::to_string),
        }),
        SetupProviderKind::OllamaCloud => Ok(ProviderConfig::Ollama {
            host: Some("https://ollama.com".to_string()),
            api_key: Some(required_string(&api_key, "provider.api_key")?),
            keep_alive: provider
                .keep_alive
                .as_deref()
                .map(str::trim)
                .filter(|keep_alive| !keep_alive.is_empty())
                .map(str::to_string),
        }),
    }
}

async fn prepare_setup_provider_credentials(providers: &[SetupProviderRequest]) -> Result<()> {
    for provider in providers {
        prepare_setup_provider_credential(provider).await?;
    }
    Ok(())
}

async fn prepare_setup_provider_credential(provider: &SetupProviderRequest) -> Result<()> {
    if provider.kind != SetupProviderKind::OpenaiCodexOauth {
        return Ok(());
    }

    match provider
        .codex_auth_method
        .unwrap_or(SetupCodexAuthMethod::ExistingAuthFile)
    {
        SetupCodexAuthMethod::ImportLocalCodex => {
            import_codex_auth_for_provider(provider, codex_cli_auth_file()).await
        }
        SetupCodexAuthMethod::ImportAuthFile => {
            let path = provider
                .codex_auth_file
                .as_deref()
                .map(str::trim)
                .filter(|path| !path.is_empty())
                .ok_or_else(|| miette!("provider.codex_auth_file is required"))?;
            import_codex_auth_for_provider(provider, expand_user_path(path)).await
        }
        SetupCodexAuthMethod::ExistingAuthFile
        | SetupCodexAuthMethod::BrowserLogin
        | SetupCodexAuthMethod::DeviceLogin => {
            let auth_file = codex_auth_file_for_provider(provider)?;
            if auth_file.exists() {
                Ok(())
            } else {
                Err(miette!(
                    "Codex OAuth auth file does not exist: {}",
                    auth_file.display()
                ))
            }
        }
    }
}

async fn run_codex_setup_provider_auth(
    provider: &SetupProviderRequest,
) -> Result<SetupProviderAuthResponse> {
    match provider
        .codex_auth_method
        .unwrap_or(SetupCodexAuthMethod::ExistingAuthFile)
    {
        SetupCodexAuthMethod::BrowserLogin => {
            let tokens = crate::config_wizard::run_codex_oauth_browser_flow(
                Locale::default(),
                |_prompt, _lines| Ok(()),
            )
            .await?;
            let auth_file = write_codex_tokens_for_provider(provider, &tokens).await?;
            Ok(SetupProviderAuthResponse {
                api_key: None,
                auth_file: Some(auth_file.display().to_string()),
                message: "OpenAI Codex browser login completed".to_string(),
            })
        }
        SetupCodexAuthMethod::ImportLocalCodex
        | SetupCodexAuthMethod::ImportAuthFile
        | SetupCodexAuthMethod::ExistingAuthFile => {
            prepare_setup_provider_credential(provider).await?;
            Ok(SetupProviderAuthResponse {
                api_key: None,
                auth_file: Some(
                    codex_auth_file_for_provider(provider)?
                        .display()
                        .to_string(),
                ),
                message: "OpenAI Codex auth file is ready".to_string(),
            })
        }
        SetupCodexAuthMethod::DeviceLogin => Err(miette!(
            "OpenAI Codex device login must be started before it can be completed"
        )),
    }
}

async fn start_github_device_auth()
-> Result<(SetupProviderAuthStartResponse, PendingSetupProviderAuthFlow)> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|err| miette!("GitHub HTTP client failed: {err}"))?;
    let response = http
        .post(GITHUB_DEVICE_CODE_URL)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!(
            "client_id={}&scope=read%3Auser",
            urlenc(GITHUB_CLIENT_ID)
        ))
        .send()
        .await
        .map_err(|err| miette!("GitHub device code request failed: {err}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(miette!(
            "GitHub device code request returned HTTP {status}: {body}"
        ));
    }

    let device: GithubDeviceCodeResponse = response
        .json()
        .await
        .map_err(|err| miette!("GitHub device code response parse failed: {err}"))?;
    let verification_url = device
        .verification_uri
        .unwrap_or_else(|| "https://github.com/login/device".to_string());
    let interval_secs = device.interval.unwrap_or(5).max(5);
    let expires_at_ms =
        chrono::Utc::now().timestamp_millis() + (device.expires_in.unwrap_or(900) as i64 * 1000);
    let flow_id = Uuid::new_v4().to_string();
    let _ = open_url(&verification_url);

    Ok((
        SetupProviderAuthStartResponse {
            flow_id: flow_id.clone(),
            provider_kind: SetupProviderKind::GithubCopilot,
            verification_url,
            user_code: device.user_code.clone(),
            expires_at_ms,
            interval_secs,
        },
        PendingSetupProviderAuthFlow::GithubDevice {
            flow_id,
            device_code: device.device_code,
            expires_at_ms,
        },
    ))
}

async fn complete_github_device_auth(device_code: String) -> Result<SetupProviderAuthResponse> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|err| miette!("GitHub HTTP client failed: {err}"))?;
    let response = http
        .post(GITHUB_ACCESS_TOKEN_URL)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!(
            "client_id={}&device_code={}&grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code",
            urlenc(GITHUB_CLIENT_ID),
            urlenc(&device_code),
        ))
        .send()
        .await
        .map_err(|err| miette!("GitHub device token request failed: {err}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(miette!(
            "GitHub device token request returned HTTP {status}: {body}"
        ));
    }

    let token: GithubDeviceTokenResponse = response
        .json()
        .await
        .map_err(|err| miette!("GitHub device token response parse failed: {err}"))?;
    if let Some(access_token) = token.access_token {
        return Ok(SetupProviderAuthResponse {
            api_key: Some(access_token),
            auth_file: None,
            message: "GitHub device login completed".to_string(),
        });
    }

    match token.error.as_deref() {
        Some("authorization_pending") => Err(miette!("GitHub authorization is still pending")),
        Some("slow_down") => Err(miette!("GitHub asked to retry more slowly")),
        Some("expired_token") => Err(miette!("GitHub device code expired")),
        Some(error) => Err(miette!(
            "GitHub device login failed: {}",
            token
                .error_description
                .as_deref()
                .filter(|description| !description.trim().is_empty())
                .unwrap_or(error)
        )),
        None => Err(miette!(
            "GitHub device token response did not include a token"
        )),
    }
}

async fn start_codex_device_auth()
-> Result<(SetupProviderAuthStartResponse, PendingSetupProviderAuthFlow)> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|err| miette!("OpenAI Codex HTTP client failed: {err}"))?;
    let response = http
        .post(format!("{CODEX_OAUTH_ISSUER}{CODEX_DEVICE_USER_CODE_PATH}"))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({ "client_id": CODEX_OAUTH_CLIENT_ID }))
        .send()
        .await
        .map_err(|err| miette!("OpenAI Codex device code request failed: {err}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(miette!(
            "OpenAI Codex device code request returned HTTP {status}: {body}"
        ));
    }

    let device: CodexDeviceUserCodeResponse = response
        .json()
        .await
        .map_err(|err| miette!("OpenAI Codex device code response parse failed: {err}"))?;
    let interval_secs = parse_codex_device_interval(&device.interval).max(5);
    let verification_url = format!("{CODEX_OAUTH_ISSUER}/codex/device");
    let expires_at_ms = chrono::Utc::now().timestamp_millis() + 15 * 60 * 1000;
    let flow_id = Uuid::new_v4().to_string();
    let _ = open_url(&verification_url);

    Ok((
        SetupProviderAuthStartResponse {
            flow_id: flow_id.clone(),
            provider_kind: SetupProviderKind::OpenaiCodexOauth,
            verification_url,
            user_code: device.user_code.clone(),
            expires_at_ms,
            interval_secs,
        },
        PendingSetupProviderAuthFlow::CodexDevice {
            flow_id,
            device_auth_id: device.device_auth_id,
            user_code: device.user_code,
            expires_at_ms,
        },
    ))
}

async fn complete_codex_device_auth(
    provider: &SetupProviderRequest,
    device_auth_id: String,
    user_code: String,
) -> Result<SetupProviderAuthResponse> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|err| miette!("OpenAI Codex HTTP client failed: {err}"))?;
    let response = http
        .post(format!("{CODEX_OAUTH_ISSUER}{CODEX_DEVICE_TOKEN_PATH}"))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "device_auth_id": device_auth_id,
            "user_code": user_code,
        }))
        .send()
        .await
        .map_err(|err| miette!("OpenAI Codex device token request failed: {err}"))?;
    let status = response.status();
    if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::NOT_FOUND {
        return Err(miette!("OpenAI Codex authorization is still pending"));
    }
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(miette!(
            "OpenAI Codex device token request returned HTTP {status}: {body}"
        ));
    }

    let token: CodexDeviceTokenResponse = response
        .json()
        .await
        .map_err(|err| miette!("OpenAI Codex device token response parse failed: {err}"))?;
    let tokens = exchange_codex_authorization_code_with_pkce(
        &http,
        &token.authorization_code,
        &token.code_verifier,
        &format!("{CODEX_OAUTH_ISSUER}/deviceauth/callback"),
    )
    .await?;
    let auth_file = write_codex_tokens_for_provider(provider, &tokens).await?;
    Ok(SetupProviderAuthResponse {
        api_key: None,
        auth_file: Some(auth_file.display().to_string()),
        message: "OpenAI Codex device login completed".to_string(),
    })
}

async fn import_codex_auth_for_provider(
    provider: &SetupProviderRequest,
    source_auth_file: PathBuf,
) -> Result<()> {
    let provider_name = required_name(&provider.name, "provider.name")?;
    let destination_auth_file = codex_oauth_auth_file(&provider_name);
    let tokens = import_codex_cli_oauth_tokens(&source_auth_file)
        .await
        .map_err(|err| {
            miette!(
                "failed to import Codex OAuth auth file {}: {err}",
                source_auth_file.display()
            )
        })?;
    write_codex_oauth_tokens(&destination_auth_file, &tokens)
        .await
        .map_err(|err| {
            miette!(
                "failed to write Codex OAuth auth file {}: {err}",
                destination_auth_file.display()
            )
        })?;
    Ok(())
}

async fn write_codex_tokens_for_provider(
    provider: &SetupProviderRequest,
    tokens: &CodexOAuthTokens,
) -> Result<PathBuf> {
    let auth_file = codex_auth_file_for_provider(provider)?;
    write_codex_oauth_tokens(&auth_file, tokens)
        .await
        .map_err(|err| {
            miette!(
                "failed to write Codex OAuth auth file {}: {err}",
                auth_file.display()
            )
        })?;
    Ok(auth_file)
}

fn codex_auth_file_for_provider(provider: &SetupProviderRequest) -> Result<PathBuf> {
    let provider_name = required_name(&provider.name, "provider.name")?;
    Ok(codex_oauth_auth_file(&provider_name))
}

async fn exchange_codex_authorization_code_with_pkce(
    http: &reqwest::Client,
    authorization_code: &str,
    code_verifier: &str,
    redirect_uri: &str,
) -> Result<CodexOAuthTokens> {
    let response = http
        .post(format!("{CODEX_OAUTH_ISSUER}{CODEX_OAUTH_TOKEN_PATH}"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!(
            "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&code_verifier={}",
            urlenc(authorization_code),
            urlenc(redirect_uri),
            urlenc(CODEX_OAUTH_CLIENT_ID),
            urlenc(code_verifier),
        ))
        .send()
        .await
        .map_err(|err| miette!("OpenAI Codex token exchange failed: {err}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|err| miette!("OpenAI Codex token exchange body read failed: {err}"))?;
    if !status.is_success() {
        return Err(miette!(
            "OpenAI Codex token exchange returned HTTP {status}: {body}"
        ));
    }

    let tokens: CodexOAuthTokenResponse = serde_json::from_str(&body)
        .map_err(|err| miette!("OpenAI Codex token exchange response parse failed: {err}"))?;
    Ok(CodexOAuthTokens {
        id_token: tokens.id_token,
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        account_id: None,
        last_refresh_at_ms: chrono::Utc::now().timestamp_millis(),
    })
}

fn parse_codex_device_interval(value: &serde_json::Value) -> u64 {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|value| value.trim().parse().ok()))
        .unwrap_or(5)
}

fn urlenc(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

#[derive(Debug, Deserialize)]
struct GithubDeviceCodeResponse {
    device_code: String,
    user_code: String,
    #[serde(default)]
    verification_uri: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    interval: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct GithubDeviceTokenResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CodexDeviceUserCodeResponse {
    device_auth_id: String,
    user_code: String,
    #[serde(default)]
    interval: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct CodexDeviceTokenResponse {
    authorization_code: String,
    code_verifier: String,
}

#[derive(Debug, Deserialize)]
struct CodexOAuthTokenResponse {
    id_token: String,
    access_token: String,
    refresh_token: String,
}

fn setup_discovered_model(
    provider: &ProviderConfig,
    model: DiscoveredModel,
) -> SetupDiscoveredModel {
    let capacity = resolve_model_capacity(
        provider,
        &model.id,
        model.context_window,
        model.max_output_tokens,
        model.supports_vision,
    );
    let reasoning_options =
        reasoning_options_for_prompt(provider, &model.id, model.reasoning_options.as_deref());
    SetupDiscoveredModel {
        id: model.id,
        context_window_tokens: Some(capacity.context_window_tokens),
        max_completion_tokens: Some(capacity.max_completion_tokens),
        supports_vision: Some(capacity.supports_vision),
        thinking_budgets: reasoning_option_values(reasoning_options),
    }
}

fn reasoning_option_values(options: Vec<ReasoningOption>) -> Vec<String> {
    options
        .into_iter()
        .flat_map(|option| match option {
            ReasoningOption::Toggle => vec!["true".to_string(), "false".to_string()],
            ReasoningOption::Effort { values } => values,
            ReasoningOption::BudgetTokens { .. } => Vec::new(),
        })
        .collect()
}

fn model_from_setup(provider_name: &str, provider: &ProviderConfig, model_id: &str) -> ModelConfig {
    let capacity = resolve_model_capacity(provider, model_id, None, None, None);
    ModelConfig {
        provider: provider_name.to_string(),
        model_id: model_id.to_string(),
        context_window_tokens: capacity.context_window_tokens,
        max_completion_tokens: capacity.max_completion_tokens,
        supports_vision: Some(capacity.supports_vision),
        ..ModelConfig::default()
    }
}

fn model_from_setup_model(
    provider_name: &str,
    provider: &ProviderConfig,
    model: &SetupModelRequest,
) -> Result<ModelConfig> {
    let model_id = required_string(&model.model_id, "model.model_id")?;
    let capacity = resolve_model_capacity(
        provider,
        &model_id,
        model.context_window_tokens.filter(|value| *value > 0),
        model.max_completion_tokens.filter(|value| *value > 0),
        model.supports_vision,
    );
    let default = ModelConfig::default();
    Ok(ModelConfig {
        provider: provider_name.to_string(),
        model_id,
        temperature: model.temperature.unwrap_or(default.temperature),
        rpm: model.rpm,
        request_timeout_secs: model
            .request_timeout_secs
            .unwrap_or(default.request_timeout_secs),
        stream_idle_timeout_secs: model
            .stream_idle_timeout_secs
            .unwrap_or(default.stream_idle_timeout_secs),
        context_window_tokens: capacity.context_window_tokens,
        auto_compact_token_limit: model.auto_compact_token_limit,
        effective_context_window_percent: model
            .effective_context_window_percent
            .unwrap_or(default.effective_context_window_percent),
        max_completion_tokens: capacity.max_completion_tokens,
        tool_output_max_tokens: model
            .tool_output_max_tokens
            .unwrap_or(default.tool_output_max_tokens),
        supports_vision: Some(capacity.supports_vision),
        thinking_budget: model
            .thinking_budget
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ThinkingBudget::new),
    })
}

fn required_name(value: &str, field: &str) -> Result<String> {
    let value = required_string(value, field)?;
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
        .and_then_nonempty(field)
}

trait NonEmptyString {
    fn and_then_nonempty(self, field: &str) -> Result<String>;
}

impl NonEmptyString for String {
    fn and_then_nonempty(self, field: &str) -> Result<String> {
        if self.trim().is_empty() {
            Err(miette!("{field} cannot be empty"))
        } else {
            Ok(self)
        }
    }
}

fn required_string(value: &str, field: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(miette!("{field} cannot be empty"))
    } else {
        Ok(trimmed.to_string())
    }
}

fn optional_normalized_url(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| normalize_provider_base_url(value))
}

fn expand_user_path(path: &str) -> PathBuf {
    let trimmed = path.trim();
    if trimmed == "~" {
        return std::env::home_dir().unwrap_or_else(|| PathBuf::from(trimmed));
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        return std::env::home_dir()
            .map(|home| home.join(rest))
            .unwrap_or_else(|| PathBuf::from(trimmed));
    }
    PathBuf::from(trimmed)
}

async fn recover_damaged_config(
    config_path: PathBuf,
    backup_path: PathBuf,
    fallback_port: u16,
    error: String,
) -> ConfigReadinessReport {
    let mut notes = Vec::new();
    match quarantine_file(&config_path, "corrupt").await {
        Ok(Some(path)) => notes.push(format!(
            "moved damaged config to {} ({error})",
            path.display()
        )),
        Ok(None) => notes.push(format!(
            "config was damaged but disappeared before recovery: {error}"
        )),
        Err(err) => notes.push(format!(
            "failed to move damaged config aside: {err}; original error: {error}"
        )),
    }

    match read_config_readiness_from_path(&backup_path).await {
        ConfigReadOutcome::Parsed(parts) => {
            match tokio::fs::copy(&backup_path, &config_path).await {
                Ok(_) => notes.push(format!("restored config from {}", backup_path.display())),
                Err(err) => notes.push(format!(
                    "backup parsed but could not be restored from {}: {err}",
                    backup_path.display()
                )),
            }
            return report(parts, config_path, backup_path, Some(notes.join("; ")));
        }
        ConfigReadOutcome::Missing => {
            notes.push("config backup was missing".to_string());
        }
        ConfigReadOutcome::Damaged { error, .. } => {
            match quarantine_file(&backup_path, "corrupt").await {
                Ok(Some(path)) => notes.push(format!(
                    "moved damaged backup to {} ({error})",
                    path.display()
                )),
                Ok(None) => notes.push(format!(
                    "config backup was damaged but disappeared before recovery: {error}"
                )),
                Err(err) => notes.push(format!(
                    "failed to move damaged backup aside: {err}; backup error: {error}"
                )),
            }
        }
    }

    match write_setup_safe_default_config(&config_path, fallback_port).await {
        Ok(()) => {
            if let Err(err) = update_config_backup(&config_path, &backup_path).await {
                tracing::warn!("failed to update setup-safe config backup: {err}");
            }
            notes.push(format!(
                "wrote setup-safe defaults with daemon.port={fallback_port}"
            ));
        }
        Err(err) => notes.push(format!("failed to write setup-safe defaults: {err}")),
    }

    report(
        ReadinessParts {
            kind: ConfigReadinessKind::Unconfigured,
            port: fallback_port,
            message: "configuration recovery fell back to setup-safe defaults".to_string(),
        },
        config_path,
        backup_path,
        Some(notes.join("; ")),
    )
}

async fn read_boot_port_from_path(path: &PathBuf) -> Option<u16> {
    let text = tokio::fs::read_to_string(path).await.ok()?;
    read_boot_port_from_str(&text)
}

fn read_boot_port_from_str(text: &str) -> Option<u16> {
    let boot = toml::from_str::<BootToml>(text).ok()?;
    boot.daemon.port.filter(|port| *port > 0)
}

async fn read_config_readiness_from_path(path: &PathBuf) -> ConfigReadOutcome {
    let text = match tokio::fs::read_to_string(path).await {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return ConfigReadOutcome::Missing;
        }
        Err(err) => {
            return ConfigReadOutcome::Damaged {
                error: format!("read {} failed: {err}", path.display()),
                port: None,
            };
        }
    };
    read_config_readiness_from_str(&text)
}

fn read_config_readiness_from_str(text: &str) -> ConfigReadOutcome {
    let value = match toml::from_str::<toml::Value>(text) {
        Ok(value) => value,
        Err(err) => {
            return ConfigReadOutcome::Damaged {
                error: format!("parse TOML failed: {err}"),
                port: read_boot_port_from_str(text),
            };
        }
    };
    let port = read_boot_port_from_str(text).unwrap_or_else(|| DaemonConfig::default().port);

    if !has_agent_config_intent(&value) {
        return ConfigReadOutcome::Parsed(ReadinessParts {
            kind: ConfigReadinessKind::Unconfigured,
            port,
            message: "configuration has no provider/model setup".to_string(),
        });
    }

    let config = match toml::from_str::<Config>(text) {
        Ok(config) => config,
        Err(err) => {
            return ConfigReadOutcome::Damaged {
                error: format!("deserialize config failed: {err}"),
                port: Some(port),
            };
        }
    };

    let raw_role_error = raw_role_error(&value);
    let validation_error = config.validate().err();
    let provider_error = provider_completeness_error(&config);
    let model_error = model_completeness_error(&config);

    if let Some(error) = raw_role_error
        .or(validation_error)
        .or(provider_error)
        .or(model_error)
    {
        return ConfigReadOutcome::Parsed(ReadinessParts {
            kind: ConfigReadinessKind::Incomplete,
            port: config.daemon.port,
            message: error,
        });
    }

    ConfigReadOutcome::Parsed(ReadinessParts {
        kind: ConfigReadinessKind::Complete,
        port: config.daemon.port,
        message: "configuration is complete".to_string(),
    })
}

fn has_agent_config_intent(value: &toml::Value) -> bool {
    table_is_nonempty(value, "providers")
        || table_is_nonempty(value, "models")
        || string_is_nonempty(value, "main_model")
        || string_is_nonempty(value, "efficient_model")
}

fn table_is_nonempty(value: &toml::Value, key: &str) -> bool {
    value
        .get(key)
        .and_then(toml::Value::as_table)
        .is_some_and(|table| !table.is_empty())
}

fn string_is_nonempty(value: &toml::Value, key: &str) -> bool {
    value
        .get(key)
        .and_then(toml::Value::as_str)
        .is_some_and(|text| !text.trim().is_empty())
}

fn raw_role_error(value: &toml::Value) -> Option<String> {
    if !string_is_nonempty(value, "main_model") {
        return Some("main_model is not configured".to_string());
    }
    if !string_is_nonempty(value, "efficient_model") {
        return Some("efficient_model is not configured".to_string());
    }
    None
}

fn provider_completeness_error(config: &Config) -> Option<String> {
    if config.providers.is_empty() {
        return Some("no providers are configured".to_string());
    }
    let mut providers = config.providers.iter().collect::<Vec<_>>();
    providers.sort_by_key(|(name, _)| *name);
    for (name, provider) in providers {
        if let Some(error) = provider_error(name, provider) {
            return Some(error);
        }
    }
    None
}

fn provider_error(name: &str, provider: &ProviderConfig) -> Option<String> {
    match provider {
        ProviderConfig::Openai { api_key, .. } => {
            credential_error(name, "api_key", api_key, Some("your-api-key"))
        }
        ProviderConfig::GithubCopilot { github_token } => {
            credential_error(name, "github_token", github_token, None)
        }
        ProviderConfig::OpenaiCodexOauth { .. } => {
            let auth_file = crate::providers::codex_oauth_auth_file(name);
            if auth_file.exists() {
                None
            } else {
                Some(format!(
                    "provider '{name}' is missing Codex OAuth auth file {}",
                    auth_file.display()
                ))
            }
        }
        ProviderConfig::OpenaiCompatible {
            base_url, api_key, ..
        } => {
            if base_url.trim().is_empty() {
                return Some(format!("provider '{name}' has an empty base_url"));
            }
            credential_error(name, "api_key", api_key, Some("your-api-key"))
        }
        ProviderConfig::Ollama { host, api_key, .. } => {
            if host.as_deref().is_some_and(|host| host.trim().is_empty()) {
                return Some(format!("provider '{name}' has an empty host"));
            }
            api_key
                .as_deref()
                .and_then(|key| credential_error(name, "api_key", key, Some("your-ollama-api-key")))
        }
    }
}

fn credential_error(
    provider_name: &str,
    field: &str,
    value: &str,
    placeholder: Option<&str>,
) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Some(format!("provider '{provider_name}' has an empty {field}"));
    }
    if placeholder.is_some_and(|placeholder| trimmed == placeholder) {
        return Some(format!(
            "provider '{provider_name}' still uses placeholder {field}"
        ));
    }
    if let Some(env_name) = env_reference_name(trimmed)
        && std::env::var(&env_name)
            .ok()
            .is_none_or(|value| value.trim().is_empty())
    {
        return Some(format!(
            "provider '{provider_name}' references unset environment variable {env_name}"
        ));
    }
    None
}

fn env_reference_name(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let name = if let Some(inner) = trimmed
        .strip_prefix("${")
        .and_then(|inner| inner.strip_suffix('}'))
    {
        inner
    } else if let Some(inner) = trimmed.strip_prefix("env:") {
        inner
    } else {
        trimmed.strip_prefix('$')?
    };
    let name = name.trim();
    is_valid_env_name(name).then(|| name.to_string())
}

fn is_valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first == '_' || first.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn model_completeness_error(config: &Config) -> Option<String> {
    if config.models.is_empty() {
        return Some("no models are configured".to_string());
    }

    let models_by_name: HashMap<&str, &ModelConfig> = config
        .models
        .iter()
        .map(|(name, model)| (name.as_str(), model))
        .collect();
    for role in [&config.main_model, &config.efficient_model] {
        let Some(model) = models_by_name.get(role.as_str()) else {
            return Some(format!("model role '{role}' references a missing model"));
        };
        if model.model_id.trim().is_empty() {
            return Some(format!("model '{role}' has an empty model_id"));
        }
        if model.provider.trim().is_empty() {
            return Some(format!("model '{role}' has an empty provider"));
        }
    }
    None
}

async fn update_config_backup(config_path: &Path, backup_path: &Path) -> std::io::Result<()> {
    if !config_path.exists() {
        return Ok(());
    }
    let bytes = tokio::fs::read(config_path).await?;
    write_bytes_atomic(
        backup_path.to_path_buf(),
        bytes,
        PersistenceFileMode::Private,
    )
    .await
}

async fn write_setup_safe_default_config(path: &Path, port: u16) -> std::io::Result<()> {
    let body = format!("[daemon]\nport = {port}\n");
    write_bytes_atomic(
        path.to_path_buf(),
        body.into_bytes(),
        PersistenceFileMode::Private,
    )
    .await
}

async fn quarantine_file(path: &PathBuf, label: &str) -> std::io::Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let timestamp_ms = chrono::Utc::now().timestamp_millis();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.toml");
    let target = path.with_file_name(format!("{file_name}.{label}-{timestamp_ms}"));
    tokio::fs::rename(path, &target).await?;
    Ok(Some(target))
}

fn report(
    parts: ReadinessParts,
    config_path: PathBuf,
    backup_path: PathBuf,
    recovery_note: Option<String>,
) -> ConfigReadinessReport {
    ConfigReadinessReport {
        kind: parts.kind,
        config_path: config_path.display().to_string(),
        backup_path: backup_path.display().to_string(),
        port: parts.port,
        message: parts.message,
        recovery_note,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_port_config_is_unconfigured() {
        let outcome = read_config_readiness_from_str("[daemon]\nport = 6000\n");
        let ConfigReadOutcome::Parsed(parts) = outcome else {
            panic!("expected parsed readiness");
        };
        assert_eq!(parts.kind, ConfigReadinessKind::Unconfigured);
        assert_eq!(parts.port, 6000);
    }

    #[test]
    fn default_placeholder_config_is_incomplete() {
        let text = toml::to_string_pretty(&Config::default()).unwrap();
        let outcome = read_config_readiness_from_str(&text);
        let ConfigReadOutcome::Parsed(parts) = outcome else {
            panic!("expected parsed readiness");
        };
        assert_eq!(parts.kind, ConfigReadinessKind::Incomplete);
        assert!(parts.message.contains("placeholder"));
    }

    #[test]
    fn missing_model_role_is_incomplete() {
        let text = r#"
[providers.openai]
type = "openai"
api_key = "sk-test"

[models.main]
provider = "openai"
model_id = "gpt-4.1"
"#;
        let outcome = read_config_readiness_from_str(text);
        let ConfigReadOutcome::Parsed(parts) = outcome else {
            panic!("expected parsed readiness");
        };
        assert_eq!(parts.kind, ConfigReadinessKind::Incomplete);
        assert!(parts.message.contains("main_model"));
    }

    #[test]
    fn configured_openai_config_is_complete() {
        let text = r#"
main_model = "main"
efficient_model = "fast"

[providers.openai]
type = "openai"
api_key = "sk-test"

[models.main]
provider = "openai"
model_id = "gpt-4.1"

[models.fast]
provider = "openai"
model_id = "gpt-4.1-mini"
"#;
        let outcome = read_config_readiness_from_str(text);
        let ConfigReadOutcome::Parsed(parts) = outcome else {
            panic!("expected parsed readiness");
        };
        assert_eq!(parts.kind, ConfigReadinessKind::Complete);
    }

    #[test]
    fn setup_discovered_model_fills_capacity_from_catalog() {
        let provider = ProviderConfig::Openai {
            api_key: "sk-test".to_string(),
            base_url: None,
        };
        let discovered = DiscoveredModel {
            id: "gpt-4.1".to_string(),
            context_window: None,
            max_output_tokens: None,
            supports_vision: None,
            reasoning_options: None,
        };

        let model = setup_discovered_model(&provider, discovered);

        assert!(model.context_window_tokens.unwrap_or_default() > 0);
        assert!(model.max_completion_tokens.unwrap_or_default() > 0);
        assert_eq!(model.supports_vision, Some(true));
    }

    #[test]
    fn setup_discovered_model_fills_codex_reasoning_defaults() {
        let provider = ProviderConfig::OpenaiCodexOauth { base_url: None };
        let discovered = DiscoveredModel {
            id: "gpt-5.4".to_string(),
            context_window: None,
            max_output_tokens: None,
            supports_vision: None,
            reasoning_options: None,
        };

        let model = setup_discovered_model(&provider, discovered);

        assert_eq!(
            model.thinking_budgets,
            vec!["none", "minimal", "low", "medium", "high", "xhigh"]
        );
    }

    #[test]
    fn setup_discovered_model_exposes_toggle_reasoning_as_true_false() {
        let provider = ProviderConfig::Openai {
            api_key: "sk-test".to_string(),
            base_url: None,
        };
        let discovered = DiscoveredModel {
            id: "custom-reasoning-model".to_string(),
            context_window: None,
            max_output_tokens: None,
            supports_vision: None,
            reasoning_options: Some(vec![ReasoningOption::Toggle]),
        };

        let model = setup_discovered_model(&provider, discovered);

        assert_eq!(model.thinking_budgets, vec!["true", "false"]);
    }

    #[test]
    fn setup_config_round_trips_telegram_settings() {
        let base_config = Config {
            telegram: TelegramConfig {
                enabled: false,
                bot_token: "$TELEGRAM_BOT_TOKEN".to_string(),
                poll_timeout_secs: 45,
            },
            ..Config::default()
        };

        let request = setup_config_from_config(&base_config);

        assert_eq!(request.telegram_enabled, Some(false));
        assert_eq!(
            request.telegram_bot_token.as_deref(),
            Some("$TELEGRAM_BOT_TOKEN")
        );

        let mut next_request = request.clone();
        next_request.telegram_enabled = Some(true);
        next_request.telegram_bot_token = Some("123456789:bot-token".to_string());

        let next_config =
            config_from_setup_request_with_base(&next_request, base_config.clone()).unwrap();

        assert!(next_config.telegram.enabled);
        assert_eq!(next_config.telegram.bot_token, "123456789:bot-token");
        assert_eq!(next_config.telegram.poll_timeout_secs, 45);
    }
}
