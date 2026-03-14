//! 本模块定义项目列表。

use std::{collections::HashMap, fmt::Display};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{device::DeviceId, get_spinova_home};

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Projects {
    projects: HashMap<Uuid, Project>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Project {
    pub title: String,
    pub origin: ProjectOrigin,
    pub success_criteria: String,
    pub status: ProjectStatus,
    pub report_back_to: Option<ReportTarget>,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
pub enum ProjectOrigin {
    SelfInitiated,
    Telegram,
    Terminal,
    System,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum ProjectStatus {
    Active,
    Blocked,
    Completed,
    Abandoned,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct ReportTarget {
    pub device: DeviceId,
    pub target: String,
}

impl Projects {
    pub async fn new() -> Self {
        let persistence_path = get_spinova_home().await.join("projects");
        tokio::fs::read(persistence_path)
            .await
            .ok()
            .and_then(|data| postcard::from_bytes::<Self>(&data).ok())
            .unwrap_or_else(Self::default)
    }

    pub fn add(
        &mut self,
        title: String,
        origin: ProjectOrigin,
        success_criteria: String,
        report_back_to: Option<ReportTarget>,
    ) -> Uuid {
        let id = Uuid::new_v4();
        self.projects.insert(
            id,
            Project {
                title,
                origin,
                success_criteria,
                status: ProjectStatus::Active,
                report_back_to,
            },
        );
        id
    }

    pub fn set_status(&mut self, id: Uuid, status: ProjectStatus) -> bool {
        let Some(project) = self.projects.get_mut(&id) else {
            return false;
        };
        if project.status == status {
            return false;
        }
        project.status = status;
        true
    }

    pub fn get(&self, id: Uuid) -> Option<&Project> {
        self.projects.get(&id)
    }

    pub fn projects(&self) -> impl Iterator<Item = (Uuid, &Project)> {
        self.projects.iter().map(|(id, project)| (*id, project))
    }

    pub fn has_active(&self) -> bool {
        self.projects
            .values()
            .any(|project| matches!(project.status, ProjectStatus::Active))
    }

    pub async fn shutdown(self) {
        let persistence_path = get_spinova_home().await.join("projects");
        let data = postcard::to_allocvec(&self).unwrap();
        tokio::fs::write(persistence_path, data).await.unwrap();
    }
}

impl Display for Projects {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.projects.is_empty() {
            return write!(f, "当前没有项目。");
        }

        let mut items = self.projects().collect::<Vec<_>>();
        items.sort_by_key(|(id, _)| id.to_string());

        for (index, (id, project)) in items.into_iter().enumerate() {
            if index > 0 {
                writeln!(f)?;
            }
            writeln!(
                f,
                "- {id}. [{} / {}] {}",
                project.status, project.origin, project.title
            )?;
            writeln!(f, "  成功标准：{}", project.success_criteria)?;
            match &project.report_back_to {
                Some(target) => writeln!(f, "  回报对象：{} / {}", target.device, target.target)?,
                None => writeln!(f, "  回报对象：无")?,
            }
        }
        Ok(())
    }
}

impl Display for ProjectOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SelfInitiated => write!(f, "Self"),
            Self::Telegram => write!(f, "Telegram"),
            Self::Terminal => write!(f, "Terminal"),
            Self::System => write!(f, "System"),
        }
    }
}

impl Display for ProjectStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active => write!(f, "Active"),
            Self::Blocked => write!(f, "Blocked"),
            Self::Completed => write!(f, "Completed"),
            Self::Abandoned => write!(f, "Abandoned"),
        }
    }
}
