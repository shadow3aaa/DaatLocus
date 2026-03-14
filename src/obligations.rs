//! 本模块定义待处理义务列表。

use std::{collections::HashMap, fmt::Display};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{get_spinova_home, projects::ReportTarget};

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Obligations {
    obligations: HashMap<Uuid, Obligation>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Obligation {
    pub source: ObligationSource,
    pub summary: String,
    pub requires_reply: bool,
    pub urgency: Urgency,
    pub status: ObligationStatus,
    pub linked_project: Option<Uuid>,
    pub reply_target: Option<ReportTarget>,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum ObligationSource {
    Telegram,
    Terminal,
    System,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    Low,
    Medium,
    High,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum ObligationStatus {
    Pending,
    Seen,
    Satisfied,
    Dropped,
}

impl Obligations {
    pub async fn new() -> Self {
        let persistence_path = get_spinova_home().await.join("obligations");
        tokio::fs::read(persistence_path)
            .await
            .ok()
            .and_then(|data| postcard::from_bytes::<Self>(&data).ok())
            .unwrap_or_else(Self::default)
    }

    pub fn add(
        &mut self,
        source: ObligationSource,
        summary: String,
        requires_reply: bool,
        urgency: Urgency,
        linked_project: Option<Uuid>,
        reply_target: Option<ReportTarget>,
    ) -> Uuid {
        let id = Uuid::new_v4();
        self.obligations.insert(
            id,
            Obligation {
                source,
                summary,
                requires_reply,
                urgency,
                status: ObligationStatus::Pending,
                linked_project,
                reply_target,
            },
        );
        id
    }

    pub fn set_status(&mut self, id: Uuid, status: ObligationStatus) -> bool {
        let Some(obligation) = self.obligations.get_mut(&id) else {
            return false;
        };
        if obligation.status == status {
            return false;
        }
        obligation.status = status;
        true
    }

    pub fn upsert_existing(
        &mut self,
        id: Uuid,
        summary: String,
        requires_reply: bool,
        urgency: Urgency,
        linked_project: Option<Uuid>,
        reply_target: Option<ReportTarget>,
    ) -> bool {
        let Some(obligation) = self.obligations.get_mut(&id) else {
            return false;
        };

        let mut changed = false;
        if obligation.summary != summary {
            obligation.summary = summary;
            changed = true;
        }
        if obligation.requires_reply != requires_reply {
            obligation.requires_reply = requires_reply;
            changed = true;
        }
        if obligation.urgency != urgency {
            obligation.urgency = urgency;
            changed = true;
        }
        if let Some(linked_project) = linked_project {
            if obligation.linked_project != Some(linked_project) {
                obligation.linked_project = Some(linked_project);
                changed = true;
            }
        }
        if let Some(reply_target) = reply_target {
            if obligation.reply_target.as_ref() != Some(&reply_target) {
                obligation.reply_target = Some(reply_target);
                changed = true;
            }
        }
        if obligation.status != ObligationStatus::Pending {
            obligation.status = ObligationStatus::Pending;
            changed = true;
        }
        changed
    }

    pub fn contains(&self, id: Uuid) -> bool {
        self.obligations.contains_key(&id)
    }

    pub fn get(&self, id: Uuid) -> Option<&Obligation> {
        self.obligations.get(&id)
    }

    pub fn link_project(&mut self, id: Uuid, project_id: Uuid) -> bool {
        let Some(obligation) = self.obligations.get_mut(&id) else {
            return false;
        };
        if obligation.linked_project == Some(project_id) {
            return false;
        }
        obligation.linked_project = Some(project_id);
        true
    }

    pub fn obligations(&self) -> impl Iterator<Item = (Uuid, &Obligation)> {
        self.obligations
            .iter()
            .map(|(id, obligation)| (*id, obligation))
    }

    pub fn active_obligations(&self) -> impl Iterator<Item = (Uuid, &Obligation)> {
        self.obligations().filter(|(_, obligation)| {
            !matches!(
                obligation.status,
                ObligationStatus::Satisfied | ObligationStatus::Dropped
            )
        })
    }

    pub fn has_pending(&self) -> bool {
        self.obligations
            .values()
            .any(|obligation| obligation.status == ObligationStatus::Pending)
    }

    pub async fn shutdown(self) {
        let persistence_path = get_spinova_home().await.join("obligations");
        let data = postcard::to_allocvec(&self).unwrap();
        tokio::fs::write(persistence_path, data).await.unwrap();
    }
}

impl Display for Obligations {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut items = self.active_obligations().collect::<Vec<_>>();
        if items.is_empty() {
            return write!(f, "当前没有待处理义务。");
        }
        items.sort_by_key(|(id, _)| id.to_string());

        for (index, (id, obligation)) in items.into_iter().enumerate() {
            if index > 0 {
                writeln!(f)?;
            }
            writeln!(
                f,
                "- {id}. [{} / {} / 需回复={}] {}",
                obligation.status,
                obligation.urgency,
                yes_no(obligation.requires_reply),
                obligation.summary
            )?;
            if let Some(project_id) = obligation.linked_project {
                writeln!(f, "  来源={}，关联项目={project_id}", obligation.source)?;
            } else {
                writeln!(f, "  来源={}，尚未关联项目", obligation.source)?;
            }
            if let Some(target) = &obligation.reply_target {
                writeln!(f, "  回复目标：{} / {}", target.device, target.target)?;
            }
        }
        Ok(())
    }
}

impl Display for ObligationSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Telegram => write!(f, "Telegram"),
            Self::Terminal => write!(f, "Terminal"),
            Self::System => write!(f, "System"),
        }
    }
}

impl Display for Urgency {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Low => write!(f, "低"),
            Self::Medium => write!(f, "中"),
            Self::High => write!(f, "高"),
        }
    }
}

impl Display for ObligationStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "Pending"),
            Self::Seen => write!(f, "Seen"),
            Self::Satisfied => write!(f, "Satisfied"),
            Self::Dropped => write!(f, "Dropped"),
        }
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "是" } else { "否" }
}
