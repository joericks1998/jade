# `src/alloc/` — the `jade` binary's global allocators

## What this subtree is

The two allocators the `jade` process can install, and nothing else. Exactly one is active in any build:

- **`pool`** — the production allocator. A thin `GlobalAlloc` shell over the size-classed free list in `jade_runtime::pool`.
- **`profile`** — a measuring allocator behind `--features alloc-profile`. Wraps the system allocator and records a size-class histogram plus live and peak bytes, dumped to stderr at exit.

## Why it was built

The shape of this directory is a scar from a real bug, and it is worth understanding before moving anything.

Jade briefly used mimalloc as its global allocator, declared in the shared `jade-runtime` crate. But `jade-runtime` is also statically linked into every native package, so a process that `dlopen`'d a package ended up holding *two* allocator instances whose duplicate symbols interposed across the boundary. It corrupted the heap and deadlocked tokio's shutdown. Both bugs were the same root cause.

The fix is the rule this directory exists to enforce: **a global allocator is declared in the binary, never in `jade-runtime`.** The `#[global_allocator]` statics live in `src/main.rs` and refer to types here, so neither allocator can reach a loaded package.

Note the distinction the split preserves. The pool *implementation* still lives in `jade-runtime`, because the AOT path calls it directly from `gc::leak_obj` / `free_obj` and both engines should share one free list. What lives here is only the `GlobalAlloc` adapter. Sharing an allocator's *code* is safe; sharing its *global-allocator registration* is not.

The profiler is Phase 0 of the same piece of work. It exists so the pool's size classes were chosen from measured data rather than guessed, and it is never built into a release.

## What each file does

- **`mod.rs`** — module declarations and the host-only rationale above. `profile` is behind `#[cfg(feature = "alloc-profile")]`.
- **`pool.rs`** — `PoolAlloc`, forwarding `alloc` / `dealloc` / `realloc` to `jade_runtime::pool`. It deliberately does *not* override `alloc_zeroed`, so it inherits the trait default of alloc-then-zero — which matters more here than for a system allocator, because a pooled block is recycled dirty.
- **`profile.rs`** — `ProfilingAlloc`, the power-of-two `bucket` function, the atomic counters, and `report()`, which formats the histogram. `main.rs` prints it at exit when `JADE_ALLOC_PROFILE` is set, so an ordinary `--features alloc-profile` run stays quiet unless asked.

Tests are inline `#[cfg(test)] mod tests` blocks rather than the sibling `tests.rs` used elsewhere in the repo, because both files need private items — `profile.rs`'s `bucket` and its counter statics — and widening their visibility purely for tests would be worse than the small inconsistency.

## Who uses it

*Depends on:* `jade_runtime::pool` for the actual free list, and `std::alloc` for the `GlobalAlloc` trait and the system allocator.

*Used by:* `src/main.rs`, and only `src/main.rs`. It holds both `#[global_allocator]` declarations and the `report()` call at exit. Nothing else in the tree should reference this module — if something does, the host-only invariant is at risk.

## Gotchas

**Never declare either of these as the global allocator in `jade-runtime`, or in any crate a native package links.** That is the whole point of the directory.

**The profiler's counters are process-global**, so tests that assert on deltas have to serialize. There is a `COUNTERS` mutex in `profile.rs`'s test module for exactly that; any new test that allocates through `ProfilingAlloc` must take it. The tests do not install `ProfilingAlloc` globally — the `#[global_allocator]` is in `main.rs`, which the library test target does not compile — which is what makes exact deltas possible at all.

**The pool tests that assert on addresses each own a size class** (32, 128, 256/512), because `cargo test` runs them in parallel against one process-wide free list. Keep new ones disjoint or they will flake.

## Building and testing

```sh
cargo test --lib alloc::                          # the pool wrapper
cargo test --lib --features alloc-profile alloc:: # adds the profiler tests

# take a profile
cargo build --features alloc-profile
JADE_ALLOC_PROFILE=1 ./target/debug/jade run bench/alloc_heavy.jde
```
