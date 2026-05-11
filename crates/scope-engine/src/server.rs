use std::path::Path;

use crate::api::*;
use crate::selector::{self, ParsedSelector, SymbolKind};

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
            let _ = params;
            JsonRpcResponse::ok(
                req.id.clone(),
                serde_json::to_value(SearchCodeResponse { selectors: vec![] }).unwrap(),
            )
        }
        "edit_code" => {
            let params: EditCodeRequest = match serde_json::from_value(req.params.clone()) {
                Ok(p) => p,
                Err(e) => return JsonRpcResponse::err(req.id.clone(), -32602, format!("Invalid params: {e}")),
            };
            let _sel = match selector::parse_selector(&params.selector) {
                Ok(s) => s,
                Err(e) => return JsonRpcResponse::err(req.id.clone(), -32602, format!("Bad selector: {e}")),
            };
            // TODO: apply stripped v4a patch via propagation engine
            JsonRpcResponse::ok(
                req.id.clone(),
                serde_json::to_value(AffectedResponse { affected_selectors: vec![] }).unwrap(),
            )
        }
        "delete_code" => {
            let params: DeleteCodeRequest = match serde_json::from_value(req.params.clone()) {
                Ok(p) => p,
                Err(e) => return JsonRpcResponse::err(req.id.clone(), -32602, format!("Invalid params: {e}")),
            };
            let _sel = match selector::parse_selector(&params.selector) {
                Ok(s) => s,
                Err(e) => return JsonRpcResponse::err(req.id.clone(), -32602, format!("Bad selector: {e}")),
            };
            // TODO: apply deletion via propagation engine
            JsonRpcResponse::ok(
                req.id.clone(),
                serde_json::to_value(AffectedResponse { affected_selectors: vec![] }).unwrap(),
            )
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
