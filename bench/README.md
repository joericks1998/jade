# `bench/` — performance measurement

## What this subtree is

Benchmark programs written twice — once in Jade, once in Python — plus the harnesses that time them and a results file.

Comparing against CPython is the point. Jade's VM and its AOT backend are each interesting on their own, but "is this fast" only means something next to a language people already have intuitions about.

## Why it exists

Performance work here has been driven by measurement rather than guesswork. The allocation-profiling work is the clearest case: the `alloc-profile` Cargo feature swaps the `jade` binary's global allocator for a size-class histogram, the numbers showed that collection churn dominates allocation-heavy code, and that finding is what produced the segregated free-list pool in `jade-runtime` and the escape analysis in `compiler/escape.rs`. `alloc_heavy.jde` is the workload that exercises it.

## What each file does

**Workloads**, each with a Jade and a Python version so the two are directly comparable:

- `heavy.jde` / `heavy.py` — general mixed workload.
- `extreme.jde` / `extreme.py` — long-running, compute-dominated. The one where mean timings are meaningful rather than dominated by startup.
- `sort.jde` / `sort.py` — sorting.
- `alloc_heavy.jde` — allocation churn, for the allocator and arena work.
- `recursion.py` — the Python side of a recursion workload.

**Harnesses:**

- `bench.py` — quick runs.
- `bench_full.py` — the full suite across all three backends (Jade VM, Jade native, CPython), producing the table in `RESULTS.md`.

**`RESULTS.md`** — a recorded snapshot with its platform and versions. Note the warning at the top: the "Jade LLVM" column dates from v1.0.9, before native compilation moved out to a build daemon and back in-process again, so those timings no longer describe the current build path. It is kept for historical comparison, not as a current claim.

## Who uses it

*Depends on:* a built `jade` binary and `python3`.

*Used by:* nothing automated. These are run by hand when someone is doing performance work; CI does not gate on them.

## Gotchas

For short suites the `min` column is the honest one — process startup and dynamic linking dominate. Only on `extreme` does compute dominate enough for `mean` to be reliable.

The native column measures the pre-compiled binary. Compilation time is not included, and for a fair comparison it usually should not be, but say which you mean when you quote a number.

Re-running the suite means updating `RESULTS.md` with a fresh date, platform, and version header. Do not edit numbers in place under an old header.

## Running them

```sh
cargo build --release
python3 bench/bench_full.py

# allocation profile
cargo build --features alloc-profile
./target/debug/jade run bench/alloc_heavy.jde
```
