//! Allocation profiler — Phase 0 of the allocation remediation (`--features
//! alloc-profile`). Wraps the system allocator and records a size-class
//! histogram plus live/peak bytes, dumped to stderr at process exit.
//!
//! HOST-ONLY BY CONSTRUCTION. This is declared as the `jade` *binary's*
//! `#[global_allocator]` in `main.rs`, never in the shared `jade-runtime` crate,
//! so — unlike mimalloc — it cannot be linked into a dlopen'd package and create
//! two allocator instances. It exists only to measure the VM's allocation
//! profile so the Phase-1 pool's size classes are chosen from data, not guessed.
//! It is never built into a release.
//!
//! Run e.g.:
//!   cargo build --features alloc-profile
//!   JADE_ALLOC_PROFILE=1 ./target/debug/jade run bench/alloc_heavy.jde

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Power-of-two size buckets: index `b` counts allocations of `2^(b+2)..2^(b+3)`
/// bytes, i.e. bucket 0 is 4–7 bytes, bucket 1 is 8–15, … bucket 21 is ≥16 MiB.
const NBUCKETS: usize = 22;

static COUNTS: [AtomicU64; NBUCKETS] = [const { AtomicU64::new(0) }; NBUCKETS];
static BYTES:  [AtomicU64; NBUCKETS] = [const { AtomicU64::new(0) }; NBUCKETS];
static TOTAL_ALLOCS: AtomicU64 = AtomicU64::new(0);
static TOTAL_FREES:  AtomicU64 = AtomicU64::new(0);
static LIVE_BYTES:   AtomicU64 = AtomicU64::new(0);
static PEAK_BYTES:   AtomicU64 = AtomicU64::new(0);

/// Bucket index for `size`: `floor(log2(size)) - 2`, clamped to `[0, NBUCKETS)`.
#[inline]
fn bucket(size: usize) -> usize {
    let log2 = (usize::BITS - 1 - size.max(1).leading_zeros()) as usize;
    log2.saturating_sub(2).min(NBUCKETS - 1)
}

pub struct ProfilingAlloc;

unsafe impl GlobalAlloc for ProfilingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            let size = layout.size();
            let b = bucket(size);
            COUNTS[b].fetch_add(1, Relaxed);
            BYTES[b].fetch_add(size as u64, Relaxed);
            TOTAL_ALLOCS.fetch_add(1, Relaxed);
            let live = LIVE_BYTES.fetch_add(size as u64, Relaxed) + size as u64;
            // Monotone peak; the read-then-max race only ever under-reports
            // slightly, which is fine for a profiler.
            if live > PEAK_BYTES.load(Relaxed) {
                PEAK_BYTES.store(live, Relaxed);
            }
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        TOTAL_FREES.fetch_add(1, Relaxed);
        LIVE_BYTES.fetch_sub(layout.size() as u64, Relaxed);
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            // Model a realloc as a free of the old block + an alloc of the new one
            // so the histogram reflects the sizes actually requested.
            TOTAL_FREES.fetch_add(1, Relaxed);
            LIVE_BYTES.fetch_sub(layout.size() as u64, Relaxed);
            let b = bucket(new_size);
            COUNTS[b].fetch_add(1, Relaxed);
            BYTES[b].fetch_add(new_size as u64, Relaxed);
            TOTAL_ALLOCS.fetch_add(1, Relaxed);
            let live = LIVE_BYTES.fetch_add(new_size as u64, Relaxed) + new_size as u64;
            if live > PEAK_BYTES.load(Relaxed) {
                PEAK_BYTES.store(live, Relaxed);
            }
        }
        p
    }
}

/// Format the accumulated histogram. Printed to stderr from `main` at exit when
/// `JADE_ALLOC_PROFILE` is set (so ordinary `--features alloc-profile` runs stay
/// quiet unless asked).
pub fn report() -> String {
    let allocs = TOTAL_ALLOCS.load(Relaxed);
    let frees = TOTAL_FREES.load(Relaxed);
    let peak = PEAK_BYTES.load(Relaxed);
    let mut out = String::new();
    out.push_str("\n── jade allocation profile ─────────────────────────────\n");
    out.push_str(&format!(
        "total allocations: {allocs}\ntotal frees:       {frees}\npeak live bytes:   {peak}\n\n"
    ));
    out.push_str("size-class          count        bytes    %count\n");
    for b in 0..NBUCKETS {
        let c = COUNTS[b].load(Relaxed);
        if c == 0 {
            continue;
        }
        let bytes = BYTES[b].load(Relaxed);
        let lo = 1usize << (b + 2);
        let hi = (1usize << (b + 3)) - 1;
        let pct = if allocs > 0 { c as f64 * 100.0 / allocs as f64 } else { 0.0 };
        out.push_str(&format!("{lo:>7}–{hi:<9} {c:>10} {bytes:>12} {pct:>8.1}%\n"));
    }
    out.push_str("────────────────────────────────────────────────────────\n");
    out
}
