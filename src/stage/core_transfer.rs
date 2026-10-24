//! Negotiated Core transfers: small calls stay inline; bulk records share a
//! bounded compressed packet. Checkpoint hashes and publication remain unchanged.
use super::{
    Result, StageError, blob,
    core_wire::{Request, Response},
    protocol::BlobRef,
    shared::ACTOR_ID,
};
use crate::actors::store::ActorStore;
use serde::{Serialize, de::DeserializeOwned};
use std::collections::HashSet;

pub(crate) const INLINE_BYTES: usize = 16 * 1024;
const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAGIC: &[u8; 4] = b"DLC3";
const HEADER: usize = 9;

trait Payload: Serialize + DeserializeOwned {
    fn references(&self) -> Vec<&BlobRef>;
}
impl Payload for Request {
    fn references(&self) -> Vec<&BlobRef> {
        match self {
            Self::Prepare { batch } => batch.changes.iter().map(|change| &change.content).collect(),
            _ => vec![],
        }
    }
}
impl Payload for Response {
    fn references(&self) -> Vec<&BlobRef> {
        match self {
            Self::Snapshot { page } => page.entries.iter().map(|change| &change.content).collect(),
            Self::SnapshotRange { pages } => pages
                .iter()
                .flat_map(|page| &page.entries)
                .map(|change| &change.content)
                .collect(),
            Self::Journal { batches } => batches
                .iter()
                .flat_map(|batch| &batch.changes)
                .map(|change| &change.content)
                .collect(),
            Self::RecoveryArchives { entries, .. } => {
                entries.iter().map(|entry| &entry.change.content).collect()
            }
            _ => vec![],
        }
    }
}

pub(crate) fn encode_request(store: &ActorStore, value: &Request, packed: bool) -> Result<Vec<u8>> {
    encode(store, value, packed)
}
pub(crate) fn encode_response(
    store: &ActorStore,
    value: &Response,
    packed: bool,
) -> Result<Vec<u8>> {
    encode(store, value, packed)
}
pub(crate) fn decode_request(store: &ActorStore, bytes: &[u8], packed: bool) -> Result<Request> {
    decode(store, bytes, packed)
}
pub(crate) fn decode_response(store: &ActorStore, bytes: &[u8], packed: bool) -> Result<Response> {
    decode(store, bytes, packed)
}

fn invalid() -> StageError {
    StageError::Invalid("Invalid packed Core transfer")
}
fn encode<T: Payload>(store: &ActorStore, value: &T, packed: bool) -> Result<Vec<u8>> {
    let metadata = serde_json::to_vec(value).map_err(|_| invalid())?;
    if !packed || metadata.len() + 8 > MAX_BYTES {
        return Ok(metadata);
    }
    let mut body = Vec::with_capacity(metadata.len() + 8);
    body.extend_from_slice(&(metadata.len() as u32).to_be_bytes());
    body.extend_from_slice(&metadata);
    let count_offset = body.len();
    body.extend_from_slice(&0u32.to_be_bytes());
    let mut count = 0u32;
    let mut seen = HashSet::new();
    for (index, reference) in value.references().into_iter().enumerate() {
        reference.validate()?;
        // Oversized records and cold content retain the resumable blob path.
        let Some(remaining) = MAX_BYTES.checked_sub(body.len() + 8) else {
            break;
        };
        if reference.size > remaining as u64 || !seen.insert(&reference.hash) {
            continue;
        }
        if let Ok(bytes) = blob::ContentStore::new(store).verified_bytes(reference) {
            body.extend_from_slice(&(index as u32).to_be_bytes());
            body.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
            body.extend_from_slice(&bytes);
            count += 1;
        }
    }
    if count == 0 && metadata.len() < 4096 {
        return Ok(metadata);
    }
    body[count_offset..count_offset + 4].copy_from_slice(&count.to_be_bytes());
    let compressed = if body.len() >= 4096 {
        let bytes = zstd::bulk::compress(&body, 1).map_err(|_| invalid())?;
        (bytes.len() + 64 < body.len()).then_some(bytes)
    } else {
        None
    };
    let mut bytes = Vec::with_capacity(HEADER + compressed.as_ref().map_or(body.len(), Vec::len));
    bytes.extend_from_slice(MAGIC);
    bytes.push(u8::from(compressed.is_some()));
    bytes.extend_from_slice(&(body.len() as u32).to_be_bytes());
    bytes.extend_from_slice(compressed.as_deref().unwrap_or(&body));
    Ok(bytes)
}

fn take<'a>(bytes: &mut &'a [u8], length: usize) -> Result<&'a [u8]> {
    if length > bytes.len() {
        return Err(invalid());
    }
    let (head, tail) = bytes.split_at(length);
    *bytes = tail;
    Ok(head)
}
fn number(bytes: &mut &[u8]) -> Result<usize> {
    Ok(u32::from_be_bytes(take(bytes, 4)?.try_into().map_err(|_| invalid())?) as usize)
}
fn decode<T: Payload>(store: &ActorStore, bytes: &[u8], packed: bool) -> Result<T> {
    if !bytes.starts_with(MAGIC) {
        return serde_json::from_slice(bytes).map_err(|_| invalid());
    }
    if !packed || bytes.len() < HEADER || bytes.len() > MAX_BYTES + HEADER {
        return Err(invalid());
    }
    let length = u32::from_be_bytes(bytes[5..9].try_into().map_err(|_| invalid())?) as usize;
    if length > MAX_BYTES {
        return Err(invalid());
    }
    let decoded;
    let mut body = match bytes[4] {
        0 => &bytes[HEADER..],
        1 => {
            let mut decoder = zstd::bulk::Decompressor::new().map_err(|_| invalid())?;
            decoder
                .set_parameter(zstd::zstd_safe::DParameter::WindowLogMax(22))
                .map_err(|_| invalid())?;
            decoded = decoder
                .decompress(&bytes[HEADER..], length)
                .map_err(|_| invalid())?;
            &decoded
        }
        _ => return Err(invalid()),
    };
    if body.len() != length {
        return Err(invalid());
    }
    let metadata_len = number(&mut body)?;
    let value: T = serde_json::from_slice(take(&mut body, metadata_len)?).map_err(|_| invalid())?;
    let references = value.references();
    let count = number(&mut body)?;
    if count > references.len() {
        return Err(invalid());
    }
    let mut records = Vec::with_capacity(count);
    let mut seen = HashSet::new();
    for _ in 0..count {
        let index = number(&mut body)?;
        let reference = references.get(index).ok_or_else(invalid)?;
        reference.validate()?;
        let length = number(&mut body)?;
        let bytes = take(&mut body, length)?;
        if !seen.insert(index)
            || reference.size != length as u64
            || super::digest(bytes) != reference.hash
        {
            return Err(invalid());
        }
        records.push(bytes);
    }
    if !body.is_empty() {
        return Err(invalid());
    }
    // Verify the entire packet before making even cached content observable.
    // Native account authorization is checked by the caller before decoding;
    // only references in the typed Core payload can enter this Actor's cache.
    if !records.is_empty() {
        store.stage_transaction(|tx| {
            for record in records {
                blob::put(tx, ACTOR_ID, record)?;
            }
            Ok(())
        })?;
    }
    drop(references);
    Ok(value)
}
