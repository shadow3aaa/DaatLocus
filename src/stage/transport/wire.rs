//! Negotiated content framing. Control facts retain their existing JSON format.
use crate::stage::{
    Result, StageError,
    protocol::{Body, CHUNK_BYTES, Envelope, MAX_FRAME},
};

pub const ALPN: &[u8] = b"daat-locus-stage/2";
const MAGIC: &[u8; 4] = b"DLB2";
const HEADER: usize = 11;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub enum Format {
    #[default]
    Json,
    BinaryZstd,
}
impl Format {
    pub fn encode(self, frame: &Envelope) -> Result<Vec<u8>> {
        let Body::BlobChunk { bytes, .. } = &frame.body else {
            return frame.encode();
        };
        if self == Self::Json {
            return frame.encode();
        }
        if bytes.len() > CHUNK_BYTES {
            return Err(StageError::Invalid("Content exceeds wire budget"));
        }
        let mut metadata = frame.clone();
        if let Body::BlobChunk { bytes, .. } = &mut metadata.body {
            bytes.clear();
        }
        let metadata = metadata.encode()?;
        let compressed = if bytes.len() >= 1024 && std::str::from_utf8(bytes).is_ok() {
            zstd::bulk::compress(bytes, 1)
                .ok()
                .filter(|encoded| encoded.len() + 64 < bytes.len())
        } else {
            None
        };
        let payload = compressed.as_deref().unwrap_or(bytes);
        if metadata.len() > u16::MAX as usize || HEADER + metadata.len() + payload.len() > MAX_FRAME
        {
            return Err(StageError::Invalid("Content exceeds wire budget"));
        }
        let mut encoded = Vec::with_capacity(HEADER + metadata.len() + payload.len());
        encoded.extend_from_slice(MAGIC);
        encoded.push(u8::from(compressed.is_some()));
        encoded.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        encoded.extend_from_slice(&(metadata.len() as u16).to_be_bytes());
        encoded.extend_from_slice(&metadata);
        encoded.extend_from_slice(payload);
        Ok(encoded)
    }
    pub fn decode(self, encoded: &[u8]) -> Result<Envelope> {
        if !encoded.starts_with(MAGIC) {
            return Envelope::decode(encoded);
        }
        let invalid = || StageError::Invalid("Invalid compact content frame");
        if self != Self::BinaryZstd || encoded.len() < HEADER || encoded.len() > MAX_FRAME {
            return Err(invalid());
        }
        let flags = encoded[4];
        let length = u32::from_be_bytes(encoded[5..9].try_into().unwrap()) as usize;
        let metadata_len = u16::from_be_bytes(encoded[9..11].try_into().unwrap()) as usize;
        if flags > 1 || length > CHUNK_BYTES || HEADER + metadata_len > encoded.len() {
            return Err(invalid());
        }
        let mut frame = Envelope::decode(&encoded[HEADER..HEADER + metadata_len])?;
        let Body::BlobChunk { bytes, .. } = &mut frame.body else {
            return Err(invalid());
        };
        if !bytes.is_empty() {
            return Err(invalid());
        }
        let payload = &encoded[HEADER + metadata_len..];
        *bytes = if flags == 1 {
            let mut decoder = zstd::bulk::Decompressor::new().map_err(|_| invalid())?;
            decoder
                .set_parameter(zstd::zstd_safe::DParameter::WindowLogMax(16))
                .map_err(|_| invalid())?;
            decoder.decompress(payload, length).map_err(|_| invalid())?
        } else {
            payload.to_vec()
        };
        if bytes.len() != length {
            return Err(invalid());
        }
        Ok(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stage::{StageId, digest, protocol::BlobRef};
    fn content(bytes: Vec<u8>) -> Envelope {
        Envelope {
            version: 1,
            source: StageId::new(),
            target: StageId::new(),
            generation: 1,
            message_id: uuid::Uuid::new_v4(),
            invocation_id: None,
            in_reply_to: None,
            created_at_ms: 1,
            expires_at_ms: 1000,
            body: Body::BlobChunk {
                content: BlobRef {
                    hash: digest(&bytes),
                    size: bytes.len() as u64,
                    version: 1,
                },
                chunk: 0,
                chunk_hash: digest(&bytes),
                bytes,
            },
        }
    }
    #[test]
    fn negotiated_frames_reduce_text_and_binary_bytes_and_preserve_original_content() {
        for (payload, flag) in [
            (
                b"source text and repeated conversation text\n".repeat(700),
                1,
            ),
            ((0..32768).map(|i| (i % 256) as u8).collect(), 0),
        ] {
            let frame = content(payload.clone());
            let legacy = Format::Json.encode(&frame).unwrap();
            assert_eq!(
                Format::Json.decode(&legacy).unwrap().encode().unwrap(),
                legacy
            );
            let compact = Format::BinaryZstd.encode(&frame).unwrap();
            assert!(compact.len() < legacy.len());
            assert_eq!(compact[4], flag);
            assert!(
                Format::Json.decode(&compact).is_err(),
                "compact format requires negotiation"
            );
            let decoded = Format::BinaryZstd.decode(&compact).unwrap();
            assert_eq!(decoded.encode().unwrap(), legacy);
            decoded.validate(frame.source, frame.target, 1, 2).unwrap();
            let Body::BlobChunk { bytes, .. } = decoded.body else {
                panic!("expected content")
            };
            assert_eq!(bytes, payload);
        }
    }
    #[test]
    fn malformed_compact_frames_and_decompression_expansion_are_bounded() {
        let encoded = Format::BinaryZstd
            .encode(&content(vec![b'x'; 32768]))
            .unwrap();
        for cut in [0, 4, 8, 10, encoded.len() - 1] {
            assert!(Format::BinaryZstd.decode(&encoded[..cut]).is_err());
        }
        for (range, replacement) in [
            (4..5, vec![2]),
            (5..9, 32769u32.to_be_bytes().to_vec()),
            (5..9, 1u32.to_be_bytes().to_vec()),
            (9..11, u16::MAX.to_be_bytes().to_vec()),
        ] {
            let mut invalid = encoded.clone();
            invalid.splice(range, replacement);
            assert!(Format::BinaryZstd.decode(&invalid).is_err());
        }
        let mut bomb = encoded.clone();
        let offset = HEADER + u16::from_be_bytes(bomb[9..11].try_into().unwrap()) as usize;
        bomb.truncate(offset);
        bomb.extend(zstd::bulk::compress(&vec![b'x'; CHUNK_BYTES * 4], 1).unwrap());
        assert!(Format::BinaryZstd.decode(&bomb).is_err());
    }
}
