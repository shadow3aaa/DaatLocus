//! Bounded Owner session HTTP requests over the authenticated device channel.
//! No cookies, filesystem paths or external destinations cross this boundary.
use super::{
    Result, StageError, StageId,
    host::Host,
    ledger::StageLedger,
    protocol::{BlobRef, Body},
};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub method: String,
    pub uri: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Reply {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}
pub(crate) fn forwarded_header(name: &str) -> bool {
    matches!(
        name,
        "content-type" | "idempotency-key" | "x-file-name" | "x-file-name-encoding"
    )
}
pub(crate) fn authorize(host: &Host, peer: StageId) -> Result<()> {
    let current = StageLedger::attach(host.system.store(), host.local)
        .peer(peer, chrono::Utc::now().timestamp_millis())?;
    let own = host.enrollment()?;
    super::identity::verify_enrollment(
        &own.root,
        &current.enrollment.chain,
        chrono::Utc::now().timestamp_millis(),
        &super::revocation::removed(host.system.store())?,
    )?;
    if current.enrollment.root != own.root
        || current.authority.read_only
        || !current.authority.converse
    {
        return Err(StageError::Forbidden(
            "Session HTTP requires writable account authority",
        ));
    }
    Ok(())
}
pub(crate) fn export(host: &Host, peer: StageId, bytes: &[u8]) -> Result<(BlobRef, uuid::Uuid)> {
    authorize(host, peer)?;
    let content = host.system.store().stage_transaction(|tx| {
        super::blob::put_control(tx, bytes).map(|manifest| manifest.content)
    })?;
    let export = uuid::Uuid::new_v4();
    StageLedger::attach(host.system.store(), host.local).export_control_content(
        peer,
        export,
        &content,
        chrono::Utc::now().timestamp_millis(),
    )?;
    Ok((content, export))
}
pub(crate) async fn call(
    runtime: &super::runtime::Runtime,
    peer: StageId,
    request: Request,
) -> Result<Reply> {
    authorize(&runtime.host, peer)?;
    let handle = runtime.handle(peer)?;
    call_handle(&runtime.host, peer, handle, request).await
}
pub(crate) async fn call_handle(
    host: &Host,
    peer: StageId,
    handle: super::peer::PeerHandle,
    request: Request,
) -> Result<Reply> {
    authorize(host, peer)?;
    let bytes =
        serde_json::to_vec(&request).map_err(|_| StageError::Invalid("Invalid session request"))?;
    let (content, export) = export(host, peer, &bytes)?;
    let Body::OwnerSessionReply { content, export } = handle
        .rpc(Body::OwnerSessionRequest { content, export })
        .await?
    else {
        return Err(StageError::Invalid("Unexpected session response"));
    };
    let bytes = handle.exported_content(export, content).await?;
    authorize(host, peer)?;
    serde_json::from_slice(&bytes).map_err(|_| StageError::Invalid("Invalid session response"))
}
