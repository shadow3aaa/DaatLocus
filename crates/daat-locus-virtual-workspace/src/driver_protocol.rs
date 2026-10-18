//! Versioned, fixed-width device protocol shared with the separately built UMDF DLL.
//! No pointers, Rust layout, native handles, or JSON cross the device boundary.

pub const VERSION: u32 = 1;
pub const INTERFACE_GUID: u128 = 0x75781583_6fb2_4a53_a242_f1734fdde531;
// FILE_DEVICE_UNKNOWN, vendor function 0x800, METHOD_BUFFERED, FILE_READ|WRITE_ACCESS.
pub const CREATE: u32 = (0x22 << 16) | (3 << 14) | (0x800 << 2);
pub const QUERY: u32 = (0x22 << 16) | (3 << 14) | (0x801 << 2);
pub const MAX_MONITORS: usize = 8;
pub const REQUEST_BYTES: usize = 20;
pub const RESPONSE_BYTES: usize = 16;

pub fn request(container: [u8; 16]) -> [u8; REQUEST_BYTES] {
    let mut bytes = [0; REQUEST_BYTES];
    bytes[..4].copy_from_slice(&VERSION.to_le_bytes());
    bytes[4..].copy_from_slice(&container);
    bytes
}

pub fn parse_request(bytes: &[u8]) -> Option<[u8; 16]> {
    if bytes.len() != REQUEST_BYTES || bytes[..4] != VERSION.to_le_bytes() {
        return None;
    }
    bytes[4..].try_into().ok()
}

pub fn response(adapter: u64, target: u32) -> [u8; RESPONSE_BYTES] {
    let mut bytes = [0; RESPONSE_BYTES];
    bytes[..4].copy_from_slice(&VERSION.to_le_bytes());
    bytes[4..12].copy_from_slice(&adapter.to_le_bytes());
    bytes[12..].copy_from_slice(&target.to_le_bytes());
    bytes
}

pub fn parse_response(bytes: &[u8]) -> Option<(u64, u32)> {
    if bytes.len() != RESPONSE_BYTES || bytes[..4] != VERSION.to_le_bytes() {
        return None;
    }
    Some((
        u64::from_le_bytes(bytes[4..12].try_into().ok()?),
        u32::from_le_bytes(bytes[12..].try_into().ok()?),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_incompatible_or_truncated_device_messages() {
        assert_eq!(parse_request(&[0; REQUEST_BYTES]), None);
        assert_eq!(parse_response(&[1, 0, 0, 0]), None);
        let bytes = [1, 0, 0, 0, 8, 7, 6, 5, 4, 3, 2, 1, 42, 0, 0, 0];
        assert_eq!(parse_response(&bytes), Some((0x0102030405060708, 42)));
        assert_eq!(parse_response(&[bytes.as_slice(), &[0]].concat()), None);
    }
}
