//! Health observations shared within groups (DESIGN §7, §7a): what this device has seen
//! of helpers (reliability records) and Nostr relays (health, read-back), published to
//! its groups through the engine's signed per-member records, and what its groups'
//! other devices have seen, folded into this device's rankings.
//!
//! - Only a device's **own** observations are shared (no echo, no amplification).
//! - Others' observations count **less** than our own and **decay** with age.
//! - A member reporting that a helper returned data failing verification excludes it
//!   here too (members are trusted within their groups; outsiders can't publish).

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::helper::{HelperMailbox, Reliability};
use crate::nostr::{NostrMailbox, RelayHealth};
use crate::sync::Syncer;

/// Others' observations weigh this much against our own, halving every [`HALF_LIFE_MS`].
pub const SHARED_WEIGHT: f64 = 0.5;
pub const HALF_LIFE_MS: u64 = 3 * 86_400_000;
const MAX_HELPERS: usize = 64;
const MAX_RELAYS: usize = 32;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HelperObs {
    pub reachable: u32,
    pub unreachable: u32,
    pub withheld: u32,
    pub restarts: u32,
    pub excluded: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RelayObs {
    pub published_ok: u64,
    pub connect_failures: u64,
    pub readback_hits: u64,
    pub readback_misses: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HealthReport {
    pub helpers: Vec<([u8; 32], HelperObs)>,
    pub relays: Vec<(String, RelayObs)>,
}

/// This device's own observations, most informative first, size-capped.
pub fn build(hmb: Option<&HelperMailbox>, nostr: &NostrMailbox) -> HealthReport {
    let mut helpers: Vec<([u8; 32], HelperObs)> = hmb
        .map(|h| {
            h.own_records()
                .into_iter()
                .map(|(id, r)| (*id.as_bytes(), HelperObs { reachable: r.reachable, unreachable: r.unreachable, withheld: r.withheld, restarts: r.restarts, excluded: r.excluded }))
                .collect()
        })
        .unwrap_or_default();
    helpers.sort_by_key(|(_, o)| std::cmp::Reverse((o.excluded, o.reachable + o.unreachable + o.withheld)));
    helpers.truncate(MAX_HELPERS);
    let mut relays: Vec<(String, RelayObs)> = nostr
        .health()
        .into_iter()
        .filter(|(u, _)| u.len() <= 200)
        .map(|(u, h)| (u, RelayObs { published_ok: h.published_ok, connect_failures: h.connect_failures, readback_hits: h.readback_hits, readback_misses: h.readback_misses }))
        .collect();
    relays.sort_by_key(|(_, o)| std::cmp::Reverse(o.readback_hits + o.readback_misses + o.connect_failures));
    relays.truncate(MAX_RELAYS);
    HealthReport { helpers, relays }
}

fn weight(age_ms: u64) -> f64 {
    SHARED_WEIGHT * 0.5f64.powf(age_ms as f64 / HALF_LIFE_MS as f64)
}

fn scaled(n: u64, w: f64) -> u64 {
    (n as f64 * w).round() as u64
}

/// Fold other members' reports (report, age in ms) into this device's shared view.
pub fn apply(reports: &[(HealthReport, u64)], hmb: Option<&HelperMailbox>, nostr: &NostrMailbox) {
    let mut helpers: HashMap<[u8; 32], Reliability> = HashMap::new();
    let mut relays: HashMap<String, RelayHealth> = HashMap::new();
    for (rep, age) in reports {
        let w = weight(*age);
        for (k, o) in &rep.helpers {
            let e = helpers.entry(*k).or_default();
            e.reachable += scaled(o.reachable as u64, w) as u32;
            e.unreachable += scaled(o.unreachable as u64, w) as u32;
            e.withheld += scaled(o.withheld as u64, w) as u32;
            e.restarts += scaled(o.restarts as u64, w) as u32;
            e.excluded |= o.excluded;
        }
        for (u, o) in &rep.relays {
            let e = relays.entry(u.clone()).or_default();
            e.published_ok += scaled(o.published_ok, w);
            e.connect_failures += scaled(o.connect_failures, w);
            e.readback_hits += scaled(o.readback_hits, w);
            e.readback_misses += scaled(o.readback_misses, w);
        }
    }
    if let Some(h) = hmb {
        h.set_shared(helpers.into_iter().filter_map(|(k, r)| iroh::EndpointId::from_bytes(&k).ok().map(|i| (i, r))).collect());
    }
    nostr.set_shared_health(relays);
}

/// One round: publish our observations to our groups and take in theirs (newest report
/// per device across all groups).
pub fn exchange(syncer: &Arc<Syncer>, hmb: Option<&HelperMailbox>, nostr: &NostrMailbox) -> usize {
    let mine = build(hmb, nostr);
    let bytes = postcard::to_stdvec(&mine).unwrap_or_default();
    let replica = syncer.replica();
    let mut r = replica.lock().expect("replica lock");
    r.set_health_report((bytes.len() <= kt_engine::replica::health::MAX_HEALTH).then_some(bytes));
    let now = kt_engine::replica::now_ms();
    let mut newest: HashMap<kt_engine::DeviceId, (u64, Vec<u8>)> = HashMap::new();
    for g in r.groups() {
        for h in r.member_health(&g) {
            if newest.get(&h.device).is_none_or(|(t, _)| *t < h.created_ms) {
                newest.insert(h.device, (h.created_ms, h.data));
            }
        }
    }
    drop(r);
    let reports: Vec<(HealthReport, u64)> = newest
        .into_values()
        .filter_map(|(t, b)| postcard::from_bytes::<HealthReport>(&b).ok().map(|rep| (rep, now.saturating_sub(t))))
        .collect();
    apply(&reports, hmb, nostr);
    reports.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_reports_count_less() {
        assert!((weight(0) - SHARED_WEIGHT).abs() < 1e-9);
        assert!((weight(HALF_LIFE_MS) - SHARED_WEIGHT / 2.0).abs() < 1e-9);
        assert_eq!(scaled(10, weight(0)), 5);
        assert_eq!(scaled(10, weight(10 * HALF_LIFE_MS)), 0);
    }
}
