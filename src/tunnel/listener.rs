//! Local listener: accept QUIC streams from the edge and forward their raw
//! bytes to `127.0.0.1:<port>`.
//!
//! The listener carries zero protocol semantics: the daemon already speaks
//! HTTP/WebSocket, so the tunnel just splices TCP. A single QUIC connection
//! can carry many proxied requests, each as its own bidirectional stream.

use std::net::SocketAddr;

use tokio::net::TcpStream;

use super::error::TunnelError;
use super::mux::{pump_quic_to_tcp, pump_tcp_to_quic};

/// Accept proxied streams on `connection` until it closes.
pub async fn serve_connection(
    connection: quinn::Connection,
    target: SocketAddr,
) -> Result<(), TunnelError> {
    loop {
        match connection.accept_bi().await {
            Ok((send, recv)) => {
                tokio::spawn(async move {
                    if let Err(err) = proxy_stream(send, recv, target).await {
                        tracing::debug!("tunnel stream to {target} ended: {err}");
                    }
                });
            }
            Err(quinn::ConnectionError::ApplicationClosed(_)) => return Ok(()),
            Err(quinn::ConnectionError::LocallyClosed) => return Ok(()),
            Err(err) => {
                return Err(TunnelError::Internal(format!(
                    "accept quic stream failed: {err}"
                )));
            }
        }
    }
}

async fn proxy_stream(
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    target: SocketAddr,
) -> Result<(), TunnelError> {
    let stream = TcpStream::connect(target)
        .await
        .map_err(|err| TunnelError::Internal(format!("connect local target {target}: {err}")))?;
    let _ = stream.set_nodelay(true);
    let (mut read_half, mut write_half) = stream.into_split();

    let to_local = pump_quic_to_tcp(&mut recv, &mut write_half);
    let to_edge = pump_tcp_to_quic(&mut read_half, &mut send);
    tokio::pin!(to_local);
    tokio::pin!(to_edge);
    tokio::select! {
        result = &mut to_local => {
            result?;
        }
        result = &mut to_edge => {
            result?;
        }
    }
    Ok(())
}
