//! A small Nostr relay for tests and local demos: addressable replacement, tag filters,
//! and knobs to imitate real relays' bad days (rate limits, size limits, "OK" without
//! storing, going offline). Not for production.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use futures_util::{SinkExt, StreamExt};
use nostr::prelude::*;
use tokio::net::TcpListener;
use tokio_websockets::{Message as WsMessage, ServerBuilder};

#[derive(Clone, Debug)]
pub struct RelayKnobs {
    /// Max EVENT messages per connection per minute before `rate-limited:` (None = no limit).
    pub events_per_minute: Option<u32>,
    /// Largest accepted event JSON (strfry's default is 64 KiB).
    pub max_event_bytes: usize,
    /// Answer OK but don't store (a relay that silently drops).
    pub drop_silently: bool,
    /// Refuse all connections (offline).
    pub offline: bool,
}

impl Default for RelayKnobs {
    fn default() -> Self {
        RelayKnobs { events_per_minute: None, max_event_bytes: 64 * 1024, drop_silently: false, offline: false }
    }
}

#[derive(Default)]
struct State {
    events: HashMap<(u16, PublicKey, String), Event>,
    received: u64,
    rate_limited: u64,
}

#[derive(Clone)]
pub struct TestRelay {
    pub addr: SocketAddr,
    state: Arc<Mutex<State>>,
    knobs: Arc<Mutex<RelayKnobs>>,
    /// Accepted events (stored or ephemeral), pushed to live subscriptions.
    bus: tokio::sync::broadcast::Sender<Event>,
}

impl TestRelay {
    /// Start on 127.0.0.1 with a random port (or the given address).
    pub async fn start(bind: Option<SocketAddr>) -> std::io::Result<TestRelay> {
        let listener = TcpListener::bind(bind.unwrap_or_else(|| "127.0.0.1:0".parse().expect("addr"))).await?;
        let relay = TestRelay {
            addr: listener.local_addr()?,
            state: Arc::new(Mutex::new(State::default())),
            knobs: Arc::new(Mutex::new(RelayKnobs::default())),
            bus: tokio::sync::broadcast::channel(1024).0,
        };
        let r = relay.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                if r.knobs.lock().expect("lock").offline {
                    drop(stream);
                    continue;
                }
                let r2 = r.clone();
                tokio::spawn(async move {
                    if let Ok((_, ws)) = ServerBuilder::new().accept(stream).await {
                        r2.serve(ws).await;
                    }
                });
            }
        });
        Ok(relay)
    }

    pub fn url(&self) -> String {
        format!("ws://{}", self.addr)
    }

    pub fn set_knobs(&self, k: RelayKnobs) {
        *self.knobs.lock().expect("lock") = k;
    }

    pub fn stored_events(&self) -> usize {
        self.state.lock().expect("lock").events.len()
    }

    pub fn received_events(&self) -> u64 {
        self.state.lock().expect("lock").received
    }

    pub fn rate_limited_count(&self) -> u64 {
        self.state.lock().expect("lock").rate_limited
    }

    async fn serve<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(&self, mut ws: tokio_websockets::WebSocketStream<S>) {
        let mut window_start = Instant::now();
        let mut in_window = 0u32;
        let mut live: HashMap<String, Vec<Filter>> = HashMap::new();
        let mut bus = self.bus.subscribe();
        loop {
            let replies: Vec<String> = tokio::select! {
                msg = ws.next() => {
                    let Some(Ok(msg)) = msg else { return };
                    if self.knobs.lock().expect("lock").offline {
                        return;
                    }
                    let Some(text) = msg.as_text() else { continue };
                    match ClientMessage::from_json(text) {
                        Ok(ClientMessage::Event(ev)) => {
                            let ev = ev.into_owned();
                            let knobs = self.knobs.lock().expect("lock").clone();
                            let mut st = self.state.lock().expect("lock");
                            st.received += 1;
                            if window_start.elapsed().as_secs() >= 60 {
                                window_start = Instant::now();
                                in_window = 0;
                            }
                            in_window += 1;
                            let reply = if knobs.events_per_minute.is_some_and(|l| in_window > l) {
                                st.rate_limited += 1;
                                (false, "rate-limited: slow down".to_string())
                            } else if text.len() > knobs.max_event_bytes {
                                (false, "invalid: event too large".to_string())
                            } else if ev.verify().is_err() {
                                (false, "invalid: bad signature".to_string())
                            } else {
                                if !knobs.drop_silently {
                                    let k = ev.kind.as_u16();
                                    let ephemeral = (20000..30000).contains(&k);
                                    if !ephemeral {
                                        let d = ev.tags.identifier().unwrap_or_default();
                                        let key = (k, ev.pubkey, d);
                                        let newer = st.events.get(&key).is_none_or(|old| ev.created_at > old.created_at || (ev.created_at == old.created_at && ev.id < old.id));
                                        if newer {
                                            st.events.insert(key, ev.clone());
                                        }
                                    }
                                    let _ = self.bus.send(ev.clone());
                                }
                                (true, String::new())
                            };
                            vec![RelayMessage::ok(ev.id, reply.0, reply.1).as_json()]
                        }
                        Ok(ClientMessage::Req { subscription_id, filters }) => {
                            let filters: Vec<Filter> = filters.into_iter().map(|f| f.into_owned()).collect();
                            let st = self.state.lock().expect("lock");
                            let mut out: Vec<String> = st
                                .events
                                .values()
                                .filter(|e| filters.iter().any(|f| f.match_event(e, MatchEventOptions::new())))
                                .map(|e| RelayMessage::event(subscription_id.clone().into_owned(), e.clone()).as_json())
                                .collect();
                            out.push(RelayMessage::eose(subscription_id.clone().into_owned()).as_json());
                            live.insert(subscription_id.as_str().to_string(), filters);
                            out
                        }
                        Ok(ClientMessage::Close(sid)) => {
                            live.remove(sid.as_str());
                            vec![]
                        }
                        _ => vec![],
                    }
                },
                ev = bus.recv() => {
                    let Ok(ev) = ev else { continue };
                    live.iter()
                        .filter(|(_, fs)| fs.iter().any(|f| f.match_event(&ev, MatchEventOptions::new())))
                        .map(|(sid, _)| RelayMessage::event(SubscriptionId::new(sid), ev.clone()).as_json())
                        .collect()
                }
            };
            for r in replies {
                if ws.send(WsMessage::text(r)).await.is_err() {
                    return;
                }
            }
        }
    }
}
