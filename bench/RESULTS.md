# Jade Benchmark Results

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
