//! Manager-to-session IPC protocol and local socket framing.

use std::time::Duration;

use interprocess::local_socket::{
    self, GenericNamespaced, ListenerOptions, ToNsName,
    tokio::prelude::{LocalSocketListener, LocalSocketStream},
    traits::tokio::Listener as _,
};
use miette::{Result, miette};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;

use crate::{
    dashboard::{
        DashboardAction, DashboardActionResult, DashboardActivityHistoryCount,
        DashboardActivityHistoryPage, DashboardContextCompositionSnapshot, DashboardInputHistory,
        DashboardPlanStep, DashboardPrimitiveOptimizationSnapshot, DashboardRuntimeActivity,
        DashboardRuntimeOptimizationSnapshot, DashboardRuntimeStatusLevel, DashboardSessionTitle,
        DashboardState, DashboardTokenUsageSnapshot,
    },
    events::{EventStatus, TelegramIncomingEvent},
    telegram_transport::state::PendingOutboundMessage,
};

use super::session::SessionId;

pub const SESSION_IPC_PROTOCOL_VERSION: u32 = 1;
const MAX_IPC_FRAME_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
pub struct IpcRequestEnvelope {
    pub protocol_version: u32,
    pub request_id: String,
    pub session_id: String,
    pub ipc_token: String,
    pub body: SessionIpcRequest,
}

impl IpcRequestEnvelope {
    pub fn new(session_id: &SessionId, ipc_token: String, body: SessionIpcRequest) -> Self {
        Self {
            protocol_version: SESSION_IPC_PROTOCOL_VERSION,
            request_id: Uuid::new_v4().to_string(),
            session_id: session_id.as_str().to_string(),
            ipc_token,
            body,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionIpcRequest {
    Status,
    StatusSummary,
    SubmitUserInput {
        origin: UserInputOrigin,
        text: String,
        #[serde(default)]
        attachments: Vec<InputAttachment>,
        wait_for_reply: bool,
    },
    DashboardCommand {
        command: String,
    },
    DashboardAction {
        action: DashboardAction,
    },
    EnqueueTelegramEvent {
        event: TelegramIncomingEvent,
    },
    DashboardSnapshot,
    DashboardHistoryPage {
        before: Option<i64>,
        after: Option<i64>,
        limit: usize,
    },
    DashboardInputHistory {
        limit: usize,
    },
    DashboardHistoryCount,
    DrainTelegramOutbox,
    RecordTelegramDelivery {
        event_id: String,
        status: EventStatus,
        note: Option<String>,
    },
    RequeueTelegramOutbound {
        message: PendingOutboundMessage,
    },
    SubscribeDashboard,
    Shutdown {
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserInputOrigin {
    WebUi,
    Tui,
    CliSend,
}

impl UserInputOrigin {
    pub fn terminal_origin_label(self) -> &'static str {
        match self {
            Self::WebUi => "webui",
            Self::Tui => "tui",
            Self::CliSend => "cli_send",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputAttachment {
    pub media_type: String,
    pub local_path: String,
    pub description: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct IpcResponseEnvelope {
    pub request_id: String,
    pub body: SessionIpcResponse,
}

impl IpcResponseEnvelope {
    pub fn ok(request_id: impl Into<String>, body: SessionIpcResponse) -> Self {
        Self {
            request_id: request_id.into(),
            body,
        }
    }

    pub fn error(
        request_id: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            body: SessionIpcResponse::Error {
                code: code.into(),
                message: message.into(),
                retryable,
            },
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionIpcResponse {
    Status {
        runtime_status: SessionRuntimeStatus,
    },
    StatusSummary {
        summary: Box<SessionStatusSummary>,
    },
    Submitted {
        event_id: String,
        reply_message: Option<String>,
        terminal_status: Option<String>,
    },
    DashboardCommandResult {
        output: String,
    },
    DashboardActionResult {
        result: DashboardActionResult,
    },
    DashboardSnapshot {
        state: Box<DashboardState>,
    },
    DashboardHistoryPage {
        page: DashboardActivityHistoryPage,
    },
    DashboardInputHistory {
        history: DashboardInputHistory,
    },
    DashboardHistoryCount {
        count: DashboardActivityHistoryCount,
    },
    TelegramOutbox {
        messages: Vec<PendingOutboundMessage>,
    },
    DeliveryRecorded,
    TelegramOutboundRequeued,
    ShutdownAccepted,
    Error {
        code: String,
        message: String,
        retryable: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRuntimeStatus {
    pub ready: bool,
    pub status: String,
    pub pending_work_count: usize,
    pub active_runtime_turn: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SessionStatusSummary {
    pub runtime_status: SessionRuntimeStatus,
    #[serde(default)]
    pub session_title: Option<DashboardSessionTitle>,
    pub dashboard: SessionStatusDashboard,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SessionStatusDashboard {
    pub agent_name: String,
    #[serde(default)]
    pub session_title: Option<DashboardSessionTitle>,
    pub last_cycle_elapsed_ms: Option<u64>,
    pub runtime_status: Option<String>,
    pub runtime_status_level: Option<DashboardRuntimeStatusLevel>,
    pub runtime_activity: DashboardRuntimeActivity,
    pub current_plan_step: Option<DashboardPlanStep>,
    pub token_usage: DashboardTokenUsageSnapshot,
    pub primitive_optimization: DashboardPrimitiveOptimizationSnapshot,
    pub runtime_optimization: DashboardRuntimeOptimizationSnapshot,
    pub context_composition: Option<DashboardContextCompositionSnapshot>,
}

impl SessionStatusDashboard {
    pub fn from_dashboard_state(state: &DashboardState) -> Self {
        Self {
            agent_name: state.agent_name.clone(),
            session_title: state.session_title.clone(),
            last_cycle_elapsed_ms: state.last_cycle_elapsed_ms,
            runtime_status: state.runtime_status.clone(),
            runtime_status_level: state.runtime_status_level,
            runtime_activity: state.runtime_activity.clone(),
            current_plan_step: state.current_plan_step.clone(),
            token_usage: state.token_usage.clone(),
            primitive_optimization: state.primitive_optimization.clone(),
            runtime_optimization: state.runtime_optimization.clone(),
            context_composition: state.context_composition.clone(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionIpcStreamEvent {
    DashboardSnapshot {
        state: Box<DashboardState>,
    },
    DashboardClosed {
        reason: String,
    },
    Error {
        code: String,
        message: String,
        retryable: bool,
    },
}

#[derive(Debug, Clone)]
pub struct SessionIpcClient {
    session_id: SessionId,
    ipc_name: String,
    ipc_token: String,
    timeout: Duration,
}

impl SessionIpcClient {
    pub fn new(session_id: SessionId, ipc_name: String, ipc_token: String) -> Self {
        Self {
            session_id,
            ipc_name,
            ipc_token,
            timeout: Duration::from_secs(30),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub async fn request(&self, body: SessionIpcRequest) -> Result<SessionIpcResponse> {
        let envelope = IpcRequestEnvelope::new(&self.session_id, self.ipc_token.clone(), body);
        let request_id = envelope.request_id.clone();
        let future = async {
            let mut stream = connect_local_socket(&self.ipc_name).await?;
            write_json_frame(&mut stream, &envelope).await?;
            let response: IpcResponseEnvelope = read_json_frame(&mut stream).await?;
            if response.request_id != request_id {
                return Err(miette!(
                    "session IPC response id mismatch: expected {}, got {}",
                    request_id,
                    response.request_id
                ));
            }
            Ok(response.body)
        };
        tokio::time::timeout(self.timeout, future)
            .await
            .map_err(|_| miette!("session IPC request timed out"))?
    }

    pub async fn subscribe_dashboard(&self) -> Result<LocalSocketStream> {
        let envelope = IpcRequestEnvelope::new(
            &self.session_id,
            self.ipc_token.clone(),
            SessionIpcRequest::SubscribeDashboard,
        );
        let mut stream = connect_local_socket(&self.ipc_name).await?;
        write_json_frame(&mut stream, &envelope).await?;
        Ok(stream)
    }
}

pub struct SessionIpcServer {
    listener: LocalSocketListener,
}

impl SessionIpcServer {
    pub async fn bind(ipc_name: impl AsRef<str>) -> Result<Self> {
        let ipc_name = ipc_name.as_ref();
        let name = build_local_socket_name(ipc_name)?;
        let listener = ListenerOptions::new()
            .name(name)
            .create_tokio()
            .map_err(|err| miette!("bind IPC socket {ipc_name} failed: {err}"))?;
        Ok(Self { listener })
    }

    pub async fn accept(&self) -> Result<LocalSocketStream> {
        self.listener
            .accept()
            .await
            .map_err(|err| miette!("accept session IPC connection failed: {err}"))
    }
}

fn build_local_socket_name(ipc_name: &str) -> Result<local_socket::Name<'_>> {
    ipc_name
        .to_ns_name::<GenericNamespaced>()
        .map_err(|err| miette!("build IPC socket name {ipc_name} failed: {err}"))
}

async fn connect_local_socket(ipc_name: &str) -> Result<LocalSocketStream> {
    let name = build_local_socket_name(ipc_name)?;
    local_socket::ConnectOptions::new()
        .name(name)
        .connect_tokio()
        .await
        .map_err(|err| miette!("connect IPC socket {ipc_name} failed: {err}"))
}

async fn write_json_frame<W, T>(writer: &mut W, value: &T) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize + ?Sized,
{
    let bytes =
        serde_json::to_vec(value).map_err(|err| miette!("encode IPC JSON frame failed: {err}"))?;
    if bytes.len() > MAX_IPC_FRAME_BYTES {
        return Err(miette!(
            "IPC frame too large: {} bytes exceeds {}",
            bytes.len(),
            MAX_IPC_FRAME_BYTES
        ));
    }
    let len = u32::try_from(bytes.len())
        .map_err(|_| miette!("IPC frame length does not fit into u32"))?;
    writer
        .write_all(&len.to_be_bytes())
        .await
        .map_err(|err| miette!("write IPC frame length failed: {err}"))?;
    writer
        .write_all(&bytes)
        .await
        .map_err(|err| miette!("write IPC frame body failed: {err}"))?;
    writer
        .flush()
        .await
        .map_err(|err| miette!("flush IPC frame failed: {err}"))?;
    Ok(())
}

pub async fn read_request(stream: &mut LocalSocketStream) -> Result<IpcRequestEnvelope> {
    read_json_frame(stream).await
}

pub async fn write_response(
    stream: &mut LocalSocketStream,
    response: &IpcResponseEnvelope,
) -> Result<()> {
    write_json_frame(stream, response).await
}

pub async fn read_stream_event(stream: &mut LocalSocketStream) -> Result<SessionIpcStreamEvent> {
    read_json_frame(stream).await
}

pub async fn write_stream_event<W>(writer: &mut W, event: &SessionIpcStreamEvent) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    write_json_frame(writer, event).await
}

async fn read_json_frame<R, T>(reader: &mut R) -> Result<T>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut len_bytes = [0u8; 4];
    reader
        .read_exact(&mut len_bytes)
        .await
        .map_err(|err| miette!("read IPC frame length failed: {err}"))?;
    let len = u32::from_be_bytes(len_bytes) as usize;
    if len > MAX_IPC_FRAME_BYTES {
        return Err(miette!(
            "IPC frame too large: {len} bytes exceeds {MAX_IPC_FRAME_BYTES}"
        ));
    }
    let mut bytes = vec![0u8; len];
    reader
        .read_exact(&mut bytes)
        .await
        .map_err(|err| miette!("read IPC frame body failed: {err}"))?;
    serde_json::from_slice(&bytes).map_err(|err| miette!("decode IPC JSON frame failed: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncWriteExt, duplex};

    fn fixed_session_id() -> SessionId {
        SessionId::from_string("session-test".to_string()).expect("valid session id")
    }

    fn test_ipc_name() -> String {
        format!("daat-locus-test-{}", uuid::Uuid::new_v4())
    }

    #[test]
    fn ipc_request_envelope_uses_public_protocol_shape() {
        let envelope = IpcRequestEnvelope::new(
            &fixed_session_id(),
            "ipc-token".to_string(),
            SessionIpcRequest::SubmitUserInput {
                origin: UserInputOrigin::WebUi,
                text: "hello".to_string(),
                attachments: Vec::new(),
                wait_for_reply: false,
            },
        );
        let value = serde_json::to_value(&envelope).expect("serialize envelope");

        assert_eq!(value["protocol_version"], SESSION_IPC_PROTOCOL_VERSION);
        assert_eq!(value["session_id"], "session-test");
        assert_eq!(value["ipc_token"], "ipc-token");
        assert_eq!(value["body"]["kind"], "submit_user_input");
        assert_eq!(value["body"]["origin"], "web_ui");
        assert_eq!(value["body"]["attachments"], serde_json::json!([]));
        assert_eq!(value["body"]["wait_for_reply"], false);
    }
    #[test]
    fn user_input_origin_labels_match_terminal_event_sources() {
        assert_eq!(UserInputOrigin::WebUi.terminal_origin_label(), "webui");
        assert_eq!(UserInputOrigin::Tui.terminal_origin_label(), "tui");
        assert_eq!(UserInputOrigin::CliSend.terminal_origin_label(), "cli_send");
    }

    #[tokio::test]
    async fn ipc_json_frame_round_trips_request_envelope() {
        let (mut writer, mut reader) = duplex(4096);
        let envelope = IpcRequestEnvelope::new(
            &fixed_session_id(),
            "ipc-token".to_string(),
            SessionIpcRequest::DashboardHistoryPage {
                before: Some(10),
                after: None,
                limit: 25,
            },
        );
        let expected_request_id = envelope.request_id.clone();

        let write_task = tokio::spawn(async move {
            write_json_frame(&mut writer, &envelope)
                .await
                .expect("write request frame");
        });
        let decoded: IpcRequestEnvelope = read_json_frame(&mut reader)
            .await
            .expect("read request frame");
        write_task.await.expect("writer task");

        assert_eq!(decoded.protocol_version, SESSION_IPC_PROTOCOL_VERSION);
        assert_eq!(decoded.request_id, expected_request_id);
        assert_eq!(decoded.session_id, "session-test");
        assert_eq!(decoded.ipc_token, "ipc-token");
        match decoded.body {
            SessionIpcRequest::DashboardHistoryPage {
                before,
                after,
                limit,
            } => {
                assert_eq!(before, Some(10));
                assert_eq!(after, None);
                assert_eq!(limit, 25);
            }
            _ => panic!("unexpected IPC request body"),
        }
    }

    #[tokio::test]
    async fn ipc_json_frame_rejects_oversized_declared_length() {
        let (mut writer, mut reader) = duplex(4);
        let write_task = tokio::spawn(async move {
            writer
                .write_all(&((MAX_IPC_FRAME_BYTES + 1) as u32).to_be_bytes())
                .await
                .expect("write oversized length");
        });

        let err = match read_json_frame::<_, IpcResponseEnvelope>(&mut reader).await {
            Ok(_) => panic!("oversized frame unexpectedly decoded"),
            Err(err) => err,
        };
        write_task.await.expect("writer task");
        assert!(
            err.to_string().contains("IPC frame too large"),
            "unexpected error: {err:?}"
        );
    }

    #[tokio::test]
    async fn ipc_client_rejects_mismatched_response_request_id() {
        let ipc_name = test_ipc_name();
        let server = SessionIpcServer::bind(&ipc_name)
            .await
            .expect("bind IPC server");
        let client = SessionIpcClient::new(fixed_session_id(), ipc_name, "ipc-token".to_string())
            .with_timeout(Duration::from_secs(2));

        let server_future = async {
            let mut stream = server.accept().await.expect("accept IPC client");
            let request = read_request(&mut stream).await.expect("read request");
            write_response(
                &mut stream,
                &IpcResponseEnvelope::ok(
                    "wrong-request-id",
                    SessionIpcResponse::Status {
                        runtime_status: SessionRuntimeStatus {
                            ready: true,
                            status: "ready".to_string(),
                            pending_work_count: 0,
                            active_runtime_turn: false,
                        },
                    },
                ),
            )
            .await
            .expect("write mismatched response");
            request
        };
        let client_future = client.request(SessionIpcRequest::Status);
        let (request, client_result) = tokio::join!(server_future, client_future);

        assert_eq!(request.session_id, "session-test");
        assert_eq!(request.ipc_token, "ipc-token");
        assert!(matches!(request.body, SessionIpcRequest::Status));
        let err = match client_result {
            Ok(_) => panic!("mismatched response id unexpectedly accepted"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("response id mismatch"),
            "unexpected error: {err:?}"
        );
    }
}
