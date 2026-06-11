use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::analyzer::Analyzer;
use crate::api::*;
use crate::lsp::TsJsConfig;
use crate::lsp::{
    GoplsConfig, JdtlsConfig, LspAnalyzer, LspServerConfig, PyrightConfig, RustAnalyzerConfig,
};
use crate::patch;
use crate::state::{PropagationState, ReadHandleRegistry, ReadHandleTarget};
use crate::treesitter::TreeSitterAnalyzer;
use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::{DirEntry, WalkBuilder};
use regex::Regex;
use std::sync::Mutex;

const DEFAULT_SEARCH_LIMIT: usize = 100;
const MAX_SEARCH_LIMIT: usize = 1000;
const DEFAULT_REVIEW_LIMIT: usize = 1;
const MAX_REVIEW_LIMIT: usize = 100;
const MAX_LSP_DID_OPEN_FILES: usize = 500;
const SEARCH_FALLBACK_CONTEXT_LINES: usize = 12;

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
) -> Result<OpenProjectResponse, String> {
    if current_project_root == Some(project_root) {
        return Ok(OpenProjectResponse {
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
        return Ok(OpenProjectResponse {
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

    Ok(OpenProjectResponse {
        status: "opened".to_string(),
        project_root: project_root.to_string_lossy().into_owned(),
        detected_lsp_language: detected_lsp_language.map(str::to_string),
        lsp: None,
    })
}

pub fn search_code(
    project_root: &Path,
    params: &SearchCodeRequest,
    read_handles: &mut ReadHandleRegistry,
) -> Result<SearchCodeResponse, String> {
    if params.query.is_empty() {
        return Err("query is required".to_string());
    }

    let limit = normalize_search_limit(params.limit);
    let target = project_relative_arg(project_root, params.path.as_deref())?;
    let targets = search_project_targets(
        project_root,
        &params.query,
        target.as_deref(),
        params.include.as_deref(),
        limit,
        read_handles,
    )?;
    Ok(SearchCodeResponse { targets })
}

pub fn is_responsible_source(
    project_root: &Path,
    params: &IsResponsibleSourceRequest,
) -> Result<IsResponsibleSourceResponse, String> {
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

    Ok(IsResponsibleSourceResponse {
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

fn search_project_targets(
    project_root: &Path,
    pattern: &str,
    target: Option<&str>,
    include: Option<&str>,
    limit: usize,
    read_handles: &mut ReadHandleRegistry,
) -> Result<Vec<SearchTarget>, String> {
    let regex = Regex::new(pattern).map_err(|e| format!("regex error: {e}"))?;
    let include = build_optional_glob_set(include)?;
    let analyzer = TreeSitterAnalyzer::new();
    let mut targets = Vec::new();
    let mut seen_labels = std::collections::HashSet::new();

    for entry in project_files(project_root, target)? {
        let path = entry.path();
        let relative = relative_file_path(project_root, path);
        if let Some(include) = include.as_ref()
            && !include.is_match(&relative)
        {
            continue;
        }

        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };
        let line_count = content.lines().count().max(1);
        for (line_index, text) in content.lines().enumerate() {
            if !regex.is_match(text) {
                continue;
            }
            let line = line_index + 1;
            let target =
                search_target_for_match(&analyzer, project_root, path, &relative, line, line_count);
            if seen_labels.insert(target.label.clone()) {
                targets.push(read_handles.intern(target)?);
            }
            if targets.len() >= limit {
                return Ok(targets);
            }
        }
    }

    Ok(targets)
}

fn search_target_for_match(
    analyzer: &TreeSitterAnalyzer,
    project_root: &Path,
    path: &Path,
    relative: &str,
    line: usize,
    line_count: usize,
) -> ReadHandleTarget {
    if let Some(symbol) = analyzer.find_containing_symbol_match(path, line) {
        let label = symbol.canonical_selector(path, project_root);
        return ReadHandleTarget::new(label, relative, symbol.start_line, symbol.end_line);
    }

    let start_line = line.saturating_sub(SEARCH_FALLBACK_CONTEXT_LINES).max(1);
    let end_line = (line + SEARCH_FALLBACK_CONTEXT_LINES).min(line_count);
    ReadHandleTarget::new(
        format!("{relative}#L{start_line}-L{end_line}"),
        relative,
        start_line,
        end_line,
    )
}

fn project_files(project_root: &Path, target: Option<&str>) -> Result<Vec<DirEntry>, String> {
    let root = project_root.to_path_buf();
    let walk_root = match target {
        Some(target) => root.join(target),
        None => root.clone(),
    };

    let mut entries = Vec::new();
    for entry in WalkBuilder::new(walk_root)
        .standard_filters(false)
        .hidden(false)
        .parents(false)
        .build()
    {
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

fn build_optional_glob_set(pattern: Option<&str>) -> Result<Option<GlobSet>, String> {
    let Some(pattern) = pattern.map(str::trim).filter(|pattern| !pattern.is_empty()) else {
        return Ok(None);
    };
    build_glob_set(pattern).map(Some)
}

fn build_glob_set(pattern: &str) -> Result<GlobSet, String> {
    let mut builder = GlobSetBuilder::new();
    builder.add(Glob::new(pattern).map_err(|e| format!("glob error: {e}"))?);
    if !pattern.contains('/') && !pattern.contains('\\') {
        builder.add(Glob::new(&format!("**/{pattern}")).map_err(|e| format!("glob error: {e}"))?);
    }
    builder.build().map_err(|e| format!("glob error: {e}"))
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

pub fn read_code(
    project_root: &Path,
    params: &ReadCodeRequest,
    read_handles: &ReadHandleRegistry,
) -> Result<ReadCodeResponse, String> {
    let target = resolve_read_target(params, read_handles)?;
    let full_path = project_root.join(&target.path);
    let file_content = fs::read_to_string(&full_path)
        .map_err(|e| format!("Failed to read {}: {e}", full_path.display()))?;
    let line_count = file_content.lines().count().max(1);
    let (start_line, end_line) = clamp_range(target.start_line, target.end_line, line_count)?;
    let raw_content = read_line_range(&file_content, start_line, end_line);
    let prefixed_content = prefix_lines_with_hash(&raw_content, start_line);

    Ok(ReadCodeResponse {
        path: target.path,
        content: prefixed_content,
    })
}

fn resolve_read_target(
    params: &ReadCodeRequest,
    read_handles: &ReadHandleRegistry,
) -> Result<ReadHandleTarget, String> {
    let handle = params.ref_handle.as_str();
    read_handles
        .resolve(handle)
        .cloned()
        .ok_or_else(|| format!("unknown read handle `{handle}`; search again"))
}

fn prefix_lines_with_hash(content: &str, start_line: usize) -> String {
    content
        .lines()
        .enumerate()
        .map(|(i, line)| {
            let line_num = start_line + i;
            let hash = patch::line_hash(line);
            format!("{line_num}#{hash}|{line}")
        })
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
    params: &EditCodeRequest,
    propagation_state: &Mutex<PropagationState>,
    lsp_analyzer: &Mutex<Option<Box<dyn Analyzer + Send>>>,
) -> Result<PropagationResponse, String> {
    match patch::edit_code_apply(&params.edits, project_root, lsp_analyzer) {
        Ok(results) => {
            if !results.is_empty()
                && let Ok(mut state) = propagation_state.lock()
            {
                state.accumulate(results.clone());
            }
            Ok(PropagationResponse {
                propagation_results: results,
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
) -> Result<NextReviewResponse, String> {
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
    Ok(NextReviewResponse {
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

        let response = open_project(dir.path(), None, &lsp_analyzer).unwrap();

        assert_eq!(response.detected_lsp_language.as_deref(), Some("rust"));
    }

    #[test]
    fn open_project_is_idempotent_for_current_project_root() {
        let dir = tempfile::tempdir().unwrap();
        let lsp_analyzer: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);

        let response = open_project(dir.path(), Some(dir.path()), &lsp_analyzer).unwrap();

        assert_eq!(response.status, "already_open");
        assert_eq!(response.detected_lsp_language, None);
    }

    #[test]
    fn search_code_returns_deduped_read_handles() {
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

        let mut handles = ReadHandleRegistry::new();
        let response = search_code(
            dir.path(),
            &SearchCodeRequest {
                query: "needle".to_string(),
                path: Some("src".to_string()),
                include: Some("*.rs".to_string()),
                limit: None,
            },
            &mut handles,
        )
        .unwrap();

        let labels = response
            .targets
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            vec![
                "src/lib.rs::fn lib #L1-L1",
                "src/nested/mod.rs::fn nested #L1-L1"
            ]
        );
        for target in &response.targets {
            assert!(
                target.handle.starts_with("1#"),
                "handle should include start line: {}",
                target.handle
            );
            assert_eq!(target.handle.len(), "1#".len() + 4);
        }
    }

    #[test]
    fn search_code_honors_explicit_limit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub fn one() { let needle = 1; }\npub fn two() { let needle = 2; }\npub fn three() { let needle = 3; }\n",
        )
        .unwrap();

        let mut handles = ReadHandleRegistry::new();
        let response = search_code(
            dir.path(),
            &SearchCodeRequest {
                query: "needle".to_string(),
                path: None,
                include: None,
                limit: Some(2),
            },
            &mut handles,
        )
        .unwrap();
        assert_eq!(response.targets.len(), 2);
    }

    #[test]
    fn search_code_clamps_zero_limit_to_one() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub fn one() { let needle = 1; }\npub fn two() { let needle = 2; }\n",
        )
        .unwrap();

        let mut handles = ReadHandleRegistry::new();
        let response = search_code(
            dir.path(),
            &SearchCodeRequest {
                query: "needle".to_string(),
                path: None,
                include: None,
                limit: Some(0),
            },
            &mut handles,
        )
        .unwrap();
        assert_eq!(response.targets.len(), 1);
    }

    #[test]
    fn search_code_falls_back_to_line_range_for_top_level_matches() {
        let dir = tempfile::tempdir().unwrap();
        let source = "use std::fmt;\n\npub fn second() {\n    println!(\"second\");\n}\n";
        std::fs::write(dir.path().join("lib.rs"), source).unwrap();

        let mut handles = ReadHandleRegistry::new();
        let response = search_code(
            dir.path(),
            &SearchCodeRequest {
                query: "std::fmt".to_string(),
                path: None,
                include: None,
                limit: None,
            },
            &mut handles,
        )
        .unwrap();

        assert_eq!(response.targets.len(), 1);
        assert_eq!(response.targets[0].label, "lib.rs#L1-L5");
        assert!(
            response.targets[0].handle.starts_with("1#"),
            "handle should include range start line"
        );
    }

    #[test]
    fn read_code_reads_search_handle_without_selector_input() {
        let dir = tempfile::tempdir().unwrap();
        let source = "pub fn first() {\n    println!(\"first\");\n}\n\npub fn second() {\n    println!(\"second\");\n}\n";
        std::fs::write(dir.path().join("lib.rs"), source).unwrap();

        let mut handles = ReadHandleRegistry::new();
        let search = search_code(
            dir.path(),
            &SearchCodeRequest {
                query: "second".to_string(),
                path: None,
                include: None,
                limit: None,
            },
            &mut handles,
        )
        .unwrap();
        let handle = search.targets[0].handle.clone();
        let result = read_code(
            dir.path(),
            &ReadCodeRequest { ref_handle: handle },
            &handles,
        )
        .unwrap();
        assert_eq!(result.path, "lib.rs");
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
    fn scope_usage_is_available() {
        let result = serde_json::to_value(crate::usage::usage_response()).unwrap();
        assert!(
            result["usage_markdown"]
                .as_str()
                .unwrap()
                .contains("stable read handles")
        );
        assert!(
            result["protocol_items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["item"] == "read_code_ref")
        );
    }

    #[test]
    fn is_responsible_source_reports_scope_owned_source() {
        let dir = tempfile::tempdir().unwrap();

        let result = is_responsible_source(
            dir.path(),
            &IsResponsibleSourceRequest {
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
            &IsResponsibleSourceRequest {
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
