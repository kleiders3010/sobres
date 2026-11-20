//! Read-only check of relay-discovery data on public relays (NIP-66 monitors, NIP-65 lists).
use std::collections::{HashMap, HashSet};
use kt_net::nostr::{NostrPolicy, RelayConn};
use nostr::prelude::*;

#[tokio::main]
async fn main() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let seeds = ["wss://relay.damus.io", "wss://nos.lol"];
    let since = Timestamp::from_secs(Timestamp::now().as_secs() - 3 * 86_400);
    for s in seeds {
        let r = RelayConn::new(s, NostrPolicy::default());
        let mon = r.query(vec![Filter::new().kind(Kind::Custom(30166)).since(since).limit(300)]).await.unwrap_or_default();
        let mut urls: HashSet<String> = HashSet::new();
        let mut monitors: HashSet<String> = HashSet::new();
        let mut tagkinds: HashMap<String, usize> = HashMap::new();
        for e in &mon {
            monitors.insert(e.pubkey.to_hex());
            if let Some(d) = e.tags.identifier() { urls.insert(d); }
            for t in e.tags.iter() { *tagkinds.entry(t.kind().to_string()).or_default() += 1; }
        }
        let lists = r.query(vec![Filter::new().kind(Kind::Custom(10002)).since(since).limit(300)]).await.unwrap_or_default();
        let mut listed: HashMap<String, usize> = HashMap::new();
        for e in &lists {
            for t in e.tags.iter().filter(|t| t.kind() == "r") {
                if let Some(u) = t.content() { *listed.entry(u.trim_end_matches('/').to_string()).or_default() += 1; }
            }
        }
        let mut top: Vec<_> = listed.into_iter().collect();
        top.sort_by(|a, b| b.1.cmp(&a.1));
        let mut tk: Vec<_> = tagkinds.into_iter().collect();
        tk.sort_by(|a, b| b.1.cmp(&a.1));
        println!("{s}: NIP-66 events {} from {} monitors, {} distinct relays; tag kinds {:?}", mon.len(), monitors.len(), urls.len(), &tk[..tk.len().min(10)]);
        println!("  NIP-65 lists {}, top relays: {:?}", lists.len(), &top[..top.len().min(12)]);
        if let Some(e) = mon.first() { println!("  sample 30166 tags: {:?}", e.tags.iter().take(8).map(|t| t.as_slice().to_vec()).collect::<Vec<_>>()); }
    }
}
