//! Helper relay: an iroh relay server embedded in the app (DESIGN §6/§7a, D49 option (a)).
//!
//! Desktops with a public IPv4 address or a UPnP router can relay live connections for
//! their groups' devices when those can't connect directly (E10: 5 of 13 NAT pairs,
//! including phone↔phone on carrier NAT, only ever connect through a relay).
//!
//! - **Access:** open (D51: desktops relay for strangers by default), or restricted to
//!   holders of a token (e.g. a per-group secret).
//! - **Per-client bandwidth cap** (E10: bulk relaying costs ~25 core-seconds per GB).
//! - **TLS without a CA** (decided 2026-09-25, replacing Let's Encrypt IP certificates):
//!   the relay presents a self-signed certificate made from the helper's endpoint key, and
//!   clients pin that key from the helper's signed announcement (`relay_tls`). Works on any
//!   port an automatic mapping gets. Plain HTTP stays available for LAN, tests and USB
//!   tunnels.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::sync::Arc;

use iroh_relay::server::{self as rs, AccessControl, ClientRateLimit, Limits, RelayConfig, ServerConfig};

#[derive(Clone, Debug)]
pub struct HelperRelayConfig {
    pub http_bind: SocketAddr,
    /// `None`: anyone may relay through us. `Some`: only clients presenting one of these.
    pub tokens: Option<HashSet<String>>,
    /// Per-client receive cap in bytes per second.
    pub client_bytes_per_sec: u32,
    /// Serve TLS with a certificate made from this endpoint secret (`relay_tls`), on
    /// `https_bind`. `None`: plain HTTP only.
    pub tls_secret: Option<[u8; 32]>,
    pub https_bind: SocketAddr,
}

impl Default for HelperRelayConfig {
    fn default() -> Self {
        HelperRelayConfig {
            http_bind: "127.0.0.1:0".parse().expect("addr"),
            tokens: None,
            client_bytes_per_sec: 512 * 1024,
            tls_secret: None,
            https_bind: "127.0.0.1:0".parse().expect("addr"),
        }
    }
}

#[derive(Debug)]
struct TokenGate(HashSet<String>);

impl AccessControl for TokenGate {
    async fn on_connect(&self, req: &rs::ClientRequest) -> rs::Access {
        match req.auth_token() {
            Some(t) if self.0.contains(t.as_str()) => rs::Access::Allow,
            _ => rs::Access::Deny { reason: Some("not allowed on this relay".into()) },
        }
    }
}

pub struct HelperRelay {
    server: rs::Server,
}

impl HelperRelay {
    pub async fn start(cfg: HelperRelayConfig) -> Result<HelperRelay, String> {
        let mut relay = RelayConfig::new(cfg.http_bind);
        let mut limits = Limits::default();
        let mut rl = ClientRateLimit::new(NonZeroU32::new(cfg.client_bytes_per_sec.max(1)).expect("non-zero"));
        rl.max_burst_bytes = NonZeroU32::new(cfg.client_bytes_per_sec.saturating_mul(4));
        limits.client_rx = Some(rl);
        relay.limits = limits;
        if let Some(t) = cfg.tokens {
            relay.access = Arc::new(TokenGate(t));
        }
        if let Some(secret) = cfg.tls_secret {
            let server_config = crate::relay_tls::server_config(&secret)?;
            relay.tls = Some(rs::TlsConfig::new(cfg.https_bind, rs::CertConfig::Manual { server_config }));
        }
        let mut sc = ServerConfig::default();
        sc.relay = Some(relay);
        let server = rs::Server::spawn(sc).await.map_err(|e| e.to_string())?;
        Ok(HelperRelay { server })
    }

    pub fn http_addr(&self) -> Option<SocketAddr> {
        self.server.http_addr()
    }

    /// The HTTPS address when serving TLS.
    pub fn https_addr(&self) -> Option<SocketAddr> {
        self.server.https_addr()
    }

    /// The URL clients put in their relay map: HTTPS when serving TLS, else plain HTTP.
    pub fn url(&self) -> Option<String> {
        match self.https_addr() {
            Some(a) => Some(format!("https://{a}")),
            None => self.http_addr().map(|a| format!("http://{a}")),
        }
    }

    pub async fn shutdown(self) {
        let _ = self.server.shutdown().await;
    }
}
