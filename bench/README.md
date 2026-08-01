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

**`RESULTS.md`** — recorded runs, newest first, each under its own dated header with the platform and versions that produced it. Two are in there: a 1.1.33 run on an M4, which is the first measurement of the current in-process build path, and the original 1.0.9 snapshot. The 1.0.9 "Jade LLVM" column predates native compilation moving out to a build daemon and back again, so it is history rather than a baseline — do not read a difference against it as a regression.

The 1.1.33 run is where the allocation work shows up: the VM went from 9.93x slower than Python on sort to 1.09x faster, a ~13x speedup, while every other VM timing held steady. That is the pool allocator and escape analysis landing exactly where `alloc_heavy.jde` predicted they would.

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
