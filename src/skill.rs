use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use miette::{Context as _, Result, miette};
use notify::{Event, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc::UnboundedSender;

use crate::workspace_paths::workspace_skills_dir;

const BUNDLED_SKILLS_DIR_NAME: &str = "skills";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSummary {
    pub id: String,
    pub name: String,
    pub when_to_use: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillContent {
    pub id: String,
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDoc {
    pub id: String,
    pub name: String,
    pub when_to_use: Vec<String>,
    pub body: String,
}

impl SkillDoc {
    pub fn summary(&self) -> SkillSummary {
        SkillSummary {
            id: self.id.clone(),
            name: self.name.clone(),
            when_to_use: self.when_to_use.clone(),
        }
    }

    pub fn content(&self) -> SkillContent {
        SkillContent {
            id: self.id.clone(),
            title: self.name.clone(),
            body: self.body.clone(),
        }
    }
}

#[derive(Debug, Default)]
pub struct GlobalSkillRegistry {
    workspace_dir: PathBuf,
    bundled_skills: BTreeMap<String, SkillDoc>,
    workspace_skills: BTreeMap<String, SkillDoc>,
    workspace_loaded_digest: Option<String>,
    workspace_attempted_digest: Option<String>,
    workspace_last_error: Option<String>,
    workspace_dirty: bool,
}

pub struct GlobalSkillBootstrap {
    pub registry: GlobalSkillRegistry,
    pub errors: Vec<String>,
}

#[derive(Debug, Default)]
pub struct GlobalSkillSyncReport {
    pub changed: bool,
    pub errors: Vec<String>,
}

impl GlobalSkillSyncReport {
    pub fn is_empty(&self) -> bool {
        !self.changed && self.errors.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceGlobalSkillsInvalidation {
    Dirty,
    FullRescan,
}

pub enum WorkspaceGlobalSkillsWatcherHandle {
    Recommended(RecommendedWatcher),
    Poll(PollWatcher),
}

impl WorkspaceGlobalSkillsWatcherHandle {
    pub fn backend_name(&self) -> &'static str {
        match self {
            Self::Recommended(watcher) => {
                let _ = watcher;
                "recommended"
            }
            Self::Poll(watcher) => {
                let _ = watcher;
                "poll"
            }
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillFrontmatter {
    name: Option<String>,
    #[serde(default)]
    when_to_use: Vec<String>,
}

pub fn bundled_global_skills_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(BUNDLED_SKILLS_DIR_NAME)
}

pub fn bootstrap_global_skills(workspace_root: &Path) -> GlobalSkillBootstrap {
    let bundled_dir = bundled_global_skills_dir();
    let workspace_dir = workspace_skills_dir(workspace_root);
    let mut registry = GlobalSkillRegistry {
        workspace_dir: workspace_dir.clone(),
        ..GlobalSkillRegistry::default()
    };
    let mut errors = Vec::new();

    match load_skill_map_from_dir(&bundled_dir) {
        Ok(skills) => registry.bundled_skills = skills,
        Err(err) => errors.push(format!(
            "failed to load bundled skills from {}: {err:?}",
            bundled_dir.display()
        )),
    }

    match sync_workspace_skill_map(&mut registry) {
        Ok(report) => errors.extend(report.errors),
        Err(err) => errors.push(format!(
            "failed to bootstrap workspace skills from {}: {err:?}",
            workspace_dir.display()
        )),
    }
    registry.workspace_dirty = false;

    GlobalSkillBootstrap { registry, errors }
}

impl GlobalSkillRegistry {
    pub fn summaries(&self) -> Vec<SkillSummary> {
        let mut merged = self.bundled_skills.clone();
        for (id, skill) in &self.workspace_skills {
            merged.insert(id.clone(), skill.clone());
        }
        merged.into_values().map(|skill| skill.summary()).collect()
    }

    pub fn read_skill(&self, id: &str) -> Option<SkillContent> {
        self.workspace_skills
            .get(id)
            .or_else(|| self.bundled_skills.get(id))
            .map(SkillDoc::content)
    }

    pub fn record_invalidation(&mut self, invalidation: WorkspaceGlobalSkillsInvalidation) {
        match invalidation {
            WorkspaceGlobalSkillsInvalidation::Dirty
            | WorkspaceGlobalSkillsInvalidation::FullRescan => {
                self.workspace_dirty = true;
            }
        }
    }

    pub fn sync_dirty_workspace_skills(&mut self) -> Result<GlobalSkillSyncReport> {
        if !self.workspace_dirty {
            return Ok(GlobalSkillSyncReport::default());
        }
        let report = sync_workspace_skill_map(self)?;
        self.workspace_dirty = false;
        Ok(report)
    }

    pub fn workspace_dir(&self) -> &Path {
        &self.workspace_dir
    }
}

fn sync_workspace_skill_map(registry: &mut GlobalSkillRegistry) -> Result<GlobalSkillSyncReport> {
    let mut report = GlobalSkillSyncReport::default();
    let digest = skill_source_digest(&registry.workspace_dir).wrap_err_with(|| {
        format!(
            "failed to hash workspace skills under {}",
            registry.workspace_dir.display()
        )
    })?;

    if registry.workspace_attempted_digest.as_deref() == Some(digest.as_str()) {
        return Ok(report);
    }

    match load_skill_map_from_dir(&registry.workspace_dir) {
        Ok(skills) => {
            report.changed = registry.workspace_loaded_digest.as_deref() != Some(digest.as_str());
            registry.workspace_skills = skills;
            registry.workspace_loaded_digest = Some(digest.clone());
            registry.workspace_attempted_digest = Some(digest);
            registry.workspace_last_error = None;
        }
        Err(err) => {
            registry.workspace_attempted_digest = Some(digest);
            registry.workspace_last_error = Some(err.to_string());
            report.errors.push(format!(
                "failed to load workspace skills from {}: {err:?}",
                registry.workspace_dir.display()
            ));
        }
    }

    Ok(report)
}

pub fn start_workspace_global_skills_watcher(
    skills_root: PathBuf,
    tx: UnboundedSender<WorkspaceGlobalSkillsInvalidation>,
) -> Result<WorkspaceGlobalSkillsWatcherHandle> {
    let recommended_callback = build_skill_watcher_callback(tx.clone());
    match notify::recommended_watcher(recommended_callback) {
        Ok(mut watcher) => {
            watcher
                .watch(&skills_root, RecursiveMode::Recursive)
                .map_err(|err| {
                    miette!(
                        "failed to watch workspace skills directory {}: {err}",
                        skills_root.display()
                    )
                })?;
            Ok(WorkspaceGlobalSkillsWatcherHandle::Recommended(watcher))
        }
        Err(recommended_err) => {
            tracing::warn!(
                "failed to start native workspace skills watcher for {}: {recommended_err}; falling back to poll watcher",
                skills_root.display()
            );
            let poll_callback = build_skill_watcher_callback(tx);
            let mut watcher = PollWatcher::new(
                poll_callback,
                notify::Config::default().with_poll_interval(Duration::from_millis(500)),
            )
            .map_err(|err| {
                miette!(
                    "failed to start poll workspace skills watcher for {}: {err}",
                    skills_root.display()
                )
            })?;
            watcher
                .watch(&skills_root, RecursiveMode::Recursive)
                .map_err(|err| {
                    miette!(
                        "failed to watch workspace skills directory {} with poll watcher: {err}",
                        skills_root.display()
                    )
                })?;
            Ok(WorkspaceGlobalSkillsWatcherHandle::Poll(watcher))
        }
    }
}

fn build_skill_watcher_callback(
    tx: UnboundedSender<WorkspaceGlobalSkillsInvalidation>,
) -> impl FnMut(notify::Result<Event>) + Send + 'static {
    move |event_result| match event_result {
        Ok(event) => {
            if event.kind.is_access() {
                return;
            }
            let _ = tx.send(WorkspaceGlobalSkillsInvalidation::Dirty);
        }
        Err(err) => {
            tracing::warn!("workspace skills watcher error: {err:?}");
            let _ = tx.send(WorkspaceGlobalSkillsInvalidation::FullRescan);
        }
    }
}

pub fn load_skills_from_dir(skills_dir: &Path) -> Result<Vec<SkillDoc>> {
    let mut skills = load_skill_map_from_dir(skills_dir)?
        .into_values()
        .collect::<Vec<_>>();
    skills.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(skills)
}

fn load_skill_map_from_dir(skills_dir: &Path) -> Result<BTreeMap<String, SkillDoc>> {
    if !skills_dir.exists() {
        return Ok(BTreeMap::new());
    }

    let mut skill_paths = fs::read_dir(skills_dir)
        .map_err(|err| {
            miette!(
                "failed to read skills directory {}: {err}",
                skills_dir.display()
            )
        })?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("md"))
        .collect::<Vec<_>>();
    skill_paths.sort();

    let mut skills = BTreeMap::new();
    for skill_path in skill_paths {
        let skill = load_skill(&skill_path)?;
        skills.insert(skill.id.clone(), skill);
    }
    Ok(skills)
}

fn load_skill(path: &Path) -> Result<SkillDoc> {
    let skill_id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| miette!("invalid skill file name {}", path.display()))?
        .to_string();
    validate_skill_id(&skill_id, path)?;
    let content = fs::read_to_string(path)
        .map_err(|err| miette!("failed to read skill file {}: {err}", path.display()))?;
    let (frontmatter, body) = split_frontmatter(&content)?;
    let frontmatter = frontmatter
        .ok_or_else(|| miette!("skill file {} is missing frontmatter", path.display()))?;
    let frontmatter: SkillFrontmatter = serde_yaml::from_str(&frontmatter).map_err(|err| {
        miette!(
            "failed to parse skill frontmatter in {}: {err}",
            path.display()
        )
    })?;
    let name = frontmatter.name.ok_or_else(|| {
        miette!(
            "skill frontmatter in {} is missing required `name`",
            path.display()
        )
    })?;
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(miette!(
            "skill frontmatter in {} has empty `name`",
            path.display()
        ));
    }
    let when_to_use = frontmatter
        .when_to_use
        .into_iter()
        .enumerate()
        .map(|(index, entry)| {
            let trimmed = entry.trim().to_string();
            if trimmed.is_empty() {
                Err(miette!(
                    "skill frontmatter in {} has empty `when_to_use[{index}]` entry",
                    path.display()
                ))
            } else {
                Ok(trimmed)
            }
        })
        .collect::<Result<Vec<_>>>()?;
    let body = body.trim().to_string();
    if body.trim().is_empty() {
        return Err(miette!(
            "skill file {} must contain non-empty markdown body",
            path.display()
        ));
    }
    Ok(SkillDoc {
        id: skill_id,
        name,
        when_to_use,
        body,
    })
}

fn validate_skill_id(skill_id: &str, path: &Path) -> Result<()> {
    if skill_id.is_empty() {
        return Err(miette!("invalid skill file name {}", path.display()));
    }
    if !skill_id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        return Err(miette!(
            "skill file {} must use only ASCII letters, numbers, `_`, or `-` in its file name",
            path.display()
        ));
    }
    Ok(())
}

fn split_frontmatter(content: &str) -> Result<(Option<String>, String)> {
    let normalized = content.replace("\r\n", "\n");
    if !normalized.starts_with("---\n") {
        return Ok((None, normalized));
    }
    let rest = &normalized[4..];
    let Some(end_index) = rest.find("\n---\n") else {
        return Err(miette!("unterminated frontmatter block"));
    };
    let frontmatter = rest[..end_index].to_string();
    let body = rest[end_index + "\n---\n".len()..].to_string();
    Ok((Some(frontmatter), body))
}

fn skill_source_digest(skills_dir: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(b"workspace-skills-v1\0");
    if !skills_dir.exists() {
        return Ok(format!("{:x}", hasher.finalize()));
    }

    let mut skill_paths = fs::read_dir(skills_dir)
        .map_err(|err| {
            miette!(
                "failed to read skills directory {}: {err}",
                skills_dir.display()
            )
        })?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("md"))
        .collect::<Vec<_>>();
    skill_paths.sort();

    for skill_path in skill_paths {
        let relative = skill_path
            .strip_prefix(skills_dir)
            .unwrap_or(&skill_path)
            .to_string_lossy()
            .into_owned();
        hasher.update(relative.as_bytes());
        hasher.update([0]);
        let content = fs::read(&skill_path)
            .map_err(|err| miette!("failed to read skill file {}: {err}", skill_path.display()))?;
        hasher.update(content);
        hasher.update([0]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn workspace_global_skills_override_bundled_skills() {
        let workspace = TempDir::new().expect("workspace tempdir");
        let bundled_root = TempDir::new().expect("bundled tempdir");
        let bundled_dir = bundled_root.path().join("skills");
        fs::create_dir_all(&bundled_dir).expect("create bundled skills dir");
        fs::write(
            bundled_dir.join("writer.md"),
            "---\nname: Bundled Writer\nwhen_to_use:\n  - bundled\n---\nBundled body\n",
        )
        .expect("write bundled skill");
        let workspace_dir = workspace_skills_dir(workspace.path());
        fs::create_dir_all(&workspace_dir).expect("create workspace skills dir");
        fs::write(
            workspace_dir.join("writer.md"),
            "---\nname: Workspace Writer\nwhen_to_use:\n  - workspace\n---\nWorkspace body\n",
        )
        .expect("write workspace skill");

        let mut registry = GlobalSkillRegistry {
            workspace_dir: workspace_dir.clone(),
            bundled_skills: load_skill_map_from_dir(&bundled_dir).expect("load bundled"),
            ..GlobalSkillRegistry::default()
        };
        let report = sync_workspace_skill_map(&mut registry).expect("sync workspace skills");
        assert!(report.changed);
        let skill = registry.read_skill("writer").expect("writer skill");
        assert_eq!(skill.title, "Workspace Writer");
        assert_eq!(skill.body, "Workspace body");
    }

    #[test]
    fn sync_dirty_workspace_skills_keeps_last_good_on_error() {
        let workspace = TempDir::new().expect("workspace tempdir");
        let workspace_dir = workspace_skills_dir(workspace.path());
        fs::create_dir_all(&workspace_dir).expect("create workspace skills dir");
        fs::write(
            workspace_dir.join("writer.md"),
            "---\nname: Writer\nwhen_to_use:\n  - draft\n---\nValid body\n",
        )
        .expect("write workspace skill");

        let mut registry = GlobalSkillRegistry {
            workspace_dir: workspace_dir.clone(),
            ..GlobalSkillRegistry::default()
        };
        let first = sync_workspace_skill_map(&mut registry).expect("initial sync");
        assert!(first.changed);
        assert_eq!(
            registry.read_skill("writer").expect("writer skill").body,
            "Valid body"
        );

        fs::write(
            workspace_dir.join("writer.md"),
            "---\nname: Writer\nwhen_to_use:\n  - draft\n",
        )
        .expect("write invalid workspace skill");
        registry.record_invalidation(WorkspaceGlobalSkillsInvalidation::Dirty);
        let second = registry
            .sync_dirty_workspace_skills()
            .expect("sync invalid workspace skill");
        assert!(!second.errors.is_empty());
        assert_eq!(
            registry.read_skill("writer").expect("writer skill").body,
            "Valid body"
        );
    }

    #[test]
    fn bundled_global_skills_include_builtin_authoring_skills() {
        let bundled_dir = bundled_global_skills_dir();
        let bundled = load_skill_map_from_dir(&bundled_dir).expect("load bundled skills");

        assert!(bundled.contains_key("write-app"));
        assert!(bundled.contains_key("write-global-skill"));
        assert_eq!(
            bundled.get("write-app").expect("write-app").name,
            "编写第三方 App"
        );
        assert_eq!(
            bundled
                .get("write-global-skill")
                .expect("write-global-skill")
                .name,
            "编写全局 Skill"
        );
    }
}
