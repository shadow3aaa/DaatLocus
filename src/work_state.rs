use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fmt::Display;
use uuid::Uuid;

use crate::get_spinova_home;

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct WorkState {
    pub objective: Option<String>,
    #[serde(default)]
    pub item_id: Option<Uuid>,
    #[serde(default)]
    pub work_phase: Option<String>,
    #[serde(default)]
    pub key_anchors: Vec<String>,
    #[serde(default)]
    pub investigation_plan: Vec<String>,
    #[serde(default)]
    pub verify_pending_check: Option<String>,
    #[serde(default)]
    pub last_touched_at_ms: Option<i64>,
}

impl WorkState {
    pub async fn new() -> Self {
        let path = get_spinova_home().await.join("work_state");
        tokio::fs::read(path)
            .await
            .ok()
            .and_then(|data| postcard::from_bytes::<Self>(&data).ok())
            .unwrap_or_default()
    }

    pub async fn shutdown(self) {
        let path = get_spinova_home().await.join("work_state");
        let data = postcard::to_allocvec(&self).unwrap();
        tokio::fs::write(path, data).await.unwrap();
    }

    pub fn has_objective(&self) -> bool {
        self.objective
            .as_ref()
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
    }

    pub fn objective(&self) -> Option<&str> {
        self.objective.as_deref()
    }

    pub fn set_objective(&mut self, objective: String, item_id: Option<Uuid>) {
        self.objective = Some(objective);
        self.item_id = item_id;
        self.touch();
    }

    pub fn clear(&mut self) {
        self.objective = None;
        self.item_id = None;
        self.work_phase = None;
        self.key_anchors.clear();
        self.investigation_plan.clear();
        self.verify_pending_check = None;
        self.last_touched_at_ms = Some(Utc::now().timestamp_millis());
    }

    pub fn clear_if_item(&mut self, item_id: Uuid) {
        if self.item_id == Some(item_id) {
            self.clear();
        }
    }

    pub fn work_phase(&self) -> Option<&str> {
        self.work_phase.as_deref()
    }

    pub fn set_phase(&mut self, work_phase: impl Into<String>) {
        self.work_phase = Some(work_phase.into());
        self.touch();
    }

    pub fn set_guidance(&mut self, key_anchors: Vec<String>, investigation_plan: Vec<String>) {
        self.key_anchors = key_anchors;
        self.investigation_plan = investigation_plan;
        self.touch();
    }

    pub fn set_verify_pending_check(&mut self, verify_pending_check: Option<String>) {
        self.verify_pending_check = verify_pending_check;
        self.touch();
    }

    pub fn touch(&mut self) {
        self.last_touched_at_ms = Some(Utc::now().timestamp_millis());
    }
}

impl Display for WorkState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(objective) = self.objective() {
            writeln!(f, "目标：{objective}")?;
            if let Some(item_id) = self.item_id {
                writeln!(f, "关联 todo：{item_id}")?;
            }
            if let Some(work_phase) = self.work_phase() {
                writeln!(f, "阶段：{work_phase}")?;
            }
            if !self.key_anchors.is_empty() {
                writeln!(f, "关键锚点：")?;
                for anchor in &self.key_anchors {
                    writeln!(f, "- {anchor}")?;
                }
            }
            if !self.investigation_plan.is_empty() {
                writeln!(f, "调查计划：")?;
                for step in &self.investigation_plan {
                    writeln!(f, "- {step}")?;
                }
            }
            if let Some(check) = &self.verify_pending_check {
                writeln!(f, "待验证命令：{check}")?;
            }
            Ok(())
        } else {
            write!(f, "当前没有工作目标。")
        }
    }
}
