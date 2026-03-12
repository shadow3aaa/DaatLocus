//! 此模块定义记忆
//!
//! 记忆分为3层:
//!
//! - L1: 工作记忆
//!   - 一个基于极短FIFO队列的最近行为描述，淘汰后进入L2
//!   - 对当前正在进行的连续行为的描述。每次都由llm重写。
//! - L2: 海马体记忆
//!   - 一个向量搜索记忆库
//! - L3: 固化记忆
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
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{embeding::EmbeddingModel, get_spinova_home};

pub struct Memory {
    l1: L1Memory,
    l2: L2Memory,
    embeder: EmbeddingModel,
    last_2_l1drop: VecDeque<L1Item>, // 记录最近两次L1淘汰的内容
}

impl Memory {
    pub async fn new() -> Self {
        let l1 = L1Memory::new().await;
        let embeder = EmbeddingModel::new();
        let l2 = L2Memory::new(&embeder).await;
        let last_2_l1drop = VecDeque::new();

        Self {
            l1,
            l2,
            embeder,
            last_2_l1drop,
        }
    }

    pub async fn record(&mut self, current_doing: String, action_description: String) {
        if let Some(l1_drop) = self.l1.update(current_doing, action_description) {
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

    pub fn current_doing(&self) -> Option<String> {
        self.l1.trail.back().map(|item| item.current_doing.clone())
    }

    pub fn trail(&self) -> Vec<String> {
        self.l1
            .trail
            .clone()
            .into_iter()
            .map(|item| item.description)
            .collect()
    }

    pub async fn shutdown(self) {
        self.l1.sync_to_disk().await;
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct L1Memory {
    trail: VecDeque<L1Item>,
}

#[derive(Clone, Serialize, Deserialize)]
struct L1Item {
    current_doing: String,
    description: String,
}

impl Display for L1Item {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "在【{}】时，发生：【{}】",
            self.current_doing, self.description
        )
    }
}

impl Default for L1Memory {
    fn default() -> Self {
        Self {
            trail: VecDeque::new(),
        }
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

    fn update(&mut self, current_doing: String, action_description: String) -> Option<L1Item> {
        let item = L1Item {
            current_doing: current_doing.clone(),
            description: action_description.clone(),
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L2MemoryRecord {
    /// 唯一标识符 (UUID v4)
    pub id: String,
    /// 时间戳
    pub timestamp: i64,
    /// 连续行为描述
    pub current_doing: String,
    /// 分立行为描述
    pub description: String,
    /// 夹心上下文
    ///
    /// 它不仅包括向量化本条目使用的current_doing和description，还包括时间上前后相邻条目的current_doing和description
    ///
    /// 这样的设计是为了提供更丰富的上下文信息
    pub sandwich_payload: String,
    /// 语义特征向量，用于相似度搜索
    pub vector: Vec<f32>,
}

impl L2Memory {
    async fn new(embedder: &EmbeddingModel) -> Self {
        let db_path = get_spinova_home().await.join("l2_memory.lancedb");
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
        let doing_col = StringArray::from(vec![l1_drop.current_doing]);
        let desc_col = StringArray::from(vec![l1_drop.description]);
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
