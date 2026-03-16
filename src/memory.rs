//! 此模块定义记忆
//!
//! 记忆分为4层:
//!
//! - L0: 印象 TODO
//!   - 一个LRU关键词set，次数越多，权重越大（越“有印象”）。用于提示LLM大概率有相关记忆
//! - L1: 工作记忆
//!   - 一个基于极短FIFO队列的最近行为描述，淘汰后进入L2
//!   - 对当前正在进行的连续行为的描述。每次都由llm重写。
//! - L2: 海马体记忆
//!   - 一个向量搜索记忆库
//! - L3: 固化记忆 TODO
//!   - 目前搁置，理论上它应该在长期空闲时整理L2记忆进入L3
use std::{collections::VecDeque, fmt::Display, sync::Arc};

use arrow_array::{
    Array, Int64Array, RecordBatch, RecordBatchIterator, StringArray,
    builder::{FixedSizeListBuilder, PrimitiveBuilder},
    types::Float32Type,
};
use chrono::Utc;
use futures::StreamExt;
use lancedb::{
    arrow::arrow_schema::{ArrowError, DataType, Field, Schema},
    query::{ExecutableQuery, QueryBase, Select},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    embeding::{EmbeddingModel, similarity},
    get_spinova_home,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub enum L3EntryKind {
    TerminalPolicy,
    InteractionBoundary,
    ProjectContinuity,
    ToolUsage,
    FailureAvoidance,
    General,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub enum L3EntryStability {
    Tentative,
    Stable,
    Canonical,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct L3EntryDraft {
    pub kind: L3EntryKind,
    pub lesson: String,
    pub evidence_summary: String,
    pub retrieval_text: String,
    pub confidence: f32,
    pub stability: L3EntryStability,
    pub source_trace_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct L3Entry {
    pub id: String,
    pub kind: L3EntryKind,
    pub lesson: String,
    pub evidence_summary: String,
    pub retrieval_text: String,
    pub confidence: f32,
    pub stability: L3EntryStability,
    pub source_trace_ids: Vec<String>,
    pub updated_at_ms: i64,
    pub vector: Vec<f32>,
}

impl L3Entry {
    fn render(&self) -> String {
        format!(
            "习得经验【{:?}/{:?}，置信度 {:.2}】：{}\n依据：{}",
            self.kind, self.stability, self.confidence, self.lesson, self.evidence_summary
        )
    }
}

pub struct Memory {
    l1: L1Memory,
    l2: L2Memory,
    l3: L3Memory,
    embeder: EmbeddingModel,
    last_2_l1drop: VecDeque<L1Item>, // 记录最近两次L1淘汰的内容
}

impl Memory {
    pub async fn new() -> Self {
        let l1 = L1Memory::new().await;
        let embeder = EmbeddingModel::new();
        let l2 = L2Memory::new(&embeder).await;
        let l3 = L3Memory::new().await;
        let last_2_l1drop = VecDeque::new();

        Self {
            l1,
            l2,
            l3,
            embeder,
            last_2_l1drop,
        }
    }

    pub async fn empty() -> Self {
        let embeder = EmbeddingModel::new();
        let l2 = L2Memory::reset(&embeder).await;

        Self {
            l1: L1Memory::default(),
            l2,
            l3: L3Memory::default(),
            embeder,
            last_2_l1drop: VecDeque::new(),
        }
    }

    pub async fn record(
        &mut self,
        thread_focus: String,
        observation: String,
        action_description: String,
        evidence_lines: Vec<String>,
    ) {
        let event_summary = format!(
            "观察与结论：{}\n采取动作：{}",
            observation.trim(),
            action_description.trim()
        );
        let anchor_source = if evidence_lines.is_empty() {
            format!(
                "{}\n{}\n{}",
                thread_focus.trim(),
                observation.trim(),
                action_description.trim()
            )
        } else {
            format!(
                "{}\n{}\n{}\n{}",
                thread_focus.trim(),
                observation.trim(),
                action_description.trim(),
                evidence_lines.join("\n")
            )
        };
        let anchors = extract_memory_anchors(&anchor_source);
        let thread_effect = infer_thread_effect(&observation, &action_description);
        self.ingest_l1_item(thread_focus, event_summary, anchors, thread_effect)
            .await;
    }

    pub async fn record_encoded(
        &mut self,
        thread_focus: String,
        event_summary: String,
        anchors: Vec<String>,
        thread_effect: String,
    ) {
        let anchors = anchors
            .into_iter()
            .filter_map(|anchor| parse_memory_anchor(anchor))
            .collect::<Vec<_>>();
        let thread_effect = parse_thread_effect(&thread_effect);
        self.ingest_l1_item(thread_focus, event_summary, anchors, thread_effect)
            .await;
    }

    async fn ingest_l1_item(
        &mut self,
        thread_focus: String,
        event_summary: String,
        anchors: Vec<MemoryAnchor>,
        thread_effect: L1ThreadEffect,
    ) {
        if let Some(l1_drop) = self
            .l1
            .update(thread_focus, event_summary, anchors, thread_effect)
        {
            let mut sandwich_payload = String::new();
            // 之前在做什么，最多取2条
            if !self.last_2_l1drop.is_empty() {
                sandwich_payload.push_str("在这之前：\n");
                for drop in &self.last_2_l1drop {
                    sandwich_payload.push_str(&drop.to_string());
                    sandwich_payload.push_str("然后\n");
                }
                if self.last_2_l1drop.len() == 2 {
                    self.last_2_l1drop.pop_front();
                }
            }
            self.last_2_l1drop.push_back(l1_drop.clone());
            // 当时在做什么
            sandwich_payload.push_str("当时：\n");
            sandwich_payload.push_str(&l1_drop.to_string());
            sandwich_payload.push_str("\n");
            // 之后在做什么，最多取3条
            if !self.l1.trail.is_empty() {
                sandwich_payload.push_str("后来：\n");
                for item in self.l1.trail.iter().take(3) {
                    sandwich_payload.push_str(&item.to_string());
                    sandwich_payload.push_str("然后\n");
                }
            }

            self.l2
                .ingest(&mut self.embeder, l1_drop, sandwich_payload)
                .await;
        }
    }

    pub async fn search_mem(&mut self, query: &str, top_k: usize) -> Vec<String> {
        self.l2.search(&mut self.embeder, query, top_k).await
    }

    pub fn search_l3(&mut self, query: &str, top_k: usize) -> Vec<String> {
        self.l3.search(&mut self.embeder, query, top_k)
    }

    pub fn upsert_l3_entries(&mut self, drafts: Vec<L3EntryDraft>) {
        self.l3.upsert(&mut self.embeder, drafts);
    }

    pub fn current_thread_focus(&self) -> Option<String> {
        self.l1.trail.back().map(|item| item.thread_focus.clone())
    }

    pub fn trail(&self) -> Vec<String> {
        self.l1
            .trail
            .clone()
            .into_iter()
            .map(|item| item.render_event())
            .collect()
    }

    pub async fn shutdown(self) {
        self.l1.sync_to_disk().await;
        self.l3.sync_to_disk().await;
    }
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct L1Memory {
    trail: VecDeque<L1Item>,
}

#[derive(Clone, Serialize, Deserialize)]
struct L1Item {
    thread_focus: String,
    event_summary: String,
    anchors: Vec<MemoryAnchor>,
    thread_effect: L1ThreadEffect,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct MemoryAnchor {
    kind: MemoryAnchorKind,
    value: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
enum MemoryAnchorKind {
    Url,
    FileName,
    Uuid,
    Command,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
enum L1ThreadEffect {
    #[default]
    Continue,
    Blocked,
    Clarified,
    Switched,
    Completed,
}

impl Display for L1Item {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.render_for_memory())
    }
}

impl Display for L1ThreadEffect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Continue => write!(f, "继续"),
            Self::Blocked => write!(f, "受阻"),
            Self::Clarified => write!(f, "澄清"),
            Self::Switched => write!(f, "切换"),
            Self::Completed => write!(f, "完成"),
        }
    }
}

impl L1Item {
    fn render_for_memory(&self) -> String {
        let mut rendered = format!(
            "主线：【{}】\n本轮事件：【{}】\n对主线影响：【{}】",
            self.thread_focus, self.event_summary, self.thread_effect
        );
        if !self.anchors.is_empty() {
            rendered.push_str("\n关键锚点：");
            for anchor in &self.anchors {
                rendered.push_str(&format!("\n- {:?}: {}", anchor.kind, anchor.value));
            }
        }
        rendered
    }

    fn render_event(&self) -> String {
        let mut rendered = format!("{}\n主线影响：{}", self.event_summary, self.thread_effect);
        if !self.anchors.is_empty() {
            let anchors = self
                .anchors
                .iter()
                .map(|anchor| anchor.value.clone())
                .collect::<Vec<_>>()
                .join(" | ");
            rendered.push_str(&format!("\n关键锚点：{anchors}"));
        }
        rendered
    }
}

impl L1Memory {
    /// 队列最大长度
    const MAX_CAPACITY: usize = 10; // TODO: 考虑按实际的token长度来限制而不是元素数量?未验证哪种更合理

    async fn new() -> Self {
        let l1_persistence_path = get_spinova_home().await.join("l1_memory");
        tokio::fs::read(l1_persistence_path)
            .await
            .ok()
            .and_then(|data| postcard::from_bytes::<Self>(&data).ok())
            .unwrap_or_else(|| Self::default())
    }

    fn update(
        &mut self,
        thread_focus: String,
        event_summary: String,
        anchors: Vec<MemoryAnchor>,
        thread_effect: L1ThreadEffect,
    ) -> Option<L1Item> {
        let item = L1Item {
            thread_focus,
            event_summary,
            anchors,
            thread_effect,
        };
        self.trail.push_back(item);
        if self.trail.len() >= Self::MAX_CAPACITY {
            self.trail.pop_front()
        } else {
            None
        }
    }

    async fn sync_to_disk(&self) {
        let l1_persistence_path = get_spinova_home().await.join("l1_memory");
        let data = postcard::to_allocvec(self).unwrap();
        tokio::fs::write(l1_persistence_path, data).await.unwrap();
    }
}

pub struct L2Memory {
    table: lancedb::Table,
}

impl L2Memory {
    async fn new(embedder: &EmbeddingModel) -> Self {
        let db_path = get_spinova_home().await.join("l2_memory.lancedb");
        Self::open_or_create(db_path, embedder).await
    }

    async fn reset(embedder: &EmbeddingModel) -> Self {
        let db_path = get_spinova_home().await.join("l2_memory.lancedb");
        if db_path.exists() {
            let _ = tokio::fs::remove_dir_all(&db_path).await;
        }
        Self::open_or_create(db_path, embedder).await
    }

    async fn open_or_create(db_path: std::path::PathBuf, embedder: &EmbeddingModel) -> Self {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new("timestamp", DataType::Int64, false),
            Field::new("current_doing", DataType::Utf8, false),
            Field::new("description", DataType::Utf8, false),
            Field::new("sandwich_payload", DataType::Utf8, false),
            Field::new(
                "vector",
                DataType::FixedSizeList(
                    Arc::new(Field::new("item", DataType::Float32, true)),
                    embedder.dimension() as i32,
                ),
                false,
            ),
        ]));
        let conn = lancedb::connect(db_path.to_str().unwrap())
            .execute()
            .await
            .unwrap();
        let table = match conn.open_table("memories").execute().await {
            Ok(t) => t,
            Err(_) => conn
                .create_empty_table("memories", schema)
                .execute()
                .await
                .unwrap(),
        };
        Self { table }
    }

    async fn ingest(
        &mut self,
        embedder: &mut EmbeddingModel,
        l1_drop: L1Item,
        sandwich_payload: String,
    ) {
        let vector_data = embedder.encode(&l1_drop.to_string());

        let id = Uuid::new_v4().to_string();
        let timestamp = Utc::now().timestamp_millis();

        let id_col = StringArray::from(vec![id]);
        let ts_col = Int64Array::from(vec![timestamp]);
        let event_render = l1_drop.render_event();
        let doing_col = StringArray::from(vec![l1_drop.thread_focus]);
        let desc_col = StringArray::from(vec![event_render]);
        let sandwich_col = StringArray::from(vec![sandwich_payload]);

        // 向量列比较特殊：它是一个 512 维的 FixedSizeList
        let mut vector_builder = FixedSizeListBuilder::new(
            PrimitiveBuilder::<Float32Type>::new(),
            embedder.dimension() as i32,
        );
        vector_builder.values().append_slice(&vector_data);
        vector_builder.append(true);
        let vector_col = vector_builder.finish();

        let schema_ref = self.table.schema().await.unwrap();
        let batch = RecordBatch::try_new(
            schema_ref.clone(),
            vec![
                Arc::new(id_col),
                Arc::new(ts_col),
                Arc::new(doing_col),
                Arc::new(desc_col),
                Arc::new(sandwich_col),
                Arc::new(vector_col),
            ],
        )
        .unwrap();
        let batches = RecordBatchIterator::new(vec![Ok::<_, ArrowError>(batch)], schema_ref);

        self.table.add(batches).execute().await.unwrap();
    }

    async fn search(
        &mut self,
        embedder: &mut EmbeddingModel,
        query: &str,
        top_k: usize,
    ) -> Vec<String> {
        let query_vector = embedder.encode_query(query);
        let mut results = self
            .table
            .vector_search(query_vector.as_slice())
            .unwrap()
            .limit(top_k)
            .select(Select::Columns(vec!["sandwich_payload".to_string()]))
            .execute()
            .await
            .unwrap();

        let mut retrieved_payloads = Vec::new();

        while let Some(batch_result) = results.next().await {
            let batch = batch_result.unwrap();

            let payload_col = batch
                .column_by_name("sandwich_payload")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();

            for i in 0..payload_col.len() {
                if !payload_col.is_null(i) {
                    retrieved_payloads.push(payload_col.value(i).to_string());
                }
            }
        }

        retrieved_payloads
    }
}

#[derive(Clone, Serialize, Deserialize, Default)]
struct L3Memory {
    entries: Vec<L3Entry>,
}

impl L3Memory {
    async fn new() -> Self {
        let persistence_path = get_spinova_home().await.join("l3_memory");
        tokio::fs::read(persistence_path)
            .await
            .ok()
            .and_then(|data| postcard::from_bytes::<Self>(&data).ok())
            .unwrap_or_default()
    }

    fn search(&mut self, embedder: &mut EmbeddingModel, query: &str, top_k: usize) -> Vec<String> {
        if self.entries.is_empty() {
            return Vec::new();
        }
        let query_vector = embedder.encode_query(query);
        let mut ranked = self
            .entries
            .iter()
            .map(|entry| (similarity(&query_vector, &entry.vector), entry))
            .collect::<Vec<_>>();
        ranked.sort_by(|left, right| {
            right
                .0
                .partial_cmp(&left.0)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        ranked
            .into_iter()
            .take(top_k)
            .map(|(_, entry)| entry.render())
            .collect()
    }

    fn upsert(&mut self, embedder: &mut EmbeddingModel, drafts: Vec<L3EntryDraft>) {
        for draft in drafts {
            let updated_at_ms = Utc::now().timestamp_millis();
            let vector = embedder.encode_query(&draft.retrieval_text);
            if let Some(existing) = self.entries.iter_mut().find(|entry| {
                entry.kind == draft.kind && entry.lesson.trim() == draft.lesson.trim()
            }) {
                existing.evidence_summary = draft.evidence_summary;
                existing.retrieval_text = draft.retrieval_text;
                existing.confidence = existing.confidence.max(draft.confidence);
                existing.stability = max_stability(&existing.stability, &draft.stability);
                for trace_id in draft.source_trace_ids {
                    if !existing.source_trace_ids.iter().any(|id| id == &trace_id) {
                        existing.source_trace_ids.push(trace_id);
                    }
                }
                existing.updated_at_ms = updated_at_ms;
                existing.vector = vector;
            } else {
                self.entries.push(L3Entry {
                    id: Uuid::new_v4().to_string(),
                    kind: draft.kind,
                    lesson: draft.lesson,
                    evidence_summary: draft.evidence_summary,
                    retrieval_text: draft.retrieval_text,
                    confidence: draft.confidence,
                    stability: draft.stability,
                    source_trace_ids: draft.source_trace_ids,
                    updated_at_ms,
                    vector,
                });
            }
        }
    }

    async fn sync_to_disk(&self) {
        let persistence_path = get_spinova_home().await.join("l3_memory");
        let data = postcard::to_allocvec(self).unwrap();
        tokio::fs::write(persistence_path, data).await.unwrap();
    }
}

fn max_stability(current: &L3EntryStability, incoming: &L3EntryStability) -> L3EntryStability {
    use L3EntryStability::*;
    match (current, incoming) {
        (Canonical, _) | (_, Canonical) => Canonical,
        (Stable, _) | (_, Stable) => Stable,
        _ => Tentative,
    }
}

fn infer_thread_effect(observation: &str, action_description: &str) -> L1ThreadEffect {
    let text = format!("{observation}\n{action_description}");
    if contains_any(&text, &["完成", "已完成", "结束", "成功标准已达到"]) {
        L1ThreadEffect::Completed
    } else if contains_any(
        &text,
        &[
            "失败", "404", "无法", "无效", "受阻", "报错", "中断", "卡住",
        ],
    ) {
        L1ThreadEffect::Blocked
    } else if contains_any(&text, &["补充说明", "澄清", "确认", "请确认", "请求提供"])
    {
        L1ThreadEffect::Clarified
    } else if contains_any(&text, &["切换", "转到", "改为", "聚焦到"]) {
        L1ThreadEffect::Switched
    } else {
        L1ThreadEffect::Continue
    }
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn extract_memory_anchors(text: &str) -> Vec<MemoryAnchor> {
    let mut anchors = Vec::new();

    for token in text.split_whitespace() {
        let cleaned = token
            .trim_matches(|c: char| {
                matches!(
                    c,
                    '。' | '，'
                        | ','
                        | ';'
                        | '；'
                        | ')'
                        | '('
                        | ']'
                        | '['
                        | '"'
                        | '\''
                        | '：'
                        | ':'
                )
            })
            .to_string();
        if cleaned.is_empty() {
            continue;
        }

        if (cleaned.starts_with("http://") || cleaned.starts_with("https://"))
            && !anchors
                .iter()
                .any(|anchor: &MemoryAnchor| anchor.value == cleaned)
        {
            anchors.push(MemoryAnchor {
                kind: MemoryAnchorKind::Url,
                value: cleaned,
            });
            continue;
        }

        if Uuid::parse_str(&cleaned).is_ok()
            && !anchors
                .iter()
                .any(|anchor: &MemoryAnchor| anchor.value == cleaned)
        {
            anchors.push(MemoryAnchor {
                kind: MemoryAnchorKind::Uuid,
                value: cleaned,
            });
            continue;
        }

        let lower = cleaned.to_ascii_lowercase();
        if [
            ".zip", ".tar", ".tgz", ".gz", ".rar", ".7z", ".apk", ".so", ".dll",
        ]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
            && !anchors
                .iter()
                .any(|anchor: &MemoryAnchor| anchor.value == cleaned)
        {
            anchors.push(MemoryAnchor {
                kind: MemoryAnchorKind::FileName,
                value: cleaned,
            });
        }
    }

    if let Some(command_line) = text
        .lines()
        .find(|line| line.contains("TerminalInput") || line.contains("终端实际输入："))
        .map(|line| line.trim().to_string())
    {
        anchors.push(MemoryAnchor {
            kind: MemoryAnchorKind::Command,
            value: command_line,
        });
    }

    anchors
}

fn parse_thread_effect(value: &str) -> L1ThreadEffect {
    match value.trim().to_ascii_lowercase().as_str() {
        "blocked" => L1ThreadEffect::Blocked,
        "clarified" => L1ThreadEffect::Clarified,
        "switched" => L1ThreadEffect::Switched,
        "completed" => L1ThreadEffect::Completed,
        _ => L1ThreadEffect::Continue,
    }
}

fn parse_memory_anchor(value: String) -> Option<MemoryAnchor> {
    let trimmed = value.trim().to_string();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    let kind = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        MemoryAnchorKind::Url
    } else if Uuid::parse_str(&trimmed).is_ok() {
        MemoryAnchorKind::Uuid
    } else if [
        ".zip", ".tar", ".tgz", ".gz", ".rar", ".7z", ".apk", ".so", ".dll",
    ]
    .iter()
    .any(|suffix| lower.ends_with(suffix))
    {
        MemoryAnchorKind::FileName
    } else if trimmed.contains("TerminalInput")
        || trimmed.contains("终端实际输入")
        || trimmed.contains("wget ")
        || trimmed.contains("curl ")
        || trimmed.contains("git ")
        || trimmed.contains("cargo ")
    {
        MemoryAnchorKind::Command
    } else {
        MemoryAnchorKind::Command
    };
    Some(MemoryAnchor {
        kind,
        value: trimmed,
    })
}
