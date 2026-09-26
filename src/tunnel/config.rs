//! Static configuration for a tunnel instance.

use std::net::SocketAddr;

/// Everything `Tunnel::start` needs. Built by the share manager from the
/// local daemon listener.
#[derive(Debug, Clone)]
pub struct TunnelConfig {
    /// Local address public traffic is forwarded to (`127.0.0.1:<port>`).
    pub target: SocketAddr,
    /// Whether tunnelling is permitted at all. Disabled tunnels fail closed
    /// with [`crate::tunnel::TunnelErrorCode::TunnelDisabled`] instead of
    /// silently producing an unreachable share link.
    pub enabled: bool,
}

impl TunnelConfig {
    /// A quick tunnel forwarding to `target` with cloudflared's defaults.
    pub fn quick(target: SocketAddr) -> Self {
        Self {
            target,
            enabled: true,
        }
    }
}
