use std::{collections::HashMap, sync::Arc, time::Duration};

use miette::{Result, miette};
use reqwest::{StatusCode, multipart};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::config::HindsightConfig;

#[derive(Clone)]
pub struct HindsightClient {
    http: reqwest::Client,
    config: HindsightConfig,
    retain_api: HindsightRetainApi,
}

#[derive(Clone)]
pub struct HindsightRetainHandle {
    tx: mpsc::UnboundedSender<RetainWorkerMessage>,
}

#[derive(Clone, Debug)]
enum HindsightRetainApi {
    MemoriesEndpoint,
    LegacyFilesEndpoint,
}

#[derive(Debug)]
enum RetainWorkerMessage {
    Retain(HindsightRetainJob),
    Flush { reply: oneshot::Sender<Result<()>> },
    Shutdown { reply: oneshot::Sender<()> },
}

#[derive(Clone, Debug)]
pub struct HindsightRetainJob {
    pub items: Vec<HindsightRetainItem>,
    pub document_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HindsightRetainItem {
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default)]
pub struct HindsightRecallOptions {
    pub types: Vec<String>,
    pub max_tokens: usize,
    pub budget: Option<String>,
    pub include_chunks: bool,
    pub max_chunk_tokens: usize,
    pub include_source_facts: bool,
    pub max_source_facts_tokens: usize,
    pub tags: Vec<String>,
    pub tags_match: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct HindsightReflectOptions {
    pub budget: Option<String>,
    pub max_tokens: Option<usize>,
    pub tags: Vec<String>,
    pub tags_match: Option<String>,
    pub include_facts: bool,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize)]
pub struct HindsightRetainResponse {
    #[serde(default)]
    pub success: bool,
    #[serde(default)]
    #[serde(alias = "items_count")]
    pub item_count: usize,
}

#[derive(Clone, Debug, Deserialize)]
pub struct HindsightRecallResponse {
    #[serde(default)]
    pub results: Vec<HindsightRecallResult>,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize)]
pub struct HindsightRecallResult {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub document_id: Option<String>,
    #[serde(default)]
    pub metadata: Option<HashMap<String, String>>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize)]
pub struct HindsightReflectResponse {
    pub text: String,
    #[serde(default)]
    pub structured_output: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize)]
struct HindsightLegacyFileRetainResponse {
    #[serde(default)]
    operation_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct HindsightOperationStatusResponse {
    status: String,
    #[serde(default)]
    error_message: Option<String>,
}

impl HindsightClient {
    pub async fn connect(config: &HindsightConfig) -> Result<Self> {
        if config.base_url.trim().is_empty() {
            return Err(miette!("hindsight base_url must not be empty"));
        }
        if config.bank_id.trim().is_empty() {
            return Err(miette!("hindsight bank_id must not be empty"));
        }
        let timeout = Duration::from_secs(config.request_timeout_secs.max(1));
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|err| miette!("failed to build hindsight http client: {err}"))?;
        let bootstrap = Self {
            http,
            config: config.clone(),
            retain_api: HindsightRetainApi::MemoriesEndpoint,
        };
        let retain_api = bootstrap.detect_retain_api().await?;
        Ok(Self {
            retain_api,
            ..bootstrap
        })
    }

    pub fn spawn_retain_worker(&self) -> HindsightRetainHandle {
        let (tx, mut rx) = mpsc::unbounded_channel::<RetainWorkerMessage>();
        let client = self.clone();
        let bank_ready = Arc::new(Mutex::new(false));
        let bank_ready_for_task = bank_ready.clone();
        tokio::spawn(async move {
            while let Some(message) = rx.recv().await {
                match message {
                    RetainWorkerMessage::Retain(job) => {
                        match retain_job(&client, &bank_ready_for_task, job).await {
                            Ok(()) => {}
                            Err(err) => {
                                let detail = err.to_string();
                                eprintln!("[hindsight] retain failed: {detail}");
                                std::process::exit(1);
                            }
                        }
                    }
                    RetainWorkerMessage::Flush { reply } => {
                        let _ = reply.send(Ok(()));
                    }
                    RetainWorkerMessage::Shutdown { reply } => {
                        let _ = reply.send(());
                        break;
                    }
                }
            }
        });
        HindsightRetainHandle { tx }
    }

    pub async fn ensure_bank(&self) -> Result<()> {
        let url = self.bank_url();
        let mut body = json!({});
        if !self.config.default_reflect_budget.trim().is_empty() {
            body["reflect_mission"] = serde_json::Value::String(format!(
                "Use concise {}-budget reflection for Spinova long-term memory retrieval.",
                self.config.default_reflect_budget
            ));
        }
        let response = self.authorized(self.http.put(url)).json(&body).send().await;
        self.expect_success(response, "create/update hindsight bank")
            .await?;
        Ok(())
    }

    pub async fn delete_bank(&self) -> Result<()> {
        let url = self.bank_url();
        let response = self.authorized(self.http.delete(url)).send().await;
        match self.expect_success(response, "delete hindsight bank").await {
            Ok(_) => Ok(()),
            Err(err) => {
                let message = err.to_string();
                if message.contains("404") {
                    Ok(())
                } else {
                    Err(err)
                }
            }
        }
    }

    pub async fn retain(
        &self,
        items: Vec<HindsightRetainItem>,
        document_id: Option<&str>,
    ) -> Result<HindsightRetainResponse> {
        let items = items
            .into_iter()
            .map(|mut item| {
                if item.document_id.is_none() {
                    item.document_id = document_id.map(|value| value.to_string());
                }
                item
            })
            .collect::<Vec<_>>();
        match self.retain_api {
            HindsightRetainApi::MemoriesEndpoint => self.retain_via_memories(items).await,
            HindsightRetainApi::LegacyFilesEndpoint => self.retain_via_legacy_files(items).await,
        }
    }

    pub async fn recall(
        &self,
        query: &str,
        options: HindsightRecallOptions,
    ) -> Result<HindsightRecallResponse> {
        let url = format!("{}/memories/recall", self.bank_url());
        let body = json!({
            "query": query,
            "types": if options.types.is_empty() { serde_json::Value::Null } else { json!(options.types) },
            "budget": options.budget.unwrap_or_else(|| self.config.default_recall_budget.clone()),
            "max_tokens": options.max_tokens.max(1),
            "include": {
                "chunks": if options.include_chunks {
                    json!({ "max_tokens": options.max_chunk_tokens.max(1) })
                } else {
                    serde_json::Value::Null
                },
                "source_facts": if options.include_source_facts {
                    json!({ "max_tokens": options.max_source_facts_tokens.max(1) })
                } else {
                    serde_json::Value::Null
                }
            },
            "tags": if options.tags.is_empty() { serde_json::Value::Null } else { json!(options.tags) },
            "tags_match": options.tags_match.unwrap_or_else(|| "any".to_string()),
        });
        let response = self
            .authorized(self.http.post(url))
            .json(&body)
            .send()
            .await;
        self.expect_json_success(response, "recall hindsight memories")
            .await
    }

    pub async fn reflect(
        &self,
        query: &str,
        options: HindsightReflectOptions,
    ) -> Result<HindsightReflectResponse> {
        let url = format!("{}/reflect", self.bank_url());
        let body = build_reflect_body(query, &self.config, options);
        let response = self
            .authorized(self.http.post(url))
            .json(&body)
            .send()
            .await;
        self.expect_json_success(response, "reflect hindsight memories")
            .await
    }

    fn bank_url(&self) -> String {
        format!(
            "{}/v1/{}/banks/{}",
            self.config.base_url.trim_end_matches('/'),
            self.config.namespace,
            self.config.bank_id
        )
    }

    async fn detect_retain_api(&self) -> Result<HindsightRetainApi> {
        let url = format!(
            "{}/openapi.json",
            self.config.base_url.trim_end_matches('/')
        );
        let response = self
            .authorized(self.http.get(url))
            .send()
            .await
            .map_err(|err| miette!("probe hindsight openapi failed: {err}"))?;
        let response = self
            .expect_success(Ok(response), "probe hindsight openapi")
            .await?;
        let body = response
            .text()
            .await
            .map_err(|err| miette!("read hindsight openapi failed: {err}"))?;
        let value = serde_json::from_str::<Value>(&body)
            .map_err(|err| miette!("parse hindsight openapi failed: {err}"))?;
        let version = value
            .get("info")
            .and_then(|info| info.get("version"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let has_memories_post = path_has_post(&value, "/v1/default/banks/{bank_id}/memories");
        let has_legacy_files_post =
            path_has_post(&value, "/v1/default/banks/{bank_id}/files/retain");
        let has_operations_status = path_has_method(
            &value,
            "/v1/default/banks/{bank_id}/operations/{operation_id}",
            "get",
        );
        if has_memories_post {
            return Ok(HindsightRetainApi::MemoriesEndpoint);
        }
        if has_legacy_files_post && has_operations_status {
            return Ok(HindsightRetainApi::LegacyFilesEndpoint);
        }
        Err(miette!(
            "unsupported hindsight API (version {version}): expected either POST /v1/default/banks/{{bank_id}}/memories or legacy POST /v1/default/banks/{{bank_id}}/files/retain + GET /operations/{{operation_id}}"
        ))
    }

    async fn retain_via_memories(
        &self,
        items: Vec<HindsightRetainItem>,
    ) -> Result<HindsightRetainResponse> {
        let url = format!("{}/memories", self.bank_url());
        let body = json!({
            "items": items,
            "async": false,
        });
        let response = self
            .authorized(self.http.post(url))
            .json(&body)
            .send()
            .await;
        self.expect_json_success(response, "retain hindsight memories")
            .await
    }

    async fn retain_via_legacy_files(
        &self,
        items: Vec<HindsightRetainItem>,
    ) -> Result<HindsightRetainResponse> {
        let url = format!("{}/files/retain", self.bank_url());
        let files_metadata = items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                json!({
                    "document_id": item.document_id.clone().unwrap_or_else(|| format!("legacy-memory-{}", index + 1)),
                    "context": item.context,
                    "metadata": item.metadata,
                    "tags": item.tags,
                    "timestamp": item.timestamp,
                })
            })
            .collect::<Vec<_>>();
        let request_payload = json!({
            "files_metadata": files_metadata,
        });

        let mut form = multipart::Form::new().text("request", request_payload.to_string());
        for (index, item) in items.iter().enumerate() {
            let file_name = item
                .document_id
                .clone()
                .unwrap_or_else(|| format!("legacy-memory-{}", index + 1));
            let part = multipart::Part::text(item.content.clone())
                .file_name(format!("{file_name}.md"))
                .mime_str("text/plain")
                .map_err(|err| miette!("build hindsight legacy multipart part failed: {err}"))?;
            form = form.part("files", part);
        }

        let response = self
            .authorized(self.http.post(url))
            .multipart(form)
            .send()
            .await;
        let submit = self
            .expect_json_success::<HindsightLegacyFileRetainResponse>(
                response,
                "retain hindsight memories (legacy files)",
            )
            .await?;
        if submit.operation_ids.is_empty() {
            return Err(miette!(
                "legacy hindsight file retain returned no operation ids"
            ));
        }
        for operation_id in &submit.operation_ids {
            self.wait_for_operation(operation_id).await?;
        }
        Ok(HindsightRetainResponse {
            success: true,
            item_count: items.len(),
        })
    }

    async fn wait_for_operation(&self, operation_id: &str) -> Result<()> {
        let url = format!("{}/operations/{}", self.bank_url(), operation_id);
        for _ in 0..120 {
            let response = self.authorized(self.http.get(&url)).send().await;
            let status = self
                .expect_json_success::<HindsightOperationStatusResponse>(
                    response,
                    "poll hindsight operation",
                )
                .await?;
            match status.status.as_str() {
                "completed" | "not_found" => return Ok(()),
                "failed" => {
                    return Err(miette!(
                        "hindsight legacy retain operation failed: {}",
                        status
                            .error_message
                            .unwrap_or_else(|| "unknown failure".to_string())
                    ));
                }
                "pending" => tokio::time::sleep(Duration::from_millis(500)).await,
                other => {
                    return Err(miette!("unknown hindsight operation status: {other}"));
                }
            }
        }
        Err(miette!(
            "timed out waiting for hindsight retain operation {operation_id}"
        ))
    }

    fn authorized(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.config.api_key.trim().is_empty() {
            request
        } else {
            request.bearer_auth(&self.config.api_key)
        }
    }

    async fn expect_success(
        &self,
        response: std::result::Result<reqwest::Response, reqwest::Error>,
        action: &str,
    ) -> Result<reqwest::Response> {
        let response = response.map_err(|err| miette!("{action} failed: {err}"))?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "<failed to read hindsight error body>".to_string());
        let detail = truncate_for_error(&body);
        if status == StatusCode::NOT_FOUND {
            return Err(miette!("{action} failed with 404: {detail}"));
        }
        Err(miette!("{action} failed with HTTP {}: {}", status, detail))
    }

    async fn expect_json_success<T: for<'de> Deserialize<'de>>(
        &self,
        response: std::result::Result<reqwest::Response, reqwest::Error>,
        action: &str,
    ) -> Result<T> {
        let response = self.expect_success(response, action).await?;
        let body = response
            .text()
            .await
            .map_err(|err| miette!("{action} body read failed: {err}"))?;
        serde_json::from_str::<T>(&body).map_err(|err| {
            miette!(
                "{action} returned invalid JSON: {err}; body={}",
                truncate_for_error(&body)
            )
        })
    }
}

impl HindsightRetainHandle {
    pub fn enqueue(&self, job: HindsightRetainJob) -> Result<()> {
        self.tx
            .send(RetainWorkerMessage::Retain(job))
            .map_err(|_| miette!("hindsight retain worker channel closed"))?;
        Ok(())
    }

    pub async fn flush(&self) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(RetainWorkerMessage::Flush { reply: reply_tx })
            .map_err(|_| miette!("hindsight retain worker channel closed"))?;
        reply_rx
            .await
            .map_err(|_| miette!("hindsight retain worker flush reply dropped"))?
    }

    pub async fn shutdown(&self) {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(RetainWorkerMessage::Shutdown { reply: reply_tx })
            .is_ok()
        {
            let _ = reply_rx.await;
        }
    }
}

fn path_has_post(openapi: &Value, path: &str) -> bool {
    path_has_method(openapi, path, "post")
}

fn path_has_method(openapi: &Value, path: &str, method: &str) -> bool {
    openapi
        .get("paths")
        .and_then(|paths| paths.get(path))
        .and_then(|methods| methods.get(method))
        .is_some()
}

async fn retain_job(
    client: &HindsightClient,
    bank_ready: &Arc<Mutex<bool>>,
    job: HindsightRetainJob,
) -> Result<()> {
    {
        let mut ready = bank_ready.lock().await;
        if !*ready {
            client.ensure_bank().await?;
            *ready = true;
        }
    }
    client.retain(job.items, job.document_id.as_deref()).await?;
    Ok(())
}

fn truncate_for_error(text: &str) -> String {
    const MAX_LEN: usize = 600;
    if text.chars().count() <= MAX_LEN {
        return text.to_string();
    }
    let truncated = text.chars().take(MAX_LEN).collect::<String>();
    format!("{truncated}...")
}

fn build_reflect_body(
    query: &str,
    config: &HindsightConfig,
    options: HindsightReflectOptions,
) -> Value {
    let mut body = serde_json::Map::new();
    body.insert("query".to_string(), Value::String(query.to_string()));
    body.insert(
        "budget".to_string(),
        Value::String(
            options
                .budget
                .unwrap_or_else(|| config.default_reflect_budget.clone()),
        ),
    );
    if let Some(max_tokens) = options.max_tokens {
        body.insert("max_tokens".to_string(), json!(max_tokens.max(1)));
    }
    if !options.tags.is_empty() {
        body.insert("tags".to_string(), json!(options.tags));
    }
    if let Some(tags_match) = options.tags_match {
        body.insert("tags_match".to_string(), Value::String(tags_match));
    } else {
        body.insert("tags_match".to_string(), Value::String("any".to_string()));
    }
    if options.include_facts {
        body.insert("include".to_string(), json!({ "facts": {} }));
    }
    Value::Object(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> HindsightConfig {
        HindsightConfig {
            base_url: "http://localhost:8888".to_string(),
            api_key: String::new(),
            namespace: "default".to_string(),
            bank_id: "spinova".to_string(),
            request_timeout_secs: 120,
            default_recall_budget: "mid".to_string(),
            default_reflect_budget: "low".to_string(),
        }
    }

    #[test]
    fn reflect_body_omits_null_only_fields() {
        let body = build_reflect_body(
            "hello",
            &test_config(),
            HindsightReflectOptions {
                budget: None,
                max_tokens: None,
                tags: Vec::new(),
                tags_match: None,
                include_facts: false,
            },
        );
        let object = body.as_object().expect("reflect body should be an object");
        assert_eq!(object.get("query"), Some(&json!("hello")));
        assert_eq!(object.get("budget"), Some(&json!("low")));
        assert_eq!(object.get("tags_match"), Some(&json!("any")));
        assert!(!object.contains_key("max_tokens"));
        assert!(!object.contains_key("include"));
        assert!(!object.contains_key("tags"));
    }

    #[test]
    fn reflect_body_includes_requested_options() {
        let body = build_reflect_body(
            "hello",
            &test_config(),
            HindsightReflectOptions {
                budget: Some("high".to_string()),
                max_tokens: Some(500),
                tags: vec!["user:alice".to_string()],
                tags_match: Some("any_strict".to_string()),
                include_facts: true,
            },
        );
        assert_eq!(
            body,
            json!({
                "query": "hello",
                "budget": "high",
                "max_tokens": 500,
                "tags": ["user:alice"],
                "tags_match": "any_strict",
                "include": { "facts": {} }
            })
        );
    }
}
