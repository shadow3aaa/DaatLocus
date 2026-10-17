//! Link-local service discovery is a hint; TLS enrollment pins decide trust.
use crate::stage::{Result, StageError, StageId};
use mdns_sd::{IfKind, ScopedIp, ServiceDaemon, ServiceEvent, ServiceInfo};
use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, SocketAddr, SocketAddrV6},
};

pub const SERVICE: &str = "_daat-locus._tcp.local.";
#[derive(Clone, Debug)]
pub struct Candidate {
    pub stage: StageId,
    pub addresses: Vec<SocketAddr>,
}
pub struct LanDiscovery {
    daemon: ServiceDaemon,
    events: flume::Receiver<ServiceEvent>,
    known: HashSet<StageId>,
    interfaces: HashSet<String>,
    own: StageId,
    services: parking_lot::Mutex<HashMap<String, StageId>>,
}
impl LanDiscovery {
    /// Discovery advertises no Actor or workspace metadata.
    pub fn start(
        own: StageId,
        address: SocketAddr,
        known: HashSet<StageId>,
        interfaces: Vec<String>,
    ) -> Result<Self> {
        if address.port() == 0 {
            return Err(StageError::Invalid("Invalid LAN discovery configuration"));
        }
        let daemon =
            ServiceDaemon::new().map_err(|_| StageError::Unavailable("mDNS socket unavailable"))?;
        if !interfaces.is_empty() {
            daemon
                .disable_interface(IfKind::All)
                .map_err(|_| StageError::Unavailable("mDNS interface selection failed"))?;
            for name in &interfaces {
                daemon
                    .enable_interface(IfKind::Name(name.clone()))
                    .map_err(|_| StageError::Unavailable("mDNS interface selection failed"))?;
            }
        } else {
            daemon
                .disable_interface(IfKind::LoopbackV4)
                .map_err(|_| StageError::Unavailable("mDNS interface selection failed"))?;
        }
        let properties = [("id", own.to_string()), ("protocol", "1".to_owned())];
        let info = ServiceInfo::new(
            SERVICE,
            &format!("dl-{own}"),
            &format!("dl-{own}.local."),
            if address.ip().is_unspecified() {
                String::new()
            } else {
                address.ip().to_string()
            },
            address.port(),
            &properties[..],
        )
        .map_err(|_| StageError::Invalid("Invalid mDNS registration"))?;
        let info = if address.ip().is_unspecified() {
            info.enable_addr_auto()
        } else {
            info
        };
        daemon
            .register(info)
            .map_err(|_| StageError::Unavailable("mDNS registration failed"))?;
        let events = daemon
            .browse(SERVICE)
            .map_err(|_| StageError::Unavailable("mDNS browsing failed"))?;
        Ok(Self {
            daemon,
            events,
            known,
            interfaces: interfaces.into_iter().collect(),
            own,
            services: parking_lot::Mutex::new(HashMap::new()),
        })
    }
    pub async fn next(&self) -> Result<Candidate> {
        loop {
            let event = self
                .events
                .recv_async()
                .await
                .map_err(|_| StageError::Unavailable("mDNS discovery stopped"))?;
            let info = match event {
                ServiceEvent::ServiceResolved(info) => info,
                ServiceEvent::ServiceRemoved(_, name) => {
                    if let Some(stage) = self.services.lock().remove(&name) {
                        return Ok(Candidate {
                            stage,
                            addresses: vec![],
                        });
                    }
                    continue;
                }
                _ => continue,
            };
            if info.get_property_val_str("protocol") != Some("1") {
                continue;
            }
            let Some(id) = info
                .get_property_val_str("id")
                .and_then(|id| uuid::Uuid::parse_str(id).ok())
                .map(StageId)
            else {
                continue;
            };
            if id == self.own || !self.known.contains(&id) {
                continue;
            }
            self.services
                .lock()
                .insert(info.get_fullname().to_owned(), id);
            let mut addresses = Vec::new();
            for address in info.get_addresses().iter().take(32) {
                let ip = address.to_ip_addr();
                if ip.is_unspecified() || ip.is_multicast() || ip.is_loopback() {
                    continue;
                }
                let socket = match address {
                    ScopedIp::V4(v4) => {
                        if !self.interfaces.is_empty()
                            && !v4
                                .interface_ids()
                                .iter()
                                .any(|id| self.interfaces.contains(&id.name))
                        {
                            continue;
                        }
                        SocketAddr::new(ip, info.get_port())
                    }
                    ScopedIp::V6(v6) => {
                        if !self.interfaces.is_empty()
                            && !self.interfaces.contains(&v6.scope_id().name)
                        {
                            continue;
                        }
                        SocketAddr::V6(SocketAddrV6::new(
                            *v6.addr(),
                            info.get_port(),
                            0,
                            v6.scope_id().index,
                        ))
                    }
                    _ => continue,
                };
                if !addresses.contains(&socket) {
                    addresses.push(socket);
                }
            }
            if !addresses.is_empty() {
                return Ok(Candidate {
                    stage: id,
                    addresses,
                });
            }
        }
    }
    pub fn update_known(&mut self, known: HashSet<StageId>) -> Result<()> {
        self.known = known;
        Ok(())
    }
}
impl Drop for LanDiscovery {
    fn drop(&mut self) {
        let _ = self.daemon.shutdown();
    }
}
pub async fn connect(address: SocketAddr) -> Result<tokio::net::TcpStream> {
    if address.ip().is_unspecified() || address.ip().is_multicast() {
        return Err(StageError::Invalid("Invalid LAN endpoint"));
    }
    let stream = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        tokio::net::TcpStream::connect(address),
    )
    .await
    .map_err(|_| StageError::Unavailable("LAN connection timed out"))??;
    stream.set_nodelay(true)?;
    Ok(stream)
}
pub fn is_local_candidate(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_private() || ip.is_link_local() || ip.is_loopback(),
        IpAddr::V6(ip) => ip.is_unique_local() || ip.is_unicast_link_local() || ip.is_loopback(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn service_removal_is_reported_instead_of_waiting_for_another_resolution() {
        let (sender, events) = flume::unbounded();
        let peer = StageId::new();
        let name = format!("dl-{peer}.{SERVICE}");
        let discovery = LanDiscovery {
            daemon: ServiceDaemon::new().unwrap(),
            events,
            known: HashSet::from([peer]),
            interfaces: HashSet::new(),
            own: StageId::new(),
            services: parking_lot::Mutex::new(HashMap::from([(name.clone(), peer)])),
        };
        sender
            .send(ServiceEvent::ServiceRemoved(SERVICE.into(), name))
            .unwrap();
        let candidate =
            tokio::time::timeout(std::time::Duration::from_millis(100), discovery.next())
                .await
                .unwrap()
                .unwrap();
        assert_eq!(candidate.stage, peer);
        assert!(candidate.addresses.is_empty());
    }
}
