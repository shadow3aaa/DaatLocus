//! Obsidian vault import and study graph export.
//!
//! These adapters are executed by code, not by the model: the vault layout is
//! mechanical (notes, folders, wikilinks, frontmatter), so the Study tab
//! drives them through explicit Manager endpoints with a dry-run preview.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    io::Write as _,
    path::{Path, PathBuf},
};

use miette::{Result, miette};
use serde::{Deserialize, Serialize};

use super::store::{
    StudyGraphSnapshot, StudyImportMergeInput, StudyImportNodeInput, StudyQuestion, StudyStore,
    normalize_identity,
};

pub const MAX_IMPORT_NOTES: usize = 2000;
pub const MAX_IMPORT_FILE_BYTES: u64 = 1024 * 1024;
const SAMPLE_LIMIT: usize = 20;
const NOTION_IGNORED_DIRS: &[&str] = &[".obsidian", ".trash", ".git"];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ObsidianImportPreview {
    pub vault_dir: String,
    pub proposed_module_title: String,
    pub notes_found: usize,
    pub new_node_count: usize,
    pub duplicate_count: usize,
    pub dangling_link_count: usize,
    pub skipped_file_count: usize,
    pub truncated: bool,
    pub duplicates: Vec<ImportDuplicate>,
    pub dangling_links: Vec<ImportDanglingLink>,
    pub skipped_files: Vec<ImportSkippedFile>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImportDuplicate {
    pub title: String,
    pub rel_path: String,
    pub existing_node_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImportDanglingLink {
    pub from_title: String,
    pub target: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImportSkippedFile {
    pub rel_path: String,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ObsidianImportResult {
    pub module_id: String,
    pub module_title: String,
    pub created_nodes: usize,
    pub merged_nodes: usize,
    pub created_edges: usize,
    pub skipped_duplicates: usize,
    pub skipped_links: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExportArtifact {
    pub file_name: String,
    pub path: String,
    pub size_bytes: u64,
}

#[derive(Clone, Debug)]
struct ObsidianNote {
    rel_path: String,
    absolute: PathBuf,
    title: String,
    aliases: Vec<String>,
    tags: Vec<String>,
    body: String,
    links: Vec<String>,
}

#[derive(Clone, Debug)]
struct ImportPlan {
    module_title: String,
    new_notes: Vec<(String, ObsidianNote)>,
    merges: Vec<(String, ObsidianNote)>,
    duplicates: Vec<ImportDuplicate>,
    dangling_links: Vec<ImportDanglingLink>,
    skipped_files: Vec<ImportSkippedFile>,
    edges: Vec<(String, String)>,
    notes_found: usize,
    truncated: bool,
}

pub fn preview_obsidian_import(
    store: &StudyStore,
    vault_dir: &Path,
) -> Result<ObsidianImportPreview> {
    let plan = build_import_plan(store, vault_dir)?;
    Ok(ObsidianImportPreview {
        vault_dir: vault_dir.display().to_string(),
        proposed_module_title: plan.module_title.clone(),
        notes_found: plan.notes_found,
        new_node_count: plan.new_notes.len(),
        duplicate_count: plan.duplicates.len(),
        dangling_link_count: plan.dangling_links.len(),
        skipped_file_count: plan.skipped_files.len(),
        truncated: plan.truncated,
        duplicates: plan.duplicates.iter().take(SAMPLE_LIMIT).cloned().collect(),
        dangling_links: plan
            .dangling_links
            .iter()
            .take(SAMPLE_LIMIT)
            .cloned()
            .collect(),
        skipped_files: plan
            .skipped_files
            .iter()
            .take(SAMPLE_LIMIT)
            .cloned()
            .collect(),
    })
}

pub fn import_obsidian_vault(
    store: &StudyStore,
    vault_dir: &Path,
    module_id: Option<&str>,
    merge_duplicates: bool,
) -> Result<ObsidianImportResult> {
    let plan = build_import_plan(store, vault_dir)?;
    let new_nodes: Vec<StudyImportNodeInput> = plan
        .new_notes
        .iter()
        .map(|(id, note)| StudyImportNodeInput {
            id: id.clone(),
            title: note.title.clone(),
            aliases: note.aliases.clone(),
            tags: note.tags.clone(),
            body: note.body.clone(),
            source_title: note.rel_path.clone(),
            source_url: file_url(&note.absolute),
        })
        .collect();
    let merges: Vec<StudyImportMergeInput> = plan
        .merges
        .iter()
        .map(|(node_id, note)| StudyImportMergeInput {
            node_id: node_id.clone(),
            body: note.body.clone(),
            source_title: note.rel_path.clone(),
            source_url: file_url(&note.absolute),
        })
        .collect();
    let outcome = store.apply_obsidian_import(
        module_id,
        &plan.module_title,
        &new_nodes,
        &merges,
        &plan.edges,
        merge_duplicates,
    )?;
    Ok(ObsidianImportResult {
        module_id: outcome.module_id,
        module_title: outcome.module_title,
        created_nodes: outcome.created_nodes,
        merged_nodes: outcome.merged_nodes,
        created_edges: outcome.created_edges,
        skipped_duplicates: outcome.skipped_duplicates,
        skipped_links: outcome.skipped_links,
    })
}

pub fn export_obsidian_vault(
    store: &StudyStore,
    module_id: Option<&str>,
) -> Result<Vec<(String, String)>> {
    let snapshot = store.graph_snapshot()?;
    let modules = module_titles(&snapshot);
    let nodes = store.all_nodes(module_id)?;
    let mut files = Vec::with_capacity(nodes.len() + 1);
    let mut by_id: BTreeMap<String, String> = BTreeMap::new();
    for node in &nodes {
        by_id.insert(node.id.clone(), node.title.clone());
    }

    let mut relations_by_node: BTreeMap<String, BTreeMap<String, Vec<String>>> = BTreeMap::new();
    for edge in &snapshot.edges {
        if !by_id.contains_key(&edge.from) || !by_id.contains_key(&edge.to) {
            continue;
        }
        relations_by_node
            .entry(edge.from.clone())
            .or_default()
            .entry(edge.relation.clone())
            .or_default()
            .push(
                by_id
                    .get(&edge.to)
                    .cloned()
                    .unwrap_or_else(|| edge.to.clone()),
            );
    }

    let progress_by_id: BTreeMap<String, i64> = snapshot
        .nodes
        .iter()
        .map(|summary| (summary.id.clone(), summary.progress.understanding))
        .collect();

    for node in &nodes {
        let module_title = modules
            .get(&node.module_id)
            .cloned()
            .unwrap_or_else(|| "Unsorted".to_string());
        let mut frontmatter = String::from("---\n");
        frontmatter.push_str(&format!("module: {}\n", yaml_scalar(&module_title)));
        if !node.aliases.is_empty() {
            frontmatter.push_str(&format!("aliases: {}\n", yaml_list(&node.aliases)));
        }
        if !node.tags.is_empty() {
            frontmatter.push_str(&format!("tags: {}\n", yaml_list(&node.tags)));
        }
        frontmatter.push_str(&format!(
            "understanding: {}\n",
            progress_by_id.get(&node.id).copied().unwrap_or(0)
        ));
        if let Some(relations) = relations_by_node.get(&node.id) {
            frontmatter.push_str("relations:\n");
            for (relation, targets) in relations {
                frontmatter.push_str(&format!("  {}: {}\n", relation, yaml_list(targets)));
            }
        }
        if !node.sources.is_empty() {
            frontmatter.push_str("sources:\n");
            for source in &node.sources {
                frontmatter.push_str(&format!("  - title: {}\n", yaml_scalar(&source.title)));
                frontmatter.push_str(&format!("    url: {}\n", yaml_scalar(&source.url)));
            }
        }
        frontmatter.push_str("---\n\n");

        let mut content = frontmatter;
        content.push_str(&format!("# {}\n\n", node.title));
        if !node.summary.trim().is_empty() {
            content.push_str(&format!("> {}\n\n", node.summary.trim()));
        }
        content.push_str(node.body.trim());
        content.push('\n');

        let file_name = format!(
            "{}/{}.md",
            sanitize_path_component(&module_title),
            sanitize_path_component(&node.title)
        );
        files.push((file_name, content));
    }

    Ok(files)
}

pub fn export_graph_json(
    store: &StudyStore,
    module_id: Option<&str>,
    include_questions: bool,
) -> Result<String> {
    let snapshot = store.graph_snapshot()?;
    let nodes = store.all_nodes(module_id)?;
    let node_ids: BTreeSet<String> = nodes.iter().map(|node| node.id.clone()).collect();

    let questions: Vec<StudyQuestion> = if include_questions {
        store
            .all_questions()?
            .into_iter()
            .filter(|question| node_ids.contains(&question.node_id))
            .collect()
    } else {
        Vec::new()
    };

    let filtered_modules: Vec<_> = snapshot
        .modules
        .iter()
        .filter(|summary| nodes.iter().any(|node| node.module_id == summary.module.id))
        .cloned()
        .collect();
    let filtered_edges: Vec<_> = snapshot
        .edges
        .iter()
        .filter(|edge| node_ids.contains(&edge.from) && node_ids.contains(&edge.to))
        .cloned()
        .collect();
    let filtered_summaries: Vec<_> = snapshot
        .nodes
        .iter()
        .filter(|summary| node_ids.contains(&summary.id))
        .cloned()
        .collect();

    let document = serde_json::json!({
        "format": "daat-locus-study-graph",
        "version": 1,
        "exported_at_ms": snapshot.generated_at_ms,
        "modules": filtered_modules,
        "nodes": nodes,
        "node_summaries": filtered_summaries,
        "edges": filtered_edges,
        "questions": questions,
    });
    serde_json::to_string_pretty(&document)
        .map_err(|err| miette!("encode study graph JSON failed: {err}"))
}

pub fn write_export_zip(files: &[(String, String)], dest: &Path) -> Result<u64> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| miette!("create export directory {} failed: {err}", parent.display()))?;
    }
    let file = std::fs::File::create(dest)
        .map_err(|err| miette!("create export archive {} failed: {err}", dest.display()))?;
    let mut writer = zip::ZipWriter::new(file);
    let options: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, content) in files {
        writer
            .start_file(name.as_str(), options)
            .map_err(|err| miette!("add export entry `{name}` failed: {err}"))?;
        writer
            .write_all(content.as_bytes())
            .map_err(|err| miette!("write export entry `{name}` failed: {err}"))?;
    }
    writer
        .finish()
        .map_err(|err| miette!("finalize export archive failed: {err}"))?;
    let size = std::fs::metadata(dest)
        .map(|metadata| metadata.len())
        .unwrap_or_default();
    Ok(size)
}

fn build_import_plan(store: &StudyStore, vault_dir: &Path) -> Result<ImportPlan> {
    let vault_dir = vault_dir.to_path_buf();
    if !vault_dir.is_dir() {
        return Err(miette!(
            "vault directory does not exist: {}",
            vault_dir.display()
        ));
    }
    let module_title = vault_dir
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("Obsidian Import")
        .to_string();

    let mut files = Vec::new();
    let mut skipped_files = Vec::new();
    let mut truncated = false;
    collect_markdown_files(
        &vault_dir,
        &vault_dir,
        &mut files,
        &mut skipped_files,
        &mut truncated,
    )?;

    let identity_index = store.node_identity_index()?;
    let mut planned_ids: HashMap<String, String> = HashMap::new();
    let mut planned_notes: Vec<(String, ObsidianNote)> = Vec::new();
    let mut duplicates = Vec::new();
    let mut merges = Vec::new();
    let mut dangling_links = Vec::new();
    let mut notes_found = 0usize;

    for relative_path in files {
        let absolute = vault_dir.join(&relative_path);
        let raw = match std::fs::read_to_string(&absolute) {
            Ok(raw) => raw,
            Err(err) => {
                skipped_files.push(ImportSkippedFile {
                    rel_path: relative_path.clone(),
                    reason: format!("read failed: {err}"),
                });
                continue;
            }
        };
        let note = match parse_obsidian_note(&relative_path, &raw) {
            Ok(mut note) => {
                note.absolute = absolute;
                note
            }
            Err(err) => {
                skipped_files.push(ImportSkippedFile {
                    rel_path: relative_path.clone(),
                    reason: err.to_string(),
                });
                continue;
            }
        };
        notes_found += 1;

        let identities = note_identities(&note);
        let existing = identities
            .iter()
            .find_map(|identity| identity_index.get(identity).cloned());
        if let Some(existing_node_id) = existing {
            duplicates.push(ImportDuplicate {
                title: note.title.clone(),
                rel_path: note.rel_path.clone(),
                existing_node_id: existing_node_id.clone(),
            });
            merges.push((existing_node_id, note));
            continue;
        }
        let planned = identities
            .iter()
            .find_map(|identity| planned_ids.get(identity).cloned());
        if let Some(existing_node_id) = planned {
            duplicates.push(ImportDuplicate {
                title: note.title.clone(),
                rel_path: note.rel_path.clone(),
                existing_node_id: existing_node_id.clone(),
            });
            merges.push((existing_node_id, note));
            continue;
        }

        let node_id = format!("node-{}", uuid::Uuid::new_v4());
        for identity in identities {
            planned_ids.insert(identity, node_id.clone());
        }
        planned_notes.push((node_id, note));
    }

    let mut edges = Vec::new();
    let mut edge_keys = HashSet::new();
    for (node_id, note) in &planned_notes {
        for link in &note.links {
            let target_identity = normalize_identity(link);
            if target_identity.is_empty() {
                continue;
            }
            let target = planned_ids
                .get(&target_identity)
                .cloned()
                .or_else(|| identity_index.get(&target_identity).cloned());
            let Some(target) = target else {
                dangling_links.push(ImportDanglingLink {
                    from_title: note.title.clone(),
                    target: link.clone(),
                });
                continue;
            };
            if &target == node_id {
                continue;
            }
            if edge_keys.insert((node_id.clone(), target.clone())) {
                edges.push((node_id.clone(), target));
            }
        }
    }

    Ok(ImportPlan {
        module_title,
        new_notes: planned_notes,
        merges,
        duplicates,
        dangling_links,
        skipped_files,
        edges,
        notes_found,
        truncated,
    })
}

fn collect_markdown_files(
    root: &Path,
    dir: &Path,
    files: &mut Vec<String>,
    skipped: &mut Vec<ImportSkippedFile>,
    truncated: &mut bool,
) -> Result<()> {
    if *truncated {
        return Ok(());
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            skipped.push(ImportSkippedFile {
                rel_path: relative_path_string(root, dir),
                reason: format!("directory read failed: {err}"),
            });
            return Ok(());
        }
    };
    let mut entries = entries.flatten().collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if *truncated {
            return Ok(());
        }
        let path = entry.path();
        let file_name = entry.file_name().to_string_lossy().to_string();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(err) => {
                skipped.push(ImportSkippedFile {
                    rel_path: relative_path_string(root, &path),
                    reason: format!("entry type failed: {err}"),
                });
                continue;
            }
        };
        if file_type.is_dir() {
            if file_name.starts_with('.') || NOTION_IGNORED_DIRS.contains(&file_name.as_str()) {
                continue;
            }
            collect_markdown_files(root, &path, files, skipped, truncated)?;
            continue;
        }
        if !file_type.is_file() || file_name.starts_with('.') {
            continue;
        }
        if !file_name.to_lowercase().ends_with(".md") {
            continue;
        }
        if files.len() >= MAX_IMPORT_NOTES {
            *truncated = true;
            return Ok(());
        }
        let size = entry.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        if size > MAX_IMPORT_FILE_BYTES {
            skipped.push(ImportSkippedFile {
                rel_path: relative_path_string(root, &path),
                reason: format!("file exceeds {} KiB", MAX_IMPORT_FILE_BYTES / 1024),
            });
            continue;
        }
        files.push(relative_path_string(root, &path));
    }
    Ok(())
}

fn parse_obsidian_note(rel_path: &str, raw: &str) -> Result<ObsidianNote> {
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let (frontmatter, rest) = split_frontmatter(raw);
    let mut aliases = Vec::new();
    let mut tags = Vec::new();
    if let Some(frontmatter) = frontmatter
        && let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(&frontmatter)
    {
        aliases = yaml_string_list(value.get("aliases"));
        tags = yaml_string_list(value.get("tags"));
    }

    let mut title = Path::new(rel_path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| rel_path.to_string());
    let mut body_lines = Vec::new();
    let mut heading_taken = false;
    for line in rest.lines() {
        if !heading_taken && !line.trim().is_empty() && line.trim_start().starts_with("# ") {
            title = line
                .trim_start()
                .trim_start_matches("# ")
                .trim()
                .to_string();
            heading_taken = true;
            continue;
        }
        body_lines.push(line);
    }
    let body = body_lines.join("\n");
    let links = extract_wikilinks(&body);

    Ok(ObsidianNote {
        rel_path: rel_path.to_string(),
        absolute: PathBuf::new(),
        title,
        aliases,
        tags,
        body: body.trim().to_string(),
        links,
    })
}

fn split_frontmatter(raw: &str) -> (Option<String>, &str) {
    let Some(remainder) = raw
        .strip_prefix("---\r\n")
        .or_else(|| raw.strip_prefix("---\n"))
    else {
        return (None, raw);
    };
    let mut offset = 0usize;
    for line in remainder.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed == "---" || trimmed == "..." {
            let frontmatter = remainder[..offset].to_string();
            let rest = &remainder[offset + line.len()..];
            return (Some(frontmatter), rest);
        }
        offset += line.len();
    }
    (None, raw)
}

fn yaml_string_list(value: Option<&serde_yaml::Value>) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    match value {
        serde_yaml::Value::Sequence(items) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect(),
        serde_yaml::Value::String(text) => text
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn extract_wikilinks(text: &str) -> Vec<String> {
    let mut links = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("[[") {
        let after_start = &rest[start + 2..];
        let Some(end) = after_start.find("]]") else {
            break;
        };
        let inner = &after_start[..end];
        let target = inner.split('|').next().unwrap_or(inner);
        let target = target.split('#').next().unwrap_or(target).trim();
        if !target.is_empty() {
            links.push(target.to_string());
        }
        rest = &after_start[end + 2..];
    }
    links
}

fn note_identities(note: &ObsidianNote) -> Vec<String> {
    std::iter::once(note.title.clone())
        .chain(note.aliases.iter().cloned())
        .map(|identity| normalize_identity(&identity))
        .filter(|identity| !identity.is_empty())
        .collect()
}

fn module_titles(snapshot: &StudyGraphSnapshot) -> BTreeMap<String, String> {
    snapshot
        .modules
        .iter()
        .map(|summary| (summary.module.id.clone(), summary.module.title.clone()))
        .collect()
}

fn relative_path_string(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn sanitize_path_component(value: &str) -> String {
    let mut sanitized: String = value
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\n' | '\r' | '\t' => '_',
            ch => ch,
        })
        .collect();
    sanitized = sanitized.trim().trim_matches('.').to_string();
    if sanitized.is_empty() {
        sanitized = "untitled".to_string();
    }
    sanitized
}

fn yaml_scalar(value: &str) -> String {
    let needs_quotes = value.is_empty()
        || value.chars().any(|ch| {
            matches!(
                ch,
                ':' | '#'
                    | '\''
                    | '"'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | ','
                    | '&'
                    | '*'
                    | '!'
                    | '|'
                    | '>'
                    | '%'
                    | '@'
                    | '`'
            )
        });
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    if needs_quotes || escaped != value {
        format!("\"{escaped}\"")
    } else {
        value.to_string()
    }
}

fn yaml_list(values: &[String]) -> String {
    let items = values
        .iter()
        .map(|value| yaml_scalar(value))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{items}]")
}

fn file_url(path: &Path) -> String {
    format!("file:///{}", path.display().to_string().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::study_app::store::{CreateNodeInput, StudySource, StudyWriteOrigin};

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create dir");
        }
        std::fs::write(path, content).expect("write file");
    }

    fn store() -> (tempfile::TempDir, StudyStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = StudyStore::open_at(dir.path().join("graph.sqlite3")).expect("open store");
        (dir, store)
    }

    #[test]
    fn imports_notes_folders_links_and_frontmatter() {
        let (dir, store) = store();
        let vault = dir.path().join("MyVault");
        write(
            &vault.join("Algebra/Numbers.md"),
            "---\naliases: [Arithmetic]\ntags: basics\n---\n\n# Numbers\n\nSee [[Group]] for structure.",
        );
        write(
            &vault.join("Algebra/Group.md"),
            "# Group\n\nA set with an operation. See [[Numbers]] and [[Missing]].",
        );

        let preview = preview_obsidian_import(&store, &vault).expect("preview");
        assert_eq!(preview.proposed_module_title, "MyVault");
        assert_eq!(preview.new_node_count, 2);
        assert_eq!(preview.dangling_link_count, 1);
        assert_eq!(preview.duplicate_count, 0);

        let result = import_obsidian_vault(&store, &vault, None, true).expect("import");
        assert_eq!(result.created_nodes, 2);
        assert_eq!(result.created_edges, 2);

        let snapshot = store.graph_snapshot().expect("snapshot");
        assert_eq!(snapshot.nodes.len(), 2);
        let numbers = snapshot
            .nodes
            .iter()
            .find(|node| node.title == "Numbers")
            .expect("numbers node");
        assert!(numbers.aliases.iter().any(|alias| alias == "Arithmetic"));
        assert_eq!(
            store
                .search_nodes("Arithmetic", None, 5)
                .expect("search")
                .len(),
            1
        );
    }

    #[test]
    fn duplicate_notes_are_skipped_and_can_merge_body() {
        let (dir, store) = store();
        let vault = dir.path().join("Vault");
        write(&vault.join("Group.md"), "# Group\n\nImported body.");

        let module = store
            .create_module("Algebra", "", StudyWriteOrigin::Agent)
            .expect("module");
        store
            .create_node(
                &CreateNodeInput {
                    module_id: module.id,
                    title: "Group".to_string(),
                    sources: vec![StudySource {
                        title: "textbook".to_string(),
                        url: "https://example.com".to_string(),
                    }],
                    ..CreateNodeInput::default()
                },
                StudyWriteOrigin::Agent,
            )
            .expect("existing node");

        let preview = preview_obsidian_import(&store, &vault).expect("preview");
        assert_eq!(preview.new_node_count, 0);
        assert_eq!(preview.duplicate_count, 1);

        let result = import_obsidian_vault(&store, &vault, None, true).expect("import");
        assert_eq!(result.created_nodes, 0);
        assert_eq!(result.merged_nodes, 1);
    }

    #[test]
    fn vault_export_round_trips_relations_and_understanding() {
        let (dir, store) = store();
        let vault = dir.path().join("Vault");
        write(&vault.join("A.md"), "# A\n\nAlpha.");
        write(&vault.join("B.md"), "# B\n\nBeta. See [[A]].");
        import_obsidian_vault(&store, &vault, None, true).expect("import");

        let files = export_obsidian_vault(&store, None).expect("export");
        assert_eq!(files.len(), 2);
        let beta = files
            .iter()
            .find(|(name, _)| name.ends_with("B.md"))
            .expect("B export");
        assert!(beta.1.contains("related: [A]"));
        assert!(beta.1.contains("[A]"));

        let json = export_graph_json(&store, None, true).expect("json");
        assert!(json.contains("\"daat-locus-study-graph\""));
        assert!(json.contains("\"understanding\""));
    }

    #[test]
    fn writes_zip_archive() {
        let (dir, _store) = store();
        let dest = dir.path().join("out/export.zip");
        let files = vec![("a.md".to_string(), "# A\n".to_string())];
        let size = write_export_zip(&files, &dest).expect("zip");
        assert!(size > 0);
        assert!(dest.exists());
    }
}
