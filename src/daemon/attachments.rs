//! Attachment decoding and upload limits, independent of a conversation protocol.
use base64::Engine;

pub const MAX_IMAGE_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;
pub const MAX_FILE_ATTACHMENT_BYTES: usize = 32 * 1024 * 1024;

pub fn sanitize_attachment_extension(extension: &str) -> String {
    extension
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .take(12)
        .collect::<String>()
        .to_ascii_lowercase()
}

pub fn decode_attachment_data_url(data_url: &str) -> std::result::Result<Vec<u8>, String> {
    let parsed =
        parse_data_url(data_url).ok_or_else(|| "invalid attachment data URL".to_string())?;
    decode_parsed_data_url_payload(&parsed, "invalid base64 attachment", false)
}

pub fn decode_image_data_url(
    data_url: &str,
    expected_media_type: &str,
) -> std::result::Result<Vec<u8>, String> {
    let parsed = parse_data_url(data_url).ok_or_else(|| "invalid image data URL".to_string())?;
    if parsed.is_bare_payload {
        return decode_parsed_data_url_payload(&parsed, "invalid base64 image attachment", false);
    }
    let data_media_type = normalize_image_media_type(parsed.media_type)
        .ok_or_else(|| "unsupported image data URL media type".to_string())?;
    if data_media_type != expected_media_type {
        return Err("image data URL media type does not match attachment media type".to_string());
    }
    decode_parsed_data_url_payload(&parsed, "invalid base64 image attachment", false)
}

struct ParsedDataUrl<'a> {
    media_type: &'a str,
    base64: bool,
    payload: &'a str,
    /// Input had no `data:` scheme, so the whole string is a raw base64 payload.
    is_bare_payload: bool,
}

/// Parse a data URL into media type, parameters, the base64 flag, and payload.
///
/// A string without a `data:` prefix is treated as a bare base64 payload so
/// existing attachment callers keep accepting the inputs they already do.
/// Percent-decoding is only applied for non-base64 payloads; current callers
/// require base64 and therefore decode the payload as base64 instead.
fn parse_data_url(input: &str) -> Option<ParsedDataUrl<'_>> {
    let trimmed = input.trim();
    let Some(rest) = trimmed.strip_prefix("data:") else {
        return Some(ParsedDataUrl {
            media_type: "",
            base64: true,
            payload: trimmed,
            is_bare_payload: true,
        });
    };
    let (metadata, payload) = rest.split_once(',')?;
    let mut parts = metadata.split(';');
    let media_type = parts.next().unwrap_or("");
    let mut base64 = false;
    for part in parts {
        if part.eq_ignore_ascii_case("base64") {
            base64 = true;
        }
    }
    Some(ParsedDataUrl {
        media_type,
        base64,
        payload,
        is_bare_payload: false,
    })
}
/// Decode a parsed data URL into bytes.
///
/// Bare payloads and base64 data URLs are base64-decoded, matching the inputs
/// existing attachment callers already accept. A non-base64 data URL is
/// percent-decoded only when `decode_non_base64` is set; otherwise it is rejected.
fn decode_parsed_data_url_payload(
    parsed: &ParsedDataUrl<'_>,
    base64_error_prefix: &str,
    decode_non_base64: bool,
) -> std::result::Result<Vec<u8>, String> {
    if parsed.is_bare_payload || parsed.base64 {
        return decode_base64_payload(parsed.payload, base64_error_prefix);
    }
    if !decode_non_base64 {
        return Err("data URL must be base64 encoded".to_string());
    }
    percent_decode_data_url_payload(parsed.payload)
}
fn decode_base64_payload(
    payload: &str,
    error_prefix: &str,
) -> std::result::Result<Vec<u8>, String> {
    base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map_err(|err| format!("{error_prefix}: {err}"))
}

/// Percent-decode a non-base64 data-URL payload into bytes.
fn percent_decode_data_url_payload(payload: &str) -> std::result::Result<Vec<u8>, String> {
    let mut bytes = Vec::with_capacity(payload.len());
    let input = payload.as_bytes();
    let mut index = 0;
    while index < input.len() {
        if input[index] == b'%' {
            if index + 2 >= input.len() {
                return Err("invalid percent-encoding in data URL payload".to_string());
            }
            let hi = hex_nibble(input[index + 1])
                .ok_or_else(|| "invalid percent-encoding in data URL payload".to_string())?;
            let lo = hex_nibble(input[index + 2])
                .ok_or_else(|| "invalid percent-encoding in data URL payload".to_string())?;
            bytes.push((hi << 4) | lo);
            index += 3;
            continue;
        }
        bytes.push(input[index]);
        index += 1;
    }
    Ok(bytes)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub fn normalize_image_media_type(media_type: &str) -> Option<String> {
    match media_type.trim().to_ascii_lowercase().as_str() {
        "image/png" => Some("image/png".to_string()),
        "image/jpeg" | "image/jpg" => Some("image/jpeg".to_string()),
        "image/webp" => Some("image/webp".to_string()),
        "image/gif" => Some("image/gif".to_string()),
        _ => None,
    }
}
