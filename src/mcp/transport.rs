use super::*;
use crate::{
    host_process::HostChild,
    sandbox::{SandboxProcessOptions, SandboxStdio},
    sudo_host::{BrokerChild, Client},
};
use rmcp::{
    RoleClient, ServiceExt,
    service::RunningService,
    transport::{
        StreamableHttpClientTransport, async_rw::AsyncRwTransport,
        streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

#[derive(Clone)]
pub struct Handler(pub Arc<AtomicBool>);
impl rmcp::ClientHandler for Handler {
    async fn on_tool_list_changed(&self, _: rmcp::service::NotificationContext<RoleClient>) {
        self.0.store(true, Ordering::Release);
    }
}
enum Process {
    User(HostChild),
    Administrator(BrokerChild),
}
struct ProcessGuard {
    process: Process,
    administrator: Option<Arc<Client>>,
}
impl ProcessGuard {
    fn spawn(
        administrator: Option<Arc<Client>>,
        command: &str,
        arguments: Vec<String>,
        options: SandboxProcessOptions,
    ) -> std::io::Result<Self> {
        let process = match &administrator {
            Some(client) => Process::Administrator(BrokerChild::spawn(
                client.clone(),
                command,
                arguments,
                options,
            )?),
            None => Process::User(HostChild::spawn(command, arguments, options)?),
        };
        Ok(Self {
            process,
            administrator,
        })
    }
    fn take_stdin(&mut self) -> Option<crate::sandbox::SandboxChildStdin> {
        match &mut self.process {
            Process::User(child) => child.child.stdin.take().map(|pipe| Box::new(pipe) as _),
            Process::Administrator(child) => child.take_stdin().map(|pipe| Box::new(pipe) as _),
        }
    }
    fn take_stdout(&mut self) -> Option<crate::sandbox::SandboxChildStdout> {
        match &mut self.process {
            Process::User(child) => child.child.stdout.take().map(|pipe| Box::new(pipe) as _),
            Process::Administrator(child) => child.take_stdout().map(|pipe| Box::new(pipe) as _),
        }
    }
    fn start_kill(&mut self) -> std::io::Result<()> {
        match &mut self.process {
            Process::User(child) => child.start_kill(),
            Process::Administrator(child) => child.start_kill(),
        }
    }
    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        match &mut self.process {
            Process::User(child) => child.try_wait(),
            Process::Administrator(child) => child.try_wait(),
        }
    }
}
impl Drop for ProcessGuard {
    fn drop(&mut self) {
        let _ = self.start_kill();
    }
}
#[cfg(windows)]
fn windows_command(
    command: &str,
    arguments: &[String],
    cwd: &std::path::Path,
    environment: &BTreeMap<String, String>,
) -> Result<(String, Vec<String>)> {
    let requested = std::path::Path::new(command);
    let candidates = if requested.is_absolute() {
        vec![requested.to_owned()]
    } else if requested.components().count() > 1 {
        vec![cwd.join(requested)]
    } else {
        let path = environment
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("PATH"))
            .map(|(_, value)| std::ffi::OsString::from(value))
            .or_else(|| std::env::var_os("PATH"))
            .unwrap_or_default();
        std::env::split_paths(&path)
            .filter(|directory| directory.is_absolute())
            .map(|directory| directory.join(requested))
            .collect()
    };
    let resolved = candidates
        .into_iter()
        .flat_map(|candidate| {
            if candidate.extension().is_some() {
                vec![candidate]
            } else {
                ["exe", "com", "cmd", "bat"]
                    .map(|extension| candidate.with_extension(extension))
                    .to_vec()
            }
        })
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| miette!("MCP executable was not found"))?;
    let resolved = dunce::canonicalize(resolved).into_diagnostic()?;
    if resolved
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"))
    {
        // Quote every value as a PowerShell literal; arguments are never shell code.
        let quote = |text: &str| format!("'{}'", text.replace('\'', "''"));
        let script = format!(
            "& {} {}; exit $LASTEXITCODE",
            quote(&resolved.to_string_lossy()),
            arguments
                .iter()
                .map(|value| quote(value))
                .collect::<Vec<_>>()
                .join(" ")
        );
        use base64::Engine;
        let bytes = script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        let shell =
            PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into()))
                .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        return Ok((
            shell.to_string_lossy().into_owned(),
            vec![
                "-NoLogo".into(),
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-EncodedCommand".into(),
                base64::engine::general_purpose::STANDARD.encode(bytes),
            ],
        ));
    }
    Ok((resolved.to_string_lossy().into_owned(), arguments.to_vec()))
}
pub struct Session {
    pub service: Option<RunningService<RoleClient, Handler>>,
    child: Option<ProcessGuard>,
    pub dirty: Arc<AtomicBool>,
}
impl Session {
    pub fn ensure_authority(&self, context: &AppToolExecutionContext) -> Result<()> {
        if let Some(child) = &self.child
            && child.administrator != context.sandbox_policy.administrator
        {
            return Err(miette!(
                "The MCP connection uses different administrator authority. Explicitly restart it to change authority; no tool call was replayed."
            ));
        }
        Ok(())
    }
    pub async fn connect(source: &Source, context: &AppToolExecutionContext) -> Result<Self> {
        source.validate()?;
        let dirty = Arc::new(AtomicBool::new(false));
        let handler = Handler(dirty.clone());
        let mut child_guard = None;
        let service = match source {
            Source::Http { url, bearer_token } => {
                let client = crate::http_client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .connect_timeout(Duration::from_secs(5))
                    .build()
                    .map_err(|_| miette!("MCP HTTP transport unavailable"))?;
                let mut config = StreamableHttpClientTransportConfig::with_uri(url.clone())
                    .max_sse_event_size(4 * 1024 * 1024)
                    .max_concurrent_requests(1)
                    .reinit_on_expired_session(false);
                if let Some(token) = bearer_token {
                    config = config.auth_header(token.clone());
                }
                let transport = StreamableHttpClientTransport::with_client(client, config);
                tokio::time::timeout(Duration::from_secs(20), handler.serve(transport))
                    .await
                    .map_err(|_| miette!("MCP handshake timed out"))?
                    .map_err(|_| {
                        miette!("MCP connection failed; check the address and authentication")
                    })?
            }
            Source::Stdio {
                command,
                arguments,
                environment,
            } => {
                #[cfg(windows)]
                let (command, arguments) =
                    windows_command(command, arguments, &context.execution_cwd, environment)?;
                let administrator = context.sandbox_policy.administrator.clone();
                let cwd = dunce::simplified(&context.execution_cwd).to_path_buf();
                let command = command.clone();
                let arguments = arguments.clone();
                let environment = environment
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                let cancelled = Arc::new(AtomicBool::new(false));
                struct CancelStartup(Arc<AtomicBool>);
                impl Drop for CancelStartup {
                    fn drop(&mut self) {
                        self.0.store(true, Ordering::Release);
                    }
                }
                let _cancel = CancelStartup(cancelled.clone());
                let child = tokio::time::timeout(
                    Duration::from_secs(30),
                    tokio::task::spawn_blocking(move || {
                        ProcessGuard::spawn(
                            administrator,
                            &command,
                            arguments,
                            SandboxProcessOptions {
                                environment,
                                current_dir: Some(cwd),
                                stdin: SandboxStdio::Piped,
                                stdout: SandboxStdio::Piped,
                                stderr: SandboxStdio::Null,
                                spawn_cancelled: Some(cancelled),
                                capture_denials: false,
                            },
                        )
                    }),
                )
                .await
                .map_err(|_| miette!("MCP process startup timed out"))?
                .into_diagnostic()?
                .map_err(|_| miette!("MCP process could not start"))?;
                let mut guard = Self {
                    service: None,
                    child: Some(child),
                    dirty: dirty.clone(),
                };
                let child = guard.child.as_mut().unwrap();
                let input = child
                    .take_stdin()
                    .ok_or_else(|| miette!("MCP stdin unavailable"))?;
                let output = child
                    .take_stdout()
                    .ok_or_else(|| miette!("MCP stdout unavailable"))?;
                let result = tokio::time::timeout(
                    Duration::from_secs(20),
                    handler.serve(AsyncRwTransport::new_client(output, input)),
                )
                .await;
                let result = match result {
                    Ok(Ok(service)) => service,
                    outcome => {
                        let message = if outcome.is_err() {
                            "MCP handshake timed out"
                        } else {
                            "MCP process handshake failed"
                        };
                        if let Some(child) = &mut guard.child {
                            let _ = child.start_kill();
                            let _ = tokio::time::timeout(Duration::from_secs(3), async {
                                while child.try_wait().ok().flatten().is_none() {
                                    tokio::time::sleep(Duration::from_millis(20)).await;
                                }
                            })
                            .await;
                        }
                        return Err(miette!("{message}"));
                    }
                };
                child_guard = guard.child.take();
                result
            }
        };
        Ok(Self {
            service: Some(service),
            child: child_guard,
            dirty,
        })
    }
    pub async fn tools(&mut self) -> Result<Vec<Tool>> {
        // Clear before discovery so a notification racing the query remains pending.
        self.dirty.store(false, Ordering::Release);
        let service = self
            .service
            .as_ref()
            .ok_or_else(|| miette!("MCP session closed"))?;
        let mut tools = Vec::new();
        let mut cursor = None;
        let mut cursors = HashSet::new();
        loop {
            let page = tokio::time::timeout(
                Duration::from_secs(20),
                service.list_tools(cursor.clone().map(|cursor| {
                    rmcp::model::PaginatedRequestParams::default().with_cursor(Some(cursor))
                })),
            )
            .await
            .map_err(|_| miette!("MCP tool discovery timed out"))?
            .map_err(|_| miette!("MCP tool discovery failed"))?;
            for tool in page.tools {
                let name = tool.name.into_owned();
                tools.push(Tool {
                    // 36-byte App id + separator + 11-byte tool + optional device
                    // prefix fit the 64-byte provider tool-name limit.
                    local_name: format!("t{}", &crate::stage::digest(name.as_bytes())[..10]),
                    name,
                    description: tool.description.map(|s| s.into_owned()).unwrap_or_default(),
                    input_schema: Value::Object((*tool.input_schema).clone()),
                });
            }
            cursor = page.next_cursor;
            let Some(next) = &cursor else { break };
            if !cursors.insert(next.clone()) {
                return Err(miette!("MCP tool discovery cursor repeated"));
            }
        }
        validate_tools(&tools)?;
        Ok(tools)
    }
    pub async fn close(&mut self) {
        if let Some(service) = self.service.take() {
            let _ = tokio::time::timeout(Duration::from_secs(3), service.cancel()).await;
        }
        self.child.take();
    }
}
