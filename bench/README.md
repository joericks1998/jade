# `bench/`: performance measurement

## What this subtree is

These are benchmark programs written twice, once in Jade and once in Python, plus the harnesses that time them and a file of results.

Comparing against CPython is the point. Jade's compiled backend, and until v1.4.8 its VM, are interesting on their own, but "is this fast" only means something next to a language people already have a feel for.

## Why it exists

Performance work here has been driven by measurement rather than guesswork. The allocation profiling was the clearest case. In v1, the `alloc-profile` Cargo feature swapped the `jade` binary's global allocator for one that recorded a size-class histogram. The numbers showed that collection churn dominated allocation-heavy code, and that finding produced both the segregated free-list pool in v1's runtime and its escape analysis. `alloc_heavy.jde` is the workload that exercised it.

Phase 6 of `design/v2-static-values.md` removed all of that: the runtime, the pool, the escape analysis, and the profiler. A v2 binary allocates through the C library's `malloc` and frees by reference count, with the retain and release code emitted per program by `src/backend/rc.rs`. `alloc_heavy.jde` is still a fair test of allocation-heavy code; there is just no profiler built into `jade` to read it with.

## What each file does

*Workloads*, each with a Jade version and a Python version so the two compare directly:

- `heavy.jde` and `heavy.py` are a general mixed workload.
- `extreme.jde` and `extreme.py` are long-running and dominated by computation. This is the one where mean timings mean something, rather than being swamped by startup.
- `sort.jde` and `sort.py` cover sorting.
- `alloc_heavy.jde` covers allocation churn. It was the workload for v1's allocator work.
- `recursion.py` is the Python side of a recursion workload.

*Harnesses:*

- `bench.py` does quick runs. It times `jade <file>`, which is shorthand for `jade run <file>`, against CPython.
- `bench_full.py` runs the full suite in three columns: `jade run`, which it still labels the bytecode VM, a binary compiled ahead of time with `jade build`, and CPython. It produces the table in `RESULTS.md`.

Since v2, `jade run` builds the program before running it, so every `jade run` timing in both harnesses now includes a compile and link step.

*`RESULTS.md`* holds the recorded runs, newest first, each under its own dated header naming the platform and versions that produced it. There are two runs in it: a 1.1.33 run on an M4, which is the first measurement of the current in-process build path, and the original 1.0.9 snapshot.

The 1.0.9 "Jade LLVM" column predates native compilation moving out to a build daemon and back again. Treat it as history rather than a baseline, and do not read a difference against it as a regression.

The 1.1.33 run is where the allocation work shows up. On sort, the VM went from 9.93 times slower than Python to 1.09 times faster, which is roughly a 13-fold speedup, while every other VM timing held steady. That is v1's pool allocator and escape analysis landing exactly where `alloc_heavy.jde` predicted they would. Both runs measured v1's runtime, so neither says anything about a v2 binary.

## Who uses it

*Depends on:* a built `jade` binary and `python3`.

*Used by:* nothing automated. Someone runs these by hand while doing performance work, and CI does not gate on them.

## Gotchas

For short suites, the `min` column is the honest one, because process startup and dynamic linking dominate the timing. Only on `extreme` does computation dominate enough for `mean` to be reliable.

The native column measures a binary that was compiled beforehand. It does not include compilation time, and for a fair comparison it usually should not. Say which you mean when you quote a number.

Re-running the suite means adding a fresh dated header to `RESULTS.md`, naming the platform and version. Never edit numbers in place under an old header.

## Running them

```sh
cargo build --release
python3 bench/bench_full.py
```
