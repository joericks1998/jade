# `src/alloc/`: the `jade` binary's global allocators

## What this subtree is

This holds the two allocators the `jade` process can install, and nothing else. Exactly one is active in any given build.

- *`pool`* is the production allocator. It is a thin `GlobalAlloc` shell over the size-classed free list in `jade_runtime::pool`.
- *`profile`* is a measuring allocator, built only with `--features alloc-profile`. It wraps the system allocator and records a size-class histogram plus live and peak byte counts, printed to stderr at exit.

## Why it was built

The shape of this directory comes from a real bug, and it is worth understanding before moving anything.

Jade briefly used mimalloc as its global allocator, declared in the shared `jade-runtime` crate. But `jade-runtime` is also statically linked into every native package. So a process that had `dlopen`ed a package ended up holding *two* allocator instances, whose duplicate symbols interposed across the boundary. That corrupted the heap and deadlocked tokio's shutdown. Both bugs had the same root cause.

The fix is the rule this directory exists to enforce: *a global allocator is declared in the binary, never in `jade-runtime`*. The `#[global_allocator]` statics live in `src/main.rs` and refer to types defined here, so neither allocator can reach a loaded package.

Note the distinction the split preserves. The pool *implementation* still lives in `jade-runtime`, because the AOT path calls it directly from `gc::leak_obj` and `gc::free_obj`, and both engines should share one free list. What lives here is only the `GlobalAlloc` adapter. Sharing an allocator's *code* is safe. Sharing its *registration as the global allocator* is not.

The profiler is the first phase of the same piece of work. It exists so the pool's size classes came from measured data rather than a guess. It is never built into a release.

## What each file does

- *`mod.rs`* holds the module declarations and the host-only reasoning above. `profile` sits behind `#[cfg(feature = "alloc-profile")]`.
- *`pool.rs`* holds `PoolAlloc`, which forwards `alloc`, `dealloc`, and `realloc` to `jade_runtime::pool`. It deliberately does *not* override `alloc_zeroed`, so it inherits the trait default of allocating and then zeroing. That matters more here than for a system allocator, because a pooled block comes back recycled and still holding old bytes.
- *`profile.rs`* holds `ProfilingAlloc`, the power-of-two `bucket` function, the atomic counters, and `report()`, which formats the histogram. `main.rs` prints the report at exit when `JADE_ALLOC_PROFILE` is set, so an ordinary `--features alloc-profile` run stays quiet unless you ask for output.

Tests here are inline `#[cfg(test)] mod tests` blocks rather than the sibling `tests.rs` the rest of the repo uses. Both files need private items, specifically `bucket` in `profile.rs` and its counter statics. Widening their visibility purely for tests would be worse than the small inconsistency.

## Who uses it

*Depends on:* `jade_runtime::pool` for the actual free list, and `std::alloc` for the `GlobalAlloc` trait and the system allocator.

*Used by:* `src/main.rs`, and only `src/main.rs`. It holds both `#[global_allocator]` declarations and the `report()` call at exit. Nothing else in the tree should reference this module. If something does, the host-only rule is at risk.

## Gotchas

*Never declare either of these as the global allocator in `jade-runtime`, or in any crate a native package links.* That is the whole point of this directory.

*The profiler's counters are process-global*, so any test asserting on a change in them has to run alone. The test module in `profile.rs` has a `COUNTERS` mutex for exactly that, and every new test allocating through `ProfilingAlloc` must take it.

The tests never install `ProfilingAlloc` globally. The `#[global_allocator]` lives in `main.rs`, which the library test target does not compile. That is what makes exact measurements possible at all.

*Each pool test that asserts on addresses owns its own size class*, which is 32, 128, or 256 and 512. `cargo test` runs them in parallel against one process-wide free list. Keep any new one on a size class of its own, or it will fail intermittently.

## Building and testing

```sh
cargo test --lib alloc::                          # the pool wrapper
cargo test --lib --features alloc-profile alloc:: # adds the profiler tests

# take a profile
cargo build --features alloc-profile
JADE_ALLOC_PROFILE=1 ./target/debug/jade run bench/alloc_heavy.jde
```
