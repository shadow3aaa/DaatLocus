use crate::analyzer::Analyzer;
use std::cell::RefCell;
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::api::{PropagationResult, PropagationSource};
use crate::treesitter::TreeSitterAnalyzer;

const LSP_INITIALIZE_TIMEOUT: Duration = Duration::from_mins(2);
const LSP_REQUEST_TIMEOUT: Duration = Duration::from_mins(2);
const LSP_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

type LspResponse = Result<serde_json::Value, String>;
type SpawnedLsp = (
    Child,
    BufWriter<ChildStdin>,
    Receiver<LspResponse>,
    JoinHandle<()>,
);

// ── LSP Server Configuration ──────────────────────────────────

/// Configuration for a language server binary.
///
/// Each supported language provides an implementation that knows how to
/// locate/download the server binary and what parameters to send during
/// LSP initialization.
pub trait LspServerConfig: Send + Sync {
    /// Human-readable name for logging.
    fn server_name(&self) -> &str;

    /// The binary name to look for on PATH (e.g. "rust-analyzer", "pyright-langserver").
    fn binary_name(&self) -> &str;

    /// The LSP language ID for `textDocument/didOpen` (e.g. "rust", "python").
    fn language_id(&self) -> &str;

    /// Cache directory relative filename for the downloaded binary (if applicable).
    fn cached_binary_name(&self) -> String;

    /// URL to download the binary from (if PATH lookup fails and download feature enabled).
    fn download_url(&self) -> Option<String>;

    /// Extra initialization parameters to include in the LSP `initialize` request.
    fn init_params_extra(&self, _root_uri: &str) -> serde_json::Value {
        serde_json::json!({})
    }

    /// Seconds to sleep after initialization to let the server index.
    fn post_init_delay_secs(&self) -> u64 {
        3
    }

    /// Arguments to pass to the server binary when spawning.
    fn spawn_args(&self) -> Vec<String> {
        vec![]
    }

    /// Optional command to install the server when not found on PATH or cache.
    /// Returns `(command, args)` — e.g. `("go", vec!["install", "golang.org/x/tools/gopls@v0.21.1"])`.
    /// The `locate_or_download` logic will run this and then re-check PATH.
    fn install_command(&self) -> Option<(String, Vec<String>)> {
        None
    }

    /// Human-readable setup/installation hints for this LSP server.
    /// Returns a description of how to install the server, for the agent to act on.
    fn setup_hints(&self) -> String {
        format!(
            "LSP server '{}' (binary: '{}') for language '{}'. ",
            self.server_name(),
            self.binary_name(),
            self.language_id()
        )
    }
}

// ── Rust LSP config ────────────────────────────────────────────

pub struct RustAnalyzerConfig;

const RA_VERSION: &str = "2025-05-05";

impl LspServerConfig for RustAnalyzerConfig {
    fn server_name(&self) -> &'static str {
        "rust-analyzer"
    }
    fn binary_name(&self) -> &'static str {
        "rust-analyzer"
    }
    fn language_id(&self) -> &'static str {
        "rust"
    }
    fn cached_binary_name(&self) -> String {
        format!("rust-analyzer-{RA_VERSION}")
    }
    fn download_url(&self) -> Option<String> {
        Some(format!(
            "https://github.com/rust-lang/rust-analyzer/releases/download/{RA_VERSION}/rust-analyzer-x86_64-apple-darwin"
        ))
    }

    fn setup_hints(&self) -> String {
        "For Rust: rust-analyzer is auto-downloaded by scope-engine. No manual setup needed."
            .to_string()
    }
}

// ── Python LSP config ──────────────────────────────────────────

pub struct PyrightConfig;

impl LspServerConfig for PyrightConfig {
    fn server_name(&self) -> &'static str {
        "pyright-langserver"
    }
    fn binary_name(&self) -> &'static str {
        "pyright-langserver"
    }
    fn language_id(&self) -> &'static str {
        "python"
    }
    fn cached_binary_name(&self) -> String {
        "pyright-langserver".to_string()
    }
    fn download_url(&self) -> Option<String> {
        None
    } // installed via npm/pip
    fn spawn_args(&self) -> Vec<String> {
        vec!["--stdio".to_string()]
    }
    fn post_init_delay_secs(&self) -> u64 {
        2
    }

    fn setup_hints(&self) -> String {
        "For Python: install pyright-langserver via 'npm install -g pyright' or 'pip install pyright'.".to_string()
    }
}

// ── TypeScript/JavaScript LSP config ──────────────────────────

pub struct TsJsConfig;

impl LspServerConfig for TsJsConfig {
    fn server_name(&self) -> &'static str {
        "typescript-language-server"
    }
    fn binary_name(&self) -> &'static str {
        "typescript-language-server"
    }
    fn language_id(&self) -> &'static str {
        "typescript"
    }
    fn cached_binary_name(&self) -> String {
        "typescript-language-server".to_string()
    }
    fn download_url(&self) -> Option<String> {
        None
    } // installed via npm
    fn spawn_args(&self) -> Vec<String> {
        vec!["--stdio".to_string()]
    }
    fn post_init_delay_secs(&self) -> u64 {
        3
    }

    fn setup_hints(&self) -> String {
        "For TypeScript/JavaScript: install typescript-language-server via 'npm install -g typescript-language-server typescript'.".to_string()
    }
}

// ── Go LSP config ──────────────────────────────────────────────

const GOPLS_VERSION: &str = "v0.21.1";

pub struct GoplsConfig;

impl LspServerConfig for GoplsConfig {
    fn server_name(&self) -> &'static str {
        "gopls"
    }
    fn binary_name(&self) -> &'static str {
        "gopls"
    }
    fn language_id(&self) -> &'static str {
        "go"
    }
    fn cached_binary_name(&self) -> String {
        format!("gopls-{GOPLS_VERSION}")
    }
    fn download_url(&self) -> Option<String> {
        None
    }
    fn spawn_args(&self) -> Vec<String> {
        vec!["serve".to_string()]
    }
    fn post_init_delay_secs(&self) -> u64 {
        4
    }
    fn install_command(&self) -> Option<(String, Vec<String>)> {
        Some((
            "go".to_string(),
            vec![
                "install".to_string(),
                format!("golang.org/x/tools/gopls@{GOPLS_VERSION}"),
            ],
        ))
    }
}

// ── Java LSP config (Eclipse JDT Language Server) ──────────────

pub struct JdtlsConfig;

impl LspServerConfig for JdtlsConfig {
    fn server_name(&self) -> &'static str {
        "jdtls"
    }
    fn binary_name(&self) -> &'static str {
        "jdtls"
    }
    fn language_id(&self) -> &'static str {
        "java"
    }
    fn cached_binary_name(&self) -> String {
        "jdtls".to_string()
    }
    fn download_url(&self) -> Option<String> {
        None
    }
    fn spawn_args(&self) -> Vec<String> {
        vec![]
    }
    fn post_init_delay_secs(&self) -> u64 {
        5
    }

    fn setup_hints(&self) -> String {
        "For Java: install Eclipse JDT Language Server (jdtls). On macOS: 'brew install eclipse-jdtls'. On Linux: download from https://download.eclipse.org/jdtls/snapshots/ and add 'jdtls' to PATH. Requires JDK 17+.".to_string()
    }
}

// ── LspClient (was LspAnalyzer) ───────────────────────────────

/// Internal mutable state for `LspClient`, wrapped in `RefCell` for interior mutability.
struct LspClientInner {
    process: Option<Child>,
    stdin_writer: Option<BufWriter<ChildStdin>>,
    response_receiver: Option<Receiver<LspResponse>>,
    reader_thread: Option<JoinHandle<()>>,
    next_id: u64,
    initialized: bool,
    /// The language ID for didOpen notifications.
    language_id: String,
}

/// Manages an LSP language server subprocess and communicates via JSON-RPC 2.0.
///
/// Uses `RefCell<LspClientInner>` for interior mutability so that the
/// `Analyzer` trait's `&self` methods can perform LSP I/O without needing
/// `&mut self`.
///
/// Lifecycle:
/// 1. `new()` — locate or download the server binary, spawn it, perform LSP `initialize`.
/// 2. `notify_did_open()` / `notify_did_change()` / `notify_did_close()` — keep LSP in sync.
/// 3. `find_references_for_symbol()` — query cross-file references via `textDocument/references`.
/// 4. `Drop` — send `shutdown`, then kill the subprocess.
pub struct LspClient {
    inner: RefCell<LspClientInner>,
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

impl LspClient {
    pub fn new(project_root: &Path, config: &dyn LspServerConfig) -> Self {
        let project_root = project_root.to_path_buf();
        let language_id = config.language_id().to_string();

        let binary_path = match Self::locate_or_download(config) {
            Ok(p) => p,
            Err(e) => {
                eprintln!(
                    "[scope-engine/lsp] cannot locate {}: {e}",
                    config.server_name()
                );
                return Self {
                    inner: RefCell::new(LspClientInner {
                        process: None,
                        stdin_writer: None,
                        response_receiver: None,
                        reader_thread: None,
                        next_id: 0,
                        initialized: false,
                        language_id,
                    }),
                };
            }
        };

        match Self::spawn_and_initialize(&binary_path, &project_root, config) {
            Ok((process, stdin_w, response_receiver, reader_thread)) => Self {
                inner: RefCell::new(LspClientInner {
                    process: Some(process),
                    stdin_writer: Some(stdin_w),
                    response_receiver: Some(response_receiver),
                    reader_thread: Some(reader_thread),
                    next_id: 1,
                    initialized: true,
                    language_id,
                }),
            },
            Err(e) => {
                eprintln!(
                    "[scope-engine/lsp] failed to spawn/initialize {}: {e}",
                    config.server_name()
                );
                Self {
                    inner: RefCell::new(LspClientInner {
                        process: None,
                        stdin_writer: None,
                        response_receiver: None,
                        reader_thread: None,
                        next_id: 0,
                        initialized: false,
                        language_id,
                    }),
                }
            }
        }
    }

    // ── Binary location ─────────────────────────────────────────

    fn find_binary_on_path(binary_name: &str) -> Option<PathBuf> {
        let output = Command::new("which").arg(binary_name).output().ok()?;
        if !output.status.success() {
            return None;
        }
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        (!path.is_empty()).then(|| PathBuf::from(path))
    }

    fn find_go_binary(config: &dyn LspServerConfig) -> Option<PathBuf> {
        let candidate_dirs = [
            std::env::var("GOBIN").ok().map(PathBuf::from),
            std::env::var("GOPATH")
                .ok()
                .map(|go_path| PathBuf::from(go_path).join("bin")),
            Command::new("go")
                .args(["env", "GOPATH"])
                .output()
                .ok()
                .map(|output| {
                    PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()).join("bin")
                }),
            Command::new("go")
                .args(["env", "GOBIN"])
                .output()
                .ok()
                .and_then(|output| {
                    let directory = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    (!directory.is_empty()).then(|| PathBuf::from(directory))
                }),
        ];
        candidate_dirs.into_iter().flatten().find_map(|directory| {
            let candidate = directory.join(config.binary_name());
            candidate.is_file().then_some(candidate)
        })
    }

    fn install_server(config: &dyn LspServerConfig) -> Result<PathBuf, String> {
        let (command, arguments) = config.install_command().ok_or_else(|| {
            format!(
                "{} not found on PATH and no download URL or install command configured",
                config.server_name()
            )
        })?;
        eprintln!(
            "[scope-engine/lsp] attempting to install {} via: {} {}",
            config.server_name(),
            command,
            arguments.join(" ")
        );
        let output = Command::new(&command)
            .args(&arguments)
            .output()
            .map_err(|error| {
                format!(
                    "failed to run install command '{} {}': {error}",
                    command,
                    arguments.join(" ")
                )
            })?;
        if !output.status.success() {
            return Err(format!(
                "install command for {} failed: {}",
                config.server_name(),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        if let Some(path) = Self::find_binary_on_path(config.binary_name()) {
            return Ok(path);
        }
        if let Some(path) = Self::find_go_binary(config) {
            return Ok(path);
        }
        Err(format!(
            "{} was installed via '{}' but could not be found on PATH, GOPATH/bin, or GOBIN",
            config.server_name(),
            command
        ))
    }

    fn locate_or_download(config: &dyn LspServerConfig) -> Result<PathBuf, String> {
        if let Some(path) = Self::find_binary_on_path(config.binary_name()) {
            eprintln!(
                "[scope-engine/lsp] found {} on PATH: {}",
                config.server_name(),
                path.display()
            );
            return Ok(path);
        }

        let cache_dir = Self::cache_dir()?;
        let cached = cache_dir.join(config.cached_binary_name());
        if cached.is_file() {
            eprintln!(
                "[scope-engine/lsp] found cached {}: {}",
                config.server_name(),
                cached.display()
            );
            return Ok(cached);
        }

        config.download_url().map_or_else(
            || Self::install_server(config),
            |url| Self::download_binary(&cache_dir, &cached, &url, config.server_name()),
        )
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
    fn download_binary(
        cache_dir: &Path,
        target: &Path,
        url: &str,
        name: &str,
    ) -> Result<PathBuf, String> {
        eprintln!("[scope-engine/lsp] downloading {name}...");
        let tmp = cache_dir.join("download.tmp");
        let mut resp = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_mins(2))
            .build()
            .map_err(|e| format!("HTTP client build failed: {e}"))?
            .get(url)
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

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| format!("cannot chmod: {e}"))?;
        }

        eprintln!(
            "[scope-engine/lsp] downloaded {} to {}",
            name,
            target.display()
        );
        Ok(target.to_path_buf())
    }

    #[cfg(not(feature = "download-ra"))]
    fn download_binary(
        _cache_dir: &Path,
        _target: &Path,
        _url: &str,
        name: &str,
    ) -> Result<PathBuf, String> {
        Err(format!(
            "{name} download not available (feature 'download-ra' disabled)"
        ))
    }

    // ── Subprocess management ──────────────────────────────────

    fn spawn_and_initialize(
        binary_path: &Path,
        project_root: &Path,
        config: &dyn LspServerConfig,
    ) -> Result<SpawnedLsp, String> {
        let mut child = Command::new(binary_path)
            .args(config.spawn_args())
            .current_dir(project_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot spawn {}: {e}", config.server_name()))?;

        let stdin = child.stdin.take().ok_or("cannot take stdin")?;
        let stdout = child.stdout.take().ok_or("cannot take stdout")?;

        let mut writer = BufWriter(stdin);
        let mut reader = std::io::BufReader::new(stdout);
        let (response_sender, response_receiver) = mpsc::channel();
        let reader_thread = std::thread::spawn(move || {
            loop {
                let response = Self::read_message(&mut reader);
                let failed = response.is_err();
                if response_sender.send(response).is_err() || failed {
                    break;
                }
            }
        });

        // ── LSP initialize ─────────────────────────────────────
        let root_uri = path_to_file_uri(project_root);
        let mut init_params = serde_json::json!({
            "rootUri": root_uri,
            "capabilities": {},
        });
        // Merge extra params from config
        let extra = config.init_params_extra(&root_uri);
        if let serde_json::Value::Object(extra_map) = extra
            && let serde_json::Value::Object(ref mut params_map) = init_params
        {
            for (k, v) in extra_map {
                params_map.insert(k, v);
            }
        }

        let resp = Self::send_request_raw(
            &mut writer,
            &response_receiver,
            0,
            "initialize",
            &init_params,
            LSP_INITIALIZE_TIMEOUT,
        )
        .map_err(|e| {
            let _ = child.kill();
            format!("initialize failed: {e}")
        })?;

        if let Some(err) = resp.get("error") {
            let _ = child.kill();
            return Err(format!("initialize error: {err}"));
        }

        // ── initialized notification ────────────────────────────
        Self::send_notification(&mut writer, "initialized", &serde_json::json!({})).map_err(
            |e| {
                let _ = child.kill();
                format!("initialized notification failed: {e}")
            },
        )?;

        eprintln!(
            "[scope-engine/lsp] {} initialized for {}",
            config.server_name(),
            project_root.display()
        );
        std::thread::sleep(std::time::Duration::from_secs(
            config.post_init_delay_secs(),
        ));
        Ok((child, writer, response_receiver, reader_thread))
    }

    // ── LSP file synchronization ────────────────────────────────

    pub fn notify_did_open(&self, file_path: &Path, text: &str) {
        let mut inner = self.inner.borrow_mut();
        if !inner.initialized {
            return;
        }
        let uri = path_to_file_uri(file_path);
        let lang_id = inner.language_id.clone();
        let params = serde_json::json!({
            "textDocument": {
                "uri": uri,
                "languageId": lang_id,
                "version": 0,
                "text": text,
            }
        });
        if let Some(ref mut writer) = inner.stdin_writer {
            let _ = Self::send_notification(writer, "textDocument/didOpen", &params);
        }
    }

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
            let _ = Self::send_notification(writer, "textDocument/didChange", &params);
        }
    }

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
            let _ = Self::send_notification(writer, "textDocument/didClose", &params);
        }
    }

    // ── LSP requests ──────────────────────────────────────────

    fn request_references(
        inner: &mut LspClientInner,
        params: &serde_json::Value,
    ) -> Option<Vec<serde_json::Value>> {
        let (Some(writer), Some(response_receiver)) =
            (&mut inner.stdin_writer, &inner.response_receiver)
        else {
            return None;
        };
        let id = inner.next_id;
        inner.next_id += 1;
        let response = Self::send_request_raw(
            writer,
            response_receiver,
            id,
            "textDocument/references",
            params,
            LSP_REQUEST_TIMEOUT,
        )
        .map_err(|error| {
            eprintln!("[scope-engine/lsp] textDocument/references failed: {error}");
            error
        })
        .ok()?;
        if let Some(error) = response.get("error") {
            eprintln!("[scope-engine/lsp] textDocument/references error: {error}");
            return Some(Vec::new());
        }
        response
            .get("result")
            .and_then(serde_json::Value::as_array)
            .cloned()
    }

    fn reference_results_from_locations(
        locations: &[serde_json::Value],
        project_root: &Path,
    ) -> Vec<PropagationResult> {
        let analyzer = TreeSitterAnalyzer::new();
        locations
            .iter()
            .filter_map(|location| {
                Self::reference_result_from_location(&analyzer, location, project_root)
            })
            .collect()
    }

    fn reference_result_from_location(
        analyzer: &TreeSitterAnalyzer,
        location: &serde_json::Value,
        project_root: &Path,
    ) -> Option<PropagationResult> {
        let uri = location.get("uri")?.as_str()?;
        let line = location
            .get("range")?
            .get("start")?
            .get("line")?
            .as_u64()
            .and_then(|line| usize::try_from(line).ok())?;
        let path = uri_to_path(uri);
        if analyzer.is_import_only_reference(&path, line + 1) {
            return None;
        }
        let relative_path = path.strip_prefix(project_root).ok().map_or_else(
            || path.to_string_lossy().to_string(),
            |relative| relative.to_string_lossy().to_string(),
        );
        let context_line = std::fs::read_to_string(&path)
            .ok()
            .and_then(|content| content.lines().nth(line).map(str::to_string))
            .unwrap_or_default();
        let selector = analyzer
            .find_containing_symbol(&path, line + 1, project_root)
            .unwrap_or_else(|| format!("{relative_path}::line {}", line + 1));
        let reference = (selector.clone(), line + 1, context_line);
        Some(PropagationResult {
            selector,
            reason: format!("LSP reference found at {relative_path}:{}", line + 1),
            source: PropagationSource::Lsp,
            lsp_references: Some(vec![reference]),
            diff_summary: None,
            file_snippet: None,
            project_files: None,
        })
    }

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
            return Vec::new();
        }
        let params = serde_json::json!({
            "textDocument": { "uri": path_to_file_uri(file_path) },
            "position": { "line": line.saturating_sub(1), "character": character },
            "context": { "includeDeclaration": false }
        });
        let Some(locations) = Self::request_references(inner, &params) else {
            Self::disable(inner);
            return Vec::new();
        };
        let locations = if locations.is_empty() {
            eprintln!("[scope-engine/lsp] references returned empty, waiting and retrying...");
            std::thread::sleep(Duration::from_secs(2));
            Self::request_references(inner, &params).unwrap_or_default()
        } else {
            locations
        };
        eprintln!(
            "[scope-engine/lsp] found {} reference locations",
            locations.len()
        );
        Self::reference_results_from_locations(&locations, project_root)
    }

    // ── JSON-RPC transport ─────────────────────────────────────

    fn send_request_raw(
        writer: &mut BufWriter<ChildStdin>,
        response_receiver: &Receiver<LspResponse>,
        id: u64,
        method: &str,
        params: &serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value, String> {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        Self::write_message(writer, &request)?;
        Self::wait_for_response(response_receiver, id, timeout)
    }

    fn send_notification(
        writer: &mut BufWriter<ChildStdin>,
        method: &str,
        params: &serde_json::Value,
    ) -> Result<(), String> {
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        Self::write_message(writer, &notification)
    }

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

    fn read_message(
        reader: &mut std::io::BufReader<ChildStdout>,
    ) -> Result<serde_json::Value, String> {
        let mut header = String::new();
        loop {
            let mut byte = [0u8; 1];
            reader
                .read_exact(&mut byte)
                .map_err(|e| format!("read header byte failed: {e}"))?;
            header.push(byte[0] as char);
            if header.ends_with("\r\n\r\n") {
                break;
            }
            if header.len() > 4096 {
                return Err("header too long, possibly malformed LSP response".to_string());
            }
        }

        let content_length: usize = header
            .lines()
            .find_map(|line| {
                line.strip_prefix("Content-Length: ")
                    .and_then(|value| value.trim().parse().ok())
            })
            .ok_or("missing Content-Length header")?;

        let mut body = vec![0u8; content_length];
        reader
            .read_exact(&mut body)
            .map_err(|e| format!("read body failed: {e}"))?;
        serde_json::from_slice(&body).map_err(|e| format!("json parse failed: {e}"))
    }

    fn wait_for_response(
        response_receiver: &Receiver<LspResponse>,
        expected_id: u64,
        timeout: Duration,
    ) -> Result<serde_json::Value, String> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!(
                    "request {expected_id} timed out after {} ms",
                    timeout.as_millis()
                ));
            }

            match response_receiver.recv_timeout(remaining) {
                Ok(Ok(message)) => {
                    let is_response =
                        message.get("result").is_some() || message.get("error").is_some();
                    if message.get("id").and_then(serde_json::Value::as_u64) == Some(expected_id)
                        && is_response
                    {
                        return Ok(message);
                    }
                }
                Ok(Err(error)) => return Err(error),
                Err(RecvTimeoutError::Timeout) => {
                    return Err(format!(
                        "request {expected_id} timed out after {} ms",
                        timeout.as_millis()
                    ));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("LSP response channel disconnected".to_string());
                }
            }
        }
    }

    fn disable(inner: &mut LspClientInner) {
        inner.initialized = false;
        inner.stdin_writer = None;
        inner.response_receiver = None;
        inner.reader_thread = None;
        if let Some(mut child) = inner.process.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        let inner = self.inner.get_mut();
        if !inner.initialized {
            return;
        }
        if let (Some(writer), Some(response_receiver)) =
            (&mut inner.stdin_writer, &inner.response_receiver)
        {
            let _ = Self::send_request_raw(
                writer,
                response_receiver,
                inner.next_id,
                "shutdown",
                &serde_json::json!(null),
                LSP_SHUTDOWN_TIMEOUT,
            );
            let _ = Self::send_notification(writer, "exit", &serde_json::json!(null));
        }
        Self::disable(inner);
        eprintln!("[scope-engine/lsp] language server shut down");
    }
}

// ── URI helpers ──────────────────────────────────────────────

fn path_to_file_uri(path: &Path) -> String {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    file_uri_from_absolute_path_string(&abs.to_string_lossy())
}

fn file_uri_from_absolute_path_string(path: &str) -> String {
    let mut normalized = path.replace('\\', "/");

    if let Some(rest) = normalized.strip_prefix("//?/UNC/") {
        return unc_path_to_file_uri(rest);
    }
    if let Some(rest) = normalized.strip_prefix("//?/") {
        normalized = rest.to_string();
    } else if let Some(rest) = normalized.strip_prefix("//./") {
        normalized = rest.to_string();
    }

    if let Some(rest) = normalized.strip_prefix("//") {
        return unc_path_to_file_uri(rest);
    }

    if has_windows_drive_prefix(&normalized) {
        return format!("file:///{}", percent_encode_file_path(&normalized));
    }

    if normalized.starts_with('/') {
        return format!("file://{}", percent_encode_file_path(&normalized));
    }

    format!("file:///{}", percent_encode_file_path(&normalized))
}

fn unc_path_to_file_uri(path: &str) -> String {
    let (host, rest) = path.split_once('/').unwrap_or((path, ""));
    if rest.is_empty() {
        format!("file://{}", percent_encode_file_path(host))
    } else {
        format!(
            "file://{}/{}",
            percent_encode_file_path(host),
            percent_encode_file_path(rest)
        )
    }
}

fn has_windows_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn percent_encode_file_path(path: &str) -> String {
    let mut encoded = String::new();
    for &byte in path.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                encoded.push(byte as char);
            }
            _ => {
                write!(encoded, "%{byte:02X}").expect("writing to String cannot fail");
            }
        }
    }
    encoded
}

fn percent_decode_file_path(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2]))
        {
            decoded.push((hi << 4) | lo);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn uri_to_path(uri: &str) -> PathBuf {
    PathBuf::from(file_uri_to_path_string(uri))
}

fn file_uri_to_path_string(uri: &str) -> String {
    let Some(rest) = uri.strip_prefix("file://") else {
        return uri.to_string();
    };
    let decoded = percent_decode_file_path(rest);
    if let Some(stripped) = decoded.strip_prefix('/') {
        if has_windows_drive_prefix(stripped) {
            return stripped.to_string();
        }
        return decoded;
    }
    format!("//{decoded}")
}

// ── Analyzer trait impl ──────────────────────────────────────

impl Analyzer for LspClient {
    fn find_references_for_symbol(
        &self,
        file_path: &Path,
        line: usize,
        character: usize,
        project_root: &Path,
    ) -> Vec<PropagationResult> {
        Self::find_references_for_symbol(self, file_path, line, character, project_root)
    }

    fn notify_did_open(&self, file_path: &Path, text: &str) {
        Self::notify_did_open(self, file_path, text);
    }

    fn notify_did_change(&self, file_path: &Path, version: i32, text: &str) {
        Self::notify_did_change(self, file_path, version, text);
    }

    fn notify_did_close(&self, file_path: &Path) {
        Self::notify_did_close(self, file_path);
    }

    fn is_initialized(&self) -> bool {
        self.inner.borrow().initialized
    }
}

// ── Backward-compatible type alias ────────────────────────────

/// `LspAnalyzer` is a type alias for `LspClient`, preserving API compatibility.
pub type LspAnalyzer = LspClient;

#[cfg(test)]
mod tests {
    use super::{
        LspClient, LspResponse, file_uri_from_absolute_path_string, file_uri_to_path_string,
    };
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn windows_verbatim_paths_become_valid_file_uris() {
        let uri =
            file_uri_from_absolute_path_string(r"\\?\C:\Users\Name With Space\src\main#test.rs");

        assert_eq!(
            uri,
            "file:///C:/Users/Name%20With%20Space/src/main%23test.rs"
        );
    }

    #[test]
    fn unix_paths_become_valid_file_uris() {
        let uri = file_uri_from_absolute_path_string("/tmp/name with space/main#test.rs");

        assert_eq!(uri, "file:///tmp/name%20with%20space/main%23test.rs");
    }

    #[test]
    fn file_uris_decode_windows_drive_paths() {
        let path =
            file_uri_to_path_string("file:///C:/Users/Name%20With%20Space/src/main%23test.rs");

        assert_eq!(path, "C:/Users/Name With Space/src/main#test.rs");
    }

    #[test]
    fn response_wait_ignores_unrelated_messages() {
        let (sender, receiver) = mpsc::channel::<LspResponse>();
        sender
            .send(Ok(serde_json::json!({"method": "window/logMessage"})))
            .unwrap();
        sender
            .send(Ok(serde_json::json!({"id": 6, "result": []})))
            .unwrap();
        sender
            .send(Ok(serde_json::json!({"id": 7, "result": ["match"]})))
            .unwrap();

        let response = LspClient::wait_for_response(&receiver, 7, Duration::from_millis(50))
            .expect("matching response should be returned");

        assert_eq!(response["result"], serde_json::json!(["match"]));
    }

    #[test]
    fn response_wait_times_out() {
        let (_sender, receiver) = mpsc::channel::<LspResponse>();

        let error = LspClient::wait_for_response(&receiver, 9, Duration::from_millis(10))
            .expect_err("missing response should time out");

        assert!(error.contains("request 9 timed out"));
    }
}
