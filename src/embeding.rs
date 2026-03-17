//! 此模块封装向量模型功能

use std::thread;

use ort::{
    session::{Session, builder::GraphOptimizationLevel},
    value::Value,
};
use tokenizers::Tokenizer;

/// 内置向量模型
pub struct EmbeddingModel {
    session: Session,
    tokenizer: Tokenizer,
}

impl EmbeddingModel {
    const MODEL_DIMENSION: usize = 512;
    const MAX_SEQUENCE_LENGTH: usize = 512;
    const QUERY_INSTRUCTION_FOR_RETRIEVAL: &str = "为这个句子生成表示以用于检索相关文章："; // 见<https://huggingface.co/BAAI/bge-large-zh-v1.5#model-list>

    pub fn new() -> Self {
        let tokenizer = Tokenizer::from_bytes(
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/models/bge-small-zh-v1.5/tokenizer.json"
            ))
            .as_slice(),
        )
        .unwrap();
        let session = Session::builder()
            .unwrap()
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .unwrap()
            .with_intra_threads(thread::available_parallelism().unwrap().into())
            .unwrap()
            .commit_from_memory(include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/models/bge-small-zh-v1.5/model.onnx"
            )))
            .unwrap();

        Self { tokenizer, session }
    }

    /// 返回向量维度
    pub fn dimension(&self) -> usize {
        Self::MODEL_DIMENSION
    }

    /// 将查询文本编码为向量，归一化后返回
    pub fn encode_query(&mut self, text: &str) -> Vec<f32> {
        let query = format!("{}{}", Self::QUERY_INSTRUCTION_FOR_RETRIEVAL, text);
        self.encode(&query)
    }

    /// 将文本编码为向量，归一化后返回
    pub fn encode(&mut self, text: &str) -> Vec<f32> {
        let encoding = self.tokenizer.encode(text, true).unwrap();

        let mut input_ids: Vec<i64> = encoding.get_ids().iter().map(|&i| i as i64).collect();
        let mut attention_mask: Vec<i64> = encoding
            .get_attention_mask()
            .iter()
            .map(|&i| i as i64)
            .collect();
        let mut token_type_ids: Vec<i64> =
            encoding.get_type_ids().iter().map(|&i| i as i64).collect();

        // BGE small uses a 512-token context window. Long traces/snapshots must be
        // truncated before ONNX inference, otherwise positional embeddings fail.
        if input_ids.len() > Self::MAX_SEQUENCE_LENGTH {
            input_ids.truncate(Self::MAX_SEQUENCE_LENGTH);
            attention_mask.truncate(Self::MAX_SEQUENCE_LENGTH);
            token_type_ids.truncate(Self::MAX_SEQUENCE_LENGTH);
        }

        let seq_len = input_ids.len();

        let input_ids_arr = ndarray::Array2::from_shape_vec((1, seq_len), input_ids).unwrap();
        let attention_mask_arr =
            ndarray::Array2::from_shape_vec((1, seq_len), attention_mask).unwrap();
        let token_type_ids_arr =
            ndarray::Array2::from_shape_vec((1, seq_len), token_type_ids).unwrap();

        let input_ids_tensor = Value::from_array(input_ids_arr).unwrap();
        let attention_mask_tensor = Value::from_array(attention_mask_arr).unwrap();
        let token_type_ids_tensor = Value::from_array(token_type_ids_arr).unwrap();

        let outputs = self
            .session
            .run(ort::inputs![
                "input_ids" => input_ids_tensor,
                "attention_mask" => attention_mask_tensor,
                "token_type_ids" => token_type_ids_tensor,
            ])
            .unwrap();

        let (_shape, data) = outputs["last_hidden_state"]
            .try_extract_tensor::<f32>()
            .unwrap();

        // bge-small-zh-v1.5的输出为512维，所以直接截取前512维作为文本的向量表示
        let mut embedding: Vec<f32> = data[0..Self::MODEL_DIMENSION].to_vec();

        let norm: f32 = embedding.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > 0.0 {
            for v in embedding.iter_mut() {
                *v /= norm;
            }
        }

        embedding
    }
}

/// 计算两个向量的相似度
#[inline]
pub fn similarity(vec1: &[f32], vec2: &[f32]) -> f32 {
    // 因为向量已经归一化，直接计算点积即可得到余弦相似度
    vec1.iter().zip(vec2.iter()).map(|(a, b)| a * b).sum()
}

#[test]
fn test_embedding() {
    let mut model = EmbeddingModel::new();
    let embedding = model.encode("早上好");
    let embedding2 = model.encode("早安");
    assert!(embedding.len() == EmbeddingModel::MODEL_DIMENSION);
    assert!(embedding2.len() == EmbeddingModel::MODEL_DIMENSION);
    let sim = similarity(&embedding, &embedding2);
    assert!(sim > 0.6); // 语义相似度应该较高
}
