use std::path::Path;
use std::process::Command;

use crate::api::*;
use crate::lsp::{LspAnalyzer, LspServerConfig, RustAnalyzerConfig, PyrightConfig, GoplsConfig, JdtlsConfig};
use crate::lsp::TsJsConfig;
use crate::patch;
use crate::selector;
use crate::state::PropagationState;
use crate::treesitter::TreeSitterAnalyzer;
use std::sync::Mutex;
use crate::analyzer::Analyzer;

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
                    let scan_dir = if src_dir.exists() { src_dir.as_path() } else { root };
                    if let Ok(entries) = std::fs::read_dir(scan_dir) {
                        let exts: &[&str] = match lsp_lang {
                            "rust" => &["rs"],
                            "python" => &["py"],
                            "typescript" | "javascript" => &["ts", "tsx", "js", "jsx"],
                            "go" => &["go"],
                            "java" => &["java"],
                            "c" | "h" => &["c", "h"],
                            "cpp" | "cxx" | "cc" | "hpp" | "hxx" | "hh" => &["cpp", "cxx", "cc", "hpp", "hxx", "hh"],
                            "rb" => &["rb"],
                            "php" => &["php"],
                            _ => &["rs"],
                        };
                        for entry in entries.flatten() {
                            let path = entry.path();
                            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                                if exts.contains(&ext)
                                    && let Ok(content) = std::fs::read_to_string(&path)
                                {
                                    lsp.notify_did_open(&path, &content);
                                }
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
        "delete_code" => {
            let params: DeleteCodeRequest = match serde_json::from_value(req.params.clone()) {
                Ok(p) => p,
                Err(e) => {
                    return JsonRpcResponse::err(
                        req.id.clone(),
                        -32602,
                        format!("Invalid params: {e}"),
                    );
                }
            };
            handle_delete_code(req, &params, project_root, propagation_state, lsp_analyzer)
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

    // 1. Run rg
    let rg_output = match Command::new("rg")
        .args([
            "--no-heading",
            "-n",
            "--color",
            "never",
            "--no-ignore-vcs",
            &params.query,
        ])
        .current_dir(project_root)
        .output()
    {
        Ok(out) => out,
        Err(e) => {
            return JsonRpcResponse::err(req.id.clone(), -32001, format!("Failed to run rg: {e}"));
        }
    };

    if !rg_output.status.success()
        && rg_output.status.code() != Some(1)
        && rg_output.status.code() == Some(2)
    {
        let stderr = String::from_utf8_lossy(&rg_output.stderr);
        return JsonRpcResponse::err(req.id.clone(), -32001, format!("rg error: {stderr}"));
    }

    let stdout = String::from_utf8_lossy(&rg_output.stdout);

    // 2. Parse rg output: file:line:text
    let analyzer = TreeSitterAnalyzer::new();
    let mut matches: Vec<SearchMatch> = Vec::new();

    for line_str in stdout.lines() {
        let line_str = line_str.trim();
        if line_str.is_empty() {
            continue;
        }
        let (file_path, rest) = match line_str.split_once(':') {
            Some(p) => p,
            None => continue,
        };
        let (line_num_str, text) = match rest.split_once(':') {
            Some(p) => p,
            None => continue,
        };
        let line_num: usize = match line_num_str.parse() {
            Ok(n) => n,
            Err(_) => continue,
        };

        let full_path = project_root.join(file_path);

        // 3. Find containing symbol
        let selector = analyzer.find_containing_symbol(&full_path, line_num, project_root);

        matches.push(SearchMatch {
            file: file_path.to_string(),
            line: line_num,
            text: text.to_string(),
            selector,
        });
    }

    JsonRpcResponse::ok(
        req.id.clone(),
        serde_json::to_value(SearchCodeResponse { matches }).unwrap(),
    )
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

    let content = match std::fs::read_to_string(&full_path) {
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

    JsonRpcResponse::ok(
        req.id.clone(),
        serde_json::to_value(ReadCodeResponse {
            selector: params.selector.clone(),
            content,
            language: language.to_string(),
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

    match patch::edit_code_apply(&params.selector, &params.patch, project_root, lsp_analyzer) {
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

fn handle_delete_code(
    req: &JsonRpcRequest,
    params: &DeleteCodeRequest,
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

    match patch::delete_code_apply(&params.selector, project_root, lsp_analyzer) {
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


fn handle_get_config_hints(req: &JsonRpcRequest) -> JsonRpcResponse {
    use crate::language::LanguageRegistry;
    use crate::lsp::{LspServerConfig, RustAnalyzerConfig, PyrightConfig, TsJsConfig, GoplsConfig, JdtlsConfig};

    let registry = LanguageRegistry::new();
    let configs: Vec<Box<dyn LspServerConfig>> = vec![
        Box::new(RustAnalyzerConfig),
        Box::new(PyrightConfig),
        Box::new(TsJsConfig),   // covers both TS and JS
        Box::new(GoplsConfig),
        Box::new(JdtlsConfig),
    ];

    let mut languages = Vec::new();

    // Tree-sitter languages from registry
    let ts_langs: Vec<serde_json::Value> = registry.list_languages().into_iter().map(|(name, exts)| {
        serde_json::json!({
            "name": name,
            "extensions": exts,
        })
    }).collect();

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