use std::path::Path;
use std::process::Command;

use crate::patch;
use crate::api::*;
use crate::selector;
use crate::treesitter::TreeSitterAnalyzer;

pub fn dispatch(req: &JsonRpcRequest, project_root: Option<&Path>) -> JsonRpcResponse {
    match req.method.as_str() {
        "open_project" => {
            let params: OpenProjectRequest = match serde_json::from_value(req.params.clone()) {
                Ok(p) => p,
                Err(e) => return JsonRpcResponse::err(req.id.clone(), -32602, format!("Invalid params: {e}")),
            };
            JsonRpcResponse::ok(req.id.clone(), serde_json::json!({
                "status": "opened",
                "project_root": params.project_root,
                "language": params.language.unwrap_or_else(|| "auto".to_string()),
            }))
        }
        "read_code" => {
            let params: ReadCodeRequest = match serde_json::from_value(req.params.clone()) {
                Ok(p) => p,
                Err(e) => return JsonRpcResponse::err(req.id.clone(), -32602, format!("Invalid params: {e}")),
            };
            handle_read_code(req, &params, project_root)
        }
        "search_code" => {
            let params: SearchCodeRequest = match serde_json::from_value(req.params.clone()) {
                Ok(p) => p,
                Err(e) => return JsonRpcResponse::err(req.id.clone(), -32602, format!("Invalid params: {e}")),
            };
            handle_search_code(req, &params, project_root)
        }
        "edit_code" => {
            let params: EditCodeRequest = match serde_json::from_value(req.params.clone()) {
                Ok(p) => p,
                Err(e) => return JsonRpcResponse::err(req.id.clone(), -32602, format!("Invalid params: {e}")),
            };
            handle_edit_code(req, &params, project_root)
        }
        "delete_code" => {
            let params: DeleteCodeRequest = match serde_json::from_value(req.params.clone()) {
                Ok(p) => p,
                Err(e) => return JsonRpcResponse::err(req.id.clone(), -32602, format!("Invalid params: {e}")),
            };
            handle_delete_code(req, &params, project_root)
        }
        "ack_next_event" => {
            JsonRpcResponse::ok(
                req.id.clone(),
                serde_json::to_value(NextReviewResponse { review: None }).unwrap(),
            )
        }
        _ => JsonRpcResponse::err(req.id.clone(), -32601, format!("Method not found: {}", req.method)),
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
            "--color", "never",
            "--no-ignore-vcs",
            &params.query,
        ])
        .current_dir(project_root)
        .output()
    {
        Ok(out) => out,
        Err(e) => {
            return JsonRpcResponse::err(
                req.id.clone(),
                -32001,
                format!("Failed to run rg: {e}"),
            );
        }
    };

    if !rg_output.status.success() && rg_output.status.code() != Some(1) {
        if rg_output.status.code() == Some(2) {
            let stderr = String::from_utf8_lossy(&rg_output.stderr);
            return JsonRpcResponse::err(
                req.id.clone(),
                -32001,
                format!("rg error: {stderr}"),
            );
        }
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
        Err(e) => return JsonRpcResponse::err(req.id.clone(), -32602, format!("Bad selector: {e}")),
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
            )
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

    match patch::edit_code_apply(&params.selector, &params.patch, project_root) {
        Ok(affected) => JsonRpcResponse::ok(
            req.id.clone(),
            serde_json::to_value(AffectedResponse {
                affected_selectors: affected,
            })
            .unwrap(),
        ),
        Err(e) => JsonRpcResponse::err(req.id.clone(), -32001, e),
    }
}

fn handle_delete_code(
    req: &JsonRpcRequest,
    params: &DeleteCodeRequest,
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

    match patch::delete_code_apply(&params.selector, project_root) {
        Ok(affected) => JsonRpcResponse::ok(
            req.id.clone(),
            serde_json::to_value(AffectedResponse {
                affected_selectors: affected,
            })
            .unwrap(),
        ),
        Err(e) => JsonRpcResponse::err(req.id.clone(), -32001, e),
    }
}
