use std::{
    collections::{HashMap, HashSet, VecDeque},
    env, fmt, fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    daat_locus_paths::daat_locus_paths_sync,
    persistence::{PersistenceFileMode, PersistenceStore, write_bytes_atomic_sync},
};

const SKILL_FILE_NAME: &str = "SKILL.md";
const OPENSKILLS_CONFIG_FILE_NAME: &str = "openskills.toml";
const AGENTS_DIR_NAME: &str = ".agents";
const SKILLS_DIR_NAME: &str = "skills";
const SKILL_METADATA_BUDGET_CHARS: usize = 8_000;
const MAX_SCAN_DEPTH: usize = 6;
const MAX_SKILL_DIRS_PER_ROOT: usize = 2_000;
const MAX_NAME_LEN: usize = 64;
const MAX_DESCRIPTION_LEN: usize = 1_024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenSkillsCatalog {
    skills: Vec<OpenSkill>,
    errors: Vec<OpenSkillError>,
    roots: Vec<PathBuf>,
    root_by_skill_path: HashMap<PathBuf, PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenSkill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub scope: OpenSkillScope,
    pub allow_implicit_invocation: bool,
    pub user_disabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpenSkillScope {
    Project,
    DaatLocusHome,
    User,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenSkillError {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OpenSkillDashboardSummary {
    pub name: String,
    pub description: String,
    pub path: String,
    pub scope: String,
    pub allow_implicit_invocation: bool,
    pub user_disabled: bool,
    pub auto_use_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OpenSkillDashboardError {
    pub path: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenSkillInjection {
    pub name: String,
    pub path: PathBuf,
    pub contents: String,
}

#[derive(Debug, Clone)]
struct SkillRoot {
    path: PathBuf,
    scope: OpenSkillScope,
}

#[derive(Debug, Deserialize)]
struct SkillFrontmatter {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct OpenAiSkillMetadata {
    #[serde(default)]
    policy: OpenAiSkillPolicy,
}

#[derive(Debug, Default, Deserialize)]
struct OpenAiSkillPolicy {
    #[serde(default)]
    allow_implicit_invocation: Option<bool>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct OpenSkillsUserConfig {
    #[serde(default)]
    disabled_paths: Vec<PathBuf>,
}

#[derive(Debug)]
enum SkillParseError {
    Read(io::Error),
    MissingFrontmatter,
    InvalidYaml(serde_yaml::Error),
    MissingField(&'static str),
    InvalidField { field: &'static str, reason: String },
}

impl fmt::Display for SkillParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(err) => write!(f, "failed to read file: {err}"),
            Self::MissingFrontmatter => {
                write!(f, "missing YAML frontmatter delimited by ---")
            }
            Self::InvalidYaml(err) => write!(f, "invalid YAML: {err}"),
            Self::MissingField(field) => write!(f, "missing field `{field}`"),
            Self::InvalidField { field, reason } => write!(f, "invalid {field}: {reason}"),
        }
    }
}

impl OpenSkillsCatalog {
    #[cfg(test)]
    fn skills(&self) -> &[OpenSkill] {
        &self.skills
    }

    #[cfg(test)]
    fn errors(&self) -> &[OpenSkillError] {
        &self.errors
    }

    pub fn render_prompt_block(&self) -> Option<String> {
        self.render_prompt_block_with_budget(SKILL_METADATA_BUDGET_CHARS)
    }

    pub fn dashboard_summaries(&self) -> Vec<OpenSkillDashboardSummary> {
        self.skills
            .iter()
            .map(|skill| OpenSkillDashboardSummary {
                name: skill.name.clone(),
                description: skill.description.clone(),
                path: display_path(&skill.path),
                scope: skill.scope.label().to_string(),
                allow_implicit_invocation: skill.allow_implicit_invocation,
                user_disabled: skill.user_disabled,
                auto_use_enabled: skill.auto_use_enabled(),
            })
            .collect()
    }

    pub fn dashboard_errors(&self) -> Vec<OpenSkillDashboardError> {
        self.errors
            .iter()
            .map(|error| OpenSkillDashboardError {
                path: display_path(&error.path),
                message: error.message.clone(),
            })
            .collect()
    }

    fn render_prompt_block_with_budget(&self, metadata_budget_chars: usize) -> Option<String> {
        let skills = self
            .skills
            .iter()
            .filter(|skill| skill.auto_use_enabled())
            .collect::<Vec<_>>();
        if skills.is_empty() {
            return None;
        }

        let alias_plan = SkillPathAliasPlan::new(self, &skills);
        let skill_lines = render_skill_lines(&skills, &alias_plan, metadata_budget_chars.max(1));
        if skill_lines.lines.is_empty() {
            return None;
        }

        let mut lines = vec![
            "## Skills".to_string(),
            "A skill is a set of local instructions stored in a `SKILL.md` file. Below is the list of OpenSkills available in this session. Each entry includes a name, description, and path so the model can read the source instructions when a skill applies.".to_string(),
        ];

        if !alias_plan.root_lines.is_empty() {
            lines.push("### Skill roots".to_string());
            lines.extend(alias_plan.root_lines.clone());
        }

        lines.push("### Available skills".to_string());
        lines.extend(skill_lines.lines);
        if let Some(warning) = skill_lines.warning {
            lines.push(warning);
        }

        lines.push("### How to use skills".to_string());
        if alias_plan.root_lines.is_empty() {
            lines.push(SKILLS_HOW_TO_USE_WITH_ABSOLUTE_PATHS.to_string());
        } else {
            lines.push(SKILLS_HOW_TO_USE_WITH_ALIASES.to_string());
        }

        Some(lines.join("\n"))
    }

    pub fn explicit_skill_injections_for_text(&self, text: &str) -> Vec<OpenSkillInjection> {
        let mentions = extract_dollar_mentions(text);
        if mentions.is_empty() {
            return Vec::new();
        }

        let name_counts = skill_name_counts(&self.skills);
        let mut seen_paths = HashSet::new();
        let mut injections = Vec::new();
        for skill in &self.skills {
            if !mentions.contains(skill.name.as_str()) {
                continue;
            }
            if name_counts.get(skill.name.as_str()).copied().unwrap_or(0) != 1 {
                continue;
            }
            if !seen_paths.insert(skill.path.clone()) {
                continue;
            }
            match fs::read_to_string(&skill.path) {
                Ok(contents) => injections.push(OpenSkillInjection {
                    name: skill.name.clone(),
                    path: skill.path.clone(),
                    contents,
                }),
                Err(err) => tracing::warn!(
                    "failed to read explicitly mentioned skill {} at {}: {err}",
                    skill.name,
                    skill.path.display()
                ),
            }
        }
        injections
    }
}

impl OpenSkillScope {
    fn rank(self) -> u8 {
        match self {
            Self::Project => 0,
            Self::DaatLocusHome => 1,
            Self::User => 2,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::DaatLocusHome => "daat-locus",
            Self::User => "user",
        }
    }
}

impl OpenSkill {
    pub fn auto_use_enabled(&self) -> bool {
        self.allow_implicit_invocation && !self.user_disabled
    }
}

pub fn load_openskills_for_runtime(execution_cwd: &Path) -> OpenSkillsCatalog {
    let disabled_paths = load_openskills_user_config().disabled_paths;
    let catalog =
        load_openskills_from_roots_with_disabled_paths(skill_roots(execution_cwd), disabled_paths);
    for error in &catalog.errors {
        tracing::warn!(
            "failed to load OpenSkill at {}: {}",
            error.path.display(),
            error.message
        );
    }
    catalog
}

pub fn reload_openskills_for_runtime(execution_cwd: &Path) -> OpenSkillsCatalog {
    load_openskills_for_runtime(execution_cwd)
}

pub fn set_openskill_auto_use(path: &Path, enabled: bool) -> Result<(), String> {
    let target = canonicalize_lossy(path);
    let mut config = load_openskills_user_config();
    config.disabled_paths = config
        .disabled_paths
        .into_iter()
        .map(|path| canonicalize_lossy(&path))
        .filter(|path| path != &target)
        .collect();
    if !enabled {
        config.disabled_paths.push(target);
    }
    save_openskills_user_config(&mut config)
}

fn skill_roots(execution_cwd: &Path) -> Vec<SkillRoot> {
    let mut roots = Vec::new();

    roots.extend(project_skill_roots(execution_cwd));

    roots.push(SkillRoot {
        path: daat_locus_paths_sync().root().join(SKILLS_DIR_NAME),
        scope: OpenSkillScope::DaatLocusHome,
    });

    if let Some(home) = env::home_dir() {
        roots.push(SkillRoot {
            path: home.join(AGENTS_DIR_NAME).join(SKILLS_DIR_NAME),
            scope: OpenSkillScope::User,
        });
    }

    dedupe_roots(roots)
}

fn project_skill_roots(execution_cwd: &Path) -> Vec<SkillRoot> {
    let cwd = canonicalize_lossy(execution_cwd);
    let project_root = find_project_root(&cwd);
    let mut dirs = cwd
        .ancestors()
        .scan(false, |done, dir| {
            if *done {
                None
            } else {
                if dir == project_root {
                    *done = true;
                }
                Some(dir.to_path_buf())
            }
        })
        .collect::<Vec<_>>();
    dirs.reverse();
    dirs.into_iter()
        .map(|dir| SkillRoot {
            path: dir.join(AGENTS_DIR_NAME).join(SKILLS_DIR_NAME),
            scope: OpenSkillScope::Project,
        })
        .collect()
}

fn find_project_root(cwd: &Path) -> &Path {
    const MARKERS: &[&str] = &[
        ".git",
        "AGENTS.md",
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "go.mod",
    ];
    cwd.ancestors()
        .find(|dir| MARKERS.iter().any(|marker| dir.join(marker).exists()))
        .unwrap_or(cwd)
}

fn dedupe_roots(roots: Vec<SkillRoot>) -> Vec<SkillRoot> {
    let mut seen = HashSet::new();
    roots
        .into_iter()
        .filter(|root| seen.insert(canonicalize_lossy(&root.path)))
        .collect()
}

#[cfg(test)]
fn load_openskills_from_roots(roots: Vec<SkillRoot>) -> OpenSkillsCatalog {
    load_openskills_from_roots_with_disabled_paths(roots, Vec::new())
}

fn load_openskills_from_roots_with_disabled_paths(
    roots: Vec<SkillRoot>,
    disabled_paths: Vec<PathBuf>,
) -> OpenSkillsCatalog {
    let mut catalog = OpenSkillsCatalog::default();
    let mut root_by_skill_path = HashMap::new();
    let mut used_roots = Vec::new();
    let disabled_paths = disabled_paths
        .into_iter()
        .map(|path| canonicalize_lossy(&path))
        .collect::<HashSet<_>>();

    for root in roots {
        let root_path = canonicalize_lossy(&root.path);
        let before = catalog.skills.len();
        discover_skills_under_root(&root_path, root.scope, &mut catalog);
        if catalog.skills.len() > before {
            used_roots.push(root_path.clone());
            for skill in &catalog.skills[before..] {
                root_by_skill_path
                    .entry(skill.path.clone())
                    .or_insert_with(|| root_path.clone());
            }
        }
    }

    let mut seen_paths = HashSet::new();
    catalog
        .skills
        .retain(|skill| seen_paths.insert(skill.path.clone()));
    let retained_paths = catalog
        .skills
        .iter()
        .map(|skill| skill.path.clone())
        .collect::<HashSet<_>>();
    root_by_skill_path.retain(|path, _| retained_paths.contains(path));
    let roots_with_skills = root_by_skill_path
        .values()
        .cloned()
        .collect::<HashSet<PathBuf>>();
    used_roots.retain(|root| roots_with_skills.contains(root));
    used_roots.sort();
    used_roots.dedup();

    catalog.roots = used_roots;
    catalog.root_by_skill_path = root_by_skill_path;
    for skill in &mut catalog.skills {
        skill.user_disabled = disabled_paths.contains(&skill.path);
    }
    catalog.skills.sort_by(|a, b| {
        a.scope
            .rank()
            .cmp(&b.scope.rank())
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.path.cmp(&b.path))
    });
    catalog
}

fn openskills_user_config_path() -> PathBuf {
    PersistenceStore::runtime_sync().config_file(OPENSKILLS_CONFIG_FILE_NAME)
}

fn load_openskills_user_config() -> OpenSkillsUserConfig {
    let path = openskills_user_config_path();
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return OpenSkillsUserConfig::default();
        }
        Err(err) => {
            tracing::warn!("failed to read OpenSkills config {}: {err}", path.display());
            return OpenSkillsUserConfig::default();
        }
    };
    match toml::from_str::<OpenSkillsUserConfig>(&content) {
        Ok(mut config) => {
            normalize_openskills_user_config(&mut config);
            config
        }
        Err(err) => {
            tracing::warn!(
                "failed to parse OpenSkills config {}: {err}",
                path.display()
            );
            OpenSkillsUserConfig::default()
        }
    }
}

fn save_openskills_user_config(config: &mut OpenSkillsUserConfig) -> Result<(), String> {
    normalize_openskills_user_config(config);
    let path = openskills_user_config_path();
    if config.disabled_paths.is_empty() {
        match fs::remove_file(&path) {
            Ok(()) => return Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(err) => {
                return Err(format!(
                    "failed to remove OpenSkills config {}: {err}",
                    path.display()
                ));
            }
        }
    }
    let content = toml::to_string_pretty(config)
        .map_err(|err| format!("failed to encode OpenSkills config: {err}"))?;
    write_bytes_atomic_sync(&path, content.as_bytes(), PersistenceFileMode::Private).map_err(
        |err| {
            format!(
                "failed to write OpenSkills config {}: {err}",
                path.display()
            )
        },
    )
}

fn normalize_openskills_user_config(config: &mut OpenSkillsUserConfig) {
    config.disabled_paths = config
        .disabled_paths
        .drain(..)
        .map(|path| canonicalize_lossy(&path))
        .collect();
    config.disabled_paths.sort();
    config.disabled_paths.dedup();
}

fn discover_skills_under_root(root: &Path, scope: OpenSkillScope, catalog: &mut OpenSkillsCatalog) {
    if !root.is_dir() {
        return;
    }

    let mut queue = VecDeque::from([(root.to_path_buf(), 0usize)]);
    let mut visited = HashSet::from([canonicalize_lossy(root)]);
    let mut truncated = false;

    while let Some((dir, depth)) = queue.pop_front() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) => {
                catalog.errors.push(OpenSkillError {
                    path: dir,
                    message: format!("failed to read skills directory: {err}"),
                });
                continue;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy();
            if file_name.starts_with('.') {
                continue;
            }

            let metadata = match fs::metadata(&path) {
                Ok(metadata) => metadata,
                Err(err) => {
                    catalog.errors.push(OpenSkillError {
                        path,
                        message: format!("failed to stat skills path: {err}"),
                    });
                    continue;
                }
            };

            if metadata.is_dir() {
                if depth >= MAX_SCAN_DEPTH {
                    continue;
                }
                if visited.len() >= MAX_SKILL_DIRS_PER_ROOT {
                    truncated = true;
                    continue;
                }
                let canonical = canonicalize_lossy(&path);
                if visited.insert(canonical.clone()) {
                    queue.push_back((canonical, depth + 1));
                }
                continue;
            }

            if metadata.is_file() && file_name == SKILL_FILE_NAME {
                match parse_skill_file(&path, scope) {
                    Ok(skill) => catalog.skills.push(skill),
                    Err(err) => catalog.errors.push(OpenSkillError {
                        path: path.clone(),
                        message: err.to_string(),
                    }),
                }
            }
        }
    }

    if truncated {
        catalog.errors.push(OpenSkillError {
            path: root.to_path_buf(),
            message: format!("skills scan truncated after {MAX_SKILL_DIRS_PER_ROOT} directories"),
        });
    }
}

fn parse_skill_file(path: &Path, scope: OpenSkillScope) -> Result<OpenSkill, SkillParseError> {
    let contents = fs::read_to_string(path).map_err(SkillParseError::Read)?;
    let frontmatter = extract_frontmatter(&contents).ok_or(SkillParseError::MissingFrontmatter)?;
    let parsed: SkillFrontmatter =
        serde_yaml::from_str(&frontmatter).map_err(SkillParseError::InvalidYaml)?;

    let name = parsed
        .name
        .as_deref()
        .map(sanitize_single_line)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default_skill_name(path));
    validate_required_len(&name, MAX_NAME_LEN, "name")?;

    let description = parsed
        .description
        .as_deref()
        .map(sanitize_single_line)
        .unwrap_or_default();
    validate_required_len(&description, MAX_DESCRIPTION_LEN, "description")?;

    Ok(OpenSkill {
        name,
        description,
        path: canonicalize_lossy(path),
        scope,
        allow_implicit_invocation: load_allow_implicit_invocation(path),
        user_disabled: false,
    })
}

fn load_allow_implicit_invocation(skill_path: &Path) -> bool {
    let Some(skill_dir) = skill_path.parent() else {
        return true;
    };
    let metadata_path = skill_dir.join("agents").join("openai.yaml");
    let contents = match fs::read_to_string(metadata_path) {
        Ok(contents) => contents,
        Err(_) => return true,
    };
    serde_yaml::from_str::<OpenAiSkillMetadata>(&contents)
        .ok()
        .and_then(|metadata| metadata.policy.allow_implicit_invocation)
        .unwrap_or(true)
}

fn default_skill_name(path: &Path) -> String {
    path.parent()
        .and_then(|parent| parent.file_name())
        .map(|name| sanitize_single_line(&name.to_string_lossy()))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "skill".to_string())
}

fn extract_frontmatter(contents: &str) -> Option<String> {
    let mut lines = contents.lines();
    if !matches!(lines.next(), Some(line) if line.trim() == "---") {
        return None;
    }

    let mut frontmatter_lines = Vec::new();
    let mut found_closing = false;
    for line in lines.by_ref() {
        if line.trim() == "---" {
            found_closing = true;
            break;
        }
        frontmatter_lines.push(line);
    }

    (!frontmatter_lines.is_empty() && found_closing).then(|| frontmatter_lines.join("\n"))
}

fn sanitize_single_line(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn validate_required_len(
    value: &str,
    max_len: usize,
    field: &'static str,
) -> Result<(), SkillParseError> {
    if value.is_empty() {
        return Err(SkillParseError::MissingField(field));
    }
    if value.chars().count() > max_len {
        return Err(SkillParseError::InvalidField {
            field,
            reason: format!("exceeds maximum length of {max_len} characters"),
        });
    }
    Ok(())
}

fn canonicalize_lossy(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

struct SkillPathAliasPlan {
    root_lines: Vec<String>,
    root_aliases: HashMap<PathBuf, String>,
    root_by_skill_path: HashMap<PathBuf, PathBuf>,
}

impl SkillPathAliasPlan {
    fn new(catalog: &OpenSkillsCatalog, skills: &[&OpenSkill]) -> Self {
        let skill_paths = skills
            .iter()
            .map(|skill| skill.path.clone())
            .collect::<HashSet<_>>();
        let mut roots = catalog
            .roots
            .iter()
            .filter(|root| {
                catalog
                    .root_by_skill_path
                    .iter()
                    .any(|(path, skill_root)| skill_paths.contains(path) && skill_root == *root)
            })
            .cloned()
            .collect::<Vec<_>>();
        roots.sort();
        roots.dedup();

        let root_aliases = roots
            .iter()
            .enumerate()
            .map(|(index, root)| (root.clone(), format!("r{index}")))
            .collect::<HashMap<_, _>>();
        let root_lines = roots
            .iter()
            .enumerate()
            .map(|(index, root)| format!("- `r{index}` = `{}`", display_path(root)))
            .collect();
        let root_by_skill_path = catalog
            .root_by_skill_path
            .iter()
            .filter(|(path, _)| skill_paths.contains(*path))
            .map(|(path, root)| (path.clone(), root.clone()))
            .collect();

        Self {
            root_lines,
            root_aliases,
            root_by_skill_path,
        }
    }

    fn render_path(&self, skill: &OpenSkill) -> String {
        let Some(root) = self.root_by_skill_path.get(&skill.path) else {
            return display_path(&skill.path);
        };
        let Some(alias) = self.root_aliases.get(root) else {
            return display_path(&skill.path);
        };
        let Ok(relative) = skill.path.strip_prefix(root) else {
            return display_path(&skill.path);
        };
        let relative = display_path(relative);
        if relative.is_empty() {
            alias.clone()
        } else {
            format!("{alias}/{relative}")
        }
    }
}

struct RenderedSkillLines {
    lines: Vec<String>,
    warning: Option<String>,
}

fn render_skill_lines(
    skills: &[&OpenSkill],
    alias_plan: &SkillPathAliasPlan,
    budget_chars: usize,
) -> RenderedSkillLines {
    let full_lines = skills
        .iter()
        .map(|skill| render_skill_line(skill, alias_plan, true))
        .collect::<Vec<_>>();
    if lines_char_cost(&full_lines) <= budget_chars {
        return RenderedSkillLines {
            lines: full_lines,
            warning: None,
        };
    }

    let minimum_lines = skills
        .iter()
        .map(|skill| render_skill_line(skill, alias_plan, false))
        .collect::<Vec<_>>();
    if lines_char_cost(&minimum_lines) <= budget_chars {
        return RenderedSkillLines {
            lines: minimum_lines,
            warning: Some(
                "warning: Skill descriptions were omitted to fit the skills metadata budget."
                    .to_string(),
            ),
        };
    }

    let mut lines = Vec::new();
    let mut used = 0usize;
    let mut omitted = 0usize;
    for line in minimum_lines {
        let cost = line.chars().count().saturating_add(1);
        if used.saturating_add(cost) <= budget_chars {
            used = used.saturating_add(cost);
            lines.push(line);
        } else {
            omitted = omitted.saturating_add(1);
        }
    }

    RenderedSkillLines {
        lines,
        warning: (omitted > 0).then(|| {
            format!(
                "warning: Exceeded skills metadata budget; {omitted} additional skill(s) were not included."
            )
        }),
    }
}

fn render_skill_line(
    skill: &OpenSkill,
    alias_plan: &SkillPathAliasPlan,
    include_description: bool,
) -> String {
    let path = alias_plan.render_path(skill);
    if include_description {
        format!("- {}: {} (file: {})", skill.name, skill.description, path)
    } else {
        format!("- {}: (file: {})", skill.name, path)
    }
}

fn lines_char_cost(lines: &[String]) -> usize {
    lines
        .iter()
        .map(|line| line.chars().count().saturating_add(1))
        .sum()
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn skill_name_counts(skills: &[OpenSkill]) -> HashMap<&str, usize> {
    let mut counts = HashMap::new();
    for skill in skills {
        *counts.entry(skill.name.as_str()).or_insert(0) += 1;
    }
    counts
}

fn extract_dollar_mentions(text: &str) -> HashSet<&str> {
    let bytes = text.as_bytes();
    let mut mentions = HashSet::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'$' {
            index += 1;
            continue;
        }

        let name_start = index + 1;
        let Some(first) = bytes.get(name_start) else {
            index += 1;
            continue;
        };
        if !is_mention_name_char(*first) {
            index += 1;
            continue;
        }

        let mut name_end = name_start + 1;
        while let Some(next) = bytes.get(name_end)
            && is_mention_name_char(*next)
        {
            name_end += 1;
        }

        let name = &text[name_start..name_end];
        if !is_common_env_var(name) {
            mentions.insert(name);
        }
        index = name_end;
    }
    mentions
}

fn is_mention_name_char(byte: u8) -> bool {
    matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' | b':')
}

fn is_common_env_var(name: &str) -> bool {
    matches!(
        name.to_ascii_uppercase().as_str(),
        "PATH" | "HOME" | "USER" | "SHELL" | "PWD" | "TMPDIR" | "TEMP" | "TMP" | "LANG" | "TERM"
    )
}

const SKILLS_HOW_TO_USE_WITH_ABSOLUTE_PATHS: &str = r#"- Discovery: The list above is the OpenSkills available in this session (name + description + file path). Skill bodies live on disk at the listed paths.
- Trigger rules: If the user names a skill with `$SkillName` or plain text, or the task clearly matches a skill description above, use that skill for the current turn. Multiple mentions mean use them all. Do not carry skills across turns unless re-mentioned.
- How to use a skill:
  1. Before task actions that rely on a skill, use Terminal or Coding tools as needed and read that skill's `SKILL.md` completely.
  2. Resolve relative paths mentioned by `SKILL.md` relative to the directory containing that `SKILL.md`.
  3. Read only the referenced files needed for the task; avoid loading unrelated `references/`, `scripts/`, or `assets/`.
  4. Prefer running or adapting provided `scripts/` and reusing provided `assets/` or templates when applicable.
- If a named skill is missing, unreadable, or invalid, say so briefly and continue with the best fallback."#;

const SKILLS_HOW_TO_USE_WITH_ALIASES: &str = r#"- Discovery: The list above is the OpenSkills available in this session (name + description + short path). Expand short paths with the matching alias from `### Skill roots`.
- Trigger rules: If the user names a skill with `$SkillName` or plain text, or the task clearly matches a skill description above, use that skill for the current turn. Multiple mentions mean use them all. Do not carry skills across turns unless re-mentioned.
- How to use a skill:
  1. Expand the listed short `file` path with `### Skill roots`, then use Terminal or Coding tools as needed and read that skill's `SKILL.md` completely before task actions that rely on it.
  2. Resolve relative paths mentioned by `SKILL.md` relative to the directory containing that expanded `SKILL.md`.
  3. Read only the referenced files needed for the task; avoid loading unrelated `references/`, `scripts/`, or `assets/`.
  4. Prefer running or adapting provided `scripts/` and reusing provided `assets/` or templates when applicable.
- If a named skill is missing, unreadable, or invalid, say so briefly and continue with the best fallback."#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_skill_from_project_agents_skills() {
        let temp = tempfile::tempdir().expect("tempdir");
        let skill_dir = temp.path().join(".agents").join("skills").join("charts");
        fs::create_dir_all(&skill_dir).expect("create skill dir");
        fs::write(
            skill_dir.join(SKILL_FILE_NAME),
            "---\nname: charts\ndescription: Build charts from metrics\n---\n\n# Charts\n",
        )
        .expect("write skill");

        let catalog = load_openskills_from_roots(vec![SkillRoot {
            path: temp.path().join(".agents").join("skills"),
            scope: OpenSkillScope::Project,
        }]);

        assert_eq!(catalog.skills().len(), 1);
        assert_eq!(catalog.skills()[0].name, "charts");
        let prompt = catalog
            .render_prompt_block_with_budget(8_000)
            .expect("prompt");
        assert!(prompt.contains("charts: Build charts from metrics"));
        assert!(prompt.contains("### Skill roots"));
        assert!(prompt.contains("r0/charts/SKILL.md"));
    }

    #[test]
    fn reports_invalid_skill_frontmatter() {
        let temp = tempfile::tempdir().expect("tempdir");
        let skill_dir = temp.path().join(".agents").join("skills").join("broken");
        fs::create_dir_all(&skill_dir).expect("create skill dir");
        fs::write(skill_dir.join(SKILL_FILE_NAME), "# Missing frontmatter\n").expect("write skill");

        let catalog = load_openskills_from_roots(vec![SkillRoot {
            path: temp.path().join(".agents").join("skills"),
            scope: OpenSkillScope::Project,
        }]);

        assert!(catalog.skills().is_empty());
        assert_eq!(catalog.errors().len(), 1);
        assert!(
            catalog.errors()[0]
                .message
                .contains("missing YAML frontmatter")
        );
    }

    #[test]
    fn respects_allow_implicit_invocation_policy() {
        let temp = tempfile::tempdir().expect("tempdir");
        let skill_dir = temp.path().join(".agents").join("skills").join("private");
        fs::create_dir_all(skill_dir.join("agents")).expect("create metadata dir");
        fs::write(
            skill_dir.join(SKILL_FILE_NAME),
            "---\nname: private\ndescription: Hidden unless explicitly invoked\n---\n\n# Private\n",
        )
        .expect("write skill");
        fs::write(
            skill_dir.join("agents").join("openai.yaml"),
            "policy:\n  allow_implicit_invocation: false\n",
        )
        .expect("write metadata");

        let catalog = load_openskills_from_roots(vec![SkillRoot {
            path: temp.path().join(".agents").join("skills"),
            scope: OpenSkillScope::Project,
        }]);

        assert_eq!(catalog.skills().len(), 1);
        assert!(catalog.render_prompt_block_with_budget(8_000).is_none());
    }

    #[test]
    fn renders_minimum_lines_when_descriptions_exceed_budget() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join(".agents").join("skills");
        let skill_dir = root.join("verbose");
        fs::create_dir_all(&skill_dir).expect("create skill dir");
        fs::write(
            skill_dir.join(SKILL_FILE_NAME),
            "---\nname: verbose\ndescription: This description is intentionally long\n---\n\n# Verbose\n",
        )
        .expect("write skill");

        let catalog = load_openskills_from_roots(vec![SkillRoot {
            path: temp.path().join(".agents").join("skills"),
            scope: OpenSkillScope::Project,
        }]);
        let prompt = catalog.render_prompt_block_with_budget(64).expect("prompt");

        assert!(prompt.contains("- verbose: (file:"));
        assert!(prompt.contains("descriptions were omitted"));
    }

    #[test]
    fn explicit_dollar_mention_injects_skill_body() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join(".agents").join("skills");
        let skill_dir = root.join("writer");
        fs::create_dir_all(&skill_dir).expect("create skill dir");
        fs::write(
            skill_dir.join(SKILL_FILE_NAME),
            "---\nname: writer\ndescription: Write release notes\n---\n\n# Writer\nUse concise notes.\n",
        )
        .expect("write skill");
        let catalog = load_openskills_from_roots(vec![SkillRoot {
            path: root,
            scope: OpenSkillScope::Project,
        }]);

        let injections = catalog.explicit_skill_injections_for_text("please use $writer here");

        assert_eq!(injections.len(), 1);
        assert_eq!(injections[0].name, "writer");
        assert!(injections[0].contents.contains("Use concise notes."));
    }

    #[test]
    fn user_disabled_skill_is_manual_only_but_explicit_mentions_still_work() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join(".agents").join("skills");
        let skill_dir = root.join("writer");
        fs::create_dir_all(&skill_dir).expect("create skill dir");
        let skill_path = skill_dir.join(SKILL_FILE_NAME);
        fs::write(
            &skill_path,
            "---\nname: writer\ndescription: Write release notes\n---\n\n# Writer\nUse concise notes.\n",
        )
        .expect("write skill");
        let catalog = load_openskills_from_roots_with_disabled_paths(
            vec![SkillRoot {
                path: root,
                scope: OpenSkillScope::Project,
            }],
            vec![skill_path],
        );

        assert_eq!(catalog.skills().len(), 1);
        assert!(catalog.skills()[0].user_disabled);
        assert!(!catalog.skills()[0].auto_use_enabled());
        assert!(catalog.render_prompt_block_with_budget(8_000).is_none());

        let injections = catalog.explicit_skill_injections_for_text("please use $writer here");
        assert_eq!(injections.len(), 1);
        assert!(injections[0].contents.contains("Use concise notes."));
    }

    #[test]
    fn duplicate_skill_names_are_not_injected_by_plain_mention() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join(".agents").join("skills");
        for dir in ["a", "b"] {
            let skill_dir = root.join(dir);
            fs::create_dir_all(&skill_dir).expect("create skill dir");
            fs::write(
                skill_dir.join(SKILL_FILE_NAME),
                "---\nname: dup\ndescription: Duplicate name\n---\n\n# Dup\n",
            )
            .expect("write skill");
        }
        let catalog = load_openskills_from_roots(vec![SkillRoot {
            path: root,
            scope: OpenSkillScope::Project,
        }]);

        let injections = catalog.explicit_skill_injections_for_text("use $dup");

        assert!(injections.is_empty());
    }
}
