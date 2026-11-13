//! Search through the current model provider, including compatible gateways.
use super::{SearchResult, providers};
use crate::config::{
    Config, ModelConfig, ProviderConfig, normalize_provider_base_url, resolve_env_reference,
};
use miette::{Result, miette};
use reqwest::{Request, header::HeaderValue};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    sync::{LazyLock, Mutex},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Openai,
    Deepseek,
}

pub(super) struct Native {
    kind: Kind,
    base_url: String,
    api_key: String,
    model_id: String,
    auto: bool,
}

// Only confirmed unsupported capabilities are remembered. A real search is the
// probe, so Auto never makes an extra billable request.
static UNSUPPORTED: LazyLock<Mutex<HashSet<[u8; 32]>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

impl Native {
    fn fingerprint(&self) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update([self.kind as u8]);
        for part in [&self.base_url, &self.model_id, &self.api_key] {
            hash.update((part.len() as u64).to_le_bytes());
            hash.update(part.as_bytes());
        }
        hash.finalize().into()
    }
    pub(super) fn remember_unsupported(&self) -> bool {
        if self.auto {
            UNSUPPORTED.lock().unwrap().insert(self.fingerprint());
        }
        self.auto
    }
}

pub(super) fn selected(model: &ModelConfig, config: &Config) -> Option<Native> {
    if model.native_web_search == Some(false) {
        return None;
    }
    let provider = config.providers.get(&model.provider)?;
    let id = model.model_id.to_ascii_lowercase();
    let family = id.rsplit('/').next().unwrap_or(&id);
    // The dedicated search models use Chat Completions rather than Responses web_search.
    if family.contains("-search-") || family.ends_with("-search") || family == "o4-mini" {
        return None;
    }
    let (kind, key, base) = match provider {
        ProviderConfig::Openai { api_key, base_url }
            if family.starts_with("gpt-")
                || family.starts_with("o1")
                || family.starts_with("o3")
                || family.starts_with("o4") =>
        {
            (
                Kind::Openai,
                api_key.as_str(),
                base_url.as_deref().unwrap_or("https://api.openai.com/v1"),
            )
        }
        ProviderConfig::OpenaiCompatible { api_key, base_url }
            if family.starts_with("deepseek-") =>
        {
            (Kind::Deepseek, api_key.as_str(), base_url.as_str())
        }
        ProviderConfig::OpenaiCompatible { api_key, base_url }
            if family.starts_with("gpt-")
                || family.starts_with("o1")
                || family.starts_with("o3")
                || family.starts_with("o4") =>
        {
            (Kind::Openai, api_key.as_str(), base_url.as_str())
        }
        _ => return None,
    };
    let base_url = normalize_provider_base_url(base);
    let native = Native {
        kind,
        base_url,
        api_key: resolve_env_reference(key),
        model_id: model.model_id.clone(),
        auto: model.native_web_search.is_none(),
    };
    if native.auto && UNSUPPORTED.lock().unwrap().contains(&native.fingerprint()) {
        None
    } else {
        Some(native)
    }
}

#[derive(Serialize, JsonSchema)]
pub(super) struct Citation {
    pub url: String,
    pub title: String,
    pub start_index: usize,
    pub end_index: usize,
}
pub(super) struct NativeResponse {
    pub provider: String,
    pub results: Vec<SearchResult>,
    pub answer: Option<String>,
    pub citations: Vec<Citation>,
}

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error("The provider returned a response without running web search")]
struct SearchSkipped;

pub(super) fn search_skipped(error: &miette::Report) -> bool {
    error.downcast_ref::<SearchSkipped>().is_some()
}

pub(super) async fn search(native: &Native, query: &str, limit: usize) -> Result<NativeResponse> {
    if native.api_key.is_empty() || crate::config::parse_env_ref(&native.api_key).is_some() {
        return Err(miette!("The model provider's API key is unavailable"));
    }
    if native.kind == Kind::Deepseek && query.chars().count() > 128 {
        return Err(miette!(
            "DeepSeek search queries must be at most 128 characters"
        ));
    }
    let client = crate::http_client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| miette!("Native WebSearch HTTP client unavailable"))?;
    let request = request(&client, native, query, limit)?;
    let body = providers::send_native(request).await?;
    let mut response = match native.kind {
        Kind::Deepseek => deepseek_response(body)?,
        Kind::Openai => openai_response(body)?,
    };
    sanitize_response(&mut response, &native.api_key, limit);
    Ok(response)
}

fn sanitize_response(response: &mut NativeResponse, api_key: &str, limit: usize) {
    providers::sanitize_results(&mut response.results, api_key, limit);
    response.citations.retain(|citation| {
        crate::web_app::validate_url(&citation.url).is_ok() && !citation.url.contains(api_key)
    });
    for citation in &mut response.citations {
        citation.title = crate::config::redact_secret_text(&citation.title, api_key);
    }
    if response.citations.is_empty()
        || response
            .answer
            .as_deref()
            .is_some_and(|answer| answer.contains(api_key))
    {
        response.answer = None;
        response.citations.clear();
    }
}

fn request(
    client: &reqwest::Client,
    native: &Native,
    query: &str,
    limit: usize,
) -> Result<Request> {
    let endpoint = match native.kind {
        Kind::Openai => format!("{}/responses", native.base_url),
        Kind::Deepseek if native.base_url.ends_with("/v1") => {
            format!("{}/tools/search", native.base_url)
        }
        Kind::Deepseek => format!("{}/v1/tools/search", native.base_url),
    };
    let body = match native.kind {
        Kind::Openai => json!({
            "model": native.model_id, "input": query, "tools": [{"type":"web_search"}],
            "tool_choice": "required", "include": ["web_search_call.action.sources"],
            "store": false,
        }),
        Kind::Deepseek => json!({"query":query, "count":limit, "need_content":false}),
    };
    let mut credential = HeaderValue::from_str(&format!("Bearer {}", native.api_key))
        .map_err(|_| miette!("Invalid model provider credential"))?;
    credential.set_sensitive(true);
    client
        .post(endpoint)
        .header("authorization", credential)
        .header("accept", "application/json")
        .json(&body)
        .build()
        .map_err(|_| miette!("Cannot build native search request"))
}

fn deepseek_response(body: Value) -> Result<NativeResponse> {
    let entries = body
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| miette!("DeepSeek returned an invalid search result set"))?;
    let results = entries
        .iter()
        .map(|entry| {
            Ok(SearchResult {
                url: entry["url"]
                    .as_str()
                    .ok_or_else(|| miette!("Invalid DeepSeek search URL"))?
                    .to_owned(),
                title: entry["title"].as_str().unwrap_or_default().to_owned(),
                content: entry["summary"]
                    .as_str()
                    .or_else(|| entry["snippet"].as_str())
                    .unwrap_or_default()
                    .to_owned(),
                published_at: entry["date"].as_str().map(str::to_owned),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(NativeResponse {
        provider: "deepseek".into(),
        results,
        answer: None,
        citations: Vec::new(),
    })
}

fn openai_response(body: Value) -> Result<NativeResponse> {
    if body["status"] != "completed" {
        return Err(miette!("OpenAI web search did not complete"));
    }
    let output = body["output"]
        .as_array()
        .ok_or_else(|| miette!("OpenAI returned no search output"))?;
    if !output
        .iter()
        .any(|item| item["type"] == "web_search_call" && item["status"] == "completed")
    {
        return Err(SearchSkipped.into());
    }
    let mut results = Vec::new();
    let mut answer = String::new();
    let mut citations = Vec::new();
    for item in output {
        if item["type"] == "web_search_call"
            && item["status"] == "completed"
            && let Some(sources) = item["action"]["sources"].as_array()
        {
            for source in sources {
                if let Some(url) = source["url"].as_str() {
                    results.push(SearchResult {
                        url: url.to_owned(),
                        title: source["title"].as_str().unwrap_or_default().to_owned(),
                        content: String::new(),
                        published_at: None,
                    });
                }
            }
        }
        if item["type"] != "message" {
            continue;
        }
        let Some(parts) = item["content"].as_array() else {
            continue;
        };
        for part in parts {
            if part["type"] != "output_text" {
                continue;
            }
            let Some(text) = part["text"].as_str() else {
                continue;
            };
            if !answer.is_empty() {
                answer.push_str("\n\n");
            }
            let offset = answer.chars().count();
            answer.push_str(text);
            for annotation in part["annotations"].as_array().into_iter().flatten() {
                if annotation["type"] != "url_citation" {
                    continue;
                }
                let Some(url) = annotation["url"].as_str() else {
                    continue;
                };
                let Some(start) = annotation["start_index"]
                    .as_u64()
                    .and_then(|at| usize::try_from(at).ok())
                else {
                    continue;
                };
                let Some(end) = annotation["end_index"]
                    .as_u64()
                    .and_then(|at| usize::try_from(at).ok())
                else {
                    continue;
                };
                if start >= end || end > text.chars().count() {
                    continue;
                }
                let title = annotation["title"].as_str().unwrap_or_default().to_owned();
                citations.push(Citation {
                    url: url.to_owned(),
                    title: title.clone(),
                    start_index: offset + start,
                    end_index: offset + end,
                });
                results.push(SearchResult {
                    url: url.to_owned(),
                    title,
                    content: String::new(),
                    published_at: None,
                });
            }
        }
    }
    // A generated answer without a verifiable source is not evidence of a web search.
    let answer = (!answer.trim().is_empty() && !citations.is_empty()).then_some(answer);
    Ok(NativeResponse {
        provider: "openai".into(),
        results,
        answer,
        citations,
    })
}

#[cfg(test)]
mod tests;
