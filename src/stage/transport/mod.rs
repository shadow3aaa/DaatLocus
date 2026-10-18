//! Authenticated transport primitives. Protocol, grants and durability live above this layer.
pub mod lan;
pub mod p2p;
mod punch;
pub mod signaling;
pub mod ssh;
pub mod tls;
pub(crate) mod websocket;

use crate::stage::{
    Result, StageError,
    protocol::{Envelope, MAX_FRAME},
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, frame: &Envelope) -> Result<usize> {
    let bytes = frame.encode()?;
    writer.write_u32(bytes.len() as u32).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(bytes.len() + 4)
}
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<(Envelope, usize)> {
    let length = reader.read_u32().await? as usize;
    if length == 0 || length > MAX_FRAME {
        return Err(StageError::Invalid("Stage frame length exceeds budget"));
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).await?;
    Ok((Envelope::decode(&bytes)?, length + 4))
}
