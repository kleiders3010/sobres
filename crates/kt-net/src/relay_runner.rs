//! Relaying for others (D51): a desktop runs a helper relay and announces it, but only
//! when strangers can actually reach it, and without the user doing anything.
//!
//! Reachable means one of:
//! - the interface used to reach the internet has a **public IPv4 address**; or
//! - the router grants an **automatic TCP port mapping** (UPnP / NAT-PMP / PCP) whose
//!   external address is public (not a carrier-grade NAT or private range).
//!
//! Anything else (no mapping, carrier-grade NAT): the relay isn't announced and nobody is
//! bothered. The relay's TLS uses the endpoint key (`relay_tls`), so any port works.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::num::NonZeroU16;
use std::sync::Arc;
use std::time::Duration;

use crate::helper_relay::{HelperRelay, HelperRelayConfig};
use crate::p2p::P2p;

/// Whether other people's devices could reach this address from the internet.
pub fn is_public_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    let cgnat = o[0] == 100 && (o[1] & 0xc0) == 64; // 100.64.0.0/10
    let benchmarking = o[0] == 198 && (o[1] & 0xfe) == 18; // 198.18.0.0/15
    !(ip.is_private() || ip.is_loopback() || ip.is_link_local() || ip.is_unspecified() || ip.is_broadcast() || ip.is_documentation() || ip.is_multicast() || cgnat || benchmarking || o[0] == 0 || o[0] >= 240)
}

/// The local address used to reach the internet (a UDP `connect` sends nothing).
fn outbound_ipv4() -> Option<Ipv4Addr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("1.1.1.1:443").ok()?;
    match s.local_addr().ok()?.ip() {
        IpAddr::V4(v4) => Some(v4),
        IpAddr::V6(_) => None,
    }
}

/// How the relay is reachable, if it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reach {
    /// A public address on this machine.
    PublicInterface(SocketAddr),
    /// An automatic mapping on the router.
    Mapped(SocketAddr),
}

pub struct RelayForOthers {
    relay: HelperRelay,
    mapper: Option<portmapper::Client>,
    pub reach: Option<Reach>,
}

impl RelayForOthers {
    /// Start relaying for others. `bind` is where the relay listens (all interfaces in the
    /// app; loopback in tests). `assume` skips the reachability check and announces that
    /// address with the relay's port (tests only).
    pub async fn start(p2p: &Arc<P2p>, bind: IpAddr, assume: Option<IpAddr>) -> Result<RelayForOthers, String> {
        let cfg = HelperRelayConfig {
            http_bind: SocketAddr::new(bind, 0),
            https_bind: SocketAddr::new(bind, 0),
            tls_secret: Some(p2p.endpoint_secret()),
            ..Default::default()
        };
        let relay = HelperRelay::start(cfg).await?;
        let port = relay.https_addr().ok_or("relay has no HTTPS address")?.port();
        let mut mapper = None;
        let reach = match assume {
            Some(ip) => Some(Reach::PublicInterface(SocketAddr::new(ip, port))),
            None => match outbound_ipv4().filter(|ip| is_public_v4(*ip)) {
                Some(ip) => Some(Reach::PublicInterface(SocketAddr::new(IpAddr::V4(ip), port))),
                None => {
                    let m = portmapper::Client::new(portmapper::Config { protocol: portmapper::Protocol::Tcp, ..Default::default() });
                    m.update_local_port(NonZeroU16::new(port).ok_or("port 0")?);
                    m.procure_mapping();
                    let mut w = m.watch_external_address();
                    let got = tokio::time::timeout(Duration::from_secs(15), async {
                        loop {
                            if let Some(a) = *w.borrow_and_update() {
                                return Some(a);
                            }
                            if w.changed().await.is_err() {
                                return None;
                            }
                        }
                    })
                    .await
                    .ok()
                    .flatten();
                    mapper = Some(m);
                    got.filter(|a| is_public_v4(*a.ip())).map(|a| Reach::Mapped(SocketAddr::V4(a)))
                }
            },
        };
        let url = reach.as_ref().map(|r| match r {
            Reach::PublicInterface(a) | Reach::Mapped(a) => format!("https://{a}"),
        });
        // Use it ourselves too (pinned to our own key): our connections stay local, and
        // devices that can only use relays reach us through it. If the router can't loop
        // our public address back, iroh just marks it unreachable and uses our others.
        if let Some(u) = &url {
            let own = *iroh::SecretKey::from_bytes(&p2p.endpoint_secret()).public().as_bytes();
            let _ = p2p.use_helper_relay(u, own).await;
        }
        p2p.set_own_relay(url);
        Ok(RelayForOthers { relay, mapper, reach })
    }

    /// The URL announced to others (`None` when not reachable).
    pub fn announced_url(&self) -> Option<String> {
        self.reach.as_ref().map(|r| match r {
            Reach::PublicInterface(a) | Reach::Mapped(a) => format!("https://{a}"),
        })
    }

    /// Stop relaying and stop announcing it.
    pub async fn stop(self, p2p: &Arc<P2p>) {
        if let Some(u) = self.announced_url() {
            p2p.drop_helper_relay(&u).await;
        }
        p2p.set_own_relay(None);
        if let Some(m) = self.mapper {
            m.deactivate();
        }
        self.relay.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_addresses() {
        for ip in ["8.8.8.8", "186.130.71.139", "203.0.114.1"] {
            assert!(is_public_v4(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["10.0.0.1", "192.168.1.37", "172.16.5.5", "100.64.0.1", "100.127.255.254", "127.0.0.1", "169.254.1.1", "0.0.0.0", "192.0.2.1", "198.18.0.1", "255.255.255.255"] {
            assert!(!is_public_v4(ip.parse().unwrap()), "{ip}");
        }
    }
}
