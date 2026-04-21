//! 交互式配置向导：首次运行 setup 和 `config` 子命令

use std::{collections::HashMap, time::Duration};

use dialoguer::{Confirm, Input, Password, Select, theme::ColorfulTheme};
use miette::{Result, miette};

use crate::config::{Config, JudgeConfig, ModelConfig, ProviderConfig, write_config};

// ---------------------------------------------------------------------------
// GitHub OAuth device code flow
// ---------------------------------------------------------------------------

// GitHub Copilot 官方应用的公开 Client ID。
// 使用此 ID 获取的 token 可访问 copilot_internal/v2/token 换取 session token，
// 从而使用全量 Copilot 模型（包括 Claude）。
const GITHUB_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";

const GITHUB_DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const GITHUB_ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";

/// 执行 GitHub OAuth device code 流程，返回 access token。
async fn run_github_device_flow() -> Result<String> {
    let client_id = GITHUB_CLIENT_ID;

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| miette!("HTTP client 初始化失败: {e}"))?;

    // Step 1: 请求 device code
    println!("  正在向 GitHub 请求 device code...");
    let resp = http
        .post(GITHUB_DEVICE_CODE_URL)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!(
            "client_id={}&scope=read%3Auser",
            urlenc(&client_id)
        ))
        .send()
        .await
        .map_err(|e| miette!("请求 device code 失败: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(miette!("GitHub 返回错误 HTTP {status}: {body}"));
    }

    let device: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| miette!("解析 device code 响应失败: {e}"))?;

    let device_code = device["device_code"]
        .as_str()
        .ok_or_else(|| miette!("响应缺少 device_code 字段"))?
        .to_string();
    let user_code = device["user_code"]
        .as_str()
        .ok_or_else(|| miette!("响应缺少 user_code 字段"))?
        .to_string();
    let verification_uri = device["verification_uri"]
        .as_str()
        .unwrap_or("https://github.com/login/device")
        .to_string();
    let expires_in = device["expires_in"].as_u64().unwrap_or(900);
    let interval_secs = device["interval"].as_u64().unwrap_or(5).max(5);

    // Step 2: 提示用户在浏览器操作
    println!();
    println!("  GitHub 授权");
    println!("  1. 打开: {}", verification_uri);
    println!("  2. 输入验证码: {}", user_code);
    println!();

    // 尝试自动打开浏览器（失败则静默）
    let _ = open_browser(&verification_uri);

    // Step 3: 轮询直到用户完成授权
    let expires_at = std::time::Instant::now() + Duration::from_secs(expires_in);
    let poll_interval = Duration::from_secs(interval_secs);
    let mut dots = 0usize;

    loop {
        if std::time::Instant::now() >= expires_at {
            return Err(miette!("device code 已过期，请重新运行"));
        }

        tokio::time::sleep(poll_interval).await;

        dots = (dots + 1) % 4;
        print!("\r  等待 GitHub 授权{}   ", ".".repeat(dots + 1));
        let _ = std::io::Write::flush(&mut std::io::stdout());

        let poll_resp = http
            .post(GITHUB_ACCESS_TOKEN_URL)
            .header("Accept", "application/json")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(format!(
                "client_id={}&device_code={}&grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code",
                urlenc(&client_id),
                urlenc(&device_code),
            ))
            .send()
            .await
            .map_err(|e| miette!("轮询 access token 失败: {e}"))?;

        let body: serde_json::Value = poll_resp
            .json()
            .await
            .map_err(|e| miette!("解析 token 响应失败: {e}"))?;

        if let Some(token) = body["access_token"].as_str() {
            println!("\r  授权成功                                  ");
            return Ok(token.to_string());
        }

        match body["error"].as_str() {
            Some("authorization_pending") => continue,
            Some("slow_down") => {
                // GitHub 要求降速：额外等待 5 秒
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
            Some("expired_token") => return Err(miette!("device code 已过期，请重新运行")),
            Some("access_denied") => return Err(miette!("用户取消了授权")),
            Some(other) => return Err(miette!("GitHub 授权错误: {other}")),
            None => return Err(miette!("未知的 token 响应: {body}")),
        }
    }
}

/// 简单的 percent-encoding（仅编码非 unreserved 字符，满足 client_id / device_code 需求）
fn urlenc(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => {
                vec![c]
            }
            _ => format!("%{:02X}", c as u32).chars().collect(),
        })
        .collect()
}

fn open_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    std::process::Command::new("open")
        .arg(url)
        .spawn()?
        .wait()?;
    #[cfg(target_os = "linux")]
    std::process::Command::new("xdg-open")
        .arg(url)
        .spawn()?
        .wait()?;
    #[cfg(target_os = "windows")]
    std::process::Command::new("cmd")
        .args(["/c", "start", url])
        .spawn()?
        .wait()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 工具函数
// ---------------------------------------------------------------------------

fn theme() -> ColorfulTheme {
    ColorfulTheme::default()
}

/// 格式化输出一条信息行
fn info(msg: &str) {
    println!("  {msg}");
}

fn header(msg: &str) {
    println!("\n{msg}");
    println!("{}", "─".repeat(msg.len()));
}

// ---------------------------------------------------------------------------
// Provider 向导
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum ProviderKind {
    OpenAI,
    GithubCopilot,
    OpenAICompatible,
}

impl ProviderKind {
    const LABELS: &'static [&'static str] = &[
        "OpenAI",
        "GitHub Copilot",
        "OpenAI-compatible（Ollama / LMStudio / 本地）",
    ];

    fn from_index(i: usize) -> Self {
        match i {
            0 => Self::OpenAI,
            1 => Self::GithubCopilot,
            _ => Self::OpenAICompatible,
        }
    }
}

/// 交互式填写一个 provider 的凭据，返回 (provider_name, ProviderConfig)
async fn prompt_provider(existing_names: &[String]) -> Result<(String, ProviderConfig)> {
    let kind_idx = Select::with_theme(&theme())
        .with_prompt("Provider 类型")
        .items(ProviderKind::LABELS)
        .default(0)
        .interact()
        .map_err(|e| miette!("交互中断: {e}"))?;
    let kind = ProviderKind::from_index(kind_idx);

    let default_name = match kind {
        ProviderKind::OpenAI => "openai",
        ProviderKind::GithubCopilot => "copilot",
        ProviderKind::OpenAICompatible => "local",
    };
    // 若名称已存在则加序号避免冲突
    let default_name = if existing_names.contains(&default_name.to_string()) {
        format!("{}-2", default_name)
    } else {
        default_name.to_string()
    };

    let name: String = Input::with_theme(&theme())
        .with_prompt("Provider 名称（在 config.toml 中的 key）")
        .default(default_name)
        .interact_text()
        .map_err(|e| miette!("交互中断: {e}"))?;

    let provider = match kind {
        ProviderKind::OpenAI => {
            let api_key: String = Password::with_theme(&theme())
                .with_prompt("OpenAI API key（sk-...）")
                .interact()
                .map_err(|e| miette!("交互中断: {e}"))?;
            let use_custom_url = Confirm::with_theme(&theme())
                .with_prompt("使用自定义 base URL？（默认 api.openai.com）")
                .default(false)
                .interact()
                .map_err(|e| miette!("交互中断: {e}"))?;
            let base_url = if use_custom_url {
                let url: String = Input::with_theme(&theme())
                    .with_prompt("Base URL")
                    .interact_text()
                    .map_err(|e| miette!("交互中断: {e}"))?;
                Some(url)
            } else {
                None
            };
            ProviderConfig::Openai { api_key, base_url }
        }
        ProviderKind::GithubCopilot => {
            let auth_method = Select::with_theme(&theme())
                .with_prompt("GitHub 认证方式")
                .items(&[
                    "Device code 登录（推荐，浏览器授权）",
                    "手动填写 GitHub Token（PAT）",
                    "使用环境变量（GITHUB_TOKEN / GH_TOKEN）",
                ])
                .default(0)
                .interact()
                .map_err(|e| miette!("交互中断: {e}"))?;

            let github_token = match auth_method {
                0 => run_github_device_flow().await?,
                1 => {
                    info(
                        "在 https://github.com/settings/tokens 创建 Classic Token，scope 选 read:user。",
                    );
                    Password::with_theme(&theme())
                        .with_prompt("GitHub Token（ghp_...）")
                        .interact()
                        .map_err(|e| miette!("交互中断: {e}"))?
                }
                _ => {
                    info("启动时将从 GITHUB_TOKEN / GH_TOKEN 环境变量读取。");
                    "${GITHUB_TOKEN}".to_string()
                }
            };
            ProviderConfig::GithubCopilot { github_token }
        }
        ProviderKind::OpenAICompatible => {
            let base_url: String = Input::with_theme(&theme())
                .with_prompt("Base URL（含 /v1，例如 http://localhost:11434/v1）")
                .default("http://localhost:11434/v1".to_string())
                .interact_text()
                .map_err(|e| miette!("交互中断: {e}"))?;
            let api_key: String = Input::with_theme(&theme())
                .with_prompt("API key（Ollama 等本地服务可填 ollama 或任意值）")
                .default("ollama".to_string())
                .interact_text()
                .map_err(|e| miette!("交互中断: {e}"))?;
            ProviderConfig::OpenaiCompatible { base_url, api_key }
        }
    };

    Ok((name, provider))
}

// ---------------------------------------------------------------------------
// Model 探测
// ---------------------------------------------------------------------------

/// GitHub Copilot 已知可用模型（openclaw 同款静态列表）
const COPILOT_DEFAULT_MODELS: &[&str] = &[
    "claude-sonnet-4.6",
    "claude-sonnet-4.5",
    "claude-opus-4.5",
    "gpt-4o",
    "gpt-4.1",
    "gpt-4.1-mini",
    "gpt-4.1-nano",
    "o3-mini",
    "o1",
    "o1-mini",
];

/// 根据 model_id 推断合理的上下文窗口和最大输出 token 默认值
fn infer_model_capacity(model_id: &str) -> (usize, usize) {
    let lower = model_id.to_lowercase();
    if lower.contains("claude") {
        (200_000, 16_384)
    } else if lower.contains("gpt-4.1") {
        (1_047_576, 32_768)
    } else if lower.contains("gpt-4o") || lower.contains("gpt-4") {
        (128_000, 16_384)
    } else if lower.contains("o1") || lower.contains("o3") {
        (200_000, 100_000)
    } else if lower.contains("mini") || lower.contains("nano") {
        (128_000, 8_192)
    } else {
        (32_768, 8_192)
    }
}

/// 展开 `${VAR}` / `$VAR` 形式的环境变量引用，失败时返回空字符串。
fn resolve_token(raw: &str) -> String {
    let t = raw.trim();
    if let Some(inner) = t.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
        return std::env::var(inner).unwrap_or_default();
    }
    if let Some(var) = t.strip_prefix('$') {
        return std::env::var(var).unwrap_or_default();
    }
    t.to_string()
}

/// 模型探测：先试 session token（内部 API，全量模型），失败降级到公共 API，再失败用静态列表。
async fn fetch_copilot_models(github_token: &str) -> Vec<DiscoveredModel> {
    let fallback = || {
        COPILOT_DEFAULT_MODELS
            .iter()
            .map(|s| DiscoveredModel {
                id: s.to_string(),
                context_window: None,
                max_output_tokens: None,
            })
            .collect::<Vec<_>>()
    };

    let token = resolve_token(github_token);
    if token.is_empty() {
        tracing::warn!("copilot model discovery: github token empty, using static list");
        return fallback();
    }

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("copilot model discovery: http client error: {e}");
            return fallback();
        }
    };

    match try_fetch_via_session_token(&client, &token).await {
        Some(models) => {
            tracing::info!(
                "copilot model discovery: {} models via internal API",
                models.len()
            );
            models
        }
        None => {
            tracing::warn!(
                "copilot model discovery: session token exchange failed, using static list"
            );
            fallback()
        }
    }
}

async fn try_fetch_via_session_token(
    client: &reqwest::Client,
    github_token: &str,
) -> Option<Vec<DiscoveredModel>> {
    let resp = client
        .get("https://api.github.com/copilot_internal/v2/token")
        .header("Authorization", format!("Bearer {github_token}"))
        .header("Accept", "application/json")
        .header("User-Agent", "GitHubCopilotChat/0.26.7")
        .header("Editor-Version", "vscode/1.96.2")
        .header("X-Github-Api-Version", "2025-04-01")
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        tracing::debug!(http_status = %resp.status(), "copilot model discovery: session token exchange failed");
        return None;
    }

    let json: serde_json::Value = resp.json().await.ok()?;
    let session_token = json["token"].as_str()?.to_string();

    let base_url = session_token
        .split(';')
        .find_map(|part| {
            let trimmed = part.trim();
            let host = trimmed.strip_prefix("proxy-ep=").or_else(|| {
                if trimmed.to_lowercase().starts_with("proxy-ep=") {
                    Some(&trimmed[9..])
                } else {
                    None
                }
            })?;
            if host.is_empty() {
                return None;
            }
            let host = if host.to_lowercase().starts_with("proxy.") {
                format!("api.{}", &host[6..])
            } else {
                host.to_string()
            };
            Some(format!("https://{host}"))
        })
        .unwrap_or_else(|| "https://api.individual.githubcopilot.com".to_string());

    let models =
        fetch_copilot_internal_models(client, &format!("{base_url}/models"), &session_token).await;
    if models.is_empty() {
        None
    } else {
        Some(models)
    }
}

/// API 返回的模型信息（context window 和 max tokens 来自响应，非推测）
#[derive(Debug, Clone)]
struct DiscoveredModel {
    id: String,
    context_window: Option<usize>,
    max_output_tokens: Option<usize>,
}

/// 从 provider 的 /v1/models 接口拉取模型列表；失败时返回空 Vec。
async fn fetch_model_ids(provider: &ProviderConfig) -> Vec<DiscoveredModel> {
    match provider {
        ProviderConfig::GithubCopilot { github_token } => fetch_copilot_models(github_token).await,
        ProviderConfig::Openai { api_key, base_url } => {
            let base = base_url.as_deref().unwrap_or("https://api.openai.com");
            fetch_openai_models(base, api_key).await
        }
        ProviderConfig::OpenaiCompatible { base_url, api_key } => {
            fetch_openai_models(base_url, api_key).await
        }
    }
}

async fn fetch_openai_models(base_url: &str, api_key: &str) -> Vec<DiscoveredModel> {
    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
    fetch_openai_models_path(&url, api_key).await
}

async fn fetch_openai_models_path(url: &str, api_key: &str) -> Vec<DiscoveredModel> {
    let url = url.to_string();
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("fetch_openai_models: failed to build http client: {e}");
            return vec![];
        }
    };
    let resp = match client
        .get(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(url = %url, "fetch_openai_models: request failed: {e}");
            return vec![];
        }
    };
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        tracing::warn!(url = %url, http_status = %status, body = %body, "fetch_openai_models: non-2xx response");
        return vec![];
    }
    parse_models_response(resp.json().await.ok())
}

async fn fetch_copilot_internal_models(
    client: &reqwest::Client,
    url: &str,
    session_token: &str,
) -> Vec<DiscoveredModel> {
    let resp = match client
        .get(url)
        .header("Authorization", format!("Bearer {session_token}"))
        .header("User-Agent", "GitHubCopilotChat/0.26.7")
        .header("Editor-Version", "vscode/1.96.2")
        .header("X-Github-Api-Version", "2025-04-01")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(url, "copilot internal models request failed: {e}");
            return vec![];
        }
    };
    if !resp.status().is_success() {
        let s = resp.status();
        let b = resp.text().await.unwrap_or_default();
        tracing::warn!(url, http_status = %s, body = %b, "copilot internal models non-2xx");
        return vec![];
    }
    parse_models_response(resp.json().await.ok())
}

fn parse_models_response(json: Option<serde_json::Value>) -> Vec<DiscoveredModel> {
    let json = match json {
        Some(j) => j,
        None => return vec![],
    };
    let mut models: Vec<DiscoveredModel> = json["data"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|m| {
            let id = m["id"].as_str()?.to_string();
            let limits = &m["capabilities"]["limits"];
            let context_window = limits["max_context_window_tokens"]
                .as_u64()
                .map(|v| v as usize);
            let max_output_tokens = limits["max_output_tokens"].as_u64().map(|v| v as usize);
            Some(DiscoveredModel {
                id,
                context_window,
                max_output_tokens,
            })
        })
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models
}

// ---------------------------------------------------------------------------
// Model 向导
// ---------------------------------------------------------------------------

/// 交互式填写一个 model 定义，返回 (model_name, ModelConfig)
async fn prompt_model(
    provider_name: &str,
    provider: &ProviderConfig,
) -> Result<(String, ModelConfig)> {
    // 获取模型列表
    print!("  正在获取 {provider_name} 的模型列表...");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let discovered = fetch_model_ids(provider).await;
    println!(
        "\r  {}                                    ",
        if discovered.is_empty() {
            "无法获取模型列表，请手动输入"
        } else {
            "模型列表已就绪"
        }
    );

    let (model_id, api_ctx, api_out) = if discovered.is_empty() {
        let id = Input::with_theme(&theme())
            .with_prompt("Model ID")
            .interact_text()
            .map_err(|e| miette!("交互中断: {e}"))?;
        (id, None, None)
    } else {
        const MANUAL: &str = "手动输入...";
        let labels: Vec<String> = discovered
            .iter()
            .map(|m| m.id.clone())
            .chain(std::iter::once(MANUAL.to_string()))
            .collect();

        let idx = Select::with_theme(&theme())
            .with_prompt("选择模型")
            .items(&labels)
            .default(0)
            .interact()
            .map_err(|e| miette!("交互中断: {e}"))?;

        if labels[idx] == MANUAL {
            let id = Input::with_theme(&theme())
                .with_prompt("Model ID")
                .interact_text()
                .map_err(|e| miette!("交互中断: {e}"))?;
            (id, None, None)
        } else {
            let m = &discovered[idx];
            (m.id.clone(), m.context_window, m.max_output_tokens)
        }
    };

    // API 提供的值优先，否则按 model ID 推断
    let (inferred_ctx, inferred_out) = infer_model_capacity(&model_id);
    let default_ctx = api_ctx.unwrap_or(inferred_ctx);
    let default_out = api_out.unwrap_or(inferred_out);

    let default_name = model_id
        .split(['/', ':'])
        .last()
        .unwrap_or(&model_id)
        .to_string();
    let name: String = Input::with_theme(&theme())
        .with_prompt("Model 名称（config.toml 中的 key）")
        .default(default_name)
        .interact_text()
        .map_err(|e| miette!("交互中断: {e}"))?;

    let context_window: usize = Input::with_theme(&theme())
        .with_prompt("Context window tokens")
        .default(default_ctx)
        .interact_text()
        .map_err(|e| miette!("交互中断: {e}"))?;

    let max_completion: usize = Input::with_theme(&theme())
        .with_prompt("Max completion tokens")
        .default(default_out)
        .interact_text()
        .map_err(|e| miette!("交互中断: {e}"))?;

    Ok((
        name,
        ModelConfig {
            provider: provider_name.to_string(),
            model_id,
            context_window_tokens: context_window,
            max_completion_tokens: max_completion,
            ..ModelConfig::default()
        },
    ))
}

// ---------------------------------------------------------------------------
// 公开 API
// ---------------------------------------------------------------------------

/// 首次运行时的完整 setup 向导，写入 config.toml 后返回生成的 Config
pub async fn run_first_time_setup() -> Result<Config> {
    println!();
    println!("欢迎使用 Daat Locus");
    println!("未找到配置文件，先来完成初始化设置。");
    println!();

    let skip = Select::with_theme(&theme())
        .with_prompt("如何初始化？")
        .items(&[
            "交互式配置（推荐）",
            "跳过，创建默认配置（需手动编辑 config.toml）",
        ])
        .default(0)
        .interact()
        .map_err(|e| miette!("交互中断: {e}"))?;

    if skip == 1 {
        let config = Config::default();
        write_config(&config).await?;
        info("已创建默认配置。请编辑 ~/.daat_locus/config.toml 后重启。");
        return Ok(config);
    }

    // === 配置第一个 Provider ===
    header("步骤 1/2：添加 Provider");
    let (provider_name, provider_config) = prompt_provider(&[]).await?;

    let mut providers = HashMap::new();
    providers.insert(provider_name.clone(), provider_config.clone());

    // === 配置第一个 Model ===
    header("步骤 2/2：添加模型");
    let (model_name, model_config) = prompt_model(&provider_name, &provider_config).await?;

    let mut models = HashMap::new();
    models.insert(model_name.clone(), model_config);

    let config = Config {
        providers,
        models,
        main_model: model_name.clone(),
        judge: JudgeConfig::default(),
        ..Config::default()
    };

    write_config(&config).await?;

    println!();
    println!("配置已写入 ~/.daat_locus/config.toml");
    println!("  main_model = \"{model_name}\" （provider: {provider_name}）");
    println!();

    Ok(config)
}

/// `config add-provider` 子命令
pub async fn run_add_provider() -> Result<()> {
    let mut config = crate::config::load_config()
        .await
        .map_err(|e| miette!("加载配置失败: {e}"))?;

    header("添加 Provider");
    let existing: Vec<String> = config.providers.keys().cloned().collect();
    let (name, provider) = prompt_provider(&existing).await?;

    if config.providers.contains_key(&name) {
        let overwrite = Confirm::with_theme(&theme())
            .with_prompt(format!("provider '{name}' 已存在，是否覆盖？"))
            .default(false)
            .interact()
            .map_err(|e| miette!("交互中断: {e}"))?;
        if !overwrite {
            info("已取消。");
            return Ok(());
        }
    }

    config.providers.insert(name.clone(), provider);
    write_config(&config).await?;
    info(&format!("Provider '{name}' 已保存。"));
    Ok(())
}

/// `config add-model` 子命令
pub async fn run_add_model() -> Result<()> {
    let mut config = crate::config::load_config()
        .await
        .map_err(|e| miette!("加载配置失败: {e}"))?;

    header("添加 Model");
    let provider_names: Vec<String> = config.providers.keys().cloned().collect();
    if provider_names.is_empty() {
        return Err(miette!("没有可用的 provider，请先运行 config add-provider"));
    }
    let provider_idx = if provider_names.len() == 1 {
        info(&format!("使用 provider: {}", provider_names[0]));
        0
    } else {
        Select::with_theme(&theme())
            .with_prompt("绑定 Provider")
            .items(&provider_names)
            .default(0)
            .interact()
            .map_err(|e| miette!("交互中断: {e}"))?
    };
    let provider_name = &provider_names[provider_idx];
    let provider_config = config.providers.get(provider_name).unwrap();
    let (name, model) = prompt_model(provider_name, provider_config).await?;

    if config.models.contains_key(&name) {
        let overwrite = Confirm::with_theme(&theme())
            .with_prompt(format!("model '{name}' 已存在，是否覆盖？"))
            .default(false)
            .interact()
            .map_err(|e| miette!("交互中断: {e}"))?;
        if !overwrite {
            info("已取消。");
            return Ok(());
        }
    }

    config.models.insert(name.clone(), model);

    let set_main = Confirm::with_theme(&theme())
        .with_prompt(format!("将 '{name}' 设为 main_model？"))
        .default(false)
        .interact()
        .map_err(|e| miette!("交互中断: {e}"))?;
    if set_main {
        config.main_model = name.clone();
    }

    write_config(&config).await?;
    info(&format!("Model '{name}' 已保存。"));
    Ok(())
}

/// `config set-main-model` 子命令
pub async fn run_set_main_model() -> Result<()> {
    let mut config = crate::config::load_config()
        .await
        .map_err(|e| miette!("加载配置失败: {e}"))?;

    let model_names: Vec<String> = config.models.keys().cloned().collect();
    if model_names.is_empty() {
        return Err(miette!("没有已配置的 model，请先运行 config add-model"));
    }

    let current_idx = model_names
        .iter()
        .position(|n| n == &config.main_model)
        .unwrap_or(0);

    let idx = Select::with_theme(&theme())
        .with_prompt("选择 main_model")
        .items(&model_names)
        .default(current_idx)
        .interact()
        .map_err(|e| miette!("交互中断: {e}"))?;

    config.main_model = model_names[idx].clone();
    write_config(&config).await?;
    info(&format!("main_model 已设为 '{}'。", config.main_model));
    Ok(())
}

/// `config show` 子命令：打印当前配置摘要（隐藏 secrets）
pub async fn show_config() -> Result<()> {
    let config = crate::config::load_config()
        .await
        .map_err(|e| miette!("加载配置失败: {e}"))?;

    header("Providers");
    for (name, provider) in &config.providers {
        let desc = match provider {
            ProviderConfig::Openai { api_key, base_url } => {
                let masked = mask_secret(api_key);
                let url = base_url.as_deref().unwrap_or("https://api.openai.com");
                format!("openai  url={url}  key={masked}")
            }
            ProviderConfig::GithubCopilot { github_token } => {
                let masked = mask_secret(github_token);
                format!("github-copilot  token={masked}")
            }
            ProviderConfig::OpenaiCompatible { base_url, api_key } => {
                let masked = mask_secret(api_key);
                format!("openai-compatible  url={base_url}  key={masked}")
            }
        };
        println!("  [{name}]  {desc}");
    }

    header("Models");
    for (name, model) in &config.models {
        let main_mark = if name == &config.main_model {
            " ← main"
        } else {
            ""
        };
        println!(
            "  [{name}]{main_mark}  provider={}  model_id={}  ctx={}  max_out={}",
            model.provider,
            model.model_id,
            model.context_window_tokens,
            model.max_completion_tokens
        );
    }

    header("Judge");
    let judge_model = config.judge.model.as_deref().unwrap_or(&config.main_model);
    println!(
        "  enabled={}  model={}  candidates={}  cases={}",
        config.judge.enabled,
        judge_model,
        config.judge.max_pairwise_candidates,
        config.judge.max_pairwise_cases
    );

    header("Hindsight");
    let hindsight_model = config
        .hindsight
        .model
        .as_deref()
        .unwrap_or(&config.main_model);
    println!(
        "  model={}{}  port={}  profile={}",
        hindsight_model,
        if config.hindsight.model.is_none() {
            " (同 main_model)"
        } else {
            ""
        },
        config.hindsight.port,
        config.hindsight.profile,
    );

    println!();
    Ok(())
}

/// `config`（无子命令）：交互式菜单，循环直到用户选择退出
pub async fn run_config_menu() -> Result<()> {
    loop {
        // 加载最新 config 用于状态展示（出错时也继续，允许从无 config 状态开始配置）
        let has_config = crate::config::config_file_exists().await;
        let status = if has_config {
            match crate::config::load_config().await {
                Ok(cfg) => format!(
                    "main_model={} | providers={} | models={}",
                    cfg.main_model,
                    cfg.providers.len(),
                    cfg.models.len()
                ),
                Err(e) => format!("配置加载错误: {e}"),
            }
        } else {
            "尚未配置".to_string()
        };

        println!("\n当前状态：{status}");

        const ITEMS: &[&str] = &[
            "查看配置详情",
            "添加 Provider",
            "添加 Model",
            "更改 main_model",
            "更改 hindsight 模型",
            "退出",
        ];

        let idx = Select::with_theme(&theme())
            .with_prompt("配置管理")
            .items(ITEMS)
            .default(0)
            .interact()
            .map_err(|e| miette!("交互中断: {e}"))?;

        match idx {
            0 => show_config().await?,
            1 => run_add_provider().await?,
            2 => run_add_model().await?,
            3 => run_set_main_model().await?,
            4 => run_set_hindsight_model().await?,
            _ => break,
        }
    }
    Ok(())
}

/// `config set-hindsight-model` 子命令
pub async fn run_set_hindsight_model() -> Result<()> {
    let mut config = crate::config::load_config()
        .await
        .map_err(|e| miette!("加载配置失败: {e}"))?;

    let model_names: Vec<String> = config.models.keys().cloned().collect();
    if model_names.is_empty() {
        return Err(miette!("没有已配置的 model，请先运行 config add-model"));
    }

    const USE_MAIN: &str = "与 main_model 相同（默认）";
    let mut items: Vec<String> = model_names.clone();
    items.push(USE_MAIN.to_string());

    let current_idx = config
        .hindsight
        .model
        .as_ref()
        .and_then(|m| model_names.iter().position(|n| n == m))
        .unwrap_or(items.len() - 1); // 默认选最后一项（USE_MAIN）

    let idx = Select::with_theme(&theme())
        .with_prompt("选择 hindsight 使用的模型")
        .items(&items)
        .default(current_idx)
        .interact()
        .map_err(|e| miette!("交互中断: {e}"))?;

    config.hindsight.model = if items[idx] == USE_MAIN {
        None
    } else {
        Some(model_names[idx].clone())
    };

    write_config(&config).await?;
    let display = config
        .hindsight
        .model
        .as_deref()
        .unwrap_or(&config.main_model);
    info(&format!("hindsight 模型已设为 '{display}'。"));
    Ok(())
}

fn mask_secret(s: &str) -> String {
    let s = s.trim();
    if s.len() <= 8 {
        return "*".repeat(s.len());
    }
    // 显示前4位和最后4位
    let prefix = &s[..4];
    let suffix = &s[s.len() - 4..];
    format!("{prefix}...{suffix}")
}
