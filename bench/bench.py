#!/usr/bin/env python3
"""
Benchmark: Jade vs Python — recursive functions
Two suites:
  light  — inputs from recursion.jde  (fib(10), factorial(7), sum_to(10))
  heavy  — inputs from heavy.jde      (fib(28), factorial(15), sum_to(500))
"""

import subprocess
import time
import statistics
import sys
from pathlib import Path

ROOT       = Path(__file__).parent.parent
JADE_BIN   = ROOT / "target" / "release" / "jade"
PYTHON_BIN = sys.executable

SUITES = [
    {
        "name":  "light  (fib(10), factorial(7), sum_to(10))",
        "jade":  ROOT / "jade_evals" / "functions" / "recursion.jde",
        "py":    ROOT / "bench" / "recursion.py",
        "runs":  20,
    },
    {
        "name":  "heavy  (fib(28), factorial(15), sum_to(500))",
        "jade":  ROOT / "bench" / "heavy.jde",
        "py":    ROOT / "bench" / "heavy.py",
        "runs":  10,
    },
]


def measure(cmd: list[str], runs: int) -> list[float]:
    times = []
    for _ in range(runs):
        t0 = time.perf_counter()
        subprocess.run(cmd, check=True, capture_output=True)
        times.append(time.perf_counter() - t0)
    return times


def report(label: str, times: list[float]):
    ms = [t * 1000 for t in times]
    print(f"    {label}")
    print(f"      min  : {min(ms):.1f} ms")
    print(f"      mean : {statistics.mean(ms):.1f} ms")
    print(f"      max  : {max(ms):.1f} ms")
    print(f"      stdev: {statistics.stdev(ms):.1f} ms")


for suite in SUITES:
    runs = suite["runs"]
    print(f"\n── {suite['name']}  ({runs} runs each) ──")

    jade_times = measure([str(JADE_BIN), str(suite["jade"])], runs)
    py_times   = measure([PYTHON_BIN,    str(suite["py"])],   runs)

    report("Jade  (release)", jade_times)
    print()
    report("Python 3        ", py_times)
    print()

    jade_mean = statistics.mean(jade_times)
    py_mean   = statistics.mean(py_times)

    if jade_mean < py_mean:
        print(f"    → Jade is {py_mean / jade_mean:.2f}x faster (mean)")
    else:
        print(f"    → Python is {jade_mean / py_mean:.2f}x faster (mean)")
