#[cfg(test)]
use std::thread::JoinHandle;
use std::{
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use miette::{Result, miette};
use uuid::Uuid;

use crate::sandbox::{
    FileSystemSandboxPolicy, RuntimeSandboxPolicy, StrongFilesystemSandboxMode, WritableRoot,
};
#[cfg(not(test))]
use crate::sandbox::{SandboxChild, SandboxProcessOptions, SandboxStdio};
use crate::{
    app::AppId,
    workspace_app::{
        WORKSPACE_APP_COLD_START_TIMEOUT, WORKSPACE_APP_REQUEST_TIMEOUT, WorkspaceAppConfigOutput,
        protocol::{
            WorkerHello, WorkerRequest, WorkerRequestOp, WorkerResponse, WorkerResponsePayload,
            WorkerResponseResult,
        },
        worker::WorkspaceAppWorkerArgs,
    },
};

const WORKSPACE_APP_WORKER_START_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub(super) struct WorkspaceAppWorkerClient {
    app_id: AppId,
    app_dir: PathBuf,
    state_dir: PathBuf,
    entry_relative_path: String,
    request_timeout: Duration,
    cold_start_timeout: Duration,
    protected_env_vars: Vec<String>,
    strong_filesystem: StrongFilesystemSandboxMode,
    next_request_id: u64,
    handle: Option<WorkspaceAppWorkerHandle>,
    reader: Option<BufReader<TcpStream>>,
    writer: Option<TcpStream>,
}

#[derive(Debug)]
enum WorkspaceAppWorkerHandle {
    #[cfg(not(test))]
    Process(SandboxChild),
    #[cfg(test)]
    Thread(JoinHandle<()>),
}

impl WorkspaceAppWorkerClient {
    pub(super) fn start(
        app_id: AppId,
        app_dir: PathBuf,
        state_dir: PathBuf,
        entry_relative_path: String,
        protected_env_vars: Vec<String>,
        strong_filesystem: StrongFilesystemSandboxMode,
    ) -> Result<Self> {
        let mut client = Self {
            app_id,
            app_dir,
            state_dir,
            entry_relative_path,
            request_timeout: WORKSPACE_APP_REQUEST_TIMEOUT,
            cold_start_timeout: WORKSPACE_APP_COLD_START_TIMEOUT,
            protected_env_vars,
            strong_filesystem,
            next_request_id: 1,
            handle: None,
            reader: None,
            writer: None,
        };
        client.ensure_started()?;
        Ok(client)
    }

    pub(super) fn request(&mut self, op: WorkerRequestOp) -> Result<WorkerResponsePayload> {
        self.ensure_started()?;
        self.send_request(op, self.request_timeout)
    }

    fn send_request(
        &mut self,
        op: WorkerRequestOp,
        timeout: Duration,
    ) -> Result<WorkerResponsePayload> {
        self.configure_connection_timeouts(timeout)?;
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.saturating_add(1).max(1);
        let request = WorkerRequest { id, op };
        let mut request_line = serde_json::to_vec(&request).map_err(|err| {
            miette!(
                "failed to encode request for workspace app `{}` worker: {err}",
                self.app_id
            )
        })?;
        request_line.push(b'\n');
        let write_result = {
            let writer = self
                .writer
                .as_mut()
                .ok_or_else(|| miette!("workspace app `{}` worker writer missing", self.app_id))?;
            writer
                .write_all(&request_line)
                .and_then(|()| writer.flush())
        };
        if let Err(err) = write_result {
            self.terminate();
            return Err(miette!(
                "failed to write request to workspace app `{}` worker: {err}",
                self.app_id
            ));
        }

        let reader = self
            .reader
            .as_mut()
            .ok_or_else(|| miette!("workspace app `{}` worker reader missing", self.app_id))?;
        let mut line = String::new();
        let bytes = reader.read_line(&mut line).map_err(|err| {
            self.terminate();
            miette!(
                "workspace app `{}` worker did not respond to request `{id}`: {err}",
                self.app_id
            )
        })?;
        if bytes == 0 {
            self.terminate();
            return Err(miette!(
                "workspace app `{}` worker exited before responding to request `{id}`",
                self.app_id
            ));
        }
        let response = serde_json::from_str::<WorkerResponse>(&line).map_err(|err| {
            self.terminate();
            miette!(
                "workspace app `{}` worker returned invalid response to request `{id}`: {err}",
                self.app_id
            )
        })?;
        if response.id != id {
            self.terminate();
            return Err(miette!(
                "workspace app `{}` worker returned response id {} for request `{id}`",
                self.app_id,
                response.id
            ));
        }
        match response.result {
            WorkerResponseResult::Ok { payload } => Ok(payload),
            WorkerResponseResult::Err { message } => Err(miette!("{message}")),
        }
    }

    pub(super) fn shutdown(&mut self) {
        if self.writer.is_some() && self.reader.is_some() {
            let _ = self.request(WorkerRequestOp::Shutdown);
        }
        self.terminate();
    }

    #[cfg(test)]
    pub(super) fn set_request_timeout_for_tests(&mut self, timeout: Duration) {
        self.request_timeout = timeout.max(Duration::from_millis(1));
        self.terminate();
    }

    #[cfg(test)]
    pub(super) fn restart_for_tests(&mut self) {
        self.terminate();
    }

    fn ensure_started(&mut self) -> Result<()> {
        if let Some(handle) = self.handle.as_mut()
            && handle.is_running(&self.app_id)?
            && self.reader.is_some()
            && self.writer.is_some()
        {
            return Ok(());
        }
        self.terminate();
        self.spawn_worker()
    }

    fn spawn_worker(&mut self) -> Result<()> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|err| {
            miette!(
                "failed to bind workspace app `{}` worker IPC listener: {err}",
                self.app_id
            )
        })?;
        listener.set_nonblocking(true).map_err(|err| {
            miette!(
                "failed to configure workspace app `{}` worker IPC listener: {err}",
                self.app_id
            )
        })?;
        let addr = listener
            .local_addr()
            .map_err(|err| miette!("failed to inspect workspace app worker listener: {err}"))?;
        let token = Uuid::new_v4().to_string();
        std::fs::create_dir_all(&self.state_dir).map_err(|err| {
            miette!(
                "failed to create workspace app `{}` state directory {}: {err}",
                self.app_id,
                self.state_dir.display()
            )
        })?;
        let mut handle = spawn_worker_handle(
            WorkspaceAppWorkerArgs {
                app_id: self.app_id.to_string(),
                app_dir: self.app_dir.clone(),
                state_dir: self.state_dir.clone(),
                entry: self.entry_relative_path.clone(),
                connect_addr: addr.to_string(),
                token: token.clone(),
            },
            &self.app_id,
            &self.protected_env_vars,
            self.strong_filesystem,
        )?;

        let stream = match accept_worker_connection(&listener, &mut handle, &self.app_id) {
            Ok(stream) => stream,
            Err(err) => {
                handle.terminate_and_wait();
                return Err(err);
            }
        };
        if let Err(err) = stream.set_nonblocking(false) {
            handle.terminate_and_wait();
            return Err(miette!(
                "failed to configure workspace app worker stream: {err}"
            ));
        }
        if let Err(err) = stream.set_read_timeout(Some(WORKSPACE_APP_WORKER_START_TIMEOUT)) {
            handle.terminate_and_wait();
            return Err(miette!(
                "failed to configure workspace app worker hello timeout: {err}"
            ));
        }
        let writer = match stream.try_clone() {
            Ok(writer) => writer,
            Err(err) => {
                handle.terminate_and_wait();
                return Err(miette!(
                    "failed to clone workspace app worker IPC stream: {err}"
                ));
            }
        };
        let mut reader = BufReader::new(stream);
        let mut hello_line = String::new();
        if let Err(err) = reader.read_line(&mut hello_line) {
            handle.terminate_and_wait();
            return Err(miette!(
                "failed to read workspace app `{}` worker hello message: {err}",
                self.app_id
            ));
        }
        let hello = match serde_json::from_str::<WorkerHello>(&hello_line) {
            Ok(hello) => hello,
            Err(err) => {
                handle.terminate_and_wait();
                return Err(miette!(
                    "workspace app `{}` worker returned invalid hello message: {err}",
                    self.app_id
                ));
            }
        };
        if hello.token != token || hello.app_id != self.app_id.as_str() {
            handle.terminate_and_wait();
            return Err(miette!(
                "workspace app `{}` worker failed IPC authentication",
                self.app_id
            ));
        }
        self.handle = Some(handle);
        self.reader = Some(reader);
        self.writer = Some(writer);
        let config = match self.send_request(WorkerRequestOp::Configure, self.cold_start_timeout) {
            Ok(WorkerResponsePayload::Config(config)) => config,
            Ok(other) => {
                self.terminate();
                return Err(miette!(
                    "workspace app `{}` worker returned unexpected config payload: {other:?}",
                    self.app_id
                ));
            }
            Err(err) => {
                self.terminate();
                return Err(err);
            }
        };
        self.apply_config(config);
        if let Err(err) = self.send_request(WorkerRequestOp::Initialize, self.cold_start_timeout) {
            self.terminate();
            return Err(err);
        }
        Ok(())
    }

    fn apply_config(&mut self, config: WorkspaceAppConfigOutput) {
        if let Some(timeout_ms) = config.request_timeout_ms {
            self.request_timeout = Duration::from_millis(timeout_ms.max(1));
        }
        if let Some(timeout_ms) = config.cold_start_timeout_ms {
            self.cold_start_timeout = Duration::from_millis(timeout_ms.max(1));
        }
    }

    fn configure_connection_timeouts(&mut self, timeout: Duration) -> Result<()> {
        if let Some(reader) = self.reader.as_mut() {
            reader
                .get_mut()
                .set_read_timeout(Some(timeout))
                .map_err(|err| {
                    miette!("failed to configure workspace app worker read timeout: {err}")
                })?;
        }
        if let Some(writer) = self.writer.as_mut() {
            writer.set_write_timeout(Some(timeout)).map_err(|err| {
                miette!("failed to configure workspace app worker write timeout: {err}")
            })?;
        }
        Ok(())
    }

    fn terminate(&mut self) {
        self.reader = None;
        self.writer = None;
        if let Some(handle) = self.handle.take() {
            handle.terminate_and_wait();
        }
    }
}

impl Drop for WorkspaceAppWorkerClient {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn accept_worker_connection(
    listener: &TcpListener,
    handle: &mut WorkspaceAppWorkerHandle,
    app_id: &AppId,
) -> Result<TcpStream> {
    let deadline = Instant::now() + WORKSPACE_APP_WORKER_START_TIMEOUT;
    loop {
        match listener.accept() {
            Ok((stream, _addr)) => return Ok(stream),
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(err) => {
                handle.terminate();
                return Err(miette!(
                    "workspace app `{app_id}` worker IPC accept failed: {err}"
                ));
            }
        }
        if let Some(status) = handle.exit_status(app_id)? {
            return Err(miette!(
                "workspace app `{app_id}` worker exited during startup with {status}"
            ));
        }
        if Instant::now() >= deadline {
            handle.terminate();
            return Err(miette!(
                "workspace app `{app_id}` worker did not connect within {} ms",
                WORKSPACE_APP_WORKER_START_TIMEOUT.as_millis()
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

impl WorkspaceAppWorkerHandle {
    fn is_running(&mut self, app_id: &AppId) -> Result<bool> {
        Ok(self.exit_status(app_id)?.is_none())
    }

    fn exit_status(&mut self, _app_id: &AppId) -> Result<Option<String>> {
        match self {
            #[cfg(not(test))]
            Self::Process(child) => Ok(child
                .try_wait()
                .map_err(|err| {
                    miette!("failed to inspect workspace app `{_app_id}` worker process: {err}")
                })?
                .map(|status| format!("status {status}"))),
            #[cfg(test)]
            Self::Thread(handle) => {
                if handle.is_finished() {
                    Ok(Some("worker thread exit".to_string()))
                } else {
                    Ok(None)
                }
            }
        }
    }

    fn terminate(&mut self) {
        match self {
            #[cfg(not(test))]
            Self::Process(child) => {
                if child.try_wait().ok().flatten().is_none() {
                    let _ = child.kill();
                }
            }
            #[cfg(test)]
            Self::Thread(_) => {}
        }
    }

    fn terminate_and_wait(mut self) {
        self.terminate();
        match self {
            #[cfg(not(test))]
            Self::Process(mut child) => {
                let _ = child.wait();
            }
            #[cfg(test)]
            Self::Thread(handle) => {
                if handle.is_finished() {
                    let _ = handle.join();
                }
            }
        }
    }
}

#[cfg(not(test))]
fn spawn_worker_handle(
    args: WorkspaceAppWorkerArgs,
    app_id: &AppId,
    protected_env_vars: &[String],
    strong_filesystem: StrongFilesystemSandboxMode,
) -> Result<WorkspaceAppWorkerHandle> {
    let executable = std::env::current_exe()
        .map_err(|err| miette!("failed to locate current executable for app worker: {err}"))?;
    let worker_policy =
        workspace_app_worker_sandbox_policy(&args, protected_env_vars, strong_filesystem);
    let spawn_args = workspace_worker_command_args(&args);
    let child = SandboxChild::spawn_strong(
        &worker_policy,
        executable,
        spawn_args,
        SandboxProcessOptions {
            stdin: SandboxStdio::Null,
            stdout: SandboxStdio::Null,
            stderr: SandboxStdio::Inherit,
            ..Default::default()
        },
    )
    .map_err(|err| {
        miette!(
            "failed to spawn workspace app `{}` worker process: {err}",
            app_id
        )
    })?;
    Ok(WorkspaceAppWorkerHandle::Process(child))
}

fn workspace_worker_command_args(args: &WorkspaceAppWorkerArgs) -> Vec<String> {
    vec![
        "workspace-app-worker".to_string(),
        "--app-id".to_string(),
        args.app_id.clone(),
        "--app-dir".to_string(),
        args.app_dir.display().to_string(),
        "--state-dir".to_string(),
        args.state_dir.display().to_string(),
        "--entry".to_string(),
        args.entry.clone(),
        "--connect-addr".to_string(),
        args.connect_addr.clone(),
        "--token".to_string(),
        args.token.clone(),
    ]
}

fn workspace_app_worker_sandbox_policy(
    args: &WorkspaceAppWorkerArgs,
    protected_env_vars: &[String],
    strong_filesystem: StrongFilesystemSandboxMode,
) -> RuntimeSandboxPolicy {
    let deny_read_paths = protected_runtime_read_paths_for_worker(&args.state_dir);
    RuntimeSandboxPolicy {
        filesystem: FileSystemSandboxPolicy {
            full_disk_read: true,
            full_disk_write: false,
            readable_roots: vec![args.app_dir.clone()],
            writable_roots: vec![WritableRoot {
                root: args.state_dir.clone(),
                read_only_subpaths: Vec::new(),
            }],
            deny_read_paths,
            deny_write_paths: vec![args.app_dir.clone()],
        },
        protected_env_vars: protected_env_vars.to_vec(),
        strong_filesystem,
    }
}

fn protected_runtime_read_paths_for_worker(state_dir: &Path) -> Vec<PathBuf> {
    let Some(apps_dir) = state_dir.parent() else {
        return Vec::new();
    };
    let Some(state_root) = apps_dir.parent() else {
        return Vec::new();
    };
    let Some(runtime_root) = state_root.parent() else {
        return Vec::new();
    };
    [
        "config",
        "memory",
        "runtime",
        "cache",
        "artifacts",
        "journals",
        "logs",
    ]
    .into_iter()
    .map(|name| runtime_root.join(name))
    .collect()
}

#[cfg(test)]
fn spawn_worker_handle(
    args: WorkspaceAppWorkerArgs,
    app_id: &AppId,
    _protected_env_vars: &[String],
    _strong_filesystem: StrongFilesystemSandboxMode,
) -> Result<WorkspaceAppWorkerHandle> {
    let app_id_for_log = app_id.clone();
    let handle = std::thread::Builder::new()
        .name(format!("workspace-app-worker-{app_id_for_log}"))
        .spawn(move || {
            if let Err(err) = crate::workspace_app::worker::run_workspace_app_worker(args) {
                eprintln!("{err:?}");
            }
        })
        .map_err(|err| {
            miette!(
                "failed to spawn workspace app `{}` worker thread: {err}",
                app_id
            )
        })?;
    Ok(WorkspaceAppWorkerHandle::Thread(handle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_sandbox_policy_allows_only_app_state_writes() {
        let root = PathBuf::from("/home/user/.daat-locus");
        let args = WorkspaceAppWorkerArgs {
            app_id: "sample".to_string(),
            app_dir: PathBuf::from("/home/user/daat-locus-workspace/apps/sample"),
            state_dir: root.join("state/apps/sample"),
            entry: "runtime/app.lua".to_string(),
            connect_addr: "127.0.0.1:12345".to_string(),
            token: "token".to_string(),
        };

        let policy = workspace_app_worker_sandbox_policy(
            &args,
            &["APP_SECRET".to_string()],
            StrongFilesystemSandboxMode::Required,
        );

        assert_eq!(
            policy.strong_filesystem,
            StrongFilesystemSandboxMode::Required
        );
        assert!(policy.is_path_writable(&args.state_dir.join("state.json")));
        assert!(policy.is_path_readable(&args.app_dir.join("runtime/app.lua")));
        assert!(!policy.is_path_writable(&args.app_dir.join("app.toml")));
        assert!(!policy.is_path_readable(&root.join("config/config.toml")));
        assert!(policy.is_env_var_protected("APP_SECRET"));
    }

    #[test]
    fn worker_command_args_keep_worker_ipc_arguments_explicit() {
        let args = WorkspaceAppWorkerArgs {
            app_id: "sample".to_string(),
            app_dir: PathBuf::from("/apps/sample"),
            state_dir: PathBuf::from("/state/sample"),
            entry: "runtime/app.lua".to_string(),
            connect_addr: "127.0.0.1:12345".to_string(),
            token: "token".to_string(),
        };

        let command_args = workspace_worker_command_args(&args);

        assert_eq!(
            command_args.first().map(String::as_str),
            Some("workspace-app-worker")
        );
        assert!(
            command_args
                .windows(2)
                .any(|item| item == ["--app-id", "sample"])
        );
        assert!(
            command_args
                .windows(2)
                .any(|item| item == ["--token", "token"])
        );
    }
}
