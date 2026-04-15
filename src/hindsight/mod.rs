//! Hindsight long-term memory system.

pub mod preprocess;

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

#[derive(Clone, Debug)]
pub(crate) struct HindsightDirectiveConfig {
    pub name: String,
    pub content: String,
    pub priority: i64,
    pub is_active: bool,
    pub tags: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct HindsightMentalModelTemplateConfig {
    pub id: String,
    pub name: String,
    pub source_query: String,
    pub max_tokens: usize,
    pub tags: Vec<String>,
    pub refresh_after_consolidation: bool,
}

const DEFAULT_HINDSIGHT_RECALL_BUDGET: &str = "mid";
const DEFAULT_HINDSIGHT_REFLECT_BUDGET: &str = "low";

fn default_hindsight_reflect_mission() -> String {
    "Reason like a persistent Daat Locus runtime maintainer. Prefer grounded, reviewable judgments about project continuity, runtime boundaries, tool usage, user preferences, and operational risk. Distinguish stable knowledge from transient state, and surface uncertainty when evidence is incomplete.".to_string()
}

fn default_hindsight_retain_mission() -> String {
    "Retain durable engineering knowledge for Daat Locus. Prefer architectural boundaries, event/app semantics, user preferences, failure patterns, tool usage constraints, and decisions with future reuse value. Ignore greetings, transient bookkeeping, redundant retries, and low-signal logs unless they materially explain a durable lesson.".to_string()
}

fn default_hindsight_observations_mission() -> String {
    "Observations should capture stable facts about the project, runtime behavior, user preferences, and recurring engineering patterns. Consolidate repeated evidence into reusable knowledge. Avoid overfitting to one-off events or transient machine state.".to_string()
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

pub(crate) fn builtin_hindsight_directives() -> Vec<HindsightDirectiveConfig> {
    vec![
        HindsightDirectiveConfig {
            name: "Ground Claims In Evidence".to_string(),
            content: "Prefer conclusions that can be tied back to retrieved memories, observations, or mental models. If evidence is weak or mixed, say so explicitly instead of overstating certainty.".to_string(),
            priority: 100,
            is_active: true,
            tags: vec!["runtime".to_string(), "reasoning".to_string()],
        },
        HindsightDirectiveConfig {
            name: "Respect Stable Runtime Boundaries".to_string(),
            content: "Preserve stable contracts around App, Event, PendingWork, Plan, Memory, and finish_and_send. Do not collapse distinct runtime concepts or rewrite boundaries based on one-off situations.".to_string(),
            priority: 90,
            is_active: true,
            tags: vec!["runtime".to_string(), "architecture".to_string()],
        },
        HindsightDirectiveConfig {
            name: "Avoid Transient Overfitting".to_string(),
            content: "Do not elevate transient machine state, temporary confusion, or one-off logs into durable preferences or project facts unless the evidence repeats across turns.".to_string(),
            priority: 80,
            is_active: true,
            tags: vec!["memory".to_string(), "retention".to_string()],
        },
    ]
}

pub(crate) fn builtin_hindsight_mental_models() -> Vec<HindsightMentalModelTemplateConfig> {
    vec![
        HindsightMentalModelTemplateConfig {
            id: "project-state".to_string(),
            name: "Project State".to_string(),
            source_query: "What is the current project state of Daat Locus, including active workstreams, unresolved technical threads, and recently stabilized decisions?".to_string(),
            max_tokens: 1600,
            tags: vec![
                "mental-model".to_string(),
                "scope:project".to_string(),
                "scope:runtime".to_string(),
            ],
            refresh_after_consolidation: true,
        },
        HindsightMentalModelTemplateConfig {
            id: "runtime-boundaries".to_string(),
            name: "Runtime Boundaries".to_string(),
            source_query: "What stable runtime boundaries and agent-facing contracts define how Daat Locus should treat App, Event, PendingWork, Plan, Memory, and finish_and_send?".to_string(),
            max_tokens: 1400,
            tags: vec![
                "mental-model".to_string(),
                "scope:runtime".to_string(),
                "kind:runtime_boundary".to_string(),
            ],
            refresh_after_consolidation: true,
        },
        HindsightMentalModelTemplateConfig {
            id: "user-preferences".to_string(),
            name: "User Preferences".to_string(),
            source_query: "What stable user preferences, communication expectations, and collaboration patterns should Daat Locus preserve in this workspace?".to_string(),
            max_tokens: 1200,
            tags: vec![
                "mental-model".to_string(),
                "scope:user".to_string(),
                "kind:user_preference".to_string(),
            ],
            refresh_after_consolidation: true,
        },
        HindsightMentalModelTemplateConfig {
            id: "runtime-strategy".to_string(),
            name: "Runtime Strategy".to_string(),
            source_query: "What stable runtime strategies, learned heuristics, and prompt-level lessons should guide Daat Locus when continuing work in this repository?".to_string(),
            max_tokens: 1400,
            tags: vec![
                "mental-model".to_string(),
                "scope:runtime".to_string(),
                "kind:strategy_lesson".to_string(),
            ],
            refresh_after_consolidation: true,
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
pub struct HindsightDirectiveListResponse {
    #[serde(default)]
    pub items: Vec<HindsightDirective>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HindsightDirective {
    pub id: String,
    pub bank_id: String,
    pub name: String,
    pub content: String,
    #[serde(default)]
    pub priority: i64,
    #[serde(default = "default_true")]
    pub is_active: bool,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HindsightMentalModelListResponse {
    #[serde(default)]
    pub items: Vec<HindsightMentalModel>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HindsightMentalModel {
    pub id: String,
    pub bank_id: String,
    pub name: String,
    #[serde(default)]
    pub source_query: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub max_tokens: Option<usize>,
    #[serde(default)]
    pub trigger: Option<HindsightMentalModelTrigger>,
    #[serde(default)]
    pub last_refreshed_at: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub reflect_response: Option<HindsightReflectResponse>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HindsightMentalModelTrigger {
    #[serde(default)]
    pub refresh_after_consolidation: bool,
    #[serde(default)]
    pub fact_types: Vec<String>,
    #[serde(default)]
    pub exclude_mental_models: bool,
    #[serde(default)]
    pub exclude_mental_model_ids: Vec<String>,
    #[serde(default)]
    pub tags_match: Option<String>,
    #[serde(default)]
    pub tag_groups: Vec<HindsightTagGroup>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HindsightTagGroup {
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub r#match: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HindsightMentalModelCreateResponse {
    #[serde(default)]
    pub mental_model_id: Option<String>,
    pub operation_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HindsightAsyncOperationResponse {
    pub operation_id: String,
    pub status: String,
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
        match self.sync_directives(&builtin_hindsight_directives()).await {
            Ok(_) => {}
            Err(err) if err.to_string().contains("404") => {
                tracing::warn!(
                    "hindsight directives endpoint unavailable; skipping directive sync: {err}"
                );
            }
            Err(err) => return Err(err),
        }
        Ok(())
    }

    pub async fn ensure_bank(&self) -> Result<()> {
        let url = self.bank_url();
        let mut body = json!({});
        body["reflect_mission"] = serde_json::Value::String(default_hindsight_reflect_mission());
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
        let body = build_reflect_body(query, options);
        let response = self
            .authorized(self.http.post(url))
            .json(&body)
            .send()
            .await;
        self.expect_json_success(response, "reflect hindsight memories")
            .await
    }

    pub async fn get_bank_config(&self) -> Result<HindsightBankConfigEnvelope> {
        let url = format!("{}/config", self.bank_url());
        let response = self.authorized(self.http.get(url)).send().await;
        self.expect_json_success(response, "get hindsight bank config")
            .await
    }

    pub async fn update_bank_config(
        &self,
        updates: serde_json::Map<String, Value>,
    ) -> Result<HindsightBankConfigEnvelope> {
        let url = format!("{}/config", self.bank_url());
        let response = self
            .authorized(self.http.patch(url))
            .json(&json!({ "updates": updates }))
            .send()
            .await;
        self.expect_json_success(response, "update hindsight bank config")
            .await
    }

    pub async fn list_directives(&self, active_only: bool) -> Result<Vec<HindsightDirective>> {
        let mut url = reqwest::Url::parse(&format!("{}/directives", self.bank_url()))
            .map_err(|err| miette!("build hindsight directives url failed: {err}"))?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("active_only", &active_only.to_string());
            query.append_pair("limit", "1000");
            query.append_pair("offset", "0");
        }
        let response = self.authorized(self.http.get(url)).send().await;
        Ok(self
            .expect_json_success::<HindsightDirectiveListResponse>(
                response,
                "list hindsight directives",
            )
            .await?
            .items)
    }

    pub async fn create_directive(
        &self,
        directive: &HindsightDirectiveConfig,
    ) -> Result<HindsightDirective> {
        let url = format!("{}/directives", self.bank_url());
        let response = self
            .authorized(self.http.post(url))
            .json(&json!({
                "name": directive.name,
                "content": directive.content,
                "priority": directive.priority,
                "is_active": directive.is_active,
                "tags": directive.tags,
            }))
            .send()
            .await;
        self.expect_json_success(response, "create hindsight directive")
            .await
    }

    pub async fn update_directive(
        &self,
        directive_id: &str,
        directive: &HindsightDirectiveConfig,
    ) -> Result<HindsightDirective> {
        let url = format!("{}/directives/{}", self.bank_url(), directive_id);
        let response = self
            .authorized(self.http.patch(url))
            .json(&json!({
                "name": directive.name,
                "content": directive.content,
                "priority": directive.priority,
                "is_active": directive.is_active,
                "tags": directive.tags,
            }))
            .send()
            .await;
        self.expect_json_success(response, "update hindsight directive")
            .await
    }

    pub async fn sync_directives(
        &self,
        directives: &[HindsightDirectiveConfig],
    ) -> Result<Vec<HindsightDirective>> {
        let existing = self.list_directives(false).await?;
        let mut by_name = HashMap::new();
        for directive in existing {
            by_name.insert(directive.name.clone(), directive);
        }
        let mut synced = Vec::new();
        for directive in directives
            .iter()
            .filter(|item| !item.name.trim().is_empty())
        {
            let result = if let Some(existing) = by_name.get(&directive.name) {
                self.update_directive(&existing.id, directive).await?
            } else {
                self.create_directive(directive).await?
            };
            synced.push(result);
        }
        Ok(synced)
    }

    pub async fn delete_all_observations(&self) -> Result<HindsightDeleteObservationsResponse> {
        let url = format!("{}/observations", self.bank_url());
        let response = self.authorized(self.http.delete(url)).send().await;
        self.expect_json_success(response, "delete hindsight observations")
            .await
    }

    pub async fn list_mental_models(
        &self,
        tags: &[String],
        detail: &str,
    ) -> Result<Vec<HindsightMentalModel>> {
        let mut url = reqwest::Url::parse(&format!("{}/mental-models", self.bank_url()))
            .map_err(|err| miette!("build hindsight mental-model url failed: {err}"))?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("detail", detail);
            query.append_pair("limit", "100");
            query.append_pair("offset", "0");
            if !tags.is_empty() {
                query.append_pair("tags_match", "all");
                for tag in tags {
                    query.append_pair("tags", tag);
                }
            }
        }
        let request = self.authorized(self.http.get(url));
        let response = request.send().await;
        Ok(self
            .expect_json_success::<HindsightMentalModelListResponse>(
                response,
                "list hindsight mental models",
            )
            .await?
            .items)
    }

    pub async fn create_mental_model(
        &self,
        template: &HindsightMentalModelTemplateConfig,
    ) -> Result<HindsightMentalModelCreateResponse> {
        let url = format!("{}/mental-models", self.bank_url());
        let response = self
            .authorized(self.http.post(url))
            .json(&json!({
                "id": normalize_optional_id(&template.id),
                "name": template.name,
                "source_query": template.source_query,
                "max_tokens": template.max_tokens,
                "tags": template.tags,
                "trigger": {
                    "refresh_after_consolidation": template.refresh_after_consolidation,
                },
            }))
            .send()
            .await;
        self.expect_json_success(response, "create hindsight mental model")
            .await
    }

    pub async fn update_mental_model(
        &self,
        model_id: &str,
        template: &HindsightMentalModelTemplateConfig,
    ) -> Result<HindsightMentalModel> {
        let url = format!("{}/mental-models/{}", self.bank_url(), model_id);
        let response = self
            .authorized(self.http.patch(url))
            .json(&json!({
                "name": template.name,
                "source_query": template.source_query,
                "max_tokens": template.max_tokens,
                "tags": template.tags,
                "trigger": {
                    "refresh_after_consolidation": template.refresh_after_consolidation,
                },
            }))
            .send()
            .await;
        self.expect_json_success(response, "update hindsight mental model")
            .await
    }

    pub async fn refresh_mental_model(
        &self,
        model_id: &str,
    ) -> Result<HindsightAsyncOperationResponse> {
        let url = format!("{}/mental-models/{}/refresh", self.bank_url(), model_id);
        let response = self.authorized(self.http.post(url)).send().await;
        self.expect_json_success(response, "refresh hindsight mental model")
            .await
    }

    pub async fn sync_mental_models(
        &self,
        templates: &[HindsightMentalModelTemplateConfig],
        refresh_existing: bool,
    ) -> Result<Vec<String>> {
        let existing = self.list_mental_models(&[], "metadata").await?;
        let mut by_id = HashMap::new();
        for model in existing {
            by_id.insert(model.id.clone(), model);
        }

        let mut operation_ids = Vec::new();
        for template in templates
            .iter()
            .filter(|item| !item.id.trim().is_empty() && !item.name.trim().is_empty())
        {
            if by_id.contains_key(&template.id) {
                self.update_mental_model(&template.id, template).await?;
                if refresh_existing {
                    operation_ids.push(self.refresh_mental_model(&template.id).await?.operation_id);
                }
            } else {
                let response = self.create_mental_model(template).await?;
                operation_ids.push(response.operation_id);
            }
        }
        Ok(operation_ids)
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

fn normalize_optional_id(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn default_true() -> bool {
    true
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
}
