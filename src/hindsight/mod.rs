//! Hindsight long-term memory system.

pub mod managed;

use std::{collections::HashMap, sync::Arc, time::Duration};

use miette::{Result, miette};
use reqwest::{StatusCode, multipart};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::config::HindsightConfig;
use crate::hindsight::managed::HindsightManagedServer;

#[derive(Clone)]
pub struct HindsightClient {
    http: reqwest::Client,
    config: HindsightConfig,
    retain_api: HindsightRetainApi,
    supports_update_mode_append: bool,
    restart_support: Option<Arc<HindsightRestartSupport>>,
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
struct HindsightRestartSupport {
    llm_env_vars: Vec<(String, String)>,
    restart_lock: Mutex<()>,
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
    #[serde(skip_serializing_if = "Option::is_none", rename = "update_mode")]
    pub update_mode: Option<String>,
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

#[derive(Clone, Debug, Serialize)]
pub(crate) struct HindsightEntityLabelGroupConfig {
    pub key: String,
    pub description: String,
    #[serde(rename = "type")]
    pub label_type: String,
    pub optional: bool,
    pub tag: bool,
    pub values: Vec<HindsightEntityLabelValueConfig>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct HindsightEntityLabelValueConfig {
    pub value: String,
    pub description: String,
}

const DEFAULT_HINDSIGHT_RECALL_BUDGET: &str = "mid";
const DEFAULT_HINDSIGHT_REFLECT_BUDGET: &str = "low";
const DEFAULT_HINDSIGHT_FLUSH_TIMEOUT_SECS: u64 = 15;
pub const HINDSIGHT_RUNTIME_DOCUMENT_ID: &str = "daat-locus:runtime";
const HINDSIGHT_UPDATE_MODE_APPEND: &str = "append";
const MIN_HINDSIGHT_VERSION_FOR_APPEND: (u64, u64, u64) = (0, 5, 0);

fn default_hindsight_reflect_mission() -> String {
    "This bank is the long-term memory home for the Daat Locus agent. Use it to help the same agent recover context, continue ongoing work, and answer with grounded uncertainty-aware judgments based on stored memories.".to_string()
}

fn default_hindsight_retain_mission() -> String {
    "Retain durable memories that will help the Daat Locus agent continue work over time in the same memory home. Prefer stable project facts, user instructions and preferences, important decisions, and recurring patterns. Skip low-value transient chatter and routine bookkeeping.".to_string()
}

fn default_hindsight_observations_mission() -> String {
    "Observations in this bank should capture stable facts that the Daat Locus agent may need again later in the same long-term memory home. Prefer reusable facts and repeated patterns over one-off transient state.".to_string()
}

fn default_hindsight_entity_labels() -> Vec<HindsightEntityLabelGroupConfig> {
    vec![
        HindsightEntityLabelGroupConfig {
            key: "kind".to_string(),
            description: "The durable knowledge class represented by this memory.".to_string(),
            label_type: "value".to_string(),
            optional: true,
            tag: true,
            values: vec![
                HindsightEntityLabelValueConfig {
                    value: "project_fact".to_string(),
                    description: "Stable facts about the Daat Locus codebase or runtime."
                        .to_string(),
                },
                HindsightEntityLabelValueConfig {
                    value: "user_preference".to_string(),
                    description: "Persistent user or operator preferences.".to_string(),
                },
                HindsightEntityLabelValueConfig {
                    value: "runtime_boundary".to_string(),
                    description: "Behavioral contract or boundary the agent should preserve."
                        .to_string(),
                },
                HindsightEntityLabelValueConfig {
                    value: "failure_pattern".to_string(),
                    description: "Recurring failure mode or risk pattern.".to_string(),
                },
                HindsightEntityLabelValueConfig {
                    value: "strategy_lesson".to_string(),
                    description: "Reusable operational lesson or heuristic.".to_string(),
                },
            ],
        },
        HindsightEntityLabelGroupConfig {
            key: "scope".to_string(),
            description: "The runtime surface or subsystem most relevant to the memory."
                .to_string(),
            label_type: "value".to_string(),
            optional: true,
            tag: true,
            values: vec![
                HindsightEntityLabelValueConfig {
                    value: "runtime".to_string(),
                    description: "Core runtime loop behavior.".to_string(),
                },
                HindsightEntityLabelValueConfig {
                    value: "telegram".to_string(),
                    description: "Telegram event or delivery behavior.".to_string(),
                },
                HindsightEntityLabelValueConfig {
                    value: "workspace".to_string(),
                    description: "Workspace or code editing behavior.".to_string(),
                },
                HindsightEntityLabelValueConfig {
                    value: "sleep".to_string(),
                    description: "Sleep-time reflection and self-improvement.".to_string(),
                },
            ],
        },
        HindsightEntityLabelGroupConfig {
            key: "source".to_string(),
            description: "How the memory entered the system.".to_string(),
            label_type: "value".to_string(),
            optional: true,
            tag: true,
            values: vec![
                HindsightEntityLabelValueConfig {
                    value: "runtime_step".to_string(),
                    description: "A runtime step retained from the live agent loop.".to_string(),
                },
                HindsightEntityLabelValueConfig {
                    value: "sleep_reflection".to_string(),
                    description: "A lesson synthesized during sleep.".to_string(),
                },
            ],
        },
    ]
}

#[derive(Clone, Debug, Default)]
pub struct HindsightReflectOptions {
    pub budget: Option<String>,
    pub max_tokens: Option<usize>,
    pub tags: Vec<String>,
    pub tags_match: Option<String>,
    pub include_facts: bool,
    pub include_tool_calls: bool,
    pub include_tool_call_output: bool,
    pub response_schema: Option<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HindsightBankConfigEnvelope {
    pub bank_id: String,
    pub config: Value,
    pub overrides: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HindsightDeleteObservationsResponse {
    pub success: bool,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub deleted_count: Option<usize>,
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
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub occurred_start: Option<String>,
    #[serde(default)]
    pub occurred_end: Option<String>,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HindsightReflectResponse {
    pub text: String,
    #[serde(default)]
    pub structured_output: Option<serde_json::Value>,
    #[serde(default)]
    pub based_on: Option<HindsightReflectBasedOn>,
    #[serde(default)]
    pub usage: Option<HindsightReflectUsage>,
    #[serde(default)]
    pub trace: Option<HindsightReflectTrace>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HindsightReflectBasedOn {
    #[serde(default)]
    pub memories: Vec<HindsightReflectMemoryFact>,
    #[serde(default)]
    pub mental_models: Vec<HindsightReflectMentalModelFact>,
    #[serde(default)]
    pub directives: Vec<HindsightReflectDirectiveFact>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HindsightReflectMemoryFact {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub occurred_start: Option<String>,
    #[serde(default)]
    pub occurred_end: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HindsightReflectMentalModelFact {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub context: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HindsightReflectDirectiveFact {
    pub id: String,
    pub name: String,
    pub content: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HindsightReflectUsage {
    #[serde(default)]
    pub input_tokens: usize,
    #[serde(default)]
    pub output_tokens: usize,
    #[serde(default)]
    pub total_tokens: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HindsightReflectTrace {
    #[serde(default)]
    pub tool_calls: Vec<HindsightReflectTraceToolCall>,
    #[serde(default)]
    pub llm_calls: Vec<HindsightReflectTraceLlmCall>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HindsightReflectTraceToolCall {
    pub tool: String,
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub output: Option<Value>,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub iteration: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HindsightReflectTraceLlmCall {
    pub scope: String,
    #[serde(default)]
    pub duration_ms: u64,
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
            supports_update_mode_append: false,
            restart_support: None,
        };
        let (retain_api, supports_update_mode_append) =
            bootstrap.detect_retain_capabilities().await?;
        Ok(Self {
            retain_api,
            supports_update_mode_append,
            ..bootstrap
        })
    }

    pub fn with_restart_support(mut self, llm_env_vars: Vec<(String, String)>) -> Self {
        self.restart_support = Some(Arc::new(HindsightRestartSupport {
            llm_env_vars,
            restart_lock: Mutex::new(()),
        }));
        self
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
                        let mut outage_cycle = 0usize;
                        let job_summary = summarize_retain_job(job.clone(), &client.config);
                        loop {
                            match retain_job_with_retry(&client, &bank_ready_for_task, job.clone())
                                .await
                            {
                                Ok(()) => {
                                    if outage_cycle > 0 {
                                        tracing::info!(
                                            "[hindsight] retain recovered after extended outage (cycle {}): {}",
                                            outage_cycle + 1,
                                            job_summary
                                        );
                                    }
                                    break;
                                }
                                Err(err) => {
                                    outage_cycle += 1;
                                    let delay = hindsight_retain_outage_backoff(outage_cycle);
                                    tracing::error!(
                                        "[hindsight] retain exhausted immediate retries; continuing background retry in {:.1}s (cycle {})\njob: {}\n{}",
                                        delay.as_secs_f64(),
                                        outage_cycle,
                                        job_summary,
                                        format_report(&err)
                                    );
                                    tokio::time::sleep(delay).await;
                                }
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

    pub async fn bootstrap_bank(&self) -> Result<()> {
        self.ensure_bank().await?;
        let updates = configured_bank_updates()?;
        if !updates.is_empty() {
            match self.update_bank_config(updates).await {
                Ok(_) => {}
                Err(err) if err.to_string().contains("404") => {
                    tracing::warn!(
                        "hindsight bank config endpoint unavailable; skipping bank config sync: {err}"
                    );
                }
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }

    pub async fn ensure_bank(&self) -> Result<()> {
        let url = self.bank_url();
        let mut body = json!({});
        body["reflect_mission"] = serde_json::Value::String(default_hindsight_reflect_mission());
        self.with_restart_retry("create/update hindsight bank", || {
            let url = url.clone();
            let body = body.clone();
            async move {
                let response = self.authorized(self.http.put(url)).json(&body).send().await;
                self.expect_success(response, "create/update hindsight bank")
                    .await?;
                Ok(())
            }
        })
        .await
    }

    pub async fn delete_bank(&self) -> Result<()> {
        let url = self.bank_url();
        self.with_restart_retry("delete hindsight bank", || {
            let url = url.clone();
            async move {
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
        })
        .await
    }

    pub async fn retain(
        &self,
        items: Vec<HindsightRetainItem>,
        document_id: Option<&str>,
    ) -> Result<HindsightRetainResponse> {
        let items = items
            .into_iter()
            .enumerate()
            .map(|(index, mut item)| {
                if item.document_id.is_none() {
                    item.document_id = document_id.map(|value| value.to_string());
                }
                if self.supports_update_mode_append {
                    if item.document_id.as_deref() == Some(HINDSIGHT_RUNTIME_DOCUMENT_ID)
                        && item.update_mode.is_none()
                    {
                        item.update_mode = Some(HINDSIGHT_UPDATE_MODE_APPEND.to_string());
                    }
                } else {
                    if item.document_id.as_deref() == Some(HINDSIGHT_RUNTIME_DOCUMENT_ID) {
                        item.document_id = Some(fallback_hindsight_document_id(&item, index));
                    }
                    item.update_mode = None;
                }
                item
            })
            .collect::<Vec<_>>();
        self.with_restart_retry("retain hindsight memories", || {
            let items = items.clone();
            async move {
                match self.retain_api {
                    HindsightRetainApi::MemoriesEndpoint => self.retain_via_memories(items).await,
                    HindsightRetainApi::LegacyFilesEndpoint => {
                        self.retain_via_legacy_files(items).await
                    }
                }
            }
        })
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
            "budget": options
                .budget
                .unwrap_or_else(|| DEFAULT_HINDSIGHT_RECALL_BUDGET.to_string()),
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
        self.with_restart_retry("recall hindsight memories", || {
            let url = url.clone();
            let body = body.clone();
            async move {
                let response = self
                    .authorized(self.http.post(url))
                    .json(&body)
                    .send()
                    .await;
                self.expect_json_success(response, "recall hindsight memories")
                    .await
            }
        })
        .await
    }

    pub async fn reflect(
        &self,
        query: &str,
        options: HindsightReflectOptions,
    ) -> Result<HindsightReflectResponse> {
        let url = format!("{}/reflect", self.bank_url());
        let body = build_reflect_body(query, options);
        self.with_restart_retry("reflect hindsight memories", || {
            let url = url.clone();
            let body = body.clone();
            async move {
                let response = self
                    .authorized(self.http.post(url))
                    .json(&body)
                    .send()
                    .await;
                self.expect_json_success(response, "reflect hindsight memories")
                    .await
            }
        })
        .await
    }

    pub async fn get_bank_config(&self) -> Result<HindsightBankConfigEnvelope> {
        let url = format!("{}/config", self.bank_url());
        self.with_restart_retry("get hindsight bank config", || {
            let url = url.clone();
            async move {
                let response = self.authorized(self.http.get(url)).send().await;
                self.expect_json_success(response, "get hindsight bank config")
                    .await
            }
        })
        .await
    }

    pub async fn update_bank_config(
        &self,
        updates: serde_json::Map<String, Value>,
    ) -> Result<HindsightBankConfigEnvelope> {
        let url = format!("{}/config", self.bank_url());
        self.with_restart_retry("update hindsight bank config", || {
            let url = url.clone();
            let updates = updates.clone();
            async move {
                let response = self
                    .authorized(self.http.patch(url))
                    .json(&json!({ "updates": updates }))
                    .send()
                    .await;
                self.expect_json_success(response, "update hindsight bank config")
                    .await
            }
        })
        .await
    }

    pub async fn delete_all_observations(&self) -> Result<HindsightDeleteObservationsResponse> {
        let url = format!("{}/observations", self.bank_url());
        self.with_restart_retry("delete hindsight observations", || {
            let url = url.clone();
            async move {
                let response = self.authorized(self.http.delete(url)).send().await;
                self.expect_json_success(response, "delete hindsight observations")
                    .await
            }
        })
        .await
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.config.port)
    }

    fn bank_url(&self) -> String {
        format!(
            "{}/v1/{}/banks/{}",
            self.base_url(),
            self.config.namespace,
            self.config.bank_id
        )
    }

    async fn detect_retain_capabilities(&self) -> Result<(HindsightRetainApi, bool)> {
        let url = format!("{}/openapi.json", self.base_url());
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
        let supports_update_mode_append = supports_update_mode_append(&value);
        if has_memories_post {
            return Ok((
                HindsightRetainApi::MemoriesEndpoint,
                supports_update_mode_append,
            ));
        }
        if has_legacy_files_post && has_operations_status {
            return Ok((
                HindsightRetainApi::LegacyFilesEndpoint,
                supports_update_mode_append,
            ));
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
        request
    }

    async fn with_restart_retry<T, F, Fut>(&self, action: &str, mut operation: F) -> Result<T>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let mut restart_attempted = false;
        loop {
            match operation().await {
                Ok(value) => return Ok(value),
                Err(err)
                    if !restart_attempted
                        && should_attempt_hindsight_restart(err.to_string().as_str()) =>
                {
                    restart_attempted = true;
                    if self.restart_daemon_if_needed(action, &err).await? {
                        continue;
                    }
                    return Err(err);
                }
                Err(err) => return Err(err),
            }
        }
    }

    async fn restart_daemon_if_needed(&self, action: &str, err: &miette::Report) -> Result<bool> {
        let Some(restart_support) = &self.restart_support else {
            return Ok(false);
        };
        let _guard = restart_support.restart_lock.lock().await;
        let server =
            HindsightManagedServer::new(self.config.clone(), restart_support.llm_env_vars.clone());
        if server.check_health().await {
            return Ok(false);
        }
        tracing::warn!(
            "[hindsight] {} failed; daemon appears down, attempting restart\n{}",
            action,
            format_report(err)
        );
        server.start().await?;
        Ok(true)
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
        self.flush_with_timeout(Duration::from_secs(DEFAULT_HINDSIGHT_FLUSH_TIMEOUT_SECS))
            .await
    }

    pub async fn flush_with_timeout(&self, timeout: Duration) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(RetainWorkerMessage::Flush { reply: reply_tx })
            .map_err(|_| miette!("hindsight retain worker channel closed"))?;
        tokio::time::timeout(timeout, reply_rx)
            .await
            .map_err(|_| {
                miette!(
                    "hindsight retain flush timed out after {:.1}s",
                    timeout.as_secs_f64()
                )
            })?
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

fn supports_update_mode_append(openapi: &Value) -> bool {
    let version = openapi
        .get("info")
        .and_then(|info| info.get("version"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    parse_version_triplet(version)
        .is_some_and(|version| version >= MIN_HINDSIGHT_VERSION_FOR_APPEND)
}

fn parse_version_triplet(version: &str) -> Option<(u64, u64, u64)> {
    let sanitized = version.trim().trim_start_matches(['v', 'V']);
    let sanitized = sanitized
        .split_once('-')
        .map(|(base, _)| base)
        .unwrap_or(sanitized);
    let sanitized = sanitized
        .split_once('+')
        .map(|(base, _)| base)
        .unwrap_or(sanitized);
    let mut parts = sanitized.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    Some((major, minor, patch))
}

fn path_has_method(openapi: &Value, path: &str, method: &str) -> bool {
    openapi
        .get("paths")
        .and_then(|paths| paths.get(path))
        .and_then(|methods| methods.get(method))
        .is_some()
}

fn fallback_hindsight_document_id(item: &HindsightRetainItem, index: usize) -> String {
    item.metadata
        .as_ref()
        .and_then(|metadata| metadata.get("entry_id"))
        .map(String::as_str)
        .map(str::trim)
        .filter(|entry_id| !entry_id.is_empty())
        .map(|entry_id| format!("hindsight-step:{entry_id}"))
        .or_else(|| item.document_id.clone())
        .unwrap_or_else(|| format!("legacy-memory-{}", index + 1))
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

async fn retain_job_with_retry(
    client: &HindsightClient,
    bank_ready: &Arc<Mutex<bool>>,
    job: HindsightRetainJob,
) -> Result<()> {
    const MAX_ATTEMPTS: usize = 4;
    let job_summary = summarize_retain_job(job.clone(), &client.config);

    for attempt in 1..=MAX_ATTEMPTS {
        match retain_job(client, bank_ready, job.clone()).await {
            Ok(()) => {
                if attempt > 1 {
                    tracing::info!(
                        "[hindsight] retain succeeded after retry {attempt}/{MAX_ATTEMPTS}: {job_summary}"
                    );
                }
                return Ok(());
            }
            Err(err) => {
                if attempt == MAX_ATTEMPTS {
                    return Err(miette!(
                        "retain exhausted retries after {MAX_ATTEMPTS} attempts\njob: {job_summary}\n{}",
                        format_report(&err)
                    ));
                }
                let delay = hindsight_retain_backoff(attempt);
                tracing::warn!(
                    "[hindsight] retain attempt {attempt}/{MAX_ATTEMPTS} failed; retrying in {:.1}s\njob: {}\n{}",
                    delay.as_secs_f64(),
                    job_summary,
                    format_report(&err)
                );
                tokio::time::sleep(delay).await;
            }
        }
    }

    unreachable!("retain retry loop must return or error");
}

fn truncate_for_error(text: &str) -> String {
    const MAX_LEN: usize = 600;
    if text.chars().count() <= MAX_LEN {
        return text.to_string();
    }
    let truncated = text.chars().take(MAX_LEN).collect::<String>();
    format!("{truncated}...")
}

fn should_attempt_hindsight_restart(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains("http 502")
        || text.contains("http 503")
        || text.contains("http 504")
        || text.contains("connection refused")
        || text.contains("error trying to connect")
        || text.contains("couldn't connect to server")
        || text.contains("connection reset")
        || text.contains("dns error")
        || text.contains("tcp connect error")
}

fn hindsight_retain_backoff(attempt: usize) -> Duration {
    let seconds = match attempt {
        1 => 2,
        2 => 5,
        3 => 10,
        _ => 15,
    };
    Duration::from_secs(seconds)
}

fn hindsight_retain_outage_backoff(cycle: usize) -> Duration {
    let seconds = match cycle {
        1 => 30,
        2 => 60,
        3 => 120,
        4 => 300,
        _ => 600,
    };
    Duration::from_secs(seconds)
}

fn summarize_retain_job(job: HindsightRetainJob, config: &HindsightConfig) -> String {
    let first_document_id = job
        .document_id
        .clone()
        .or_else(|| job.items.first().and_then(|item| item.document_id.clone()))
        .unwrap_or_else(|| "<none>".to_string());
    let total_chars = job
        .items
        .iter()
        .map(|item| item.content.chars().count())
        .sum::<usize>();
    format!(
        "namespace={} bank_id={} item_count={} document_id={} total_chars={} timeout_secs={}",
        config.namespace,
        config.bank_id,
        job.items.len(),
        first_document_id,
        total_chars,
        config.request_timeout_secs
    )
}

fn format_report(err: &miette::Report) -> String {
    let debug = format!("{err:?}");
    if debug.trim().is_empty() {
        err.to_string()
    } else {
        debug
    }
}

fn build_reflect_body(query: &str, options: HindsightReflectOptions) -> Value {
    let mut body = serde_json::Map::new();
    body.insert("query".to_string(), Value::String(query.to_string()));
    body.insert(
        "budget".to_string(),
        Value::String(
            options
                .budget
                .unwrap_or_else(|| DEFAULT_HINDSIGHT_REFLECT_BUDGET.to_string()),
        ),
    );
    if let Some(max_tokens) = options.max_tokens {
        body.insert("max_tokens".to_string(), json!(max_tokens.max(1)));
    }
    if let Some(response_schema) = options.response_schema {
        body.insert("response_schema".to_string(), response_schema);
    }
    if !options.tags.is_empty() {
        body.insert("tags".to_string(), json!(options.tags));
    }
    if let Some(tags_match) = options.tags_match {
        body.insert("tags_match".to_string(), Value::String(tags_match));
    } else {
        body.insert("tags_match".to_string(), Value::String("any".to_string()));
    }
    let mut include = serde_json::Map::new();
    if options.include_facts {
        include.insert("facts".to_string(), json!({}));
    }
    if options.include_tool_calls {
        let output = if options.include_tool_call_output {
            Value::Bool(true)
        } else {
            Value::Bool(false)
        };
        include.insert("tool_calls".to_string(), json!({ "output": output }));
    }
    if !include.is_empty() {
        body.insert("include".to_string(), Value::Object(include));
    }
    Value::Object(body)
}

fn configured_bank_updates() -> Result<serde_json::Map<String, Value>> {
    let mut updates = serde_json::Map::new();
    updates.insert(
        "reflect_mission".to_string(),
        Value::String(default_hindsight_reflect_mission()),
    );
    updates.insert(
        "retain_mission".to_string(),
        Value::String(default_hindsight_retain_mission()),
    );
    updates.insert(
        "retain_extraction_mode".to_string(),
        Value::String("verbose".to_string()),
    );
    updates.insert(
        "observations_mission".to_string(),
        Value::String(default_hindsight_observations_mission()),
    );
    updates.insert("enable_observations".to_string(), Value::Bool(true));
    updates.insert("disposition_skepticism".to_string(), json!(4));
    updates.insert("disposition_literalism".to_string(), json!(4));
    updates.insert("disposition_empathy".to_string(), json!(3));
    updates.insert("entities_allow_free_form".to_string(), Value::Bool(true));
    updates.insert(
        "entity_labels".to_string(),
        serde_json::to_value(default_hindsight_entity_labels())
            .map_err(|err| miette!("serialize hindsight entity labels failed: {err}"))?,
    );
    Ok(updates)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reflect_body_omits_null_only_fields() {
        let body = build_reflect_body(
            "hello",
            HindsightReflectOptions {
                budget: None,
                max_tokens: None,
                tags: Vec::new(),
                tags_match: None,
                include_facts: false,
                include_tool_calls: false,
                include_tool_call_output: false,
                response_schema: None,
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
            HindsightReflectOptions {
                budget: Some("high".to_string()),
                max_tokens: Some(500),
                tags: vec!["user:alice".to_string()],
                tags_match: Some("any_strict".to_string()),
                include_facts: true,
                include_tool_calls: true,
                include_tool_call_output: false,
                response_schema: None,
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
                "include": {
                    "facts": {},
                    "tool_calls": { "output": false }
                }
            })
        );
    }

    #[test]
    fn configured_bank_updates_include_new_fields() {
        let updates = configured_bank_updates().expect("bank config updates");
        assert_eq!(
            updates.get("reflect_mission"),
            Some(&json!(default_hindsight_reflect_mission()))
        );
        assert_eq!(
            updates.get("retain_mission"),
            Some(&json!(default_hindsight_retain_mission()))
        );
        assert_eq!(
            updates.get("retain_extraction_mode"),
            Some(&json!("verbose"))
        );
        assert_eq!(
            updates.get("observations_mission"),
            Some(&json!(default_hindsight_observations_mission()))
        );
        assert_eq!(updates.get("enable_observations"), Some(&json!(true)));
        assert_eq!(updates.get("disposition_skepticism"), Some(&json!(4)));
        assert_eq!(updates.get("disposition_literalism"), Some(&json!(4)));
        assert_eq!(updates.get("disposition_empathy"), Some(&json!(3)));
        assert_eq!(updates.get("entities_allow_free_form"), Some(&json!(true)));
    }

    #[test]
    fn parse_version_triplet_accepts_prefixed_versions() {
        assert_eq!(parse_version_triplet("0.5.0"), Some((0, 5, 0)));
        assert_eq!(parse_version_triplet("v0.5.1"), Some((0, 5, 1)));
        assert_eq!(parse_version_triplet("0.5.2-beta.1"), Some((0, 5, 2)));
        assert_eq!(parse_version_triplet("0.5.3+build.7"), Some((0, 5, 3)));
    }

    #[test]
    fn fallback_hindsight_document_id_uses_entry_id() {
        let item = HindsightRetainItem {
            content: "memory".to_string(),
            timestamp: None,
            context: None,
            metadata: Some(HashMap::from([(
                "entry_id".to_string(),
                "1234".to_string(),
            )])),
            document_id: Some(HINDSIGHT_RUNTIME_DOCUMENT_ID.to_string()),
            tags: None,
            update_mode: Some(HINDSIGHT_UPDATE_MODE_APPEND.to_string()),
        };

        assert_eq!(
            fallback_hindsight_document_id(&item, 0),
            "hindsight-step:1234"
        );
    }

    #[test]
    fn restart_trigger_matches_transport_failures() {
        assert!(should_attempt_hindsight_restart(
            "recall hindsight memories failed with HTTP 502 Bad Gateway"
        ));
        assert!(should_attempt_hindsight_restart(
            "retain hindsight memories failed: error trying to connect: tcp connect error: Connection refused"
        ));
        assert!(should_attempt_hindsight_restart(
            "probe hindsight openapi failed: couldn't connect to server"
        ));
    }

    #[test]
    fn restart_trigger_ignores_non_transport_failures() {
        assert!(!should_attempt_hindsight_restart(
            "retain hindsight memories failed with HTTP 400: model_not_supported"
        ));
        assert!(!should_attempt_hindsight_restart(
            "workflow planner failed: missing field `should_optimize`"
        ));
    }
}
