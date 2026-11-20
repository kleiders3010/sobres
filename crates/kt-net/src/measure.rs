//! MEASUREMENT ONLY — not a feature. Answers one question for D23/D54: what does a
//! per-group network identity cost in battery and connections?
//!
//! A per-group identity means one iroh endpoint per group instead of one per device. The
//! cost that matters is keeping N endpoints alive instead of one: N home-relay
//! connections (TLS + keep-alives), N sets of network reports and port-mapping probes, N
//! address publications, and N reconnects every time the phone wakes or changes network.
//!
//! [`ExtraEndpoints`] starts N-1 such endpoints next to the real one, with the same
//! settings ([`crate::p2p::endpoint_builder`]: same relay map, n0 DNS, port mapping,
//! bind), each connecting to its home relay like the real one. They serve nothing and
//! dial nothing: their traffic is only the per-endpoint upkeep, which is the difference
//! being measured. Sync traffic per group would be the same with one identity or many.
//!
//! Off unless the measurement switch asks for it (Android: `MeasureReceiver`, reachable
//! only from `adb shell`; see `scripts/battery-measure.sh`). Nothing in the app turns it on.

use std::sync::Arc;

use iroh::{Endpoint, RelayUrl, SecretKey, TransportAddr};

use crate::p2p::{endpoint_builder, P2p};

/// Idle extra endpoints, kept alive until [`ExtraEndpoints::close`] (or dropped).
pub struct ExtraEndpoints {
    eps: Vec<Endpoint>,
}

impl ExtraEndpoints {
    /// Start `count` extra endpoints with `p2p`'s settings, plus the helper relays it uses
    /// right now. Keys are derived from the real endpoint's key and the index, so relays
    /// see the same IDs across restarts (as they would with real per-group identities).
    pub async fn start(p2p: &P2p, count: u32) -> Result<ExtraEndpoints, String> {
        let base = p2p.endpoint().secret_key().to_bytes();
        let mut eps: Vec<Endpoint> = Vec::new();
        for i in 0..count {
            let key = kt_engine::crypto::derive(&base, "kt/measure-endpoint/v1", &i.to_le_bytes()).0;
            let pins: crate::relay_tls::Pins = Default::default();
            let helper_relays = p2p.helper_relay_keys();
            for (url, k) in &helper_relays {
                if let Some(ip) = url.parse::<RelayUrl>().ok().and_then(|u| u.host_str().and_then(|h| h.trim_start_matches('[').trim_end_matches(']').parse().ok())) {
                    pins.write().expect("pins lock").insert(ip, *k);
                }
            }
            let ep = match endpoint_builder(p2p.config(), SecretKey::from_bytes(&key), pins, vec![])?.bind().await {
                Ok(ep) => ep,
                Err(e) => {
                    for ep in eps {
                        ep.close().await;
                    }
                    return Err(e.to_string());
                }
            };
            for (url, _) in helper_relays {
                if let Ok(u) = url.parse::<RelayUrl>() {
                    ep.insert_relay(u.clone(), Arc::new(iroh::RelayConfig::new(u, None))).await;
                }
            }
            eps.push(ep);
        }
        Ok(ExtraEndpoints { eps })
    }

    /// The endpoints themselves (for platform nudges from other threads).
    pub fn endpoints(&self) -> Vec<Endpoint> {
        self.eps.clone()
    }

    /// Their endpoint IDs.
    pub fn ids(&self) -> Vec<[u8; 32]> {
        self.eps.iter().map(|ep| *ep.id().as_bytes()).collect()
    }

    pub fn len(&self) -> usize {
        self.eps.len()
    }

    pub fn is_empty(&self) -> bool {
        self.eps.is_empty()
    }

    /// How many are connected to a home relay right now (what a real per-group endpoint
    /// needs to be reachable).
    pub fn online(&self) -> usize {
        self.eps.iter().filter(|ep| ep.addr().addrs.iter().any(|a| matches!(a, TransportAddr::Relay(_)))).count()
    }

    /// Wait until every extra endpoint has a home relay.
    pub async fn all_online(&self) {
        for ep in &self.eps {
            ep.online().await;
        }
    }

    /// Tell them about a network change, like the real endpoint ([`crate::p2p`] users call
    /// `network_change` on it): each re-checks its paths, as N real endpoints would.
    pub async fn network_change(&self) {
        for ep in &self.eps {
            ep.network_change().await;
        }
    }

    pub async fn close(self) {
        for ep in self.eps {
            ep.close().await;
        }
    }
}
