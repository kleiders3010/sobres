//! Soft size limits (D4): split large payloads into pieces instead of rejecting them,
//! throttle heavy authors instead of refusing them, and only hard-stop at absurd sizes.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::ids::DeviceId;
use crate::{Error, Result};

/// Size of one transport piece. Chosen to fit Nostr's ~47 KiB payload budget per event
/// (64 KiB JSON after base64, D12) with room for headers.
pub const PIECE_SIZE: usize = 32 * 1024;
/// Hard stop: a single logical payload above this is refused with a clear error (D4).
pub const ABSURD_SIZE: usize = 64 * 1024 * 1024;
/// Text above this becomes a text attachment (blob) instead of inline CRDT text (D4).
pub const TEXT_ATTACHMENT_THRESHOLD: usize = 100 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Piece {
    /// Hash of the whole payload; identifies which payload this piece belongs to.
    pub payload_hash: [u8; 32],
    pub index: u32,
    pub count: u32,
    pub data: Vec<u8>,
}

/// Split a payload into pieces of at most [`PIECE_SIZE`] bytes.
pub fn split(payload: &[u8]) -> Result<Vec<Piece>> {
    if payload.len() > ABSURD_SIZE {
        return Err(Error::Rejected(format!(
            "payload of {} MB is too large",
            payload.len() / (1024 * 1024)
        )));
    }
    let payload_hash = crate::crypto::hash("kt/payload/v1", payload);
    let chunks: Vec<&[u8]> = if payload.is_empty() { vec![&[][..]] } else { payload.chunks(PIECE_SIZE).collect() };
    let count = chunks.len() as u32;
    Ok(chunks
        .into_iter()
        .enumerate()
        .map(|(i, c)| Piece { payload_hash, index: i as u32, count, data: c.to_vec() })
        .collect())
}

/// Collects pieces (in any order, with duplicates) and yields complete payloads.
#[derive(Default)]
pub struct Reassembler {
    partial: HashMap<[u8; 32], (u32, HashMap<u32, Vec<u8>>, Instant)>,
}

impl Reassembler {
    /// Maximum number of payloads being assembled at once (memory bound).
    const MAX_PARTIAL: usize = 256;

    pub fn add(&mut self, p: Piece) -> Result<Option<Vec<u8>>> {
        if p.count == 0 || p.index >= p.count || (p.count as usize) * PIECE_SIZE > ABSURD_SIZE + PIECE_SIZE {
            return Err(Error::Rejected("invalid piece header".into()));
        }
        if p.data.len() > PIECE_SIZE {
            return Err(Error::Rejected("piece too large".into()));
        }
        if !self.partial.contains_key(&p.payload_hash) && self.partial.len() >= Self::MAX_PARTIAL {
            // Evict the oldest partial payload.
            if let Some(oldest) = self.partial.iter().min_by_key(|(_, v)| v.2).map(|(k, _)| *k) {
                self.partial.remove(&oldest);
            }
        }
        let entry = self.partial.entry(p.payload_hash).or_insert_with(|| (p.count, HashMap::new(), Instant::now()));
        if entry.0 != p.count {
            return Err(Error::Rejected("inconsistent piece count".into()));
        }
        entry.1.insert(p.index, p.data);
        if entry.1.len() as u32 == entry.0 {
            let (count, mut parts, _) = self.partial.remove(&p.payload_hash).expect("present");
            let mut out = Vec::new();
            for i in 0..count {
                out.extend_from_slice(&parts.remove(&i).expect("all present"));
            }
            if crate::crypto::hash("kt/payload/v1", &out) != p.payload_hash {
                return Err(Error::Rejected("reassembled payload hash mismatch".into()));
            }
            return Ok(Some(out));
        }
        Ok(None)
    }

    /// Drop partial payloads older than `max_age`.
    pub fn expire(&mut self, max_age: Duration) {
        let now = Instant::now();
        self.partial.retain(|_, v| now.duration_since(v.2) < max_age);
    }
}

/// Per-author token bucket: heavy authors are slowed, never rejected (D4).
pub struct Throttle {
    rate_bytes_per_sec: f64,
    burst_bytes: f64,
    buckets: HashMap<DeviceId, (f64, Instant)>,
}

impl Throttle {
    pub fn new(rate_bytes_per_sec: f64, burst_bytes: f64) -> Self {
        Throttle { rate_bytes_per_sec, burst_bytes, buckets: HashMap::new() }
    }

    /// How long to delay processing `bytes` from `author`. Zero means "process now".
    pub fn delay_for(&mut self, author: DeviceId, bytes: usize, now: Instant) -> Duration {
        let (tokens, last) = self.buckets.entry(author).or_insert((self.burst_bytes, now));
        let elapsed = now.duration_since(*last).as_secs_f64();
        *tokens = (*tokens + elapsed * self.rate_bytes_per_sec).min(self.burst_bytes);
        *last = now;
        *tokens -= bytes as f64;
        if *tokens >= 0.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(-*tokens / self.rate_bytes_per_sec)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_and_reassemble_any_order_with_duplicates() {
        let payload: Vec<u8> = (0..(PIECE_SIZE * 3 + 17)).map(|i| (i % 251) as u8).collect();
        let mut pieces = split(&payload).unwrap();
        assert_eq!(pieces.len(), 4);
        pieces.reverse();
        pieces.push(pieces[1].clone());
        let mut r = Reassembler::default();
        let mut got = None;
        for p in pieces {
            if let Some(v) = r.add(p).unwrap() {
                got = Some(v);
            }
        }
        assert_eq!(got.unwrap(), payload);
    }

    #[test]
    fn tampered_piece_detected() {
        let payload = vec![7u8; PIECE_SIZE + 10];
        let mut pieces = split(&payload).unwrap();
        pieces[1].data[0] ^= 1;
        let mut r = Reassembler::default();
        assert!(r.add(pieces[0].clone()).unwrap().is_none());
        assert!(r.add(pieces[1].clone()).is_err());
    }

    #[test]
    fn absurd_size_refused() {
        assert!(split(&vec![0u8; ABSURD_SIZE + 1]).is_err());
    }

    #[test]
    fn throttle_slows_heavy_author_only() {
        let mut t = Throttle::new(1000.0, 1000.0);
        let now = Instant::now();
        let heavy = DeviceId::random();
        let light = DeviceId::random();
        assert_eq!(t.delay_for(heavy, 500, now), Duration::ZERO);
        let d = t.delay_for(heavy, 1500, now);
        assert!(d >= Duration::from_millis(900));
        assert_eq!(t.delay_for(light, 100, now), Duration::ZERO);
    }
}
