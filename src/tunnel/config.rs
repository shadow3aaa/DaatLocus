//! Static configuration for a tunnel instance.
//!
//! Nothing here performs I/O, so the whole struct is exercised by unit tests
//! without touching the network.

use std::{net::SocketAddr, time::Duration};

use super::edge::EdgeConfig;

/// Concurrent QUIC connections cloudflared keeps to the edge.
pub const DEFAULT_TUNNEL_CONNECTIONS: usize = 4;
/// How long to wait for a single edge QUIC handshake before giving up.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Backoff cap for connection-level reconnect attempts.
pub const DEFAULT_RECONNECT_BACKOFF_CAP: Duration = Duration::from_secs(30);

/// Only quick tunnels (ephemeral `*.trycloudflare.com`) are supported; there is
/// deliberately no named-tunnel or config-file mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelMode {
    /// Anonymous quick tunnel registered on the fly.
    Quick,
}

/// Everything `Tunnel::start` needs. Built by the daemon from the share
/// manager: the target is the local daemon listener.
#[derive(Debug, Clone)]
pub struct TunnelConfig {
    /// Local address public traffic is forwarded to (`127.0.0.1:<port>`).
    pub target: SocketAddr,
    /// Whether tunnelling is permitted at all. Disabled tunnels fail closed
    /// with [`crate::tunnel::TunnelErrorCode::TunnelDisabled`] instead of
    /// silently producing an unreachable share link.
    pub enabled: bool,
    /// Tunnel registration mode.
    pub mode: TunnelMode,
    /// Edge discovery / TLS parameters.
    pub edge: EdgeConfig,
    /// Per-edge-connection handshake timeout.
    pub connect_timeout: Duration,
    /// Number of parallel QUIC connections to the edge.
    pub connection_count: usize,
    /// Upper bound on reconnect backoff.
    pub reconnect_backoff_cap: Duration,
}

impl TunnelConfig {
    /// A quick tunnel forwarding to `target` with cloudflared's defaults.
    pub fn quick(target: SocketAddr) -> Self {
        Self {
            target,
            enabled: true,
            mode: TunnelMode::Quick,
            edge: EdgeConfig::default(),
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            connection_count: DEFAULT_TUNNEL_CONNECTIONS,
            reconnect_backoff_cap: DEFAULT_RECONNECT_BACKOFF_CAP,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, str::FromStr};

    use super::{DEFAULT_TUNNEL_CONNECTIONS, TunnelConfig, TunnelMode};

    #[test]
    fn quick_defaults_match_cloudflared_shapes() {
        let target = SocketAddr::from_str("127.0.0.1:53825").expect("valid socket addr");
        let config = TunnelConfig::quick(target);
        assert_eq!(config.mode, TunnelMode::Quick);
        assert!(config.enabled);
        assert_eq!(config.connection_count, DEFAULT_TUNNEL_CONNECTIONS);
        assert_eq!(config.target, target);
        assert!(!config.edge.addresses.is_empty());
    }
}
