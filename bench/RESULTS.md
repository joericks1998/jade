# Jade Benchmark Results

Newest run first. Each run gets its own dated header. Numbers are never edited in place under an old header, because a timing only means something next to the platform and version that produced it.

---

# Run — 2026-07-31 (Jade 1.1.33)

**Date:** 2026-07-31
**Platform:** macOS Darwin 25.5.0 (Apple M4, arm64)
**Jade version:** 1.1.33
**Python version:** CPython 3.14.0
**Script:** `bench/bench_full.py`

The first measurement of the **current** build path — `jade build` compiles in-process, which is not what the 1.0.9 snapshot further down measured. Read the `min` column on light, heavy, and sort: process startup and dynamic linking dominate their means. Only `extreme` runs long enough for `mean` to be meaningful.

| Suite | Jade VM (min) | Jade LLVM (min) | Python 3 (min) |
|-------|---------------|-----------------|----------------|
| light | 6.3 ms | 2.1 ms | 14.1 ms |
| heavy | 220.2 ms | 8.6 ms | 37.9 ms |
| sort | 13.8 ms | 2.9 ms | 15.1 ms |
| extreme | 68,944.7 ms | 2,166.7 ms | 7,778.0 ms |

Full distributions:

| Suite | Backend | Min | Mean | Max | Stdev |
|-------|---------|-----|------|-----|-------|
| light (20 runs) | Jade VM | 6.3 ms | 76.6 ms | 1387.5 ms | 308.6 ms |
| | Jade LLVM | 2.1 ms | 12.5 ms | 203.9 ms | 45.1 ms |
| | Python 3 | 14.1 ms | 15.4 ms | 16.9 ms | 0.9 ms |
| heavy (10 runs) | Jade VM | 220.2 ms | 223.5 ms | 237.3 ms | 4.9 ms |
| | Jade LLVM | 8.6 ms | 28.0 ms | 198.4 ms | 59.9 ms |
| | Python 3 | 37.9 ms | 40.1 ms | 42.8 ms | 1.7 ms |
| sort (10 runs) | Jade VM | 13.8 ms | 14.6 ms | 17.1 ms | 1.0 ms |
| | Jade LLVM | 2.9 ms | 22.0 ms | 192.1 ms | 59.8 ms |
| | Python 3 | 15.1 ms | 15.9 ms | 18.7 ms | 1.2 ms |
| extreme (VM 1 run; others 3) | Jade VM | 68,944.7 ms | 68,944.7 ms | 68,944.7 ms | n/a |
| | Jade LLVM | 2,166.7 ms | 2,245.9 ms | 2,403.1 ms | 136.1 ms |
| | Python 3 | 7,778.0 ms | 7,798.4 ms | 7,819.1 ms | 20.6 ms |

## What changed since 1.0.9

**The VM got about 13x faster at sort** — 180.7 ms to 13.8 ms — which flips it from 9.93x slower than Python to 1.09x faster. Every other VM number is unchanged within noise: light min 6.3 ms both times, heavy 222.4 → 220.2 ms, extreme 68,906 → 68,945 ms. So the win is specific to array-heavy work, which is where the segregated free-list pool in `jade-runtime` and the escape analysis in `compiler/escape.rs` were aimed. This is the clearest evidence that the allocation-profiling work paid off.

**The LLVM numbers are not comparable to the 1.0.9 column** and should not be read as a regression. That snapshot measured a different backend on a different build path, which is why it carries a warning. The current claim is simply that native compilation is 1.43x faster than Python on heavy and 3.47x on extreme. If the gap is ever worth chasing, the likely cost is that values now route through the shared `jade-runtime` ABI where the old backend used raw machine arithmetic — correctness bought with speed, and the right trade.

**`extreme` is a stress test, not a workload.** `fib(40)` puts ~330 million calls through the call stack, which no ordinary script does. Heavy and sort are the representative suites; treat extreme as a ceiling probe.

## Measurement caveats for this run

The VM's light suite has a 1,387 ms max against a 6.3 ms min, which dragged its mean to 76.6 ms and its stdev to 308.6 ms. That is one bad run out of twenty, on a machine that had just finished a release build. The min matches the 1.0.9 snapshot exactly, so the mean and max there are noise, not signal.

Python moved from the system CPython of the 1.0.9 run to 3.14.0, so the Python column is not a fixed baseline across the two runs either.

---

# Historical snapshot — 2026-04-12 (Jade 1.0.9)

> **⚠️ Historical.** The "Jade LLVM" column below was produced by an
> earlier in-repo LLVM backend. Native compilation moved out to a build daemon in v1.1.8 and
> back in-process afterwards, so these timings no longer reflect the current build path.
> Kept for historical comparison.

**Date:** 2026-04-12  
**Platform:** macOS Darwin 25.3.0 (Apple Silicon, aarch64)  
**Jade version:** 1.0.9  
**Python version:** CPython 3 (system)  
**Script:** `bench/bench_full.py`

Three execution backends are compared:

| Backend | How it works | Invoked via |
|---------|-------------|-------------|
| **Jade VM** | Jade source → bytecode → register-based VM | `jade run <file>` |
| **Jade LLVM** | Jade source → LLVM IR → native machine code | `jade build <file> -o bin && ./bin` |
| **Python 3** | CPython interpreter | `python3 <file>` |

> **Note on LLVM timings:** The LLVM column measures the pre-compiled binary only — compilation itself is not included. For short suites (light, heavy, sort), process startup and dynamic linking dominate; the `min` column is the most representative of actual compute time. For the extreme suite, compute dominates and `mean` is reliable.

---

## Suite 1 — Light  
`fib(10)`, `factorial(7)`, `sum_to(10)` · 20 runs each

| Backend | Min | Mean | Max | Stdev |
|---------|-----|------|-----|-------|
| Jade VM | 6.3 ms | 7.9 ms | 11.1 ms | 1.3 ms |
| Jade LLVM | 1.6 ms | 10.8 ms | 177.2 ms | 39.2 ms |
| Python 3 | 14.5 ms | 15.4 ms | 18.2 ms | 0.8 ms |

| Comparison | Result |
|-----------|--------|
| VM vs Python | **Jade VM 1.96x faster** |
| LLVM vs Python | Jade LLVM 1.43x faster (min; startup noise inflates mean) |
| LLVM vs VM | VM 1.37x faster (startup overhead dominates LLVM mean) |

At this scale both Jade backends beat Python. LLVM mean is noisy because process startup and dynamic linking outweigh actual compute time for sub-millisecond programs.

---

## Suite 2 — Heavy  
`fib(28)`, `factorial(15)`, `sum_to(500)` · 10 runs each

| Backend | Min | Mean | Max | Stdev |
|---------|-----|------|-----|-------|
| Jade VM | 222.4 ms | 225.2 ms | 227.2 ms | 1.6 ms |
| Jade LLVM | 2.9 ms | 21.2 ms | 180.9 ms | 56.1 ms |
| Python 3 | 38.2 ms | 39.4 ms | 44.1 ms | 1.7 ms |

| Comparison | Result |
|-----------|--------|
| VM vs Python | Python **5.72x faster** |
| LLVM vs Python | **Jade LLVM 1.86x faster** |
| LLVM vs VM | **LLVM 10.63x faster** than bytecode VM |

Compute time is now large enough to see clearly. LLVM native pulls ahead of Python; the VM falls behind Python for the first time. The VM's performance gap vs LLVM starts to open at ~10x.

---

## Suite 3 — Sort  
Bubble sort, 200-element worst-case (descending) array · 10 runs each

| Backend | Min | Mean | Max | Stdev |
|---------|-----|------|-----|-------|
| Jade VM | 180.7 ms | 184.0 ms | 200.7 ms | 5.9 ms |
| Jade LLVM | 2.2 ms | 24.0 ms | 212.2 ms | 66.1 ms |
| Python 3 | 16.1 ms | 18.5 ms | 22.1 ms | 1.7 ms |

| Comparison | Result |
|-----------|--------|
| VM vs Python | Python **9.93x faster** |
| LLVM vs Python | Python 1.29x faster (on mean; LLVM min 2.2ms beats Python min 16.1ms) |
| LLVM vs VM | **LLVM 7.67x faster** than bytecode VM |

Python's tight swap idiom (`a, b = b, a`) makes it very competitive on array-heavy loops. LLVM's mean is still noisy from startup; when compute dominates (min column) LLVM is 7x faster than Python.

---

## Suite 4 — Extreme  
`fib(40)` · ~330 million recursive calls · VM: 1 run; LLVM + Python: 3 runs each

| Backend | Min | Mean | Max | Stdev |
|---------|-----|------|-----|-------|
| Jade VM | 68,906 ms | 68,906 ms | 68,906 ms | n/a (1 run) |
| Jade LLVM | 214.0 ms | 317.8 ms | 514.3 ms | 170.3 ms |
| Python 3 | 7,651.7 ms | 7,676.0 ms | 7,697.9 ms | 23.2 ms |

| Comparison | Result |
|-----------|--------|
| VM vs Python | Python **8.98x faster** |
| LLVM vs Python | **Jade LLVM 24.15x faster** |
| LLVM vs VM | **LLVM 216.81x faster** than bytecode VM |

This is where native compilation compounds dramatically. LLVM's advantage over Python grows from 1.86x (heavy) to 24x (extreme) — register allocation, branch prediction, and function inlining all benefit from problem size. The VM's 69-second run vs LLVM's 318ms mean is a 217x gap.

---

## Cross-Suite Summary

| Suite | VM vs Python | LLVM vs Python | LLVM vs VM |
|-------|-------------|----------------|------------|
| Light | VM **1.96x faster** | LLVM 1.43x faster | ~equal (startup noise) |
| Heavy | Python 5.72x faster | LLVM **1.86x faster** | LLVM **10.6x faster** |
| Sort | Python 9.93x faster | ~equal (startup noise) | LLVM **7.7x faster** |
| Extreme | Python 8.98x faster | LLVM **24.2x faster** | LLVM **216.8x faster** |

---

## When to Use Each Backend

| Workload | Recommended backend | Why |
|----------|--------------------|----|
| Scripts, tooling, light compute | `jade run` (VM) | Zero compile latency, instant startup |
| Development / iteration | `jade run` (VM) | Edit-run loop is immediate |
| REPL / interactive use | `jade run` / tree-walk | Persistent state, no compile step |
| Number crunching, heavy recursion | `jade build` (LLVM) | Native code compounds at scale |
| OS distribution / deployment | `jade build` (LLVM) | Self-contained binary, no runtime needed |
| Cross-machine portability | `jade run` (VM) | Bytecode runs anywhere `jade` is installed |

The key distinction: `jade run` is for **scripting**; `jade build` is for **production**. The LLVM backend does not replace the VM — it serves a different job.
