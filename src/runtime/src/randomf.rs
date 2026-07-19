//! `std::random` runtime module — the AOT C-ABI surface, ported from the C leaf
//! `runtime_lib/random/random.c`. A self-contained xoshiro256** generator
//! (splitmix64-seeded), the exact same algorithm/constants as the C so behavior
//! is unchanged; the value *sequence* differs from the VM's `rand::StdRng` (an
//! intentional divergence — only distributions/invariants are guaranteed).
//!
//! `random.int` raises on `min > max`; since a Rust frame cannot be crossed by
//! the exception `longjmp`, the check + throw live in a thin C forwarder
//! (`jrt_random_int` in common.c) which calls the non-raising [`jrt_random_draw`]
//! here. `float`/`seed` never raise and export directly.

use std::sync::Mutex;

struct Rng {
    state: [u64; 4],
    seeded: bool,
}

static RNG: Mutex<Rng> = Mutex::new(Rng { state: [0; 4], seeded: false });

fn splitmix64(x: &mut u64) -> u64 {
    *x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[inline]
fn rotl(x: u64, k: u32) -> u64 {
    (x << k) | (x >> (64 - k))
}

impl Rng {
    fn reseed(&mut self, seed: u64) {
        let mut s = seed;
        for i in 0..4 {
            self.state[i] = splitmix64(&mut s);
        }
        // Guard against the all-zero state (degenerate for xoshiro).
        if self.state.iter().all(|&w| w == 0) {
            self.state[0] = 0x9E37_79B9_7F4A_7C15;
        }
        self.seeded = true;
    }

    /// One 64-bit draw (xoshiro256**). Reseeds from time^pid on first use.
    fn next(&mut self) -> u64 {
        if !self.seeded {
            let t = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            self.reseed(t ^ ((std::process::id() as u64) << 16));
        }
        let s = &mut self.state;
        let result = rotl(s[1].wrapping_mul(5), 7).wrapping_mul(9);
        let t = s[1] << 17;
        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];
        s[2] ^= t;
        s[3] = rotl(s[3], 45);
        result
    }

    /// Uniform value in `[0, bound)` by rejection sampling (no modulo bias);
    /// `bound == 0` means a raw 64-bit draw.
    fn bounded(&mut self, bound: u64) -> u64 {
        if bound == 0 {
            return self.next();
        }
        let limit = u64::MAX - (u64::MAX % bound);
        loop {
            let r = self.next();
            if r < limit {
                return r % bound;
            }
        }
    }
}

// ── Neutral cores (used by both engines; the VM's random.* route here too) ────

/// A uniform integer in `[lo, hi]` inclusive (`lo <= hi` required — the VM checks
/// `min > max` and the AOT's C forwarder `jrt_random_int` checks it before here).
pub fn draw(lo: i64, hi: i64) -> i64 {
    let range = (hi as u64).wrapping_sub(lo as u64).wrapping_add(1); // 0 ⇒ full 64-bit span
    let r = RNG.lock().unwrap_or_else(|e| e.into_inner()).bounded(range);
    lo.wrapping_add(r as i64)
}

/// A uniform float in `[0.0, 1.0)` (53 random bits).
pub fn float() -> f64 {
    let r = RNG.lock().unwrap_or_else(|e| e.into_inner()).bounded(0) >> 11;
    (r as f64) / ((1u64 << 53) as f64)
}

/// Reseed the generator deterministically.
pub fn seed(n: i64) {
    RNG.lock().unwrap_or_else(|e| e.into_inner()).reseed(n as u64);
}

// ── AOT C-ABI wrappers ────────────────────────────────────────────────────────

/// `random.int` core (the C forwarder guarantees `lo <= hi` before calling).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_random_draw(lo: i64, hi: i64) -> i64 {
    draw(lo, hi)
}

/// `random.float()` — a value in `[0.0, 1.0)`.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_random_float() -> f64 {
    float()
}

/// `random.seed(n)` — reseed the generator.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_random_seed(n: i64) {
    seed(n);
}
