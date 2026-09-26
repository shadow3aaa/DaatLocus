//! Stream multiplexing for the tunnel.
//!
//! QUIC already multiplexes independent byte streams over one connection, so
//! the "mux" here is (a) the bounded length-prefixed frame codec used for the
//! registration/control exchange and (b) the bounded byte pumps that move
//! payload between a QUIC stream and the local TCP target.
//!
//! Everything that parses bytes coming off the wire lives behind a length cap,
//! mirroring the `MAX_IPC_FRAME_BYTES` precedent in `daemon::session_ipc`.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::error::TunnelError;

/// Maximum size of a single control frame (mirrors session IPC's 16 MiB cap).
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
/// Bytes used by the `u32` big-endian length prefix.
pub const LENGTH_PREFIX_BYTES: usize = 4;
/// Buffer size for the copy pumps.
pub const COPY_BUFFER_BYTES: usize = 64 * 1024;

/// Encode one length-prefixed control frame.
pub fn encode_frame(payload: &[u8]) -> Result<Vec<u8>, TunnelError> {
    if payload.len() > MAX_FRAME_BYTES {
        return Err(TunnelError::Internal(format!(
            "control frame of {} bytes exceeds the {MAX_FRAME_BYTES}-byte cap",
            payload.len()
        )));
    }
    let mut out = Vec::with_capacity(LENGTH_PREFIX_BYTES + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// Incremental decoder for length-prefixed control frames.
#[derive(Debug)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
    max_frame_bytes: usize,
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameDecoder {
    /// A decoder enforcing [`MAX_FRAME_BYTES`].
    pub fn new() -> Self {
        Self::with_max(MAX_FRAME_BYTES)
    }

    /// A decoder enforcing a custom cap.
    pub fn with_max(max_frame_bytes: usize) -> Self {
        Self {
            buffer: Vec::new(),
            max_frame_bytes,
        }
    }

    /// Append a chunk of untrusted bytes.
    pub fn push(&mut self, chunk: &[u8]) -> Result<(), TunnelError> {
        if self.buffer.len().saturating_add(chunk.len())
            > self.max_frame_bytes + LENGTH_PREFIX_BYTES
        {
            return Err(TunnelError::Internal(
                "control frame buffer exceeded the configured cap".to_string(),
            ));
        }
        self.buffer.extend_from_slice(chunk);
        Ok(())
    }

    /// Pop the next complete frame, if any.
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, TunnelError> {
        if self.buffer.len() < LENGTH_PREFIX_BYTES {
            return Ok(None);
        }
        let mut length_bytes = [0u8; LENGTH_PREFIX_BYTES];
        length_bytes.copy_from_slice(&self.buffer[..LENGTH_PREFIX_BYTES]);
        let length = u32::from_be_bytes(length_bytes) as usize;
        if length > self.max_frame_bytes {
            return Err(TunnelError::Internal(format!(
                "control frame announced {length} bytes, over the {}-byte cap",
                self.max_frame_bytes
            )));
        }
        if self.buffer.len() < LENGTH_PREFIX_BYTES + length {
            return Ok(None);
        }
        let frame = self.buffer[LENGTH_PREFIX_BYTES..LENGTH_PREFIX_BYTES + length].to_vec();
        self.buffer.drain(..LENGTH_PREFIX_BYTES + length);
        Ok(Some(frame))
    }
}

/// Pump bytes from a QUIC receive stream into a local writable sink.
pub async fn pump_quic_to_tcp<W>(
    recv: &mut quinn::RecvStream,
    sink: &mut W,
) -> Result<u64, TunnelError>
where
    W: AsyncWrite + Unpin,
{
    let mut buffer = vec![0u8; COPY_BUFFER_BYTES];
    let mut total = 0u64;
    loop {
        let read = recv
            .read(&mut buffer)
            .await
            .map_err(|err| TunnelError::Internal(format!("read quic stream: {err}")))?;
        let Some(read) = read else {
            break;
        };
        if read == 0 {
            continue;
        }
        sink.write_all(&buffer[..read])
            .await
            .map_err(|err| TunnelError::Internal(format!("write local target: {err}")))?;
        total += read as u64;
    }
    sink.flush()
        .await
        .map_err(|err| TunnelError::Internal(format!("flush local target: {err}")))?;
    Ok(total)
}

/// Pump bytes from a local readable source into a QUIC send stream.
pub async fn pump_tcp_to_quic<R>(
    source: &mut R,
    send: &mut quinn::SendStream,
) -> Result<u64, TunnelError>
where
    R: AsyncRead + Unpin,
{
    let mut buffer = vec![0u8; COPY_BUFFER_BYTES];
    let mut total = 0u64;
    loop {
        let read = source
            .read(&mut buffer)
            .await
            .map_err(|err| TunnelError::Internal(format!("read local target: {err}")))?;
        if read == 0 {
            break;
        }
        send.write_all(&buffer[..read])
            .await
            .map_err(|err| TunnelError::Internal(format!("write quic stream: {err}")))?;
        total += read as u64;
    }
    let _ = send.finish();
    Ok(total)
}

/// Write one length-prefixed control frame to a QUIC send stream.
pub async fn send_frame(send: &mut quinn::SendStream, payload: &[u8]) -> Result<(), TunnelError> {
    let encoded = encode_frame(payload)?;
    send.write_all(&encoded)
        .await
        .map_err(|err| TunnelError::Internal(format!("write control frame: {err}")))?;
    Ok(())
}

/// Read one length-prefixed control frame from a QUIC receive stream.
///
/// The frame length is validated against [`MAX_FRAME_BYTES`] before any payload
/// is buffered, so a hostile edge cannot make the reader allocate without bound.
pub async fn read_frame(recv: &mut quinn::RecvStream) -> Result<Vec<u8>, TunnelError> {
    let mut decoder = FrameDecoder::new();
    let mut buffer = [0u8; 4096];
    loop {
        if let Some(frame) = decoder.next_frame()? {
            return Ok(frame);
        }
        match recv
            .read(&mut buffer)
            .await
            .map_err(|err| TunnelError::Internal(format!("read control frame: {err}")))?
        {
            Some(0) | None => {
                return Err(TunnelError::Internal(
                    "control stream closed before a full frame arrived".to_string(),
                ));
            }
            Some(read) => decoder.push(&buffer[..read])?,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FrameDecoder, LENGTH_PREFIX_BYTES, MAX_FRAME_BYTES, encode_frame};

    #[test]
    fn encode_then_decode_roundtrips() {
        let frame = encode_frame(b"hello").expect("encode");
        let mut decoder = FrameDecoder::new();
        decoder.push(&frame).expect("push");
        assert_eq!(
            decoder.next_frame().expect("decode").as_deref(),
            Some(&b"hello"[..])
        );
        assert!(decoder.next_frame().expect("decode").is_none());
    }

    #[test]
    fn decoder_handles_split_reads() {
        let frame = encode_frame(b"split-me").expect("encode");
        let mut decoder = FrameDecoder::new();
        for byte in &frame {
            decoder.push(&[*byte]).expect("push");
        }
        assert_eq!(
            decoder.next_frame().expect("decode").as_deref(),
            Some(&b"split-me"[..])
        );
    }

    #[test]
    fn decoder_rejects_oversized_length_prefix() {
        let mut decoder = FrameDecoder::new();
        decoder.push(&(u32::MAX).to_be_bytes()).expect("push");
        assert!(decoder.next_frame().is_err());
    }

    #[test]
    fn encode_rejects_oversized_payload() {
        let oversized = vec![0u8; MAX_FRAME_BYTES + 1];
        assert!(encode_frame(&oversized).is_err());
    }

    #[test]
    fn decoder_bounds_its_buffer() {
        let mut decoder = FrameDecoder::with_max(8);
        // Multiplying a huge length prefix without a complete frame must error
        // instead of buffering without bound.
        let mut huge = vec![0u8; MAX_FRAME_BYTES];
        huge[..LENGTH_PREFIX_BYTES].copy_from_slice(&(u32::MAX).to_be_bytes());
        assert!(decoder.push(&huge).is_err());
    }

    #[test]
    fn fuzz_like_sweep_never_panics() {
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut decoder = FrameDecoder::new();
        for _ in 0..4096 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let len = (state as usize) % 64;
            let chunk: Vec<u8> = (0..len).map(|index| (state >> (index % 8)) as u8).collect();
            if decoder.push(&chunk).is_err() {
                decoder = FrameDecoder::new();
            }
            while let Ok(Some(_frame)) = decoder.next_frame() {}
        }
    }
}
