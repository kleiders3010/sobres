//! Capability-based mailbox interface (DESIGN §3).
//!
//! A mailbox stores **slots**: one replaceable blob per (topic, writer). Each backend
//! declares what it can do, so the sync driver never assumes one backend's semantics.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A write budget: sustained rate plus burst.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RateBudget {
    pub per_minute: f64,
    pub burst: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Capabilities {
    /// A new write replaces the writer's previous slot (bounded storage per writer).
    pub replace: bool,
    /// Largest slot blob accepted (after the backend's own chunking).
    pub max_slot: usize,
    /// Slots disappear after this long unless rewritten.
    pub ttl: Option<Duration>,
    /// Readers must prove membership to read (false: anyone who knows the topic can).
    pub read_auth: bool,
    /// Can push new slots to a subscribed reader.
    pub subscribe: bool,
    /// Writes per unit (per relay for Nostr), shared across all groups.
    pub write_budget: Option<RateBudget>,
}

/// One slot to write.
#[derive(Clone, Debug)]
pub struct SlotWrite {
    pub topic: [u8; 32],
    /// Seed for the backend's writer identity (per device, group and epoch).
    pub writer_seed: [u8; 32],
    pub blob: Vec<u8>,
}

/// One slot read back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotRead {
    pub topic: [u8; 32],
    /// The backend-level writer identity (e.g. Nostr pubkey).
    pub writer: [u8; 32],
    /// Writer's version (e.g. creation time); higher is newer.
    pub version: u64,
    pub blob: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteOutcome {
    /// Stored on at least `stored` of `tried` units (relays).
    Stored { stored: usize, tried: usize },
    /// Nothing stored because of rate limits or the local budget; try again later.
    Deferred { retry_after: Duration },
    /// Refused for a reason retrying won't fix (too large, policy).
    Refused(String),
    /// No unit reachable.
    Unavailable,
}

pub trait Mailbox: Send + Sync {
    fn name(&self) -> String;
    fn capabilities(&self) -> Capabilities;
    /// The backend-level writer identity a seed maps to (as in [`SlotRead::writer`]).
    fn writer_id(&self, writer_seed: &[u8; 32]) -> [u8; 32];
    fn write<'a>(&'a self, w: &'a SlotWrite) -> BoxFut<'a, WriteOutcome>;
    /// Latest slot of every writer under these topics.
    fn read<'a>(&'a self, topics: &'a [[u8; 32]]) -> BoxFut<'a, Vec<SlotRead>>;
}
