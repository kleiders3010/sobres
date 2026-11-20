//! A small mutation fuzzer that runs on stable Rust, in ordinary tests (DESIGN §9: every
//! parser of untrusted input is fuzzed).
//!
//! Each target starts from **valid** inputs made by the real code (so mutations reach
//! deep into the parsers, past magic bytes and tags) and applies random byte-level
//! mutations. The property is simple: no input may panic or take unreasonably long.
//! A failing input is saved to `target/fuzz-failures/` and reported, so it can be
//! replayed with [`replay`].
//!
//! - `KT_FUZZ_ITERS`: iterations per target (default [`DEFAULT_ITERS`]; long runs: 1e6).
//! - `KT_FUZZ_SEED`: the random seed (default fixed, so runs are reproducible).
//!
//! No coverage guidance (that needs nightly `cargo fuzz`); the targets are plain
//! `Fn(&[u8])`, so they can be reused by `cargo fuzz` later unchanged.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub const DEFAULT_ITERS: u64 = 2_000;
const MAX_LEN: usize = 256 * 1024;
/// One input taking longer than this is reported as a hang.
const SLOW: Duration = Duration::from_secs(5);

/// xorshift64*: tiny and deterministic.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.max(1))
    }
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next() % n as u64) as usize }
    }
}

const INTERESTING: [u8; 9] = [0x00, 0x01, 0x02, 0x7f, 0x80, 0x81, 0xfe, 0xff, 0x40];

/// One random edit (the classic set: bit flips, interesting bytes, inserts, deletes,
/// duplication, splicing with another seed, truncation and varint-style runs).
pub fn mutate(rng: &mut Rng, input: &mut Vec<u8>, seeds: &[Vec<u8>]) {
    let len = input.len();
    match rng.below(10) {
        0 if len > 0 => {
            let i = rng.below(len);
            input[i] ^= 1 << rng.below(8);
        }
        1 if len > 0 => {
            let i = rng.below(len);
            input[i] = INTERESTING[rng.below(INTERESTING.len())];
        }
        2 => {
            let at = rng.below(len + 1);
            let n = 1 + rng.below(16);
            let bytes: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
            input.splice(at..at, bytes);
        }
        3 if len > 0 => {
            let a = rng.below(len);
            let b = (a + 1 + rng.below(32)).min(len);
            input.drain(a..b);
        }
        4 if len > 0 => {
            let a = rng.below(len);
            let b = (a + 1 + rng.below(64)).min(len);
            let chunk = input[a..b].to_vec();
            let at = rng.below(len + 1);
            input.splice(at..at, chunk);
        }
        5 if !seeds.is_empty() => {
            let other = &seeds[rng.below(seeds.len())];
            if !other.is_empty() {
                let a = rng.below(other.len());
                let b = (a + 1 + rng.below(128)).min(other.len());
                let at = rng.below(len + 1);
                input.splice(at..at, other[a..b].iter().copied());
            }
        }
        6 if len > 0 => input.truncate(rng.below(len)),
        7 if len > 0 => {
            // A run of 0xff: huge varint lengths / counts.
            let at = rng.below(len);
            let n = (1 + rng.below(10)).min(len - at);
            input[at..at + n].fill(0xff);
        }
        8 if len > 1 => {
            let i = rng.below(len - 1);
            input.swap(i, i + 1);
        }
        _ => input.push(rng.next() as u8),
    }
    input.truncate(MAX_LEN);
}

fn iters() -> u64 {
    std::env::var("KT_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_ITERS)
}

fn seed() -> u64 {
    std::env::var("KT_FUZZ_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(0x6b74_6675_7a7a)
}

fn failure_dir() -> PathBuf {
    let base = std::env::var("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|_| {
        // crates/<crate> -> workspace target/
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target")
    });
    base.join("fuzz-failures")
}

/// Fuzz `target` for `iters` (or `KT_FUZZ_ITERS` × `scale`) inputs derived from `seeds`.
/// Panics with the saved input's path on the first input that panics or hangs.
pub fn run(name: &str, seeds: &[Vec<u8>], scale: f64, mut target: impl FnMut(&[u8])) {
    assert!(!seeds.is_empty(), "{name}: no seeds");
    let n = ((iters() as f64) * scale).max(1.0) as u64;
    let mut rng = Rng::new(seed() ^ name.bytes().fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(b as u64)));
    // The seeds themselves first: valid inputs must never fail either.
    let mut inputs: Vec<Vec<u8>> = seeds.to_vec();
    for i in 0..n {
        let input = if (i as usize) < inputs.len() {
            inputs[i as usize].clone()
        } else {
            let mut x = seeds[rng.below(seeds.len())].clone();
            for _ in 0..1 + rng.below(4) {
                mutate(&mut rng, &mut x, seeds);
            }
            x
        };
        let started = Instant::now();
        let outcome = catch_unwind(AssertUnwindSafe(|| target(&input)));
        let slow = started.elapsed() > SLOW;
        if outcome.is_err() || slow {
            let dir = failure_dir();
            let _ = std::fs::create_dir_all(&dir);
            let path = dir.join(format!("{name}-{i}.bin"));
            let _ = std::fs::write(&path, &input);
            panic!("fuzz target {name}: input {i} {} (saved to {})", if slow { "hung" } else { "panicked" }, path.display());
        }
        if i as usize == inputs.len() && inputs.len() < 64 && rng.below(8) == 0 {
            inputs.push(input); // keep a few mutants as further starting points
        }
    }
}

/// Run a target on a saved failing input (debugging).
pub fn replay(path: &str, target: impl FnOnce(&[u8])) {
    let b = std::fs::read(path).expect("saved input");
    target(&b);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panicking_target_is_caught_and_saved() {
        let r = catch_unwind(|| run("selftest", &[b"hello".to_vec()], 1.0, |b| assert!(!b.contains(&0xff))));
        assert!(r.is_err(), "the 0xff mutation eventually trips the assert");
    }

    #[test]
    fn mutations_stay_bounded() {
        let mut rng = Rng::new(7);
        let seeds = vec![vec![1u8; 100]];
        let mut x = seeds[0].clone();
        for _ in 0..10_000 {
            mutate(&mut rng, &mut x, &seeds);
            assert!(x.len() <= MAX_LEN);
        }
    }
}
