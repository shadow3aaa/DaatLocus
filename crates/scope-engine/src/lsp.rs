use std::cell::RefCell;
use crate::analyzer::Analyzer;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use crate::api::{PropagationResult, PropagationSource};
use crate::treesitter::TreeSitterAnalyzer;

// ── Constants ────────────────────────────────────────────────

const RA_BINARY_NAME: &str = "rust-analyzer";
const RA_VERSION: &str = "2025-05-05";
const RA_GITHUB_RELEASE: &str = "https://github.com/rust-lang/rust-analyzer/releases/download/2025-05-05/rust-analyzer-x86_64-apple-darwin";

// ── LspAnalyzer ──────────────────────────────────────────────

/// Internal mutable state for LspAnalyzer, wrapped in RefCell for interior mutability.
struct LspAnalyzerInner {
    process: Option<Child>,
    stdin_writer: Option<BufWriter<ChildStdin>>,
    stdout_reader: Option<std::io::BufReader<ChildStdout>>,
    next_id: u64,
    initialized: bool,
}

/// Manages a rust-analyzer subprocess and communicates via LSP JSON-RPC 2.0.
///
/// Uses `RefCell<LspAnalyzerInner>` for interior mutability so that the
/// `Analyzer` trait's `&self` methods can perform LSP I/O without needing
/// `&mut self`.
///
/// Lifecycle:
/// 1. `new()` — locate or download rust-analyzer, spawn it, perform LSP `initialize`.
/// 2. `notify_did_open()` / `notify_did_change()` / `notify_did_close()` — keep LSP in sync.
/// 3. `find_references_for_symbol()` — query cross-file references via `textDocument/references`.
/// 4. `Drop` — send `shutdown`, then kill the subprocess.
pub struct LspAnalyzer {
    inner: RefCell<LspAnalyzerInner>,
}

// Newtype wrappers so we can implement Read/Write on the inner types
struct BufWriter<W: Write>(W);

impl<W: Write> Write for BufWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

impl LspAnalyzer {
    pub fn new(project_root: &Path, language: &str) -> Self {
        let project_root = project_root.to_path_buf();

        // Only support rust-analyzer for Rust projects
        if language != "rust" {
            eprintln!(
                "[scope-engine/lsp] language '{}' not supported by LSP; only 'rust' is supported",
                language
            );
            return Self {
                inner: RefCell::new(LspAnalyzerInner {
                    process: None,
                    stdin_writer: None,
                    stdout_reader: None,
                    next_id: 0,
                    initialized: false,
                }),
            };
        }

        let binary_path = match Self::locate_or_download_ra() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[scope-engine/lsp] cannot locate or download rust-analyzer: {e}");
                return Self {
                    inner: RefCell::new(LspAnalyzerInner {
                        process: None,
                        stdin_writer: None,
                        stdout_reader: None,
                        next_id: 0,
                        initialized: false,
                    }),
                };
            }
        };

        match Self::spawn_and_initialize(&binary_path, &project_root) {
            Ok((process, stdin_w, stdout_r)) => Self {
                inner: RefCell::new(LspAnalyzerInner {
                    process: Some(process),
                    stdin_writer: Some(stdin_w),
                    stdout_reader: Some(stdout_r),
                    next_id: 1, // id 0 was used for initialize
                    initialized: true,
                }),
            },
            Err(e) => {
                eprintln!("[scope-engine/lsp] failed to spawn/initialize rust-analyzer: {e}");
                Self {
                    inner: RefCell::new(LspAnalyzerInner {
                        process: None,
                        stdin_writer: None,
                        stdout_reader: None,
                        next_id: 0,
                        initialized: false,
                    }),
                }
            }
        }
    }

    // ── Binary location ───────────────────────────────────────

    /// Try PATH first; if not found, try the cache dir; if still not found,
    /// attempt to download from GitHub.
    fn locate_or_download_ra() -> Result<PathBuf, String> {
        // 1. Check PATH
        if let Ok(output) = Command::new("which").arg(RA_BINARY_NAME).output()
            && output.status.success()
        {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                eprintln!("[scope-engine/lsp] found rust-analyzer on PATH: {path}");
                return Ok(PathBuf::from(path));
            }
        }

        // 2. Check cache
        let cache_dir = Self::cache_dir()?;
        let cached = cache_dir.join(format!("rust-analyzer-{RA_VERSION}"));
        if cached.is_file() {
            eprintln!(
                "[scope-engine/lsp] found cached rust-analyzer: {}",
                cached.display()
            );
            return Ok(cached);
        }

        // 3. Download
        Self::download_ra(&cache_dir, &cached)
    }

    fn cache_dir() -> Result<PathBuf, String> {
        let base =
            dirs::cache_dir().ok_or_else(|| "cannot determine cache directory".to_string())?;
        let dir = base.join("daat-locus").join("lsp-binaries");
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create cache dir {}: {e}", dir.display()))?;
        Ok(dir)
    }

    #[cfg(feature = "download-ra")]
    fn download_ra(cache_dir: &Path, target: &Path) -> Result<PathBuf, String> {
        eprintln!("[scope-engine/lsp] downloading rust-analyzer {RA_VERSION} from GitHub...");
        let tmp = cache_dir.join("rust-analyzer-download.tmp");
        let mut resp = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| format!("HTTP client build failed: {e}"))?
            .get(RA_GITHUB_RELEASE)
            .send()
            .map_err(|e| format!("download failed: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!("download returned HTTP {}", resp.status()));
        }

        let mut file =
            std::fs::File::create(&tmp).map_err(|e| format!("cannot create tmp file: {e}"))?;
        resp.copy_to(&mut file)
            .map_err(|e| format!("download write failed: {e}"))?;
        drop(file);

        std::fs::rename(&tmp, target)
            .map_err(|e| format!("cannot rename tmp to final path: {e}"))?;

        // Make executable
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| format!("cannot chmod: {e}"))?;
        }

        eprintln!(
            "[scope-engine/lsp] downloaded rust-analyzer to {}",
            target.display()
        );
        Ok(target.to_path_buf())
    }

    #[cfg(not(feature = "download-ra"))]
    fn download_ra(_cache_dir: &Path, _target: &Path) -> Result<PathBuf, String> {
        Err("rust-analyzer download not available (feature 'download-ra' disabled)".to_string())
    }

    // ── Subprocess management ──────────────────────────────────

    fn spawn_and_initialize(
        binary_path: &Path,
        project_root: &Path,
    ) -> Result<
        (
            Child,
            BufWriter<ChildStdin>,
            std::io::BufReader<ChildStdout>,
        ),
        String,
    > {
        let mut child = Command::new(binary_path)
            .current_dir(project_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot spawn rust-analyzer: {e}"))?;

        let stdin = child.stdin.take().ok_or("cannot take stdin")?;
        let stdout = child.stdout.take().ok_or("cannot take stdout")?;

        let mut writer = BufWriter(stdin);
        let mut reader = std::io::BufReader::new(stdout);

        // ── LSP initialize ─────────────────────────────────────
        let root_uri = path_to_file_uri(project_root);
        let init_params = serde_json::json!({
            "rootUri": root_uri,
            "capabilities": {},
        });

        let resp = Self::send_request_raw(&mut writer, &mut reader, 0, "initialize", init_params)
            .map_err(|e| {
            let _ = child.kill();
            format!("initialize failed: {e}")
        })?;

        if let Some(err) = resp.get("error") {
            let _ = child.kill();
            return Err(format!("initialize error: {err}"));
        }

        // ── initialized notification ───────────────────────────
        Self::send_notification(&mut writer, "initialized", serde_json::json!({})).map_err(
            |e| {
                let _ = child.kill();
                format!("initialized notification failed: {e}")
            },
        )?;

        eprintln!(
            "[scope-engine/lsp] rust-analyzer initialized for {}",
            project_root.display()
        );
        // Give RA some time to start indexing
        std::thread::sleep(std::time::Duration::from_secs(3));
        Ok((child, writer, reader))
    }

    // ── LSP file synchronization ────────────────────────────────

    /// Notify LSP that a file was opened.
    pub fn notify_did_open(&self, file_path: &Path, text: &str) {
        let mut inner = self.inner.borrow_mut();
        if !inner.initialized {
            return;
        }
        let uri = path_to_file_uri(file_path);
        let params = serde_json::json!({
            "textDocument": {
                "uri": uri,
                "languageId": "rust",
                "version": 0,
                "text": text,
            }
        });
        if let Some(ref mut writer) = inner.stdin_writer {
            let _ = Self::send_notification(writer, "textDocument/didOpen", params);
        }
    }

    /// Notify LSP that a file was modified (full sync).
    pub fn notify_did_change(&self, file_path: &Path, version: i32, text: &str) {
        let mut inner = self.inner.borrow_mut();
        if !inner.initialized {
            return;
        }
        let uri = path_to_file_uri(file_path);
        let params = serde_json::json!({
            "textDocument": { "uri": uri, "version": version },
            "contentChanges": [{ "text": text }]
        });
        if let Some(ref mut writer) = inner.stdin_writer {
            let _ = Self::send_notification(writer, "textDocument/didChange", params);
        }
    }

    /// Notify LSP that a file was closed.
    pub fn notify_did_close(&self, file_path: &Path) {
        let mut inner = self.inner.borrow_mut();
        if !inner.initialized {
            return;
        }
        let uri = path_to_file_uri(file_path);
        let params = serde_json::json!({
            "textDocument": { "uri": uri }
        });
        if let Some(ref mut writer) = inner.stdin_writer {
            let _ = Self::send_notification(writer, "textDocument/didClose", params);
        }
    }

    // ── LSP requests ──────────────────────────────────────────

    /// Send `textDocument/references` and map results to PropagationResult.
    pub fn find_references_for_symbol(
        &self,
        file_path: &Path,
        line: usize,
        character: usize,
        project_root: &Path,
    ) -> Vec<PropagationResult> {
        let mut guard = self.inner.borrow_mut();
        let inner = &mut *guard;

        if !inner.initialized {
            return vec![];
        }

        let uri = path_to_file_uri(file_path);
        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": line.saturating_sub(1), "character": character },
            "context": { "includeDeclaration": false }
        });

        let (writer, reader) = match (&mut inner.stdin_writer, &mut inner.stdout_reader) {
            (Some(w), Some(r)) => (w, r),
            _ => return vec![],
        };

        let id = inner.next_id;
        inner.next_id += 1;

        let params = params.clone(); // clone for retry
        let resp = match Self::send_request_raw(
            writer,
            reader,
            id,
            "textDocument/references",
            params.clone(),
        ) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[scope-engine/lsp] textDocument/references failed: {e}");
                return vec![];
            }
        };

        // Parse result \u2014 LSP returns Location[] or null
        let locations = match resp.get("result") {
            Some(serde_json::Value::Array(arr)) => arr.clone(),
            Some(serde_json::Value::Null) | None => {
                // RA might still be indexing \u2014 retry after a delay
                eprintln!("[scope-engine/lsp] references returned null, waiting and retrying...");
                std::thread::sleep(std::time::Duration::from_secs(2));
                let retry_id = inner.next_id;
                inner.next_id += 1;
                let retry_resp = match Self::send_request_raw(
                    writer,
                    reader,
                    retry_id,
                    "textDocument/references",
                    params,
                ) {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!(
                            "[scope-engine/lsp] retry textDocument/references also failed: {e}"
                        );
                        return vec![];
                    }
                };
                eprintln!(
                    "[scope-engine/lsp] retry response: {}",
                    serde_json::to_string(&retry_resp).unwrap_or_default()
                );
                match retry_resp.get("result") {
                    Some(serde_json::Value::Array(arr)) => arr.clone(),
                    _ => return vec![],
                }
            }
            _ => return vec![],
        };

        eprintln!(
            "[scope-engine/lsp] found {} reference locations",
            locations.len()
        );

        let ts = TreeSitterAnalyzer::new();
        let mut results = Vec::new();

        for loc in locations {
            let loc_uri = loc.get("uri").and_then(|v| v.as_str()).unwrap_or("");
            let loc_line = loc
                .get("range")
                .and_then(|r| r.get("start"))
                .and_then(|s| s.get("line"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;

            // Convert file URI back to a path
            let loc_path = uri_to_path(loc_uri);
            let rel_path = loc_path
                .strip_prefix(project_root)
                .ok()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| loc_path.to_string_lossy().to_string());

            // Get context: read the line
            let context_line = std::fs::read_to_string(&loc_path)
                .ok()
                .and_then(|content| content.lines().nth(loc_line).map(|l| l.to_string()))
                .unwrap_or_default();

            // Map to containing symbol selector
            let selector = ts
                .find_containing_symbol(&loc_path, loc_line + 1, project_root)
                .unwrap_or_else(|| format!("{rel_path}::line {}", loc_line + 1));

            // Build lsp_references tuple
            let lsp_ref = (selector.clone(), loc_line + 1, context_line.clone());

            results.push(PropagationResult {
                selector,
                reason: format!("LSP reference found at {}:{}", rel_path, loc_line + 1),
                source: PropagationSource::Lsp,
                lsp_references: Some(vec![lsp_ref]),
                diff_summary: None,
                file_snippet: None,
                project_files: None,
            });
        }

        results
    }

    // ── JSON-RPC transport ─────────────────────────────────────

    /// Send a JSON-RPC request and read the response.
    /// Returns the full response as a serde_json::Value.
    fn send_request_raw(
        writer: &mut BufWriter<ChildStdin>,
        reader: &mut std::io::BufReader<ChildStdout>,
        id: u64,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        Self::write_message(writer, &request)?;
        Self::read_response(reader, id)
    }

    /// Send a JSON-RPC notification (no id, no response expected).
    fn send_notification(
        writer: &mut BufWriter<ChildStdin>,
        method: &str,
        params: serde_json::Value,
    ) -> Result<(), String> {
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        Self::write_message(writer, &notification)
    }

    /// Write a JSON-RPC message using LSP Content-Length header.
    fn write_message(
        writer: &mut BufWriter<ChildStdin>,
        msg: &serde_json::Value,
    ) -> Result<(), String> {
        let body = serde_json::to_string(msg).map_err(|e| format!("json serialize failed: {e}"))?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        writer
            .write_all(header.as_bytes())
            .map_err(|e| format!("write header failed: {e}"))?;
        writer
            .write_all(body.as_bytes())
            .map_err(|e| format!("write body failed: {e}"))?;
        writer.flush().map_err(|e| format!("flush failed: {e}"))?;
        Ok(())
    }

    /// Read a JSON-RPC response, matching by id.
    /// Skips over notifications and responses for other ids.
    fn read_response(
        reader: &mut std::io::BufReader<ChildStdout>,
        expected_id: u64,
    ) -> Result<serde_json::Value, String> {
        loop {
            // Read Content-Length header
            let mut header_line = String::new();
            loop {
                let mut byte = [0u8; 1];
                reader
                    .read_exact(&mut byte)
                    .map_err(|e| format!("read header byte failed: {e}"))?;
                let ch = byte[0] as char;
                header_line.push(ch);
                if header_line.ends_with("\r\n\r\n") {
                    break;
                }
                // Safety: prevent infinite loop on malformed input
                if header_line.len() > 4096 {
                    return Err("header too long, possibly malformed LSP response".to_string());
                }
            }

            // Parse Content-Length
            let content_length: usize = header_line
                .lines()
                .find_map(|line| {
                    line.strip_prefix("Content-Length: ")
                        .and_then(|v| v.trim().parse().ok())
                })
                .ok_or("missing Content-Length header")?;

            // Read body
            let mut body_buf = vec![0u8; content_length];
            reader
                .read_exact(&mut body_buf)
                .map_err(|e| format!("read body failed: {e}"))?;
            let body: serde_json::Value =
                serde_json::from_slice(&body_buf).map_err(|e| format!("json parse failed: {e}"))?;

            // Check if this is a response (has "id") or a notification (no "id")
            if let Some(resp_id) = body.get("id").and_then(|v| v.as_u64()) {
                // A response has "result" or "error" field; a request/notification has "method"
                let is_response = body.get("result").is_some() || body.get("error").is_some();
                if resp_id == expected_id && is_response {
                    return Ok(body);
                }
                // Response for a different id \u2014 skip
            }
            // Notification or wrong-id response \u2014 skip and read next message
        }
    }
}

impl Drop for LspAnalyzer {
    fn drop(&mut self) {
        let inner = self.inner.get_mut();
        if !inner.initialized {
            return;
        }
        if let (Some(writer), Some(reader)) = (&mut inner.stdin_writer, &mut inner.stdout_reader) {
            let _ = Self::send_request_raw(
                writer,
                reader,
                inner.next_id,
                "shutdown",
                serde_json::json!(null),
            );
            let _ = Self::send_notification(writer, "exit", serde_json::json!(null));
        }
        if let Some(ref mut child) = inner.process {
            let _ = child.kill();
            let _ = child.wait();
        }
        inner.initialized = false;
        inner.process = None;
        inner.stdin_writer = None;
        inner.stdout_reader = None;
        eprintln!("[scope-engine/lsp] rust-analyzer shut down");
    }
}

// \u2500\u2500 URI helpers \u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500

fn path_to_file_uri(path: &Path) -> String {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let s = abs.to_string_lossy();
    format!("file://{s}")
}

fn uri_to_path(uri: &str) -> PathBuf {
    let stripped = uri.strip_prefix("file://").unwrap_or(uri);
    PathBuf::from(stripped)
}

impl Analyzer for LspAnalyzer {
    fn find_references_for_symbol(
        &self,
        file_path: &Path,
        line: usize,
        character: usize,
        project_root: &Path,
    ) -> Vec<PropagationResult> {
        LspAnalyzer::find_references_for_symbol(self, file_path, line, character, project_root)
    }

    fn notify_did_open(&self, file_path: &Path, text: &str) {
        LspAnalyzer::notify_did_open(self, file_path, text);
    }

    fn notify_did_change(&self, file_path: &Path, version: i32, text: &str) {
        LspAnalyzer::notify_did_change(self, file_path, version, text);
    }

    fn notify_did_close(&self, file_path: &Path) {
        LspAnalyzer::notify_did_close(self, file_path);
    }

    fn is_initialized(&self) -> bool {
        self.inner.borrow().initialized
    }
}
