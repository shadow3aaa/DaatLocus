use super::{SearchResult, settings::Provider};
use crate::config::{Config, ProviderConfig};
use miette::{Result, miette};
use reqwest::{
    Method, Request, StatusCode,
    header::{HeaderMap, HeaderValue},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::HashSet, time::Duration};

const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

pub(super) async fn search(
    provider: Provider,
    key: Option<&str>,
    config: &Config,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchResult>> {
    let mut endpoint = provider.endpoint().to_owned();
    let mut org = None;
    let key = if provider == Provider::Opencode {
        let mut files: Vec<_> = config
            .providers
            .iter()
            .filter_map(|(name, value)| match value {
                ProviderConfig::OpenCodeConsoleOauth { auth_file, .. }
                | ProviderConfig::OpenCodeGoOauth { auth_file, .. } => Some((name, auth_file)),
                _ => None,
            })
            .collect();
        files.sort_by_key(|(name, _)| *name);
        let file = files
            .first()
            .map(|(_, path)| std::path::PathBuf::from(crate::config::resolve_env_reference(path)))
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(crate::providers::opencode_auth_file_path);
        let access = crate::providers::opencode_console_access(&file)
            .await
            .map_err(|_| {
                miette!("Sign in to OpenCode Console in provider settings to use hosted search")
            })?;
        let server = access
            .server
            .as_deref()
            .unwrap_or(crate::providers::OPENCODE_OAUTH_DEFAULT_SERVER);
        let server = if server == "https://console.opencode.ai"
            || server.starts_with("https://console.opencode.ai/")
        {
            server.replacen(
                "https://console.opencode.ai",
                "https://opencode.ai/console",
                1,
            )
        } else {
            server.to_owned()
        };
        endpoint = format!("{}/api/websearch", server.trim_end_matches('/'));
        org = access.org_id;
        access.access_token
    } else {
        let configured =
            key.ok_or_else(|| miette!("Configure this WebSearch provider's API key in Settings"))?;
        let resolved = crate::config::resolve_env_reference(configured);
        if resolved.is_empty() || crate::config::parse_env_ref(&resolved).is_some() {
            return Err(miette!("WebSearch API key is unavailable"));
        }
        resolved
    };
    if key.trim().is_empty() {
        return Err(miette!("WebSearch credential is unavailable"));
    }
    let client = crate::http_client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| miette!("WebSearch HTTP client unavailable"))?;
    let request = request(
        &client,
        &endpoint,
        provider,
        &key,
        org.as_deref(),
        query,
        limit,
    )?;
    let body = send(request).await?;
    let mut results = decode(provider, body)?;
    sanitize_results(&mut results, &key, limit);
    Ok(results)
}

pub(super) fn sanitize_results(results: &mut Vec<SearchResult>, key: &str, limit: usize) {
    let mut seen = HashSet::new();
    results.retain(|item| {
        crate::web_app::validate_url(&item.url).is_ok()
            && !item.url.contains(key)
            && seen.insert(item.url.clone())
    });
    results.truncate(limit);
    for item in results {
        item.title = crate::config::redact_secret_text(&item.title, key);
        item.content = crate::config::redact_secret_text(&item.content, key);
        item.published_at = item
            .published_at
            .take()
            .map(|text| crate::config::redact_secret_text(&text, key));
    }
}

fn request(
    client: &reqwest::Client,
    endpoint: &str,
    provider: Provider,
    key: &str,
    org: Option<&str>,
    query: &str,
    limit: usize,
) -> Result<Request> {
    let mut request = if provider == Provider::Brave {
        client
            .get(endpoint)
            .query(&[("q", query.to_owned()), ("count", limit.to_string())])
    } else {
        let body = match provider {
            Provider::Opencode => json!({"query":query,"providerID":"opencode"}),
            Provider::Tavily => {
                json!({"query":query,"search_depth":"basic","max_results":limit,"include_answer":false})
            }
            Provider::Exa => {
                json!({"query":query,"numResults":limit,"type":"auto","contents":{"text":{"maxCharacters":2000}}})
            }
            Provider::Serper => json!({"q":query,"num":limit}),
            Provider::Firecrawl => json!({"query":query,"limit":limit,"sources":["web"]}),
            Provider::Parallel => {
                json!({"objective":query,"search_queries":[query],"mode":"basic","advanced_settings":{"max_results":limit}})
            }
            Provider::Brave => unreachable!(),
        };
        client.post(endpoint).json(&body)
    };
    let header = match provider {
        Provider::Exa | Provider::Serper | Provider::Parallel => "x-api-key",
        Provider::Brave => "x-subscription-token",
        _ => "authorization",
    };
    let credential = if header == "authorization" {
        format!("Bearer {key}")
    } else {
        key.to_owned()
    };
    let mut credential =
        HeaderValue::from_str(&credential).map_err(|_| miette!("Invalid WebSearch credential"))?;
    credential.set_sensitive(true);
    request = request
        .header(header, credential)
        .header("accept", "application/json")
        .header("user-agent", "DaatLocus");
    if let Some(org) = org {
        request = request.header("x-org-id", org);
    }
    request
        .build()
        .map_err(|_| miette!("Cannot build WebSearch request"))
}

// Pin each hop to public DNS answers and never forward credentials across origins.
pub(super) async fn send(request: Request) -> Result<Value> {
    send_with_native_probe(request, false).await
}

pub(super) async fn send_native(request: Request) -> Result<Value> {
    send_with_native_probe(request, true).await
}

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error(
    "This provider does not support native web search; configure an external search provider or change the model setting"
)]
pub(super) struct NativeSearchUnsupported;

pub(super) fn native_search_unsupported(error: &miette::Report) -> bool {
    error.downcast_ref::<NativeSearchUnsupported>().is_some()
}

async fn send_with_native_probe(mut request: Request, native: bool) -> Result<Value> {
    for hop in 0..=5 {
        let target = request.url().clone();
        if target.scheme() != "https" {
            return Err(miette!("WebSearch requires HTTPS"));
        }
        let addresses = crate::web_app::http::resolve_public_addresses(&target).await?;
        let client = crate::http_client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .resolve_to_addrs(
                target
                    .host_str()
                    .ok_or_else(|| miette!("Search host missing"))?,
                &addresses,
            )
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(Duration::from_secs(60))
            .build()
            .map_err(|_| miette!("WebSearch HTTP client unavailable"))?;
        let retry = request
            .try_clone()
            .ok_or_else(|| miette!("Cannot retain WebSearch request"))?;
        let response = client
            .execute(request)
            .await
            .map_err(|_| miette!("WebSearch request failed; the provider may have processed it"))?;
        if matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
            if hop == 5 {
                return Err(miette!("WebSearch redirect limit exceeded"));
            }
            request = redirected(retry, response.status(), response.headers())?;
            continue;
        }
        return read_response_mode(response, native).await;
    }
    Err(miette!("WebSearch redirect limit exceeded"))
}
fn redirected(mut request: Request, status: StatusCode, headers: &HeaderMap) -> Result<Request> {
    let target = headers
        .get("location")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| request.url().join(value).ok())
        .ok_or_else(|| miette!("Invalid search redirect"))?;
    crate::web_app::validate_url(target.as_str())?;
    if target.scheme() != "https" {
        return Err(miette!("WebSearch redirect requires HTTPS"));
    }
    if target.origin() != request.url().origin() {
        for header in [
            "authorization",
            "x-api-key",
            "x-subscription-token",
            "x-org-id",
            "cookie",
            "proxy-authorization",
        ] {
            request.headers_mut().remove(header);
        }
    }
    if status == StatusCode::SEE_OTHER
        || matches!(status.as_u16(), 301 | 302) && request.method() == Method::POST
    {
        *request.method_mut() = Method::GET;
        *request.body_mut() = None;
        request.headers_mut().remove("content-type");
        request.headers_mut().remove("content-length");
    }
    *request.url_mut() = target;
    Ok(request)
}
#[cfg(test)]
async fn read_response(response: reqwest::Response) -> Result<Value> {
    read_response_mode(response, false).await
}

async fn read_response_mode(mut response: reqwest::Response, native: bool) -> Result<Value> {
    if !response.status().is_success() {
        let status = response.status().as_u16();
        if native && matches!(status, 400 | 404 | 405 | 422 | 501) {
            let mut body = Vec::new();
            let mut complete = true;
            loop {
                match response.chunk().await {
                    Ok(Some(chunk)) if body.len() + chunk.len() <= 16 * 1024 => {
                        body.extend_from_slice(&chunk)
                    }
                    Ok(None) => break,
                    Ok(Some(_)) | Err(_) => {
                        complete = false;
                        break;
                    }
                }
            }
            if complete && native_endpoint_unsupported(status, &body) {
                return Err(NativeSearchUnsupported.into());
            }
        }
        // Provider bodies can echo credentials or contain private diagnostics.
        return Err(miette!("WebSearch provider returned HTTP {status}"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| miette!("Cannot read WebSearch response"))?
    {
        if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(miette!("WebSearch response exceeds the read budget"));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| miette!("WebSearch returned invalid JSON"))
}

fn native_endpoint_unsupported(status: u16, body: &[u8]) -> bool {
    let parsed = serde_json::from_slice::<Value>(body).unwrap_or(Value::Null);
    let code = parsed["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let message = parsed["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if code.contains("model_not_found")
        || message.contains("model")
            && (message.contains("not found")
                || message.contains("does not exist")
                || message.contains("unknown model"))
    {
        return false;
    }
    if matches!(status, 404 | 405 | 501) {
        return true;
    }
    let search = [
        "web_search",
        "web search",
        "search tool",
        "tools/search",
        "tool_choice",
        "tools",
    ]
    .iter()
    .any(|word| message.contains(word) || code.contains(word));
    let incompatible = [
        "unsupported",
        "not support",
        "unrecognized",
        "unknown",
        "invalid tool",
        "not available",
        "not enabled",
    ]
    .iter()
    .any(|word| message.contains(word) || code.contains(word));
    matches!(status, 400 | 422) && search && incompatible
}

#[derive(Deserialize)]
struct Entry {
    url: String,
    title: Option<String>,
    content: Option<String>,
    text: Option<String>,
    description: Option<String>,
    snippet: Option<String>,
    #[serde(default)]
    excerpts: Vec<String>,
    #[serde(rename = "publishedDate")]
    published_date: Option<String>,
    publish_date: Option<String>,
    time: Option<SearchTime>,
}
#[derive(Deserialize)]
struct SearchTime {
    published: Option<i64>,
}
impl Entry {
    fn into_result(self) -> SearchResult {
        SearchResult {
            url: self.url,
            title: self.title.unwrap_or_default(),
            content: self
                .content
                .or(self.text)
                .or(self.description)
                .or(self.snippet)
                .unwrap_or_else(|| self.excerpts.join("\n\n")),
            published_at: self.published_date.or(self.publish_date).or_else(|| {
                self.time
                    .and_then(|time| time.published)
                    .and_then(chrono::DateTime::from_timestamp_millis)
                    .map(|date| date.to_rfc3339())
            }),
        }
    }
}
fn decode(provider: Provider, body: Value) -> Result<Vec<SearchResult>> {
    #[derive(Deserialize)]
    struct Results {
        results: Vec<Entry>,
    }
    #[derive(Deserialize)]
    struct Brave {
        #[serde(rename = "type")]
        _kind: String,
        web: Option<Results>,
    }
    #[derive(Deserialize)]
    struct Organic {
        organic: Vec<SerperEntry>,
    }
    #[derive(Deserialize)]
    struct SerperEntry {
        link: String,
        title: Option<String>,
        snippet: Option<String>,
        date: Option<String>,
    }
    #[derive(Deserialize)]
    struct Firecrawl {
        success: bool,
        data: FirecrawlData,
    }
    #[derive(Deserialize)]
    struct FirecrawlData {
        web: Vec<Entry>,
    }
    #[derive(Deserialize)]
    struct Hosted {
        #[serde(rename = "providerID")]
        provider_id: String,
        results: Vec<Entry>,
    }
    let invalid = |_| miette!("WebSearch returned an invalid result set");
    let entries = match provider {
        Provider::Opencode => {
            let body: Hosted = serde_json::from_value(body).map_err(invalid)?;
            if body.provider_id != "opencode" {
                return Err(miette!("OpenCode returned a different search provider"));
            }
            body.results
        }
        Provider::Brave => serde_json::from_value::<Brave>(body)
            .map_err(invalid)?
            .web
            .map(|web| web.results)
            .unwrap_or_default(),
        Provider::Firecrawl => {
            let body: Firecrawl = serde_json::from_value(body).map_err(invalid)?;
            if !body.success {
                return Err(miette!("Firecrawl search failed"));
            }
            body.data.web
        }
        Provider::Serper => {
            return Ok(serde_json::from_value::<Organic>(body)
                .map_err(invalid)?
                .organic
                .into_iter()
                .map(|entry| SearchResult {
                    url: entry.link,
                    title: entry.title.unwrap_or_default(),
                    content: entry.snippet.unwrap_or_default(),
                    published_at: entry.date,
                })
                .collect());
        }
        _ => {
            serde_json::from_value::<Results>(body)
                .map_err(invalid)?
                .results
        }
    };
    Ok(entries.into_iter().map(Entry::into_result).collect())
}

#[cfg(test)]
mod tests;
