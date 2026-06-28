use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::analyzer::Analyzer;
use crate::api::*;
use crate::language::LanguageRegistry;
use crate::lsp::TsJsConfig;
use crate::lsp::{
    GoplsConfig, JdtlsConfig, LspAnalyzer, LspServerConfig, PyrightConfig, RustAnalyzerConfig,
};
use crate::patch;
use crate::state::PropagationState;
use crate::treesitter::TreeSitterAnalyzer;
use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::{DirEntry, WalkBuilder};
use regex::{Regex, RegexBuilder};
use std::sync::Mutex;

const DEFAULT_SEARCH_LIMIT: usize = 100;
const MAX_SEARCH_LIMIT: usize = 1000;
const DEFAULT_REVIEW_LIMIT: usize = 1;
const MAX_REVIEW_LIMIT: usize = 100;
const MAX_LSP_DID_OPEN_FILES: usize = 500;
const READ_AROUND_CONTEXT_LINES: usize = 12;

fn lsp_config_for_language(lsp_lang: &str) -> Option<Box<dyn LspServerConfig>> {
    match lsp_lang {
        "rust" => Some(Box::new(RustAnalyzerConfig)),
        "python" => Some(Box::new(PyrightConfig)),
        "typescript" | "javascript" => Some(Box::new(TsJsConfig)),
        "go" => Some(Box::new(GoplsConfig)),
        "java" => Some(Box::new(JdtlsConfig)),
        _ => None,
    }
}

fn lsp_extensions_for_language(lsp_lang: &str) -> &'static [&'static str] {
    match lsp_lang {
        "rust" => &["rs"],
        "python" => &["py"],
        "typescript" | "javascript" => &["ts", "tsx", "js", "jsx"],
        "go" => &["go"],
        "java" => &["java"],
        _ => &[],
    }
}

fn lsp_language_for_extension(ext: &str) -> Option<&'static str> {
    match ext {
        "rs" => Some("rust"),
        "py" => Some("python"),
        "ts" | "tsx" => Some("typescript"),
        "js" | "jsx" => Some("javascript"),
        "go" => Some("go"),
        "java" => Some("java"),
        _ => None,
    }
}

fn detect_project_lsp_language(root: &Path) -> Option<&'static str> {
    if root.join("Cargo.toml").is_file() {
        return Some("rust");
    }
    if root.join("pyproject.toml").is_file()
        || root.join("requirements.txt").is_file()
        || root.join("setup.py").is_file()
    {
        return Some("python");
    }
    if root.join("go.mod").is_file() {
        return Some("go");
    }
    if root.join("pom.xml").is_file()
        || root.join("build.gradle").is_file()
        || root.join("build.gradle.kts").is_file()
    {
        return Some("java");
    }
    if root.join("tsconfig.json").is_file() {
        return Some("typescript");
    }
    if root.join("package.json").is_file() {
        return Some("typescript");
    }

    let mut counts: HashMap<&'static str, usize> = HashMap::new();
    for entry in WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .ignore(true)
        .parents(true)
        .build()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_type()
                .is_some_and(|file_type| file_type.is_file())
        })
        .take(MAX_LSP_DID_OPEN_FILES)
    {
        let Some(ext) = entry.path().extension().and_then(|ext| ext.to_str()) else {
            continue;
        };
        if let Some(language) = lsp_language_for_extension(ext) {
            *counts.entry(language).or_default() += 1;
        }
    }

    ["rust", "typescript", "javascript", "python", "go", "java"]
        .into_iter()
        .max_by_key(|language| counts.get(language).copied().unwrap_or(0))
        .filter(|language| counts.get(language).copied().unwrap_or(0) > 0)
}

fn open_existing_source_files_for_lsp(lsp: &dyn Analyzer, root: &Path, lsp_lang: &str) {
    let exts = lsp_extensions_for_language(lsp_lang);
    if exts.is_empty() {
        return;
    }
    let scan_root = {
        let src_dir = root.join("src");
        if src_dir.exists() {
            src_dir
        } else {
            root.to_path_buf()
        }
    };
    for entry in WalkBuilder::new(scan_root)
        .hidden(false)
        .git_ignore(true)
        .ignore(true)
        .parents(true)
        .build()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_type()
                .is_some_and(|file_type| file_type.is_file())
        })
        .take(MAX_LSP_DID_OPEN_FILES)
    {
        let path = entry.path();
        if let Some(ext) = path.extension().and_then(|ext| ext.to_str())
            && exts.contains(&ext)
            && let Ok(content) = std::fs::read_to_string(path)
        {
            lsp.notify_did_open(path, &content);
        }
    }
}

pub fn open_project(
    project_root: &Path,
    current_project_root: Option<&Path>,
    lsp_analyzer: &Mutex<Option<Box<dyn Analyzer + Send>>>,
) -> Result<OpenProjectOutput, String> {
    if current_project_root == Some(project_root) {
        return Ok(OpenProjectOutput {
            status: "already_open".to_string(),
            project_root: project_root.to_string_lossy().into_owned(),
            detected_lsp_language: None,
            lsp: None,
        });
    }

    let detected_lsp_language = detect_project_lsp_language(project_root);
    let Some(config) = detected_lsp_language.and_then(lsp_config_for_language) else {
        let mut lsp_guard = lsp_analyzer
            .lock()
            .map_err(|_| "lock poisoned".to_string())?;
        *lsp_guard = None;
        return Ok(OpenProjectOutput {
            status: "opened".to_string(),
            project_root: project_root.to_string_lossy().into_owned(),
            detected_lsp_language: detected_lsp_language.map(str::to_string),
            lsp: Some("unsupported".to_string()),
        });
    };

    {
        let mut lsp_guard = lsp_analyzer
            .lock()
            .map_err(|_| "lock poisoned".to_string())?;
        *lsp_guard = None;
        let new_lsp = LspAnalyzer::new(project_root, config.as_ref());
        *lsp_guard = Some(Box::new(new_lsp));
    }

    {
        let lsp_guard = lsp_analyzer
            .lock()
            .map_err(|_| "lock poisoned".to_string())?;
        if let (Some(lsp), Some(lsp_lang)) = (&*lsp_guard, detected_lsp_language) {
            open_existing_source_files_for_lsp(lsp.as_ref(), project_root, lsp_lang);
        }
    }

    Ok(OpenProjectOutput {
        status: "opened".to_string(),
        project_root: project_root.to_string_lossy().into_owned(),
        detected_lsp_language: detected_lsp_language.map(str::to_string),
        lsp: None,
    })
}

pub fn search_code(
    project_root: &Path,
    params: &SearchCodeInput,
) -> Result<SearchCodeOutput, String> {
    if params.query.is_empty() {
        return Err("query is required".to_string());
    }

    let limit = normalize_search_limit(params.limit);
    let target = project_relative_arg(project_root, params.path.as_deref())?;
    let matches = search_project_matches(project_root, params, target.as_deref(), limit)?;
    Ok(SearchCodeOutput { matches })
}

pub fn is_responsible_source(
    project_root: &Path,
    params: &SourceResponsibilityInput,
) -> Result<SourceResponsibility, String> {
    let relative = project_relative_arg(project_root, Some(&params.path))?
        .ok_or_else(|| "path is required".to_string())?;
    let path = project_root.join(&relative);
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_string);
    let analyzer = TreeSitterAnalyzer::new();
    let language = extension
        .as_deref()
        .and_then(|ext| analyzer.responsible_language_for_extension(ext))
        .map(str::to_string);
    let is_responsible = language.is_some();
    let reason = match (&extension, &language) {
        (Some(extension), Some(language)) => {
            format!("SCOPE recognizes .{extension} as {language} source")
        }
        (Some(extension), None) => {
            format!("SCOPE has no source adapter for .{extension}")
        }
        (None, _) => "path has no file extension for SCOPE source ownership".to_string(),
    };

    Ok(SourceResponsibility {
        is_responsible,
        path: relative,
        extension,
        language,
        reason,
    })
}

fn normalize_search_limit(limit: Option<usize>) -> usize {
    limit
        .unwrap_or(DEFAULT_SEARCH_LIMIT)
        .clamp(1, MAX_SEARCH_LIMIT)
}

fn search_project_matches(
    project_root: &Path,
    params: &SearchCodeInput,
    target: Option<&str>,
    limit: usize,
) -> Result<Vec<SearchHit>, String> {
    let regex = build_search_regex(params)?;
    let filters = SearchFileFilters::from_input(params)?;
    let mut matches = Vec::new();

    for entry in project_files(project_root, target, &filters)? {
        let path = entry.path();
        let relative = relative_file_path(project_root, path);
        if !filters.matches(&relative, path) {
            continue;
        }

        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };
        for (line_index, text) in content.lines().enumerate() {
            if !regex.is_match(text) {
                continue;
            }
            let line = line_index + 1;
            matches.push(SearchHit {
                path: relative.clone(),
                hit: format_line_with_hash(line, text),
            });
            if matches.len() >= limit {
                return Ok(matches);
            }
        }
    }

    Ok(matches)
}

fn build_search_regex(params: &SearchCodeInput) -> Result<Regex, String> {
    let mut pattern = match params.mode {
        SearchMode::Literal => regex::escape(&params.query),
        SearchMode::Regex => params.query.clone(),
    };
    if params.line {
        pattern = format!("^(?:{pattern})$");
    } else if params.word {
        pattern = format!(r"\b(?:{pattern})\b");
    }

    let case_insensitive = match params.case_mode {
        SearchCase::Sensitive => false,
        SearchCase::Insensitive => true,
        SearchCase::Smart => !params.query.chars().any(char::is_uppercase),
    };

    RegexBuilder::new(&pattern)
        .case_insensitive(case_insensitive)
        .build()
        .map_err(|err| match params.mode {
            SearchMode::Literal => format!("search pattern error: {err}"),
            SearchMode::Regex => {
                format!("search regex error: {err}; use mode=\"literal\" for code fragments")
            }
        })
}

struct SearchFileFilters {
    include: Option<GlobSet>,
    exclude: Option<GlobSet>,
    type_include_exts: Option<HashSet<String>>,
    type_exclude_exts: HashSet<String>,
    hidden: bool,
    respect_ignore: bool,
    follow: bool,
}

impl SearchFileFilters {
    fn from_input(params: &SearchCodeInput) -> Result<Self, String> {
        Ok(Self {
            include: build_optional_glob_set(&params.include)?,
            exclude: build_optional_glob_set(&params.exclude)?,
            type_include_exts: build_optional_type_exts(&params.types)?,
            type_exclude_exts: build_optional_type_exts(&params.type_not)?.unwrap_or_default(),
            hidden: params.hidden,
            respect_ignore: params.respect_ignore,
            follow: params.follow,
        })
    }

    fn matches(&self, relative: &str, path: &Path) -> bool {
        if let Some(include) = self.include.as_ref()
            && !include.is_match(relative)
        {
            return false;
        }
        if let Some(exclude) = self.exclude.as_ref()
            && exclude.is_match(relative)
        {
            return false;
        }
        let ext = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase());
        if let Some(type_include_exts) = self.type_include_exts.as_ref()
            && !ext
                .as_ref()
                .is_some_and(|ext| type_include_exts.contains(ext))
        {
            return false;
        }
        if ext
            .as_ref()
            .is_some_and(|ext| self.type_exclude_exts.contains(ext))
        {
            return false;
        }
        true
    }
}

fn project_files(
    project_root: &Path,
    target: Option<&str>,
    filters: &SearchFileFilters,
) -> Result<Vec<DirEntry>, String> {
    let root = project_root.to_path_buf();
    let walk_root = match target {
        Some(target) => root.join(target),
        None => root.clone(),
    };

    let mut entries = Vec::new();
    let mut builder = WalkBuilder::new(walk_root);
    builder
        .hidden(!filters.hidden)
        .follow_links(filters.follow)
        .require_git(true);
    if !filters.respect_ignore {
        builder
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false)
            .ignore(false)
            .parents(false);
    }

    for entry in builder.build() {
        let entry = entry.map_err(|e| format!("failed to walk project files: {e}"))?;
        if !entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file())
        {
            continue;
        }
        entries.push(entry);
    }

    entries.sort_by(|left, right| {
        relative_file_path(&root, left.path()).cmp(&relative_file_path(&root, right.path()))
    });
    Ok(entries)
}

fn build_optional_glob_set(patterns: &[String]) -> Result<Option<GlobSet>, String> {
    let patterns = patterns
        .iter()
        .map(|pattern| pattern.trim())
        .filter(|pattern| !pattern.is_empty())
        .collect::<Vec<_>>();
    if patterns.is_empty() {
        return Ok(None);
    }
    build_glob_set(&patterns).map(Some)
}

fn build_glob_set(patterns: &[&str]) -> Result<GlobSet, String> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern).map_err(|e| format!("glob error: {e}"))?);
        if !pattern.contains('/') && !pattern.contains('\\') {
            builder
                .add(Glob::new(&format!("**/{pattern}")).map_err(|e| format!("glob error: {e}"))?);
        }
    }
    builder.build().map_err(|e| format!("glob error: {e}"))
}

fn build_optional_type_exts(types: &[String]) -> Result<Option<HashSet<String>>, String> {
    let requested = types
        .iter()
        .map(|type_name| {
            type_name
                .trim()
                .trim_start_matches('.')
                .to_ascii_lowercase()
        })
        .filter(|type_name| !type_name.is_empty())
        .collect::<Vec<_>>();
    if requested.is_empty() {
        return Ok(None);
    }

    let registry = LanguageRegistry::new();
    let languages = registry.list_languages();
    let supported = languages
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(", ");
    let mut exts = HashSet::new();
    for requested_type in requested {
        if let Some((_, language_exts)) = languages
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(&requested_type))
        {
            exts.extend(language_exts.iter().map(|ext| ext.to_ascii_lowercase()));
            continue;
        }
        if registry.get(&requested_type).is_some() {
            exts.insert(requested_type);
            continue;
        }
        return Err(format!(
            "unknown search type `{requested_type}`; supported SCOPE types: {supported}"
        ));
    }
    Ok(Some(exts))
}

fn relative_file_path(project_root: &Path, path: &Path) -> String {
    path.strip_prefix(project_root)
        .ok()
        .map(|path| normalize_relative_path(&path.to_string_lossy()))
        .unwrap_or_else(|| normalize_relative_path(&path.to_string_lossy()))
}

fn project_relative_arg(root: &Path, path: Option<&str>) -> Result<Option<String>, String> {
    let Some(path) = path.map(str::trim).filter(|path| !path.is_empty()) else {
        return Ok(None);
    };
    let path = PathBuf::from(path);
    if path.is_absolute() {
        let relative = path.strip_prefix(root).map_err(|_| {
            format!(
                "path {} is outside project root {}",
                path.display(),
                root.display()
            )
        })?;
        Ok(Some(normalize_relative_path(&relative.to_string_lossy())))
    } else {
        Ok(Some(normalize_relative_path(&path.to_string_lossy())))
    }
}

fn normalize_relative_path(path: &str) -> String {
    path.replace('\\', "/")
}

fn clamp_range(
    start_line: usize,
    end_line: usize,
    line_count: usize,
) -> Result<(usize, usize), String> {
    if start_line == 0 || end_line == 0 || start_line > end_line {
        return Err(format!("invalid line range {start_line}-{end_line}"));
    }
    if start_line > line_count {
        return Err(format!(
            "line range starts after end of file: {start_line} > {line_count}"
        ));
    }
    Ok((start_line, end_line.min(line_count)))
}

fn read_line_range(content: &str, start_line: usize, end_line: usize) -> String {
    let lines = content.lines().collect::<Vec<_>>();
    if start_line == 0 || end_line < start_line || start_line > lines.len() {
        return String::new();
    }
    let mut snippet = lines[(start_line - 1)..end_line.min(lines.len())].join("\n");
    if content.ends_with('\n') || end_line < lines.len() {
        snippet.push('\n');
    }
    snippet
}

pub fn read_code(project_root: &Path, params: &ReadCodeInput) -> Result<ReadCodeOutput, String> {
    let relative = project_relative_arg(project_root, Some(&params.path))?
        .ok_or_else(|| "path is required".to_string())?;
    let full_path = project_root.join(&relative);
    let file_content = fs::read_to_string(&full_path)
        .map_err(|e| format!("Failed to read {}: {e}", full_path.display()))?;
    let (anchor_line, anchor_hash) = parse_line_anchor(&params.anchor)?;
    verify_anchor_line(&file_content, anchor_line, &anchor_hash)?;
    let line_count = file_content.lines().count().max(1);
    let (start_line, end_line) =
        read_range_for_mode(params.mode, &full_path, anchor_line, line_count)?;
    let raw_content = read_line_range(&file_content, start_line, end_line);
    let prefixed_content = prefix_lines_with_hash(&raw_content, start_line);

    Ok(ReadCodeOutput {
        content: prefixed_content,
    })
}

fn read_range_for_mode(
    mode: ReadCodeMode,
    full_path: &Path,
    anchor_line: usize,
    line_count: usize,
) -> Result<(usize, usize), String> {
    match mode {
        ReadCodeMode::Around => {
            let start_line = anchor_line.saturating_sub(READ_AROUND_CONTEXT_LINES).max(1);
            let end_line = (anchor_line + READ_AROUND_CONTEXT_LINES).min(line_count);
            clamp_range(start_line, end_line, line_count)
        }
        ReadCodeMode::Full => {
            let analyzer = TreeSitterAnalyzer::new();
            if let Some(symbol) = analyzer.find_containing_symbol_match(full_path, anchor_line) {
                return clamp_range(symbol.start_line, symbol.end_line, line_count);
            }
            read_range_for_mode(ReadCodeMode::Around, full_path, anchor_line, line_count)
        }
    }
}

fn parse_line_anchor(anchor: &str) -> Result<(usize, String), String> {
    let (line_str, hash_str) = anchor
        .split_once('#')
        .ok_or_else(|| format!("invalid anchor (expected line#hash): {anchor}"))?;
    let line = line_str
        .parse::<usize>()
        .map_err(|_| format!("invalid line number in anchor: {anchor}"))?;
    if line == 0 {
        return Err(format!("line number must be >= 1 in anchor: {anchor}"));
    }
    if hash_str.is_empty() {
        return Err(format!("missing hash in anchor: {anchor}"));
    }
    Ok((line, hash_str.to_string()))
}

fn verify_anchor_line(content: &str, line_num: usize, expected_hash: &str) -> Result<(), String> {
    let lines: Vec<&str> = content.lines().collect();
    if line_num > lines.len() {
        return Err(format!(
            "line {line_num} out of bounds (file has {} lines); search or read again",
            lines.len()
        ));
    }
    let actual = lines[line_num - 1];
    let actual_hash = patch::line_hash(actual);
    if actual_hash != expected_hash {
        return Err(format!(
            "line {line_num} hash mismatch: expected {expected_hash}, got {actual_hash} — file may have changed; search or read again"
        ));
    }
    Ok(())
}

fn format_line_with_hash(line_num: usize, line: &str) -> String {
    let hash = patch::line_hash(line);
    format!("{line_num}#{hash}|{line}")
}

fn prefix_lines_with_hash(content: &str, start_line: usize) -> String {
    content
        .lines()
        .enumerate()
        .map(|(i, line)| format_line_with_hash(start_line + i, line))
        .collect::<Vec<_>>()
        .join("\n")
        + if content.ends_with('\n') || content.is_empty() {
            "\n"
        } else {
            ""
        }
}

pub fn edit_code(
    project_root: &Path,
    params: &EditCodeInput,
    propagation_state: &Mutex<PropagationState>,
    lsp_analyzer: &Mutex<Option<Box<dyn Analyzer + Send>>>,
) -> Result<EditCodeOutput, String> {
    match patch::edit_code_apply(&params.edits, project_root, lsp_analyzer) {
        Ok((results, applied_summary)) => {
            if !results.is_empty()
                && let Ok(mut state) = propagation_state.lock()
            {
                state.accumulate(results.clone());
            }
            Ok(EditCodeOutput {
                propagation_results: results,
                applied_summary,
            })
        }
        Err(e) => Err(e),
    }
}

pub fn config_hints() -> serde_json::Value {
    use crate::language::LanguageRegistry;
    use crate::lsp::{
        GoplsConfig, JdtlsConfig, LspServerConfig, PyrightConfig, RustAnalyzerConfig, TsJsConfig,
    };

    let registry = LanguageRegistry::new();
    let configs: Vec<Box<dyn LspServerConfig>> = vec![
        Box::new(RustAnalyzerConfig),
        Box::new(PyrightConfig),
        Box::new(TsJsConfig), // covers both TS and JS
        Box::new(GoplsConfig),
        Box::new(JdtlsConfig),
    ];

    let mut languages = Vec::new();

    // Tree-sitter languages from registry
    let ts_langs: Vec<serde_json::Value> = registry
        .list_languages()
        .into_iter()
        .map(|(name, exts)| {
            serde_json::json!({
                "name": name,
                "extensions": exts,
            })
        })
        .collect();

    // LSP configs
    for cfg in &configs {
        let binary_found = std::process::Command::new("which")
            .arg(cfg.binary_name())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        let mut lang_entry = serde_json::json!({
            "language": cfg.language_id(),
            "lsp_server": cfg.server_name(),
            "lsp_binary": cfg.binary_name(),
            "lsp_available": binary_found,
            "setup_hints": cfg.setup_hints(),
        });

        if let Some((cmd, args)) = cfg.install_command() {
            lang_entry["install_command"] = serde_json::json!({
                "command": cmd,
                "args": args,
            });
        }

        if let Some(url) = cfg.download_url() {
            lang_entry["download_url"] = serde_json::json!(url);
        }

        languages.push(lang_entry);
    }

    serde_json::json!({
        "tree_sitter_languages": ts_langs,
        "lsp_languages": languages,
    })
}

pub fn ack_next_events(
    propagation_state: &Mutex<PropagationState>,
    limit: Option<usize>,
) -> Result<ReviewBatch, String> {
    let limit = limit
        .unwrap_or(DEFAULT_REVIEW_LIMIT)
        .clamp(1, MAX_REVIEW_LIMIT);
    let mut state = propagation_state
        .lock()
        .map_err(|_| "lock poisoned".to_string())?;
    let reviews = state.next_reviews(limit);
    let review = reviews.first().cloned();
    let returned = reviews.len();
    let remaining = state.pending_count();
    Ok(ReviewBatch {
        review,
        reviews,
        returned,
        remaining,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::PropagationState;
    use std::sync::Mutex;

    #[test]
    fn open_project_detects_lsp_language_from_project_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"tmp\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        let lsp_analyzer: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);

        let output = open_project(dir.path(), None, &lsp_analyzer).unwrap();

        assert_eq!(output.detected_lsp_language.as_deref(), Some("rust"));
    }

    #[test]
    fn open_project_is_idempotent_for_current_project_root() {
        let dir = tempfile::tempdir().unwrap();
        let lsp_analyzer: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);

        let output = open_project(dir.path(), Some(dir.path()), &lsp_analyzer).unwrap();

        assert_eq!(output.status, "already_open");
        assert_eq!(output.detected_lsp_language, None);
    }

    #[test]
    fn search_code_returns_matched_line_hits() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/nested")).unwrap();
        std::fs::create_dir_all(dir.path().join("tests")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn lib() { let needle = true; }\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/nested/mod.rs"),
            "pub fn nested() { let needle = true; }\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("tests/lib_test.rs"),
            "pub fn test() { let needle = true; }\n",
        )
        .unwrap();

        let output = search_code(
            dir.path(),
            &SearchCodeInput {
                query: "needle".to_string(),
                path: Some("src".to_string()),
                include: vec!["*.rs".to_string()],
                ..SearchCodeInput::default()
            },
        )
        .unwrap();

        let hits = output
            .matches
            .iter()
            .map(|item| (item.path.clone(), item.hit.clone()))
            .collect::<Vec<_>>();
        assert_eq!(
            hits,
            vec![
                (
                    "src/lib.rs".to_string(),
                    format_line_with_hash(1, "pub fn lib() { let needle = true; }")
                ),
                (
                    "src/nested/mod.rs".to_string(),
                    format_line_with_hash(1, "pub fn nested() { let needle = true; }")
                )
            ]
        );
    }

    #[test]
    fn search_code_defaults_to_literal_smart_case() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub fn matching_commands() {}\nlet Needle = true;\n",
        )
        .unwrap();

        let output = search_code(
            dir.path(),
            &SearchCodeInput {
                query: "matching_commands(".to_string(),
                ..SearchCodeInput::default()
            },
        )
        .unwrap();
        assert_eq!(output.matches.len(), 1);
        assert_eq!(output.matches[0].path, "lib.rs");
        assert!(output.matches[0].hit.contains("matching_commands"));

        let output = search_code(
            dir.path(),
            &SearchCodeInput {
                query: "needle".to_string(),
                ..SearchCodeInput::default()
            },
        )
        .unwrap();
        assert_eq!(output.matches.len(), 1);
    }

    #[test]
    fn search_code_supports_regex_mode_opt_in() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "let needle = true;\n").unwrap();

        let literal_output = search_code(
            dir.path(),
            &SearchCodeInput {
                query: r"needle\s+=\s+true".to_string(),
                ..SearchCodeInput::default()
            },
        )
        .unwrap();
        assert!(literal_output.matches.is_empty());

        let regex_output = search_code(
            dir.path(),
            &SearchCodeInput {
                query: r"needle\s+=\s+true".to_string(),
                mode: SearchMode::Regex,
                ..SearchCodeInput::default()
            },
        )
        .unwrap();
        assert_eq!(regex_output.matches.len(), 1);
    }

    #[test]
    fn search_code_honors_case_word_and_line_modes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lower.rs"), "let needle = true;\n").unwrap();
        std::fs::write(dir.path().join("upper.rs"), "let Needle = true;\n").unwrap();
        std::fs::write(dir.path().join("plural.rs"), "let needles = true;\n").unwrap();
        std::fs::write(dir.path().join("line.rs"), "needle extra\nneedle\n").unwrap();

        let smart_lower = search_code(
            dir.path(),
            &SearchCodeInput {
                query: "needle".to_string(),
                ..SearchCodeInput::default()
            },
        )
        .unwrap();
        assert_eq!(smart_lower.matches.len(), 5);

        let smart_upper = search_code(
            dir.path(),
            &SearchCodeInput {
                query: "Needle".to_string(),
                ..SearchCodeInput::default()
            },
        )
        .unwrap();
        assert_eq!(
            smart_upper
                .matches
                .iter()
                .map(|hit| hit.path.as_str())
                .collect::<Vec<_>>(),
            vec!["upper.rs"]
        );

        let word = search_code(
            dir.path(),
            &SearchCodeInput {
                query: "needle".to_string(),
                word: true,
                case_mode: SearchCase::Sensitive,
                ..SearchCodeInput::default()
            },
        )
        .unwrap();
        assert!(
            word.matches.iter().all(|hit| hit.path != "plural.rs"),
            "word search should not match plural.rs: {:?}",
            word.matches
        );

        let line = search_code(
            dir.path(),
            &SearchCodeInput {
                query: "needle".to_string(),
                line: true,
                case_mode: SearchCase::Sensitive,
                ..SearchCodeInput::default()
            },
        )
        .unwrap();
        assert_eq!(
            line.matches
                .iter()
                .map(|hit| (hit.path.clone(), hit.hit.clone()))
                .collect::<Vec<_>>(),
            vec![("line.rs".to_string(), format_line_with_hash(2, "needle"))]
        );
    }

    #[test]
    fn search_code_honors_path_glob_type_hidden_and_ignore_filters() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::create_dir_all(dir.path().join("src/generated")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored.rs\n").unwrap();
        std::fs::write(dir.path().join("ignored.rs"), "let needle = true;\n").unwrap();
        std::fs::write(dir.path().join(".hidden.rs"), "let needle = true;\n").unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "let needle = true;\n").unwrap();
        std::fs::write(
            dir.path().join("src/generated/mod.rs"),
            "let needle = true;\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("src/main.ts"), "let needle = true;\n").unwrap();

        let filtered = search_code(
            dir.path(),
            &SearchCodeInput {
                query: "needle".to_string(),
                path: Some("src".to_string()),
                include: vec!["*.rs".to_string()],
                exclude: vec!["src/generated/**".to_string()],
                types: vec!["rust".to_string()],
                ..SearchCodeInput::default()
            },
        )
        .unwrap();
        assert_eq!(
            filtered
                .matches
                .iter()
                .map(|hit| hit.path.as_str())
                .collect::<Vec<_>>(),
            vec!["src/lib.rs"]
        );

        let default_visibility = search_code(
            dir.path(),
            &SearchCodeInput {
                query: "needle".to_string(),
                path: None,
                include: vec!["*.rs".to_string()],
                exclude: vec!["src/**".to_string()],
                ..SearchCodeInput::default()
            },
        )
        .unwrap();
        assert!(default_visibility.matches.is_empty());

        let unrestricted_visibility = search_code(
            dir.path(),
            &SearchCodeInput {
                query: "needle".to_string(),
                path: None,
                include: vec!["*.rs".to_string()],
                exclude: vec!["src/**".to_string()],
                hidden: true,
                respect_ignore: false,
                ..SearchCodeInput::default()
            },
        )
        .unwrap();
        assert_eq!(
            unrestricted_visibility
                .matches
                .iter()
                .map(|hit| hit.path.as_str())
                .collect::<Vec<_>>(),
            vec![".hidden.rs", "ignored.rs"]
        );
    }

    #[test]
    fn search_code_input_accepts_legacy_single_include_glob() {
        let value = serde_json::json!({
            "query": "needle",
            "include": "*.rs"
        });

        let input: SearchCodeInput = serde_json::from_value(value).unwrap();
        assert_eq!(input.include, vec!["*.rs".to_string()]);
        assert_eq!(input.mode, SearchMode::Literal);
        assert_eq!(input.case_mode, SearchCase::Smart);
        assert!(input.respect_ignore);
    }

    #[test]
    fn search_code_honors_explicit_limit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub fn one() { let needle = 1; }\npub fn two() { let needle = 2; }\npub fn three() { let needle = 3; }\n",
        )
        .unwrap();

        let output = search_code(
            dir.path(),
            &SearchCodeInput {
                query: "needle".to_string(),
                limit: Some(2),
                ..SearchCodeInput::default()
            },
        )
        .unwrap();
        assert_eq!(output.matches.len(), 2);
    }

    #[test]
    fn search_code_clamps_zero_limit_to_one() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub fn one() { let needle = 1; }\npub fn two() { let needle = 2; }\n",
        )
        .unwrap();

        let output = search_code(
            dir.path(),
            &SearchCodeInput {
                query: "needle".to_string(),
                limit: Some(0),
                ..SearchCodeInput::default()
            },
        )
        .unwrap();
        assert_eq!(output.matches.len(), 1);
    }

    #[test]
    fn search_code_returns_top_level_matched_line() {
        let dir = tempfile::tempdir().unwrap();
        let source = "use std::fmt;\n\npub fn second() {\n    println!(\"second\");\n}\n";
        std::fs::write(dir.path().join("lib.rs"), source).unwrap();

        let output = search_code(
            dir.path(),
            &SearchCodeInput {
                query: "std::fmt".to_string(),
                ..SearchCodeInput::default()
            },
        )
        .unwrap();

        assert_eq!(output.matches.len(), 1);
        assert_eq!(output.matches[0].path, "lib.rs");
        assert_eq!(
            output.matches[0].hit,
            format_line_with_hash(1, "use std::fmt;")
        );
    }

    #[test]
    fn read_code_full_reads_enclosing_symbol_from_line_anchor() {
        let dir = tempfile::tempdir().unwrap();
        let source = "pub fn first() {\n    println!(\"first\");\n}\n\npub fn second() {\n    println!(\"second\");\n}\n";
        std::fs::write(dir.path().join("lib.rs"), source).unwrap();

        let search = search_code(
            dir.path(),
            &SearchCodeInput {
                query: "second".to_string(),
                ..SearchCodeInput::default()
            },
        )
        .unwrap();
        let anchor = search.matches[0]
            .hit
            .split_once('|')
            .expect("line anchor")
            .0
            .to_string();
        let result = read_code(
            dir.path(),
            &ReadCodeInput {
                path: "lib.rs".to_string(),
                anchor,
                mode: ReadCodeMode::Full,
            },
        )
        .unwrap();
        let content = result.content.as_str();
        assert!(
            content.contains("pub fn second()"),
            "should contain fn second, got: {content}"
        );
        assert!(
            !content.contains("pub fn first()"),
            "should not contain fn first"
        );
        assert!(
            content.lines().all(|line| {
                let parts: Vec<&str> = line.splitn(2, '|').collect();
                parts.len() == 2 && parts[0].contains('#')
            }),
            "each line should have line#hash| prefix, got: {content}"
        );
    }

    #[test]
    fn read_code_around_reads_fixed_context_without_enclosing_symbol() {
        let dir = tempfile::tempdir().unwrap();
        let mut source = String::new();
        for line in 1..=40 {
            source.push_str(&format!("let value_{line} = {line};\n"));
        }
        std::fs::write(dir.path().join("lib.rs"), source).unwrap();
        let anchor = format!("20#{}", patch::line_hash("let value_20 = 20;"));

        let result = read_code(
            dir.path(),
            &ReadCodeInput {
                path: "lib.rs".to_string(),
                anchor,
                mode: ReadCodeMode::Around,
            },
        )
        .unwrap();

        assert!(result.content.starts_with("8#"));
        assert!(result.content.contains("|let value_20 = 20;"));
        assert!(result.content.contains("|let value_32 = 32;"));
        assert!(!result.content.contains("|let value_7 = 7;"));
        assert!(!result.content.contains("|let value_33 = 33;"));
    }

    #[test]
    fn read_code_rejects_stale_line_anchor() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "let current = true;\n").unwrap();

        let err = read_code(
            dir.path(),
            &ReadCodeInput {
                path: "lib.rs".to_string(),
                anchor: "1#00".to_string(),
                mode: ReadCodeMode::Around,
            },
        )
        .unwrap_err();

        assert!(err.contains("hash mismatch"), "{err}");
        assert!(err.contains("search or read again"), "{err}");
    }

    #[test]
    fn ack_next_event_can_return_a_limited_batch() {
        let propagation_state = Mutex::new(PropagationState::new());
        propagation_state.lock().unwrap().accumulate(vec![
            PropagationResult {
                selector: "src/a.rs::fn foo".to_string(),
                reason: "first".to_string(),
                source: PropagationSource::Lsp,
                lsp_references: Some(vec![]),
                diff_summary: None,
                file_snippet: None,
                project_files: None,
            },
            PropagationResult {
                selector: "src/b.rs::fn bar".to_string(),
                reason: "second".to_string(),
                source: PropagationSource::Lsp,
                lsp_references: Some(vec![]),
                diff_summary: None,
                file_snippet: None,
                project_files: None,
            },
            PropagationResult {
                selector: "src/c.rs::fn baz".to_string(),
                reason: "third".to_string(),
                source: PropagationSource::Lsp,
                lsp_references: Some(vec![]),
                diff_summary: None,
                file_snippet: None,
                project_files: None,
            },
        ]);

        let result = ack_next_events(&propagation_state, Some(2)).unwrap();

        assert_eq!(result.returned, 2);
        assert_eq!(result.remaining, 1);
        assert_eq!(result.reviews.len(), 2);
        match result.review.unwrap() {
            ReviewEvent::KnownReferences {
                modified_symbol, ..
            } => assert_eq!(modified_symbol, "src/c.rs::fn baz"),
            _ => panic!("expected KnownReferences review"),
        }
    }

    #[test]
    fn is_responsible_source_reports_scope_owned_source() {
        let dir = tempfile::tempdir().unwrap();

        let result = is_responsible_source(
            dir.path(),
            &SourceResponsibilityInput {
                path: "src/lib.rs".to_string(),
            },
        )
        .unwrap();
        assert!(result.is_responsible);
        assert_eq!(result.path, "src/lib.rs");
        assert_eq!(result.extension.as_deref(), Some("rs"));
        assert_eq!(result.language.as_deref(), Some("rust"));
    }

    #[test]
    fn is_responsible_source_reports_non_source_file() {
        let dir = tempfile::tempdir().unwrap();

        let result = is_responsible_source(
            dir.path(),
            &SourceResponsibilityInput {
                path: "README.md".to_string(),
            },
        )
        .unwrap();
        assert!(!result.is_responsible);
        assert_eq!(result.path, "README.md");
        assert_eq!(result.extension.as_deref(), Some("md"));
        assert_eq!(result.language, None);
    }
}
