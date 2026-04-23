use miette::Result;

use crate::{
    config::{Config, ProviderConfig},
    providers::exchange_copilot_session_token,
};

pub async fn hindsight_llm_env_vars(config: &Config) -> Result<Vec<(String, String)>> {
    let model = config.hindsight_model_config();
    match config.hindsight_provider_config() {
        ProviderConfig::GithubCopilot { github_token } => {
            let github_token = resolve_hindsight_env_value(github_token);
            let (session_token, base_url, _) =
                exchange_copilot_session_token(&github_token).await?;
            Ok(hindsight_copilot_llm_env_vars(
                &session_token,
                &base_url,
                &model.model_id,
            ))
        }
        ProviderConfig::Openai { api_key, base_url } => {
            let mut vars = vec![
                ("HINDSIGHT_API_LLM_PROVIDER".into(), "openai".into()),
                (
                    "HINDSIGHT_API_LLM_API_KEY".into(),
                    resolve_hindsight_env_value(api_key),
                ),
                ("HINDSIGHT_API_LLM_MODEL".into(), model.model_id.clone()),
            ];
            if let Some(url) = base_url.as_deref().filter(|url| !url.trim().is_empty()) {
                vars.push((
                    "HINDSIGHT_API_LLM_BASE_URL".into(),
                    resolve_hindsight_env_value(url),
                ));
            }
            Ok(vars)
        }
        ProviderConfig::OpenaiCompatible { base_url, api_key } => Ok(vec![
            ("HINDSIGHT_API_LLM_PROVIDER".into(), "openai".into()),
            (
                "HINDSIGHT_API_LLM_API_KEY".into(),
                resolve_hindsight_env_value(api_key),
            ),
            ("HINDSIGHT_API_LLM_MODEL".into(), model.model_id.clone()),
            (
                "HINDSIGHT_API_LLM_BASE_URL".into(),
                resolve_hindsight_env_value(base_url),
            ),
        ]),
    }
}

pub(crate) fn hindsight_copilot_llm_env_vars(
    session_token: &str,
    base_url: &str,
    model_id: &str,
) -> Vec<(String, String)> {
    vec![
        ("HINDSIGHT_API_LLM_PROVIDER".into(), "openai".into()),
        (
            "HINDSIGHT_API_LLM_API_KEY".into(),
            session_token.to_string(),
        ),
        ("HINDSIGHT_API_LLM_MODEL".into(), model_id.to_string()),
        ("HINDSIGHT_API_LLM_BASE_URL".into(), base_url.to_string()),
    ]
}

fn resolve_hindsight_env_value(value: &str) -> String {
    let trimmed = value.trim();
    if let Some(inner) = trimmed.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
        std::env::var(inner).unwrap_or_else(|_| trimmed.to_string())
    } else if let Some(inner) = trimmed.strip_prefix('$') {
        std::env::var(inner).unwrap_or_else(|_| trimmed.to_string())
    } else {
        trimmed.to_string()
    }
}
