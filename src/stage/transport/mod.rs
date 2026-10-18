//! Authenticated transport primitives. Protocol, grants and durability live above this layer.
pub mod lan;
pub mod p2p;
mod punch;
pub mod signaling;
pub mod ssh;
pub mod tls;
pub(crate) mod websocket;
pub(crate) mod wire;

use crate::stage::{
    Result, StageError,
    protocol::{Envelope, MAX_FRAME},
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, frame: &Envelope) -> Result<usize> {
    write_frame_with(writer, frame, wire::Format::Json).await
}
pub(crate) async fn write_frame_with<W: AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &Envelope,
    format: wire::Format,
) -> Result<usize> {
    let bytes = format.encode(frame)?;
    writer.write_u32(bytes.len() as u32).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(bytes.len() + 4)
}
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<(Envelope, usize)> {
    read_frame_with(reader, wire::Format::Json).await
}
pub(crate) async fn read_frame_with<R: AsyncRead + Unpin>(
    reader: &mut R,
    format: wire::Format,
) -> Result<(Envelope, usize)> {
    let length = reader.read_u32().await? as usize;
    if length == 0 || length > MAX_FRAME {
        return Err(StageError::Invalid("Stage frame length exceeds budget"));
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).await?;
    Ok((format.decode(&bytes)?, length + 4))
}
