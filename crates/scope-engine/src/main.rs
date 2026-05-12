mod language;
mod analyzer;
mod api;
mod patch;
mod lsp;
mod selector;
mod server;
mod state;
mod treesitter;

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use lsp::LspAnalyzer;

fn main() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut project_root: Option<PathBuf> = None;
    let propagation_state = Mutex::new(state::PropagationState::new());
    let lsp_analyzer: Mutex<Option<LspAnalyzer>> = Mutex::new(None);

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let req: api::JsonRpcRequest = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                let err = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": {"code": -32700, "message": format!("Parse error: {e}")}
                });
                let _ = writeln!(stdout.lock(), "{err}");
                continue;
            }
        };

        // Track project_root from open_project calls
        if req.method == "open_project" {
            if let Ok(params) = serde_json::from_value::<api::OpenProjectRequest>(req.params.clone()) {
                project_root = Some(PathBuf::from(&params.project_root));
            }
        }

        let resp = server::dispatch(&req, project_root.as_deref(), &propagation_state, &lsp_analyzer);
        let json = serde_json::to_string(&resp).unwrap_or_default();
        let _ = writeln!(stdout.lock(), "{json}");
    }
}
