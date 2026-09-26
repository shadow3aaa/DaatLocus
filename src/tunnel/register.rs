//! Quick-tunnel registration, transcribed from cloudflared's
//! `cmd/cloudflared/tunnel/quick_tunnel.go`.
//!
//! cloudflared `POST`s to `https://api.trycloudflare.com/tunnel` and reads the
//! assigned hostname out of the JSON envelope's `result.hostname`. The response
//! is treated as untrusted input: it is length-capped, decoded with serde, and
//! the hostname is validated against the `*.trycloudflare.com` suffix before it
//! is ever handed to the rest of the process.

use serde::Deserialize;

use super::error::TunnelError;

/// Where cloudflared registers anonymous quick tunnels.
pub const QUICK_TUNNEL_REGISTER_URL: &str = "https://api.trycloudflare.com/tunnel";
/// The only hostname suffix a quick tunnel may return.
pub const QUICK_TUNNEL_HOSTNAME_SUFFIX: &str = ".trycloudflare.com";
/// Upper bound on a registration response body (untrusted input).
pub const MAX_REGISTER_RESPONSE_BYTES: usize = 256 * 1024;
/// Upper bound on an accepted hostname.
pub const MAX_HOSTNAME_LEN: usize = 253;

/// The credentials a quick tunnel hands back to the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickTunnelCredentials {
    /// e.g. `calm-river-1234.trycloudflare.com`.
    pub hostname: String,
    /// Cloudflare's opaque tunnel id (may be empty for legacy responses).
    pub tunnel_id: String,
}

#[derive(Debug, Deserialize)]
struct QuickTunnelResponse {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    result: QuickTunnelResult,
    #[serde(default)]
    errors: Vec<QuickTunnelApiError>,
}

#[derive(Debug, Default, Deserialize)]
struct QuickTunnelResult {
    #[serde(default)]
    id: String,
    #[serde(default)]
    hostname: String,
}

#[derive(Debug, Deserialize)]
struct QuickTunnelApiError {
    #[serde(default)]
    message: String,
}

/// Decode and validate a registration response body. Pure and total: never
/// panics and never allocates unboundedly.
pub fn parse_quick_tunnel_response(body: &[u8]) -> Result<QuickTunnelCredentials, TunnelError> {
    if body.len() > MAX_REGISTER_RESPONSE_BYTES {
        return Err(TunnelError::RegisterRejected(format!(
            "registration response exceeded {MAX_REGISTER_RESPONSE_BYTES} bytes"
        )));
    }

    let decoded: QuickTunnelResponse = serde_json::from_slice(body).map_err(|err| {
        TunnelError::RegisterRejected(format!("malformed registration response: {err}"))
    })?;

    if !decoded.success {
        let detail = decoded
            .errors
            .iter()
            .map(|error| error.message.trim())
            .find(|message| !message.is_empty())
            .unwrap_or("cloudflare rejected the registration");
        return Err(TunnelError::RegisterRejected(detail.to_string()));
    }

    let hostname = validate_hostname(&decoded.result.hostname)?;
    Ok(QuickTunnelCredentials {
        hostname,
        tunnel_id: decoded.result.id,
    })
}

/// Validate that `host` is a plausible quick-tunnel hostname.
pub fn validate_hostname(host: &str) -> Result<String, TunnelError> {
    let trimmed = host.trim();
    if trimmed.is_empty() {
        return Err(TunnelError::RegisterRejected(
            "registration response had no hostname".to_string(),
        ));
    }
    if trimmed.len() > MAX_HOSTNAME_LEN {
        return Err(TunnelError::RegisterRejected(
            "registered hostname is too long".to_string(),
        ));
    }
    if trimmed
        .chars()
        .any(|ch| ch.is_whitespace() || ch.is_control() || ch == '/' || ch == '@')
    {
        return Err(TunnelError::RegisterRejected(
            "registered hostname contains invalid characters".to_string(),
        ));
    }
    let lower = trimmed.to_ascii_lowercase();
    if !lower.ends_with(QUICK_TUNNEL_HOSTNAME_SUFFIX) {
        return Err(TunnelError::RegisterRejected(format!(
            "registered hostname is not a quick tunnel ({trimmed})"
        )));
    }
    if lower
        .trim_end_matches(QUICK_TUNNEL_HOSTNAME_SUFFIX)
        .is_empty()
    {
        return Err(TunnelError::RegisterRejected(
            "registered hostname has an empty label".to_string(),
        ));
    }
    Ok(lower)
}

/// Register a fresh quick tunnel with Cloudflare.
pub async fn register_quick_tunnel(
    client: &reqwest::Client,
) -> Result<QuickTunnelCredentials, TunnelError> {
    let response = client
        .post(QUICK_TUNNEL_REGISTER_URL)
        .header(reqwest::header::CONTENT_LENGTH, "0")
        .send()
        .await
        .map_err(map_reqwest_error)?;

    if let Some(length) = response.content_length()
        && length as usize > MAX_REGISTER_RESPONSE_BYTES
    {
        return Err(TunnelError::RegisterRejected(format!(
            "registration response announced {length} bytes"
        )));
    }

    let status = response.status();
    let body = response.bytes().await.map_err(map_reqwest_error)?;
    if !status.is_success() {
        return Err(TunnelError::RegisterRejected(format!(
            "cloudflare returned HTTP {status}"
        )));
    }
    parse_quick_tunnel_response(&body)
}

fn map_reqwest_error(err: reqwest::Error) -> TunnelError {
    if err.is_timeout() {
        TunnelError::ConnectTimeout(format!("quick tunnel registration: {err}"))
    } else if err.is_connect() {
        TunnelError::DnsFailed(format!("quick tunnel registration: {err}"))
    } else {
        TunnelError::Internal(format!("quick tunnel registration: {err}"))
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_REGISTER_RESPONSE_BYTES, parse_quick_tunnel_response, validate_hostname};
    use crate::tunnel::error::TunnelErrorCode;

    #[test]
    fn parses_successful_registration() {
        let body = br#"{"success":true,"result":{"id":"abc","hostname":"calm-river-1234.trycloudflare.com"}}"#;
        let credentials = parse_quick_tunnel_response(body).expect("parse");
        assert_eq!(credentials.hostname, "calm-river-1234.trycloudflare.com");
        assert_eq!(credentials.tunnel_id, "abc");
    }

    #[test]
    fn rejects_failed_registration_with_message() {
        let body = br#"{"success":false,"errors":[{"message":"quota exceeded"}]}"#;
        let error = parse_quick_tunnel_response(body).expect_err("rejected");
        assert_eq!(error.code(), TunnelErrorCode::RegisterRejected);
        assert!(error.message().contains("quota exceeded"));
    }

    #[test]
    fn rejects_hostname_outside_trycloudflare() {
        let body = br#"{"success":true,"result":{"hostname":"evil.example.com"}}"#;
        let error = parse_quick_tunnel_response(body).expect_err("rejected");
        assert_eq!(error.code(), TunnelErrorCode::RegisterRejected);
    }

    #[test]
    fn rejects_oversized_body() {
        let body = vec![b' '; MAX_REGISTER_RESPONSE_BYTES + 1];
        assert!(parse_quick_tunnel_response(&body).is_err());
    }

    #[test]
    fn validate_hostname_lowercases_and_trims() {
        assert_eq!(
            validate_hostname("  Foo-Bar.trycloudflare.com ").expect("ok"),
            "foo-bar.trycloudflare.com"
        );
    }

    #[test]
    fn fuzz_like_sweep_never_panics() {
        let seeds: [&[u8]; 12] = [
            b"",
            b"{",
            b"null",
            b"[]",
            b"{\"success\":true}",
            b"{\"success\":true,\"result\":{}}",
            b"{\"success\":1}",
            &[0xff, 0xfe, 0xfd],
            b"{\"success\":true,\"result\":{\"hostname\":\"\"}}",
            b"{\"success\":true,\"result\":{\"hostname\":\"a.trycloudflare.com\"}}",
            b"{\"success\":false}",
            b"{\"result\":{\"hostname\":\"x.trycloudflare.com\"}}",
        ];
        for seed in seeds {
            let _ = parse_quick_tunnel_response(seed);
        }
    }
}
