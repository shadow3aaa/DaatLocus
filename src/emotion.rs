//! 本模块包含无聊状态等

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Default)]
pub struct Emotion {
    /// 无聊程度，范围0-1
    pub boredom: f32,
}

impl Emotion {
    pub async fn new() -> Self {
        let emotion_persistence_path = crate::get_spinova_home().await.join("emotion");
        tokio::fs::read(emotion_persistence_path)
            .await
            .ok()
            .and_then(|data| postcard::from_bytes::<Self>(&data).ok())
            .unwrap_or_else(|| Self::default())
    }

    pub async fn shutdown(self) {
        let emotion_persistence_path = crate::get_spinova_home().await.join("emotion");
        let data = postcard::to_allocvec(&self).unwrap();
        tokio::fs::write(emotion_persistence_path, data)
            .await
            .unwrap();
    }
}
