use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::analyzer::Analyzer;
use crate::api::*;
use crate::lsp::TsJsConfig;
use crate::lsp::{
    GoplsConfig, JdtlsConfig, LspAnalyzer, LspServerConfig, PyrightConfig, RustAnalyzerConfig,
};
use crate::patch;
use crate::selector::{self, SelectorTarget};
use crate::state::PropagationState;
use crate::treesitter::TreeSitterAnalyzer;
use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::{DirEntry, WalkBuilder};
use regex::Regex;
use std::sync::Mutex;

const DEFAULT_SEARCH_LIMIT: usize = 100;
const MAX_SEARCH_LIMIT: usize = 1000;
const DEFAULT_FILE_SEARCH_LIMIT: usize = 100;

pub fn dispatch(
    req: &JsonRpcRequest,
    project_root: Option<&Path>,
    propagation_state: &Mutex<PropagationState>,
    lsp_analyzer: &Mutex<Option<Box<dyn Analyzer + Send>>>,
) -> JsonRpcResponse {
    match req.method.as_str() {
        "open_project" => {
            let params: OpenProjectRequest = match serde_json::from_value(req.params.clone()) {
                Ok(p) => p,
                Err(e) => {
                    return JsonRpcResponse::err(
                        req.id.clone(),
                        -32602,
                        format!("Invalid params: {e}"),
                    );
                }
            };

            // Initialize LspClient for this project
            let language = params.language.as_deref().unwrap_or("auto");
            let lsp_lang = match language {
                "auto" | "rust" => "rust",
                "python" | "py" => "python",
                "typescript" | "ts" | "tsx" => "typescript",
                "javascript" | "js" | "jsx" => "javascript",
                other => other,
            };
            let config: Box<dyn LspServerConfig> = match lsp_lang {
                "rust" => Box::new(RustAnalyzerConfig),
                "python" => Box::new(PyrightConfig),
                "typescript" | "javascript" => Box::new(TsJsConfig),
                "go" => Box::new(GoplsConfig),
                "java" => Box::new(JdtlsConfig),
                _ => {
                    // Unsupported language — skip LSP initialization
                    return JsonRpcResponse::ok(
                        req.id.clone(),
                        serde_json::json!({
                            "status": "opened",
                            "project_root": params.project_root,
                            "language": params.language.unwrap_or_else(|| "auto".to_string()),
                            "lsp": "unsupported",
                        }),
                    );
                }
            };
            {
                let mut lsp_guard = match lsp_analyzer.lock() {
                    Ok(g) => g,
                    Err(_) => return JsonRpcResponse::err(req.id.clone(), -32603, "lock poisoned"),
                };
                // Drop previous LSP analyzer — its Drop impl sends shutdown
                *lsp_guard = None;
                let new_lsp = LspAnalyzer::new(Path::new(&params.project_root), config.as_ref());
                *lsp_guard = Some(Box::new(new_lsp));
            }

            // Open all existing source files in LSP so that references work
            {
                let lsp_guard = match lsp_analyzer.lock() {
                    Ok(g) => g,
                    Err(_) => return JsonRpcResponse::err(req.id.clone(), -32603, "lock poisoned"),
                };
                if let Some(ref lsp) = *lsp_guard {
                    let root = Path::new(&params.project_root);
                    let src_dir = root.join("src");
                    let scan_dir = if src_dir.exists() {
                        src_dir.as_path()
                    } else {
                        root
                    };
                    if let Ok(entries) = std::fs::read_dir(scan_dir) {
                        let exts: &[&str] = match lsp_lang {
                            "rust" => &["rs"],
                            "python" => &["py"],
                            "typescript" | "javascript" => &["ts", "tsx", "js", "jsx"],
                            "go" => &["go"],
                            "java" => &["java"],
                            "c" | "h" => &["c", "h"],
                            "cpp" | "cxx" | "cc" | "hpp" | "hxx" | "hh" => {
                                &["cpp", "cxx", "cc", "hpp", "hxx", "hh"]
                            }
                            "rb" => &["rb"],
                            "php" => &["php"],
                            _ => &["rs"],
                        };
                        for entry in entries.flatten() {
                            let path = entry.path();
                            if let Some(ext) = path.extension().and_then(|e| e.to_str())
                                && exts.contains(&ext)
                                && let Ok(content) = std::fs::read_to_string(&path)
                            {
                                lsp.notify_did_open(&path, &content);
                            }
                        }
                    }
                }
            }

            JsonRpcResponse::ok(
                req.id.clone(),
                serde_json::json!({
                    "status": "opened",
                    "project_root": params.project_root,
                    "language": params.language.unwrap_or_else(|| "auto".to_string()),
                }),
            )
        }
        "read_code" => {
            let params: ReadCodeRequest = match serde_json::from_value(req.params.clone()) {
                Ok(p) => p,
                Err(e) => {
                    return JsonRpcResponse::err(
                        req.id.clone(),
                        -32602,
                        format!("Invalid params: {e}"),
                    );
                }
            };
            handle_read_code(req, &params, project_root)
        }
        "search_code" => {
            let params: SearchCodeRequest = match serde_json::from_value(req.params.clone()) {
                Ok(p) => p,
                Err(e) => {
                    return JsonRpcResponse::err(
                        req.id.clone(),
                        -32602,
                        format!("Invalid params: {e}"),
                    );
                }
            };
            handle_search_code(req, &params, project_root)
        }
        "grep_code" => {
            let params: GrepCodeRequest = match serde_json::from_value(req.params.clone()) {
                Ok(p) => p,
                Err(e) => {
                    return JsonRpcResponse::err(
                        req.id.clone(),
                        -32602,
                        format!("Invalid params: {e}"),
                    );
                }
            };
            handle_grep_code(req, &params, project_root)
        }
        "glob_files" => {
            let params: GlobFilesRequest = match serde_json::from_value(req.params.clone()) {
                Ok(p) => p,
                Err(e) => {
                    return JsonRpcResponse::err(
                        req.id.clone(),
                        -32602,
                        format!("Invalid params: {e}"),
                    );
                }
            };
            handle_glob_files(req, &params, project_root)
        }
        "is_responsible_source" => {
            let params: IsResponsibleSourceRequest =
                match serde_json::from_value(req.params.clone()) {
                    Ok(p) => p,
                    Err(e) => {
                        return JsonRpcResponse::err(
                            req.id.clone(),
                            -32602,
                            format!("Invalid params: {e}"),
                        );
                    }
                };
            handle_is_responsible_source(req, &params, project_root)
        }
        "edit_code" => {
            let params: EditCodeRequest = match serde_json::from_value(req.params.clone()) {
                Ok(p) => p,
                Err(e) => {
                    return JsonRpcResponse::err(
                        req.id.clone(),
                        -32602,
                        format!("Invalid params: {e}"),
                    );
                }
            };
            handle_edit_code(req, &params, project_root, propagation_state, lsp_analyzer)
        }
        "ack_next_event" => {
            let mut state = match propagation_state.lock() {
                Ok(s) => s,
                Err(_) => return JsonRpcResponse::err(req.id.clone(), -32603, "lock poisoned"),
            };
            let review = state.next_review();
            JsonRpcResponse::ok(
                req.id.clone(),
                serde_json::to_value(NextReviewResponse { review }).unwrap(),
            )
        }
        "get_config_hints" => handle_get_config_hints(req),
        "get_usage" | "scope_usage" => JsonRpcResponse::ok(
            req.id.clone(),
            serde_json::to_value(crate::usage::usage_response()).unwrap(),
        ),
        _ => JsonRpcResponse::err(
            req.id.clone(),
            -32601,
            format!("Method not found: {}", req.method),
        ),
    }
}

fn handle_search_code(
    req: &JsonRpcRequest,
    params: &SearchCodeRequest,
    project_root: Option<&Path>,
) -> JsonRpcResponse {
    let project_root = match project_root {
        Some(r) => r,
        None => {
            return JsonRpcResponse::err(
                req.id.clone(),
                -32000,
                "No project open; call open_project first",
            );
        }
    };

    let limit = normalize_search_limit(params.limit);

    let matches = match search_project(project_root, &params.query, None, None, limit) {
        Ok(matches) => matches,
        Err(e) => return JsonRpcResponse::err(req.id.clone(), -32001, e),
    };

    JsonRpcResponse::ok(
        req.id.clone(),
        serde_json::to_value(SearchCodeResponse { matches }).unwrap(),
    )
}

fn handle_grep_code(
    req: &JsonRpcRequest,
    params: &GrepCodeRequest,
    project_root: Option<&Path>,
) -> JsonRpcResponse {
    let project_root = match project_root {
        Some(r) => r,
        None => {
            return JsonRpcResponse::err(
                req.id.clone(),
                -32000,
                "No project open; call open_project first",
            );
        }
    };

    match grep_code(project_root, params) {
        Ok(response) => {
            JsonRpcResponse::ok(req.id.clone(), serde_json::to_value(response).unwrap())
        }
        Err(e) => JsonRpcResponse::err(req.id.clone(), -32001, e),
    }
}

fn handle_glob_files(
    req: &JsonRpcRequest,
    params: &GlobFilesRequest,
    project_root: Option<&Path>,
) -> JsonRpcResponse {
    let project_root = match project_root {
        Some(r) => r,
        None => {
            return JsonRpcResponse::err(
                req.id.clone(),
                -32000,
                "No project open; call open_project first",
            );
        }
    };

    match glob_files(project_root, params) {
        Ok(response) => {
            JsonRpcResponse::ok(req.id.clone(), serde_json::to_value(response).unwrap())
        }
        Err(e) => JsonRpcResponse::err(req.id.clone(), -32001, e),
    }
}

pub fn grep_code(
    project_root: &Path,
    params: &GrepCodeRequest,
) -> Result<GrepCodeResponse, String> {
    if params.pattern.is_empty() {
        return Err("pattern is required".to_string());
    }

    let target = project_relative_arg(project_root, params.path.as_deref())?;
    let matches = search_project(
        project_root,
        &params.pattern,
        target.as_deref(),
        params.include.as_deref(),
        DEFAULT_FILE_SEARCH_LIMIT,
    )?;
    let output = format_grep_output(&matches);

    Ok(GrepCodeResponse { matches, output })
}

pub fn glob_files(
    project_root: &Path,
    params: &GlobFilesRequest,
) -> Result<GlobFilesResponse, String> {
    if params.pattern.is_empty() {
        return Err("pattern is required".to_string());
    }

    let target = project_relative_arg(project_root, params.path.as_deref())?;
    let mut files = find_glob_files(project_root, &params.pattern, target.as_deref())?;
    files.sort_by(|(left_path, left_mtime), (right_path, right_mtime)| {
        right_mtime
            .cmp(left_mtime)
            .then_with(|| left_path.cmp(right_path))
    });

    let truncated = files.len() > DEFAULT_FILE_SEARCH_LIMIT;
    files.truncate(DEFAULT_FILE_SEARCH_LIMIT);
    let files = files.into_iter().map(|(path, _)| path).collect::<Vec<_>>();
    let output = format_glob_output(&files, truncated);

    Ok(GlobFilesResponse {
        files,
        truncated,
        output,
    })
}

fn handle_is_responsible_source(
    req: &JsonRpcRequest,
    params: &IsResponsibleSourceRequest,
    project_root: Option<&Path>,
) -> JsonRpcResponse {
    let project_root = match project_root {
        Some(r) => r,
        None => {
            return JsonRpcResponse::err(
                req.id.clone(),
                -32000,
                "No project open; call open_project first",
            );
        }
    };

    match is_responsible_source(project_root, params) {
        Ok(response) => {
            JsonRpcResponse::ok(req.id.clone(), serde_json::to_value(response).unwrap())
        }
        Err(e) => JsonRpcResponse::err(req.id.clone(), -32001, e),
    }
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

fn search_project(
    project_root: &Path,
    pattern: &str,
    target: Option<&str>,
    include: Option<&str>,
    limit: usize,
) -> Result<Vec<SearchMatch>, String> {
    let regex = Regex::new(pattern).map_err(|e| format!("regex error: {e}"))?;
    let include = build_optional_glob_set(include)?;
    let analyzer = TreeSitterAnalyzer::new();
    let mut matches = Vec::new();

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
        for (line_index, text) in content.lines().enumerate() {
            if !regex.is_match(text) {
                continue;
            }
            let line = line_index + 1;
            let symbol_match = analyzer.find_containing_symbol_match(path, line);
            let selector = symbol_match
                .as_ref()
                .map(|symbol| symbol.canonical_selector(path, project_root));
            let selector_info = symbol_match.as_ref().map(|symbol| {
                selector_info_for_symbol(path, project_root, symbol, "enclosing_symbol")
            });
            matches.push(SearchMatch {
                file: relative.clone(),
                line,
                match_id: format!("{relative}:{line}:{}", matches.len() + 1),
                text: text.to_string(),
                enclosing_selector: selector.clone(),
                selector,
                selector_info,
            });
            if matches.len() >= limit {
                return Ok(matches);
            }
        }
    }

    Ok(matches)
}

fn find_glob_files(
    project_root: &Path,
    pattern: &str,
    target: Option<&str>,
) -> Result<Vec<(String, SystemTime)>, String> {
    let glob_set = build_glob_set(pattern)?;
    let mut files = Vec::new();
    for entry in project_files(project_root, target)? {
        let path = entry.path();
        let relative = relative_file_path(project_root, path);
        if glob_set.is_match(&relative) {
            let mtime = entry
                .metadata()
                .or_else(|_| fs::metadata(path))
                .and_then(|metadata| metadata.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            files.push((relative, mtime));
        }
    }
    Ok(files)
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

pub fn format_grep_output(matches: &[SearchMatch]) -> String {
    if matches.is_empty() {
        return "No files found".to_string();
    }

    let mut lines = vec![format!("Found {} matches", matches.len()), String::new()];
    let mut current_file: Option<&str> = None;
    let mut current_selector: Option<Option<&str>> = None;

    for item in matches {
        if current_file != Some(item.file.as_str()) {
            if current_file.is_some() {
                lines.push(String::new());
            }
            lines.push(format!("{}:", item.file));
            current_file = Some(item.file.as_str());
            current_selector = None;
        }

        let selector = item.selector.as_deref();
        if current_selector != Some(selector) {
            match selector {
                Some(selector) => lines.push(format!("  {selector}:")),
                None => lines.push("  Unclassified matches:".to_string()),
            }
            current_selector = Some(selector);
        }

        lines.push(format!("    Line {}: {}", item.line, item.text));
    }

    lines.join("\n")
}

pub fn format_glob_output(files: &[String], truncated: bool) -> String {
    let mut lines = Vec::new();
    if files.is_empty() {
        lines.push("No files found".to_string());
    } else {
        lines.extend(files.iter().cloned());
    }

    if truncated {
        lines.push(String::new());
        lines.push(
            "(Results are truncated: showing first 100 results. Consider using a more specific path or pattern.)"
                .to_string(),
        );
    }

    lines.join("\n")
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

struct ResolvedReadSelection {
    selector: String,
    start_line: usize,
    end_line: usize,
    selector_info: SelectorInfo,
    content_override: Option<String>,
}

fn resolve_read_selection(
    analyzer: &TreeSitterAnalyzer,
    full_path: &Path,
    project_root: &Path,
    file_content: &str,
    parsed: &selector::ParsedSelector,
) -> Result<ResolvedReadSelection, String> {
    let line_count = file_content.lines().count().max(1);
    match &parsed.target {
        SelectorTarget::Symbol(_) => {
            let symbol = analyzer.resolve_selector(full_path, parsed)?;
            let selector = symbol.canonical_selector(full_path, project_root);
            Ok(ResolvedReadSelection {
                selector: selector.clone(),
                start_line: symbol.start_line,
                end_line: symbol.end_line,
                selector_info: selector_info_for_symbol(full_path, project_root, &symbol, "symbol"),
                content_override: None,
            })
        }
        SelectorTarget::LineRange {
            start_line,
            end_line,
        } => {
            let (start_line, end_line) = clamp_range(*start_line, *end_line, line_count)?;
            Ok(ResolvedReadSelection {
                selector: format!(
                    "{}#L{}-L{}",
                    relative_file_path(project_root, full_path),
                    start_line,
                    end_line
                ),
                start_line,
                end_line,
                selector_info: selector_info_for_range(
                    full_path,
                    project_root,
                    "line_range",
                    start_line,
                    end_line,
                    None,
                ),
                content_override: None,
            })
        }
        SelectorTarget::AroundLine { line, context } => {
            let start_line = line.saturating_sub(*context).max(1);
            let end_line = (*line + *context).min(line_count);
            Ok(ResolvedReadSelection {
                selector: format!(
                    "{}#around:L{}±{}",
                    relative_file_path(project_root, full_path),
                    line,
                    context
                ),
                start_line,
                end_line,
                selector_info: selector_info_for_range(
                    full_path,
                    project_root,
                    "around_line",
                    start_line,
                    end_line,
                    analyzer.find_containing_symbol_match(full_path, *line),
                ),
                content_override: None,
            })
        }
        SelectorTarget::Match { pattern, around } => {
            let regex = Regex::new(pattern).map_err(|e| format!("regex error: {e}"))?;
            let mut hits = file_content
                .lines()
                .enumerate()
                .filter_map(|(idx, line)| regex.is_match(line).then_some(idx + 1))
                .collect::<Vec<_>>();
            match hits.len() {
                0 => Err(format!("match selector found no matches for /{pattern}/")),
                1 => {
                    let line = hits.remove(0);
                    let (start_line, end_line, kind) = if let Some(context) = around {
                        (
                            line.saturating_sub(*context).max(1),
                            (line + *context).min(line_count),
                            "match_around",
                        )
                    } else {
                        (line, line, "match")
                    };
                    Ok(ResolvedReadSelection {
                        selector: if let Some(context) = around {
                            format!(
                                "{}#match:/{}/#around:{}",
                                relative_file_path(project_root, full_path),
                                pattern,
                                context
                            )
                        } else {
                            format!(
                                "{}#match:/{}/",
                                relative_file_path(project_root, full_path),
                                pattern
                            )
                        },
                        start_line,
                        end_line,
                        selector_info: selector_info_for_range(
                            full_path,
                            project_root,
                            kind,
                            start_line,
                            end_line,
                            analyzer.find_containing_symbol_match(full_path, line),
                        ),
                        content_override: None,
                    })
                }
                _ => Err(format!(
                    "match selector is ambiguous for /{pattern}/; candidate lines: {}",
                    hits.iter()
                        .map(|line| line.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            }
        }
        SelectorTarget::BeforeLine { line } | SelectorTarget::AfterLine { line } => {
            let (start_line, end_line) = clamp_range(*line, *line, line_count)?;
            Ok(ResolvedReadSelection {
                selector: format!(
                    "{}#L{}-L{}",
                    relative_file_path(project_root, full_path),
                    start_line,
                    end_line
                ),
                start_line,
                end_line,
                selector_info: selector_info_for_range(
                    full_path,
                    project_root,
                    "insertion_line",
                    start_line,
                    end_line,
                    analyzer.find_containing_symbol_match(full_path, *line),
                ),
                content_override: None,
            })
        }
        SelectorTarget::Enclosing { line } => {
            let symbol = analyzer
                .find_containing_symbol_match(full_path, *line)
                .ok_or_else(|| {
                    format!(
                        "no enclosing symbol found at {} line {}",
                        full_path.display(),
                        line
                    )
                })?;
            let selector = symbol.canonical_selector(full_path, project_root);
            Ok(ResolvedReadSelection {
                selector: selector.clone(),
                start_line: symbol.start_line,
                end_line: symbol.end_line,
                selector_info: selector_info_for_symbol(
                    full_path,
                    project_root,
                    &symbol,
                    "enclosing_symbol",
                ),
                content_override: None,
            })
        }
        SelectorTarget::Outline => {
            let symbols = analyzer.symbols_in_file(full_path)?;
            let content = symbols
                .iter()
                .map(|symbol| {
                    format!(
                        "{}{} #L{}-L{}",
                        symbol.kind_prefix, symbol.name, symbol.start_line, symbol.end_line
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let end_line = content.lines().count().max(1);
            Ok(ResolvedReadSelection {
                selector: format!("{}#outline", relative_file_path(project_root, full_path)),
                start_line: 1,
                end_line,
                selector_info: SelectorInfo {
                    file: relative_file_path(project_root, full_path),
                    kind: "outline".to_string(),
                    range: None,
                    symbol_selector: None,
                    symbol_start_line: None,
                    symbol_end_line: None,
                    definition_line: None,
                },
                content_override: Some(if content.is_empty() {
                    String::new()
                } else {
                    format!("{content}\n")
                }),
            })
        }
    }
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

fn selector_info_for_symbol(
    full_path: &Path,
    project_root: &Path,
    symbol: &crate::treesitter::SymbolMatch,
    kind: &str,
) -> SelectorInfo {
    let selector = symbol.canonical_selector(full_path, project_root);
    SelectorInfo {
        file: relative_file_path(project_root, full_path),
        kind: kind.to_string(),
        range: Some(LineRange {
            start_line: symbol.start_line,
            end_line: symbol.end_line,
        }),
        symbol_selector: Some(selector),
        symbol_start_line: Some(symbol.start_line),
        symbol_end_line: Some(symbol.end_line),
        definition_line: Some(symbol.start_line),
    }
}

fn selector_info_for_range(
    full_path: &Path,
    project_root: &Path,
    kind: &str,
    start_line: usize,
    end_line: usize,
    symbol: Option<crate::treesitter::SymbolMatch>,
) -> SelectorInfo {
    let mut info = SelectorInfo {
        file: relative_file_path(project_root, full_path),
        kind: kind.to_string(),
        range: Some(LineRange {
            start_line,
            end_line,
        }),
        symbol_selector: None,
        symbol_start_line: None,
        symbol_end_line: None,
        definition_line: None,
    };
    if let Some(symbol) = symbol {
        info.symbol_selector = Some(symbol.canonical_selector(full_path, project_root));
        info.symbol_start_line = Some(symbol.start_line);
        info.symbol_end_line = Some(symbol.end_line);
        info.definition_line = Some(symbol.start_line);
    }
    info
}

fn handle_read_code(
    req: &JsonRpcRequest,
    params: &ReadCodeRequest,
    project_root: Option<&Path>,
) -> JsonRpcResponse {
    let parsed = match selector::parse_selector(&params.selector) {
        Ok(s) => s,
        Err(e) => {
            return JsonRpcResponse::err(req.id.clone(), -32602, format!("Bad selector: {e}"));
        }
    };

    let project_root = match project_root {
        Some(r) => r,
        None => {
            return JsonRpcResponse::err(
                req.id.clone(),
                -32000,
                "No project open; call open_project first",
            );
        }
    };

    let (full_path, _ext) = match selector::resolve_file(&parsed, project_root) {
        Ok(p) => p,
        Err(e) => return JsonRpcResponse::err(req.id.clone(), -32001, e),
    };

    let file_content = match std::fs::read_to_string(&full_path) {
        Ok(c) => c,
        Err(e) => {
            return JsonRpcResponse::err(
                req.id.clone(),
                -32001,
                format!("Failed to read {}: {e}", full_path.display()),
            );
        }
    };

    let language = guess_language(&full_path);
    let analyzer = TreeSitterAnalyzer::new();
    let resolved =
        match resolve_read_selection(&analyzer, &full_path, project_root, &file_content, &parsed) {
            Ok(resolved) => resolved,
            Err(e) => return JsonRpcResponse::err(req.id.clone(), -32002, e),
        };
    let content = resolved
        .content_override
        .clone()
        .unwrap_or_else(|| read_line_range(&file_content, resolved.start_line, resolved.end_line));

    JsonRpcResponse::ok(
        req.id.clone(),
        serde_json::to_value(ReadCodeResponse {
            selector: resolved.selector,
            content,
            language: language.to_string(),
            start_line: resolved.start_line,
            end_line: resolved.end_line,
            selector_info: resolved.selector_info,
        })
        .unwrap(),
    )
}

fn guess_language(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => "rust",
        Some("py") => "python",
        Some("js") | Some("mjs") | Some("cjs") => "javascript",
        Some("ts") | Some("mts") | Some("cts") => "typescript",
        Some("go") => "go",
        Some("java") => "java",
        Some("c") | Some("h") => "c",
        Some("cpp") | Some("cc") | Some("cxx") | Some("hpp") => "cpp",
        Some("toml") => "toml",
        Some("json") => "json",
        Some("yaml") | Some("yml") => "yaml",
        Some("md") => "markdown",
        _ => "text",
    }
}

fn handle_edit_code(
    req: &JsonRpcRequest,
    params: &EditCodeRequest,
    project_root: Option<&Path>,
    propagation_state: &Mutex<PropagationState>,
    lsp_analyzer: &Mutex<Option<Box<dyn Analyzer + Send>>>,
) -> JsonRpcResponse {
    let project_root = match project_root {
        Some(r) => r,
        None => {
            return JsonRpcResponse::err(
                req.id.clone(),
                -32000,
                "No project open; call open_project first",
            );
        }
    };

    match patch::edit_code_apply(&params.diff, project_root, lsp_analyzer) {
        Ok(results) => {
            if !results.is_empty()
                && let Ok(mut state) = propagation_state.lock()
            {
                state.accumulate(results.clone());
            }
            JsonRpcResponse::ok(
                req.id.clone(),
                serde_json::to_value(PropagationResponse {
                    propagation_results: results,
                })
                .unwrap(),
            )
        }
        Err(e) => JsonRpcResponse::err(req.id.clone(), -32001, e),
    }
}

pub fn handle_get_config_hints(req: &JsonRpcRequest) -> JsonRpcResponse {
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

    JsonRpcResponse::ok(
        req.id.clone(),
        serde_json::json!({
            "tree_sitter_languages": ts_langs,
            "lsp_languages": languages,
        }),
    )
}
/// Public convenience wrapper for `handle_get_config_hints`.
pub fn dispatch_get_config_hints(req: &JsonRpcRequest) -> JsonRpcResponse {
    handle_get_config_hints(req)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::PropagationState;
    use std::sync::Mutex;

    #[test]
    fn grep_output_groups_matches_by_file_and_selector() {
        let matches = vec![
            SearchMatch {
                file: "src/coding_app.rs".to_string(),
                line: 10,
                match_id: "src/coding_app.rs:10:1".to_string(),
                text: "fn build_usage() {".to_string(),
                selector: Some("src/coding_app.rs::fn build_usage #L10-L20".to_string()),
                enclosing_selector: Some("src/coding_app.rs::fn build_usage #L10-L20".to_string()),
                selector_info: None,
            },
            SearchMatch {
                file: "src/coding_app.rs".to_string(),
                line: 12,
                match_id: "src/coding_app.rs:12:2".to_string(),
                text: "usage.push_str(\"grep\");".to_string(),
                selector: Some("src/coding_app.rs::fn build_usage #L10-L20".to_string()),
                enclosing_selector: Some("src/coding_app.rs::fn build_usage #L10-L20".to_string()),
                selector_info: None,
            },
            SearchMatch {
                file: "src/coding_app.rs".to_string(),
                line: 3,
                match_id: "src/coding_app.rs:3:3".to_string(),
                text: "use std::path::PathBuf;".to_string(),
                selector: None,
                enclosing_selector: None,
                selector_info: None,
            },
        ];

        assert_eq!(
            format_grep_output(&matches),
            "Found 3 matches\n\nsrc/coding_app.rs:\n  src/coding_app.rs::fn build_usage #L10-L20:\n    Line 10: fn build_usage() {\n    Line 12: usage.push_str(\"grep\");\n  Unclassified matches:\n    Line 3: use std::path::PathBuf;"
        );
    }

    #[test]
    fn glob_output_matches_opencode_style() {
        let files = vec!["src/coding_app.rs".to_string(), "src/app.rs".to_string()];

        assert_eq!(
            format_glob_output(&files, true),
            "src/coding_app.rs\nsrc/app.rs\n\n(Results are truncated: showing first 100 results. Consider using a more specific path or pattern.)"
        );
    }

    #[test]
    fn grep_code_filters_by_path_and_include() {
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

        let response = grep_code(
            dir.path(),
            &GrepCodeRequest {
                pattern: "needle".to_string(),
                path: Some("src".to_string()),
                include: Some("*.rs".to_string()),
            },
        )
        .unwrap();

        let files = response
            .matches
            .iter()
            .map(|item| item.file.as_str())
            .collect::<Vec<_>>();
        assert_eq!(files, vec!["src/lib.rs", "src/nested/mod.rs"]);
        assert!(response.output.starts_with("Found 2 matches"));
    }

    #[test]
    fn glob_files_sorts_by_mtime_and_filters_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::create_dir_all(dir.path().join("tests")).unwrap();
        std::fs::write(dir.path().join("src/old.rs"), "old").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        std::fs::write(dir.path().join("src/new.rs"), "new").unwrap();
        std::fs::write(dir.path().join("tests/newer.rs"), "newer").unwrap();

        let response = glob_files(
            dir.path(),
            &GlobFilesRequest {
                pattern: "*.rs".to_string(),
                path: Some("src".to_string()),
            },
        )
        .unwrap();

        assert_eq!(response.files, vec!["src/new.rs", "src/old.rs"]);
        assert!(!response.truncated);
        assert_eq!(response.output, "src/new.rs\nsrc/old.rs");
    }

    #[test]
    fn search_code_honors_explicit_limit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub fn one() { let needle = 1; }\npub fn two() { let needle = 2; }\npub fn three() { let needle = 3; }\n",
        )
        .unwrap();

        let req = JsonRpcRequest {
            _jsonrpc: "2.0".to_string(),
            id: serde_json::json!(1),
            method: "search_code".to_string(),
            params: serde_json::json!({"query": "needle", "limit": 2}),
        };
        let propagation_state = Mutex::new(PropagationState::new());
        let lsp_analyzer: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);

        let response = dispatch(&req, Some(dir.path()), &propagation_state, &lsp_analyzer);
        assert!(
            response.error.is_none(),
            "unexpected error: {:?}",
            response.error
        );
        let result = response.result.expect("search_code should return a result");
        assert_eq!(result["matches"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn search_code_clamps_zero_limit_to_one() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub fn one() { let needle = 1; }\npub fn two() { let needle = 2; }\n",
        )
        .unwrap();

        let req = JsonRpcRequest {
            _jsonrpc: "2.0".to_string(),
            id: serde_json::json!(1),
            method: "search_code".to_string(),
            params: serde_json::json!({"query": "needle", "limit": 0}),
        };
        let propagation_state = Mutex::new(PropagationState::new());
        let lsp_analyzer: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);

        let response = dispatch(&req, Some(dir.path()), &propagation_state, &lsp_analyzer);
        assert!(
            response.error.is_none(),
            "unexpected error: {:?}",
            response.error
        );
        let result = response.result.expect("search_code should return a result");
        assert_eq!(result["matches"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn read_code_returns_only_resolved_symbol_range() {
        let dir = tempfile::tempdir().unwrap();
        let source = "pub fn first() {\n    println!(\"first\");\n}\n\npub fn second() {\n    println!(\"second\");\n}\n";
        std::fs::write(dir.path().join("lib.rs"), source).unwrap();

        let req = JsonRpcRequest {
            _jsonrpc: "2.0".to_string(),
            id: serde_json::json!(1),
            method: "read_code".to_string(),
            params: serde_json::json!({"selector": "lib.rs::fn second"}),
        };
        let propagation_state = Mutex::new(PropagationState::new());
        let lsp_analyzer: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);

        let response = dispatch(&req, Some(dir.path()), &propagation_state, &lsp_analyzer);
        assert!(
            response.error.is_none(),
            "unexpected error: {:?}",
            response.error
        );
        let result = response.result.expect("read_code should return a result");

        assert_eq!(
            result["content"],
            "pub fn second() {\n    println!(\"second\");\n}\n"
        );
        assert_eq!(result["start_line"], 5);
        assert_eq!(result["end_line"], 7);
        assert_eq!(result["selector"], "lib.rs::fn second #L5-L7");
        assert!(!result["content"].as_str().unwrap().contains("first"));
    }
    #[test]
    fn read_code_supports_line_range_enclosing_outline_and_structured_info() {
        let dir = tempfile::tempdir().unwrap();
        let source = "pub fn first() {\n    println!(\"first\");\n}\n\npub fn second() {\n    println!(\"second\");\n}\n";
        std::fs::write(dir.path().join("lib.rs"), source).unwrap();
        let propagation_state = Mutex::new(PropagationState::new());
        let lsp_analyzer: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);

        let range_req = JsonRpcRequest {
            _jsonrpc: "2.0".to_string(),
            id: serde_json::json!(1),
            method: "read_code".to_string(),
            params: serde_json::json!({"selector": "lib.rs#L5-L7"}),
        };
        let response = dispatch(
            &range_req,
            Some(dir.path()),
            &propagation_state,
            &lsp_analyzer,
        );
        assert!(
            response.error.is_none(),
            "unexpected error: {:?}",
            response.error
        );
        let result = response.result.unwrap();
        assert_eq!(result["selector_info"]["kind"], "line_range");
        assert_eq!(result["selector_info"]["range"]["start_line"], 5);
        assert_eq!(
            result["content"],
            "pub fn second() {\n    println!(\"second\");\n}\n"
        );

        let enclosing_req = JsonRpcRequest {
            _jsonrpc: "2.0".to_string(),
            id: serde_json::json!(2),
            method: "read_code".to_string(),
            params: serde_json::json!({"selector": "lib.rs#enclosing:L6"}),
        };
        let response = dispatch(
            &enclosing_req,
            Some(dir.path()),
            &propagation_state,
            &lsp_analyzer,
        );
        assert!(
            response.error.is_none(),
            "unexpected error: {:?}",
            response.error
        );
        let result = response.result.unwrap();
        assert_eq!(result["selector_info"]["kind"], "enclosing_symbol");
        assert_eq!(result["selector_info"]["symbol_start_line"], 5);
        assert_eq!(result["selector_info"]["symbol_end_line"], 7);

        let outline_req = JsonRpcRequest {
            _jsonrpc: "2.0".to_string(),
            id: serde_json::json!(3),
            method: "read_code".to_string(),
            params: serde_json::json!({"selector": "lib.rs#outline"}),
        };
        let response = dispatch(
            &outline_req,
            Some(dir.path()),
            &propagation_state,
            &lsp_analyzer,
        );
        assert!(
            response.error.is_none(),
            "unexpected error: {:?}",
            response.error
        );
        let result = response.result.unwrap();
        assert_eq!(result["selector"], "lib.rs#outline");
        assert_eq!(result["selector_info"]["kind"], "outline");
        assert!(
            result["content"]
                .as_str()
                .unwrap()
                .contains("fn second #L5-L7")
        );
    }

    #[test]
    fn scope_usage_is_available_over_dispatch() {
        let req = JsonRpcRequest {
            _jsonrpc: "2.0".to_string(),
            id: serde_json::json!(1),
            method: "get_usage".to_string(),
            params: serde_json::json!({}),
        };
        let propagation_state = Mutex::new(PropagationState::new());
        let lsp_analyzer: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);

        let response = dispatch(&req, None, &propagation_state, &lsp_analyzer);
        assert!(
            response.error.is_none(),
            "unexpected error: {:?}",
            response.error
        );
        let result = response.result.unwrap();
        assert!(
            result["usage_markdown"]
                .as_str()
                .unwrap()
                .contains("positioning DSL")
        );
        assert!(
            result["selector_kinds"]
                .as_array()
                .unwrap()
                .iter()
                .any(|kind| kind["kind"] == "outline" && kind["edit"] == false)
        );
    }

    #[test]
    fn is_responsible_source_reports_scope_owned_source() {
        let dir = tempfile::tempdir().unwrap();
        let propagation_state = Mutex::new(PropagationState::new());
        let lsp_analyzer = Mutex::new(None);
        let req = JsonRpcRequest {
            _jsonrpc: "2.0".to_string(),
            id: serde_json::json!(1),
            method: "is_responsible_source".to_string(),
            params: serde_json::json!({ "path": "src/lib.rs" }),
        };

        let response = dispatch(&req, Some(dir.path()), &propagation_state, &lsp_analyzer);

        assert!(
            response.error.is_none(),
            "unexpected error: {:?}",
            response.error
        );
        let result: IsResponsibleSourceResponse =
            serde_json::from_value(response.result.unwrap()).unwrap();
        assert!(result.is_responsible);
        assert_eq!(result.path, "src/lib.rs");
        assert_eq!(result.extension.as_deref(), Some("rs"));
        assert_eq!(result.language.as_deref(), Some("rust"));
    }

    #[test]
    fn is_responsible_source_reports_non_source_file() {
        let dir = tempfile::tempdir().unwrap();
        let propagation_state = Mutex::new(PropagationState::new());
        let lsp_analyzer = Mutex::new(None);
        let req = JsonRpcRequest {
            _jsonrpc: "2.0".to_string(),
            id: serde_json::json!(1),
            method: "is_responsible_source".to_string(),
            params: serde_json::json!({ "path": "README.md" }),
        };

        let response = dispatch(&req, Some(dir.path()), &propagation_state, &lsp_analyzer);

        assert!(
            response.error.is_none(),
            "unexpected error: {:?}",
            response.error
        );
        let result: IsResponsibleSourceResponse =
            serde_json::from_value(response.result.unwrap()).unwrap();
        assert!(!result.is_responsible);
        assert_eq!(result.path, "README.md");
        assert_eq!(result.extension.as_deref(), Some("md"));
        assert_eq!(result.language, None);
    }
}
