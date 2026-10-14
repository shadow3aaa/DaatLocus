use super::{NodeId, Request, Response, Types};
use crate::stage::{Result, StageError, StageId, protocol::Body, runtime::Runtime};
use openraft::{
    EmptyNode, RaftNetwork, RaftNetworkFactory,
    error::{InstallSnapshotError, RPCError, RaftError, Unreachable},
    network::RPCOption,
    raft::{
        AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest,
        InstallSnapshotResponse, VoteRequest, VoteResponse,
    },
};
use std::sync::{Arc, Weak};
pub struct Factory(pub Weak<Runtime>);
pub struct Network {
    runtime: Weak<Runtime>,
    target: StageId,
}
pub async fn rpc(runtime: &Arc<Runtime>, target: StageId, request: Request) -> Result<Response> {
    runtime.want(target);
    let response = runtime
        .handle(target)?
        .rpc_with_timeout(
            Body::CoordinationRequest {
                request: Box::new(request),
            },
            std::time::Duration::from_secs(3),
        )
        .await?;
    match response {
        Body::CoordinationResponse { response } => Ok(*response),
        _ => Err(StageError::Invalid("Invalid device consensus response")),
    }
}
impl RaftNetworkFactory<Types> for Factory {
    type Network = Network;
    async fn new_client(&mut self, target: NodeId, _node: &EmptyNode) -> Network {
        Network {
            runtime: self.0.clone(),
            target: StageId::from(target),
        }
    }
}
impl Network {
    async fn call(&self, request: Request) -> Result<Response> {
        let runtime = self
            .runtime
            .upgrade()
            .ok_or(StageError::Unavailable("Device stopped"))?;
        rpc(&runtime, self.target, request).await
    }
}
fn error<E>(error: StageError) -> RPCError<NodeId, EmptyNode, E>
where
    E: std::error::Error,
{
    RPCError::Unreachable(Unreachable::new(&error))
}
impl RaftNetwork<Types> for Network {
    async fn append_entries(
        &mut self,
        request: AppendEntriesRequest<Types>,
        _option: RPCOption,
    ) -> std::result::Result<
        AppendEntriesResponse<NodeId>,
        RPCError<NodeId, EmptyNode, RaftError<NodeId>>,
    > {
        match self.call(Request::Append(request)).await.map_err(error)? {
            Response::Append(reply) => Ok(reply),
            _ => Err(error(StageError::Unavailable("Device append unavailable"))),
        }
    }
    async fn vote(
        &mut self,
        request: VoteRequest<NodeId>,
        _option: RPCOption,
    ) -> std::result::Result<VoteResponse<NodeId>, RPCError<NodeId, EmptyNode, RaftError<NodeId>>>
    {
        match self.call(Request::Vote(request)).await.map_err(error)? {
            Response::Vote(reply) => Ok(reply),
            _ => Err(error(StageError::Unavailable("Device vote unavailable"))),
        }
    }
    async fn install_snapshot(
        &mut self,
        request: InstallSnapshotRequest<Types>,
        _option: RPCOption,
    ) -> std::result::Result<
        InstallSnapshotResponse<NodeId>,
        RPCError<NodeId, EmptyNode, RaftError<NodeId, InstallSnapshotError>>,
    > {
        match self.call(Request::Snapshot(request)).await.map_err(error)? {
            Response::Snapshot(reply) => Ok(reply),
            _ => Err(error(StageError::Unavailable(
                "Device snapshot unavailable",
            ))),
        }
    }
}
