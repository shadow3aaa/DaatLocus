use std::{collections::HashMap, sync::Arc, time::Duration};

use miette::{Result, miette};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::config::HindsightConfig;

#[derive(Clone)]
pub struct HindsightClient {
    http: reqwest::Client,
    config: HindsightConfig,
}

#[derive(Clone)]
pub struct HindsightRetainHandle {
    tx: mpsc::UnboundedSender<RetainWorkerMessage>,
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
    pub context: Option<String>,
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

impl HindsightClient {
    pub fn from_config(config: &HindsightConfig) -> Result<Self> {
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
        Ok(Self {
            http,
            config: config.clone(),
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
                        let _ = retain_job(&client, &bank_ready_for_task, job).await;
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
        let url = format!("{}/memories/retain", self.bank_url());
        let items = items
            .into_iter()
            .map(|mut item| {
                if item.document_id.is_none() {
                    item.document_id = document_id.map(|value| value.to_string());
                }
                item
            })
            .collect::<Vec<_>>();
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
        let body = json!({
            "query": query,
            "budget": options.budget.unwrap_or_else(|| self.config.default_reflect_budget.clone()),
            "context": options.context,
            "max_tokens": options.max_tokens,
            "tags": if options.tags.is_empty() { serde_json::Value::Null } else { json!(options.tags) },
            "tags_match": options.tags_match.unwrap_or_else(|| "any".to_string()),
            "include": if options.include_facts { json!({ "facts": {} }) } else { serde_json::Value::Null }
        });
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
    client
        .retain(job.items, job.document_id.as_deref())
        .await?;
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
