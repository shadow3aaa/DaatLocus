//! OS elevation only brokers native filesystem/process operations. The account
//! service, model execution and App infrastructure stay under the ordinary user.
use crate::{
    host_process::HostChild,
    sandbox::{SandboxProcessOptions, SandboxStdio},
};
use miette::{IntoDiagnostic, Result, miette};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[cfg(test)]
pub(crate) mod tests;
mod transport;
const MAX_FRAME: usize = 16 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Ping,
    File {
        name: String,
        cwd: PathBuf,
        arguments: Value,
    },
    Spawn {
        program: String,
        arguments: Vec<String>,
        environment: Vec<(String, String)>,
        cwd: Option<PathBuf>,
        stdin: bool,
    },
    Poll {
        process: u64,
    },
    Write {
        process: u64,
        bytes: Vec<u8>,
        close: bool,
    },
    Kill {
        process: u64,
    },
}
#[derive(Serialize, Deserialize)]
struct Response {
    value: Option<Value>,
    error: Option<String>,
}
#[derive(Serialize, Deserialize)]
struct RequestEnvelope {
    id: u64,
    request: Request,
}
#[derive(Serialize, Deserialize)]
struct ResponseEnvelope {
    id: u64,
    response: Response,
}

struct Command {
    request: Request,
    reply: std::sync::mpsc::Sender<std::result::Result<Value, String>>,
}

#[derive(Debug)]
pub struct Client {
    id: uuid::Uuid,
    commands: tokio::sync::mpsc::Sender<Command>,
    closed: Arc<AtomicBool>,
    closing: Arc<tokio::sync::Notify>,
}
impl PartialEq for Client {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for Client {}
impl Client {
    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.closing.notify_one();
    }
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
    fn from_stream<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(stream: S) -> Arc<Self> {
        let (sender, mut commands) = tokio::sync::mpsc::channel::<Command>(1024);
        let closed = Arc::new(AtomicBool::new(false));
        let ended = closed.clone();
        let closing = Arc::new(tokio::sync::Notify::new());
        let shutdown = closing.clone();
        std::thread::spawn(move || {
            if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                runtime.block_on(async move {
                    let (mut reader,mut writer)=tokio::io::split(stream);
                    let (responses,mut replies)=tokio::sync::mpsc::channel(32);
                    let reading=tokio::spawn(async move {
                        loop {
                            let response=read_frame::<_,ResponseEnvelope>(&mut reader).await;
                            let failed=response.is_err();
                            if responses.send(response).await.is_err() || failed {break;}
                        }
                    });
                    let mut pending=HashMap::<u64,std::sync::mpsc::Sender<std::result::Result<Value,String>>>::new();
                    let mut next=0u64;
                    let mut heartbeat=tokio::time::interval_at(tokio::time::Instant::now() + Duration::from_secs(5),Duration::from_secs(5));
                    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    let mut probe: Option<(u64,std::time::Instant)> = None;
                    loop {
                        tokio::select! {
                            ()=shutdown.notified()=>break,
                            command=commands.recv()=> {
                                let Some(command)=command else {break;};
                                if ended.load(Ordering::Acquire) {break;}
                                if pending.len()>=1024 {let _=command.reply.send(Err("Elevated host is busy".into()));continue;}
                                let Some(id)=next.checked_add(1) else {break;}; next=id;
                                pending.insert(id,command.reply);
                                let request=RequestEnvelope {id,request:command.request};
                                if !matches!(tokio::time::timeout(Duration::from_secs(5),write_frame(&mut writer,&request)).await,Ok(Ok(()))) {break;}
                            },
                            response=replies.recv()=> {
                                let Some(Ok(response))=response else {break;};
                                if probe.as_ref().is_some_and(|(id,_)| *id == response.id) {
                                    probe = None; continue;
                                }
                                let Some(reply)=pending.remove(&response.id) else {break;};
                                let response=response.response;
                                let _=reply.send(response.error.map_or_else(||Ok(response.value.unwrap_or(Value::Null)),Err));
                            },
                            _=heartbeat.tick()=> {
                                if let Some((_,started)) = &probe {
                                    if started.elapsed()>Duration::from_secs(15) {break;}
                                } else {
                                    let Some(id)=next.checked_add(1) else {break;}; next=id;
                                    probe = Some((id,std::time::Instant::now()));
                                    let request=RequestEnvelope {id,request:Request::Ping};
                                    if !matches!(tokio::time::timeout(Duration::from_secs(5),write_frame(&mut writer,&request)).await,Ok(Ok(()))) {break;}
                                }
                            }
                        }
                    }
                    ended.store(true,Ordering::Release);
                    for (_,reply) in pending {let _=reply.send(Err("Elevated host disconnected; effects are uncertain, do not replay".into()));}
                    reading.abort();let _=reading.await;
                });
            } else {
                ended.store(true, Ordering::Release);
            }
        });
        Arc::new(Self {
            id: uuid::Uuid::new_v4(),
            commands: sender,
            closed,
            closing,
        })
    }
    fn request(&self, request: Request) -> Result<Value> {
        if self.is_closed() {
            return Err(miette!("Elevated host is disconnected"));
        }
        let (reply, result) = std::sync::mpsc::channel();
        self.commands
            .try_send(Command { request, reply })
            .map_err(|_| miette!("Elevated host is busy or disconnected"))?;
        match result.recv() {
            Ok(result) => result.map_err(|error| miette!("{error}")),
            Err(_) => {
                self.close();
                Err(miette!(
                    "Elevated operation outcome is unknown; do not replay"
                ))
            }
        }
    }
    pub(crate) async fn file<T: serde::de::DeserializeOwned>(
        self: &Arc<Self>,
        name: &str,
        cwd: &Path,
        arguments: Value,
    ) -> Result<T> {
        let client = self.clone();
        let request = Request::File {
            name: name.into(),
            cwd: cwd.into(),
            arguments,
        };
        let value = tokio::task::spawn_blocking(move || client.request(request))
            .await
            .into_diagnostic()??;
        serde_json::from_value(value).into_diagnostic()
    }
}

pub(crate) async fn elevate() -> Result<Arc<Client>> {
    let stream = transport::launch().await?;
    Ok(Client::from_stream(stream))
}

pub async fn run(endpoint: &str, parent_pid: u32) -> Result<()> {
    if !transport::is_administrator()? {
        return Err(miette!("Sudo host requires OS administrator authorization"));
    }
    let stream = transport::connect(endpoint, parent_pid).await?;
    serve(stream).await
}

pub(crate) struct BrokerChild {
    client: Arc<Client>,
    process: u64,
    pid: Option<u32>,
    stdin: Option<tokio::io::DuplexStream>,
    stdout: Option<tokio::io::DuplexStream>,
    stderr: Option<tokio::io::DuplexStream>,
    exit: Arc<std::sync::Mutex<Option<std::process::ExitStatus>>>,
    killing: bool,
    kill_complete: Arc<AtomicBool>,
    failure: Arc<std::sync::Mutex<Option<String>>>,
    tasks: Vec<tokio::task::AbortHandle>,
}
fn exit_status(code: i32) -> std::process::ExitStatus {
    #[cfg(windows)]
    {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(code as u32)
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(code << 8)
    }
}
impl BrokerChild {
    pub(crate) fn spawn(
        client: Arc<Client>,
        program: &str,
        arguments: Vec<String>,
        options: SandboxProcessOptions,
    ) -> io::Result<Self> {
        if options
            .spawn_cancelled
            .as_ref()
            .is_some_and(|cancel| cancel.load(Ordering::Acquire))
        {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Process startup cancelled",
            ));
        }
        let value = client
            .request(Request::Spawn {
                program: program.into(),
                arguments,
                environment: options.environment,
                cwd: options.current_dir,
                stdin: options.stdin == SandboxStdio::Piped,
            })
            .map_err(io::Error::other)?;
        let process = value["process"]
            .as_u64()
            .ok_or_else(|| io::Error::other("Invalid elevated process response"))?;
        let pid = value["pid"]
            .as_u64()
            .and_then(|pid| u32::try_from(pid).ok());
        if options
            .spawn_cancelled
            .as_ref()
            .is_some_and(|cancel| cancel.load(Ordering::Acquire))
        {
            client
                .request(Request::Kill { process })
                .map_err(io::Error::other)?;
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Process startup cancelled",
            ));
        }
        let (stdin, mut input) = tokio::io::duplex(64 * 1024);
        let (stdout, mut output) = tokio::io::duplex(64 * 1024);
        let (stderr, mut errors) = tokio::io::duplex(64 * 1024);
        let exit = Arc::new(std::sync::Mutex::new(None));
        let finished = exit.clone();
        let polling = client.clone();
        let failure = Arc::new(std::sync::Mutex::new(None));
        let poll_failure = failure.clone();
        let mut tasks = Vec::new();
        tasks.push(
            tokio::spawn(async move {
                let mut code = 1;
                loop {
                    let client = polling.clone();
                    let response = tokio::task::spawn_blocking(move || {
                        client.request(Request::Poll { process })
                    })
                    .await;
                    let value = match response {
                        Ok(Ok(value)) => value,
                        _ => {
                            *poll_failure
                                .lock()
                                .unwrap_or_else(|error| error.into_inner()) = Some(
                                "Elevated process connection failed; effects are uncertain".into(),
                            );
                            break;
                        }
                    };
                    let (Ok(out), Ok(err)) = (
                        serde_json::from_value::<Vec<u8>>(value["stdout"].clone()),
                        serde_json::from_value::<Vec<u8>>(value["stderr"].clone()),
                    ) else {
                        *poll_failure
                            .lock()
                            .unwrap_or_else(|error| error.into_inner()) =
                            Some("Invalid elevated process output".into());
                        polling.close();
                        break;
                    };
                    if options.stdout == SandboxStdio::Piped
                        && output.write_all(&out).await.is_err()
                    {
                        break;
                    }
                    if options.stderr == SandboxStdio::Piped
                        && errors.write_all(&err).await.is_err()
                    {
                        break;
                    }
                    if value["drained"] == true
                        && let Some(exit) = value["exit_code"].as_i64()
                    {
                        code = exit as i32;
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                let _ = output.shutdown().await;
                let _ = errors.shutdown().await;
                *finished.lock().unwrap_or_else(|error| error.into_inner()) =
                    Some(exit_status(code));
            })
            .abort_handle(),
        );
        if options.stdin == SandboxStdio::Piped {
            let writing = client.clone();
            tasks.push(
                tokio::spawn(async move {
                    let mut buffer = vec![0; 8192];
                    while let Ok(count) = input.read(&mut buffer).await {
                        let bytes = buffer[..count].to_vec();
                        let client = writing.clone();
                        let result = tokio::task::spawn_blocking(move || {
                            client.request(Request::Write {
                                process,
                                bytes,
                                close: count == 0,
                            })
                        })
                        .await;
                        if count == 0 || !matches!(result, Ok(Ok(_))) {
                            break;
                        }
                    }
                })
                .abort_handle(),
            );
        }
        Ok(Self {
            client,
            process,
            pid,
            stdin: (options.stdin == SandboxStdio::Piped).then_some(stdin),
            stdout: (options.stdout == SandboxStdio::Piped).then_some(stdout),
            stderr: (options.stderr == SandboxStdio::Piped).then_some(stderr),
            exit,
            killing: false,
            kill_complete: Arc::new(AtomicBool::new(false)),
            failure,
            tasks,
        })
    }
    pub(crate) fn id(&self) -> Option<u32> {
        self.pid
    }
    pub(crate) fn take_stdin(&mut self) -> Option<tokio::io::DuplexStream> {
        self.stdin.take()
    }
    pub(crate) fn take_stdout(&mut self) -> Option<tokio::io::DuplexStream> {
        self.stdout.take()
    }
    pub(crate) fn take_stderr(&mut self) -> Option<tokio::io::DuplexStream> {
        self.stderr.take()
    }
    pub(crate) fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        if self.killing && !self.kill_complete.load(Ordering::Acquire) {
            return Ok(None);
        }
        if let Some(error) = self
            .failure
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
        {
            return Err(io::Error::other(error.clone()));
        }
        Ok(*self.exit.lock().unwrap_or_else(|error| error.into_inner()))
    }
    pub(crate) fn start_kill(&mut self) -> io::Result<()> {
        if !self.killing {
            let (reply, acknowledged) = std::sync::mpsc::channel();
            self.client
                .commands
                .try_send(Command {
                    request: Request::Kill {
                        process: self.process,
                    },
                    reply,
                })
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "Elevated host disconnected")
                })?;
            self.killing = true;
            let complete = self.kill_complete.clone();
            let failure = self.failure.clone();
            let exit = self.exit.clone();
            let wait = move || {
                match acknowledged.recv_timeout(Duration::from_secs(35)) {
                    Ok(Ok(_)) => {
                        *failure.lock().unwrap_or_else(|error| error.into_inner()) = None;
                        *exit.lock().unwrap_or_else(|error| error.into_inner()) =
                            Some(exit_status(1));
                    }
                    _ => {
                        *failure.lock().unwrap_or_else(|error| error.into_inner()) =
                            Some("Elevated process cleanup was not acknowledged".into())
                    }
                }
                complete.store(true, Ordering::Release);
            };
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn_blocking(wait);
            } else {
                std::thread::spawn(wait);
            }
        }
        Ok(())
    }
}
impl Drop for BrokerChild {
    fn drop(&mut self) {
        let _ = self.start_kill();
        for task in &self.tasks {
            task.abort();
        }
    }
}

async fn write_frame<S: AsyncWrite + Unpin, T: Serialize>(stream: &mut S, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value).into_diagnostic()?;
    if bytes.len() > MAX_FRAME {
        return Err(miette!("Elevated native frame exceeds its limit"));
    }
    stream
        .write_u32(bytes.len() as u32)
        .await
        .into_diagnostic()?;
    stream.write_all(&bytes).await.into_diagnostic()?;
    stream.flush().await.into_diagnostic()
}
async fn read_frame<S: AsyncRead + Unpin, T: serde::de::DeserializeOwned>(
    stream: &mut S,
) -> Result<T> {
    let size = stream.read_u32().await.into_diagnostic()? as usize;
    if size == 0 || size > MAX_FRAME {
        return Err(miette!("Invalid elevated native frame"));
    }
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes).await.into_diagnostic()?;
    serde_json::from_slice(&bytes).into_diagnostic()
}

struct Process {
    child: HostChild,
    stdin: Option<tokio::process::ChildStdin>,
    stdout: tokio::sync::mpsc::Receiver<Vec<u8>>,
    stderr: tokio::sync::mpsc::Receiver<Vec<u8>>,
    exit: Option<i32>,
}
fn reader<R: AsyncRead + Unpin + Send + 'static>(
    reader: Option<R>,
) -> tokio::sync::mpsc::Receiver<Vec<u8>> {
    let (sender, receiver) = tokio::sync::mpsc::channel(8);
    if let Some(mut reader) = reader {
        tokio::spawn(async move {
            let mut buffer = vec![0; 8192];
            while let Ok(count) = reader.read(&mut buffer).await {
                if count == 0 || sender.send(buffer[..count].to_vec()).await.is_err() {
                    break;
                }
            }
        });
    }
    receiver
}
async fn serve<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(stream: S) -> Result<()> {
    let (mut reader, writer) = tokio::io::split(stream);
    let writer = Arc::new(tokio::sync::Mutex::new(writer));
    let processes = Arc::new(tokio::sync::Mutex::new(HashMap::<u64, Process>::new()));
    let next = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let files = Arc::new(tokio::sync::Semaphore::new(64));
    let mut tasks = tokio::task::JoinSet::new();
    let failure = loop {
        while tasks.try_join_next().is_some() {}
        let envelope: RequestEnvelope = match read_frame(&mut reader).await {
            Ok(request) => request,
            Err(error) => break error,
        };
        let processes = processes.clone();
        let next = next.clone();
        let writer = writer.clone();
        let files = files.clone();
        tasks.spawn(async move {
            let result = handle(envelope.request, processes, next, files).await;
            let response = match result {
                Ok(value) => Response {
                    value: Some(value),
                    error: None,
                },
                Err(error) => Response {
                    value: None,
                    error: Some(error.to_string()),
                },
            };
            let _ = write_frame(
                &mut *writer.lock().await,
                &ResponseEnvelope {
                    id: envelope.id,
                    response,
                },
            )
            .await;
        });
    };
    tasks.shutdown().await;
    for (_, mut process) in processes.lock().await.drain() {
        let _ = process.child.start_kill();
    }
    Err(failure)
}
async fn handle(
    request: Request,
    processes: Arc<tokio::sync::Mutex<HashMap<u64, Process>>>,
    next: Arc<std::sync::atomic::AtomicU64>,
    files: Arc<tokio::sync::Semaphore>,
) -> Result<Value> {
    match request {
        Request::Ping => Ok(Value::Null),
        Request::File {
            name,
            cwd,
            arguments,
        } => {
            let _slot = files
                .try_acquire_owned()
                .map_err(|_| miette!("Elevated filesystem worker is busy"))?;
            tokio::task::spawn_blocking(move || file_operation(&name, &cwd, arguments))
                .await
                .into_diagnostic()?
        }
        Request::Spawn {
            program,
            arguments,
            environment,
            cwd,
            stdin,
        } => {
            let mut processes = processes.lock().await;
            let process = next
                .fetch_add(1, Ordering::Relaxed)
                .checked_add(1)
                .ok_or_else(|| miette!("Native process ID exhausted"))?;
            let mut child = HostChild::spawn(
                &program,
                arguments,
                SandboxProcessOptions {
                    environment,
                    current_dir: cwd,
                    stdin: if stdin {
                        SandboxStdio::Piped
                    } else {
                        SandboxStdio::Null
                    },
                    stdout: SandboxStdio::Piped,
                    stderr: SandboxStdio::Piped,
                    ..Default::default()
                },
            )
            .into_diagnostic()?;
            let pid = child.child.id();
            let stdin = child.child.stdin.take();
            let stdout = reader(child.child.stdout.take());
            let stderr = reader(child.child.stderr.take());
            processes.insert(
                process,
                Process {
                    child,
                    stdin,
                    stdout,
                    stderr,
                    exit: None,
                },
            );
            Ok(json!({"process":process,"pid":pid}))
        }
        Request::Poll { process: id } => {
            let mut processes = processes.lock().await;
            let process = processes
                .get_mut(&id)
                .ok_or_else(|| miette!("Native process is not owned by this connection"))?;
            if process.exit.is_none()
                && let Some(status) = process.child.try_wait().into_diagnostic()?
            {
                process.exit = Some(status.code().unwrap_or(1));
            }
            let mut stdout = vec![];
            let mut stderr = vec![];
            for _ in 0..8 {
                if let Ok(bytes) = process.stdout.try_recv() {
                    stdout.extend(bytes);
                } else {
                    break;
                }
            }
            for _ in 0..8 {
                if let Ok(bytes) = process.stderr.try_recv() {
                    stderr.extend(bytes);
                } else {
                    break;
                }
            }
            let drained = process.stdout.is_closed()
                && process.stdout.is_empty()
                && process.stderr.is_closed()
                && process.stderr.is_empty();
            let value =
                json!({"stdout":stdout,"stderr":stderr,"exit_code":process.exit,"drained":drained});
            if drained && process.exit.is_some() {
                processes.remove(&id);
            }
            Ok(value)
        }
        Request::Write {
            process: id,
            bytes,
            close,
        } => {
            let mut stdin = processes
                .lock()
                .await
                .get_mut(&id)
                .ok_or_else(|| miette!("Native process is not owned by this connection"))?
                .stdin
                .take();
            let result = if let Some(input) = &mut stdin {
                async {
                    input.write_all(&bytes).await?;
                    input.flush().await
                }
                .await
                .into_diagnostic()
            } else if bytes.is_empty() {
                Ok(())
            } else {
                Err(miette!("Native stdin is closed or already in use"))
            };
            if !close
                && result.is_ok()
                && let Some(process) = processes.lock().await.get_mut(&id)
            {
                process.stdin = stdin;
            }
            result?;
            Ok(Value::Null)
        }
        Request::Kill { process } => {
            let removed = processes.lock().await.remove(&process);
            if let Some(mut process) = removed {
                process.child.start_kill().into_diagnostic()?;
                process.child.child.wait().await.into_diagnostic()?;
            }
            Ok(Value::Null)
        }
    }
}

fn file_operation(name: &str, cwd: &Path, arguments: Value) -> Result<Value> {
    use crate::{file_io as files, sandbox::RuntimeSandboxPolicy};
    let policy = RuntimeSandboxPolicy::disabled();
    macro_rules! file {
        ($func:ident) => {
            serde_json::to_value(files::$func(
                cwd,
                &policy,
                &serde_json::from_value(arguments).into_diagnostic()?,
            )?)
            .into_diagnostic()
        };
    }
    match name {
        "read_file" => file!(read_file),
        "list_directory" => file!(list_directory),
        "search_text" => file!(search_text),
        "write_file" => file!(write_file),
        "edit_file" => file!(edit_file),
        "copy_file" => file!(copy_file),
        "read_bytes" => {
            #[derive(Deserialize)]
            struct Args {
                path: PathBuf,
                limit: u64,
            }
            let args: Args = serde_json::from_value(arguments).into_diagnostic()?;
            if args.limit > 10 * 1024 * 1024 {
                return Err(miette!("Native file read exceeds its limit"));
            }
            let path = RuntimeSandboxPolicy::resolve_path(&args.path, Some(cwd));
            use base64::Engine;
            Ok(json!(
                base64::engine::general_purpose::STANDARD
                    .encode(files::read_bytes(&path, args.limit, &policy)?)
            ))
        }
        "ensure_directory" => {
            let path: PathBuf = serde_json::from_value(arguments).into_diagnostic()?;
            files::ensure_directory(
                &policy,
                &RuntimeSandboxPolicy::resolve_path(&path, Some(cwd)),
            )?;
            Ok(Value::Null)
        }
        _ => Err(miette!("Unknown privileged filesystem operation")),
    }
}
