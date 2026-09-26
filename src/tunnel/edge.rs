//! Edge discovery and TLS parameters, mirroring cloudflared's `edge` package.
//!
//! cloudflared connects to the Cloudflare edge over QUIC with ALPN
//! `argotunnel`, using the colo hostname as both the SNI and the QUIC server
//! name. The address list below mirrors cloudflared's compiled-in region
//! defaults so a fresh install can connect before discovery completes.

use std::net::{SocketAddr, ToSocketAddrs};

use super::error::TunnelError;

/// The ALPN protocol id the Cloudflare edge speaks.
pub const ALPN_ARGOTUNNEL: &[u8] = b"argotunnel";
/// Default QUIC port used by the argotunnel edge.
pub const DEFAULT_EDGE_PORT: u16 = 7844;
/// Compiled-in region fallbacks (mirrors cloudflared's edge defaults).
pub const DEFAULT_EDGE_HOSTS: &[&str] = &["region1.v2.argotunnel.com", "region2.v2.argotunnel.com"];
/// Maximum accepted length of an edge-address token.
const MAX_EDGE_ADDRESS_LEN: usize = 253;

/// A single edge endpoint (`host` + `port`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EdgeAddress {
    /// Hostname or literal IP.
    pub host: String,
    /// QUIC port.
    pub port: u16,
}

impl EdgeAddress {
    /// Parse a `host` or `host:port` token. IPv6 literals must be bracketed
    /// (`[::1]:7844`). Returns a typed error for anything malformed rather than
    /// panicking: this parses values that may originate from a discovery
    /// response.
    pub fn parse(raw: &str) -> Result<Self, TunnelError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(TunnelError::NoColoAvailable(
                "empty edge address".to_string(),
            ));
        }
        if trimmed.len() > MAX_EDGE_ADDRESS_LEN {
            return Err(TunnelError::NoColoAvailable(
                "edge address too long".to_string(),
            ));
        }
        if trimmed
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control())
        {
            return Err(TunnelError::NoColoAvailable(
                "edge address contains whitespace or control characters".to_string(),
            ));
        }

        let (host, port) = if let Some(rest) = trimmed.strip_prefix('[') {
            let Some((host, tail)) = rest.split_once(']') else {
                return Err(TunnelError::NoColoAvailable(
                    "unterminated IPv6 edge address".to_string(),
                ));
            };
            let port = match tail {
                "" => DEFAULT_EDGE_PORT,
                other => match other.strip_prefix(':') {
                    Some(port) => parse_port(port)?,
                    None => {
                        return Err(TunnelError::NoColoAvailable(
                            "malformed IPv6 edge address".to_string(),
                        ));
                    }
                },
            };
            (host.to_string(), port)
        } else if let Some((host, port)) = trimmed.rsplit_once(':') {
            if host.contains(':') {
                // Bare IPv6 literal without brackets: every colon belongs to the
                // address, so keep the whole token and use the default port.
                (trimmed.to_string(), DEFAULT_EDGE_PORT)
            } else {
                (host.to_string(), parse_port(port)?)
            }
        } else {
            (trimmed.to_string(), DEFAULT_EDGE_PORT)
        };

        if host.is_empty() {
            return Err(TunnelError::NoColoAvailable(
                "edge address has an empty host".to_string(),
            ));
        }

        Ok(Self { host, port })
    }

    /// The SNI / QUIC server name for this edge.
    pub fn server_name(&self) -> &str {
        &self.host
    }

    /// Resolve the address to concrete socket addresses.
    pub fn resolve(&self) -> Result<Vec<SocketAddr>, TunnelError> {
        (self.host.as_str(), self.port)
            .to_socket_addrs()
            .map(|iter| iter.collect::<Vec<_>>())
            .map_err(|err| TunnelError::DnsFailed(format!("{}: {err}", self.host)))
    }
}

fn parse_port(raw: &str) -> Result<u16, TunnelError> {
    match raw.parse::<u16>() {
        Ok(0) | Err(_) => Err(TunnelError::NoColoAvailable(
            "edge address has an invalid port".to_string(),
        )),
        Ok(port) => Ok(port),
    }
}

/// Edge discovery / TLS configuration.
#[derive(Debug, Clone)]
pub struct EdgeConfig {
    /// Candidate edge addresses, tried in order.
    pub addresses: Vec<EdgeAddress>,
    /// ALPN protocols offered during the handshake.
    pub alpn: Vec<Vec<u8>>,
    /// Optional SNI override (defaults to the per-address host).
    pub server_name: Option<String>,
}

impl Default for EdgeConfig {
    fn default() -> Self {
        Self {
            addresses: DEFAULT_EDGE_HOSTS
                .iter()
                .filter_map(|host| EdgeAddress::parse(host).ok())
                .collect(),
            alpn: vec![ALPN_ARGOTUNNEL.to_vec()],
            server_name: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ALPN_ARGOTUNNEL, DEFAULT_EDGE_PORT, EdgeAddress, EdgeConfig};

    #[test]
    fn parses_bare_host_with_default_port() {
        let address = EdgeAddress::parse("region1.v2.argotunnel.com").expect("parse");
        assert_eq!(address.host, "region1.v2.argotunnel.com");
        assert_eq!(address.port, DEFAULT_EDGE_PORT);
        assert_eq!(address.server_name(), "region1.v2.argotunnel.com");
    }

    #[test]
    fn parses_host_and_port() {
        let address = EdgeAddress::parse("edge.example.com:9000").expect("parse");
        assert_eq!(address.host, "edge.example.com");
        assert_eq!(address.port, 9000);
    }

    #[test]
    fn parses_bracketed_ipv6() {
        let address = EdgeAddress::parse("[::1]:7844").expect("parse");
        assert_eq!(address.host, "::1");
        assert_eq!(address.port, 7844);
    }

    #[test]
    fn keeps_bare_ipv6_literal_without_port() {
        let address = EdgeAddress::parse("2001:db8::1").expect("parse");
        assert_eq!(address.host, "2001:db8::1");
        assert_eq!(address.port, DEFAULT_EDGE_PORT);
    }

    #[test]
    fn rejects_clearly_malformed_input() {
        for raw in ["", "   ", "a b", "host:0", "[::1", "host:70000", "[::1]:"] {
            assert!(
                EdgeAddress::parse(raw).is_err(),
                "expected {raw:?} to be rejected"
            );
        }
        // A bare IPv6 literal without a port is legitimate and keeps the
        // default port.
        assert!(EdgeAddress::parse("2001:db8::1").is_ok());
    }

    #[test]
    fn fuzz_like_sweep_never_panics() {
        let seeds = [
            "",
            ":",
            "::",
            "[]",
            "[]:",
            "[::]",
            "[::]:",
            "a:",
            ":1",
            "\u{0}",
            "\t",
            "host:65535",
            "host:65536",
            "-1",
            "1.2.3.4:5",
            "[::1]:",
            "nested[brackets]",
        ];
        for seed in seeds {
            // Must never panic; either Ok or Err is acceptable.
            let _ = EdgeAddress::parse(seed);
        }
    }

    #[test]
    fn default_config_offers_argotunnel_alpn() {
        let config = EdgeConfig::default();
        assert_eq!(config.alpn, vec![ALPN_ARGOTUNNEL.to_vec()]);
        assert_eq!(config.addresses.len(), super::DEFAULT_EDGE_HOSTS.len());
    }
}
