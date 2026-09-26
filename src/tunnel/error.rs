//! Typed errors for the temporary-session-share tunnel.
//!
//! The tunnel runs inside the Manager process, so a panic would take the whole
//! runtime down. Every failure path therefore flows through [`TunnelError`],
//! and every variant maps to a stable [`TunnelErrorCode`] that the share API
//! exposes as `code` + `message`.

/// Stable machine-readable error codes surfaced over the share API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelErrorCode {
    /// Edge hostname resolution failed.
    DnsFailed,
    /// QUIC connection to the edge timed out.
    ConnectTimeout,
    /// TLS handshake / certificate trust failed.
    TlsFailed,
    /// Cloudflare rejected the quick-tunnel registration.
    RegisterRejected,
    /// Every discovered colo was unreachable.
    NoColoAvailable,
    /// Tunnelling disabled (e.g. explicitly turned off).
    TunnelDisabled,
    /// PIN generation, memory allocation, or an unexpected internal failure.
    Internal,
}

impl TunnelErrorCode {
    /// The wire-format `code` string used by `POST /shares`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DnsFailed => "dns_failed",
            Self::ConnectTimeout => "connect_timeout",
            Self::TlsFailed => "tls_failed",
            Self::RegisterRejected => "register_rejected",
            Self::NoColoAvailable => "no_colo_available",
            Self::TunnelDisabled => "tunnel_disabled",
            Self::Internal => "internal",
        }
    }
}

/// A typed tunnel failure. Deliberately exhaustive so callers never need to
/// string-match an opaque error to decide how to react.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum TunnelError {
    #[error("edge hostname resolution failed: {0}")]
    DnsFailed(String),
    #[error("edge connection timed out: {0}")]
    ConnectTimeout(String),
    #[error("TLS handshake with the edge failed: {0}")]
    TlsFailed(String),
    #[error("quick tunnel registration was rejected: {0}")]
    RegisterRejected(String),
    #[error("no reachable cloudflare colo: {0}")]
    NoColoAvailable(String),
    #[error("tunnelling is disabled: {0}")]
    TunnelDisabled(String),
    #[error("internal tunnel error: {0}")]
    Internal(String),
}

impl TunnelError {
    /// The stable code for this error.
    pub const fn code(&self) -> TunnelErrorCode {
        match self {
            Self::DnsFailed(_) => TunnelErrorCode::DnsFailed,
            Self::ConnectTimeout(_) => TunnelErrorCode::ConnectTimeout,
            Self::TlsFailed(_) => TunnelErrorCode::TlsFailed,
            Self::RegisterRejected(_) => TunnelErrorCode::RegisterRejected,
            Self::NoColoAvailable(_) => TunnelErrorCode::NoColoAvailable,
            Self::TunnelDisabled(_) => TunnelErrorCode::TunnelDisabled,
            Self::Internal(_) => TunnelErrorCode::Internal,
        }
    }

    /// The human-readable portion of the error (without the category prefix).
    pub fn message(&self) -> &str {
        match self {
            Self::DnsFailed(message)
            | Self::ConnectTimeout(message)
            | Self::TlsFailed(message)
            | Self::RegisterRejected(message)
            | Self::NoColoAvailable(message)
            | Self::TunnelDisabled(message)
            | Self::Internal(message) => message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{TunnelError, TunnelErrorCode};

    #[test]
    fn every_variant_maps_to_its_code() {
        let cases = [
            (
                TunnelError::DnsFailed("x".into()),
                TunnelErrorCode::DnsFailed,
            ),
            (
                TunnelError::ConnectTimeout("x".into()),
                TunnelErrorCode::ConnectTimeout,
            ),
            (
                TunnelError::TlsFailed("x".into()),
                TunnelErrorCode::TlsFailed,
            ),
            (
                TunnelError::RegisterRejected("x".into()),
                TunnelErrorCode::RegisterRejected,
            ),
            (
                TunnelError::NoColoAvailable("x".into()),
                TunnelErrorCode::NoColoAvailable,
            ),
            (
                TunnelError::TunnelDisabled("x".into()),
                TunnelErrorCode::TunnelDisabled,
            ),
            (TunnelError::Internal("x".into()), TunnelErrorCode::Internal),
        ];
        for (error, code) in cases {
            assert_eq!(error.code(), code);
            assert!(!code.as_str().is_empty());
        }
    }

    #[test]
    fn codes_are_stable_strings() {
        assert_eq!(TunnelErrorCode::DnsFailed.as_str(), "dns_failed");
        assert_eq!(
            TunnelErrorCode::RegisterRejected.as_str(),
            "register_rejected"
        );
        assert_eq!(TunnelErrorCode::Internal.as_str(), "internal");
    }
}
