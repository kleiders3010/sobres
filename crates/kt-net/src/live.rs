//! Live mode while the app is open (DESIGN §6, §8 "App open: live on every layer").
//!
//! - Live Nostr subscriptions push members' new slot versions within seconds, so two
//!   open apps sync near-instantly without polling.
//! - On start, the device knocks (ephemeral Nostr event with its sealed whereabouts) in
//!   each group; members online right now learn its address and dial at once, so both
//!   sides dial each other at the same time (D49 (b): direct paths through our own
//!   signaling, no n0 needed).
//! - Topics change with key epochs: subscriptions are renewed when they do.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use crate::nostr::NostrMailbox;
use crate::p2p::P2p;
use crate::sync::Syncer;

fn window_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
        / kt_engine::replica::LAN_WINDOW_SECS
}

/// Knock in every group (only useful with P2P: it announces where to dial us).
pub async fn knock_all(syncer: &Syncer, mb: &NostrMailbox) -> usize {
    let entries = syncer.replica().lock().expect("replica lock").whereabouts_entries(window_now());
    let mut sent = 0;
    for (_, topic, tag, sealed) in entries {
        let mut payload = tag.to_vec();
        payload.extend_from_slice(&sealed);
        sent += mb.knock(&topic, &payload).await;
    }
    sent
}

/// Run until `stop` fires.
pub async fn run_live(syncer: Arc<Syncer>, mb: Arc<NostrMailbox>, p2p: Option<Arc<P2p>>, mut stop: watch::Receiver<bool>) {
    if p2p.is_some() {
        knock_all(&syncer, &mb).await;
    }
    loop {
        let topics: Vec<[u8; 32]> = syncer.replica().lock().expect("replica lock").read_topics().into_iter().map(|t| t.topic).collect();
        let mut slots = mb.subscribe_slots(&topics).await;
        let mut knocks = mb.subscribe_knocks(&topics).await;
        let mut check = tokio::time::interval(Duration::from_secs(60));
        check.tick().await;
        loop {
            tokio::select! {
                _ = stop.changed() => return,
                s = slots.recv() => {
                    let Some(s) = s else { break };
                    syncer.apply_incoming(vec![s]).await;
                }
                k = knocks.recv() => {
                    let Some((_, payload)) = k else { break };
                    if payload.len() <= 16 {
                        continue;
                    }
                    let tag: [u8; 16] = payload[..16].try_into().expect("16");
                    let learned = syncer.replica().lock().expect("replica lock").open_lan_entry(window_now(), &tag, &payload[16..]);
                    if let (true, Some(p)) = (learned, &p2p) {
                        p.sync_round().await;
                    }
                }
                _ = check.tick() => {
                    let now: Vec<[u8; 32]> = syncer.replica().lock().expect("replica lock").read_topics().into_iter().map(|t| t.topic).collect();
                    if now != topics {
                        break; // resubscribe with the new topics
                    }
                }
            }
        }
    }
}

/// Live mode without Nostr: whenever this device changes something, exchange with every
/// reachable member at once (they apply and acknowledge), plus a slow background round.
/// Two open apps stay in sync within about a second over direct or relayed connections.
pub async fn run_p2p_live(syncer: Arc<Syncer>, p2p: Arc<P2p>, mut stop: watch::Receiver<bool>) {
    let mut seen = syncer.replica().lock().expect("replica lock").local_changes();
    let mut last_round = std::time::Instant::now() - Duration::from_secs(3600);
    loop {
        let now = syncer.replica().lock().expect("replica lock").local_changes();
        if now != seen || last_round.elapsed() > Duration::from_secs(60) {
            seen = now;
            p2p.sync_round().await;
            last_round = std::time::Instant::now();
        }
        tokio::select! {
            _ = stop.changed() => return,
            _ = tokio::time::sleep(Duration::from_millis(300)) => {}
        }
    }
}
