#!/usr/bin/env python3
"""
Benchmark: Jade (bytecode VM) vs Jade (LLVM native) vs Python 3

Three suites:
  light  — fib(10), factorial(7), sum_to(10)
  heavy  — fib(28), factorial(15), sum_to(500)
  sort   — bubble sort, 200-element descending array
"""

import subprocess
import time
import statistics
import sys
import tempfile
import os
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
    {
        "name":  "sort   (bubble sort, 200-elem descending array)",
        "jade":  ROOT / "bench" / "sort.jde",
        "py":    ROOT / "bench" / "sort.py",
        "runs":  10,
    },
]


def measure(cmd: list[str], runs: int) -> list[float]:
    times = []
    for _ in range(runs):
        t0 = time.perf_counter()
        result = subprocess.run(cmd, capture_output=True)
        elapsed = time.perf_counter() - t0
        if result.returncode != 0:
            print(f"      ERROR: {result.stderr.decode().strip()[:120]}")
            return []
        times.append(elapsed)
    return times


def report(label: str, times: list[float]):
    if not times:
        print(f"    {label}")
        print(f"      (failed)")
        return
    ms = [t * 1000 for t in times]
    stdev_str = f"{statistics.stdev(ms):.1f}" if len(ms) > 1 else "n/a"
    print(f"    {label}")
    print(f"      min  : {min(ms):.1f} ms")
    print(f"      mean : {statistics.mean(ms):.1f} ms")
    print(f"      max  : {max(ms):.1f} ms")
    print(f"      stdev: {stdev_str} ms")


def compile_llvm(jade_src: Path, out_path: str) -> bool:
    """Compile a .jde file to a native binary via jade build."""
    result = subprocess.run(
        [str(JADE_BIN), "build", str(jade_src), "-o", out_path],
        capture_output=True,
    )
    if result.returncode != 0:
        print(f"      LLVM compile failed: {result.stderr.decode().strip()[:200]}")
        return False
    return True


with tempfile.TemporaryDirectory() as tmpdir:
    for suite in SUITES:
        runs = suite["runs"]
        jade_src = suite["jade"]
        print(f"\n── {suite['name']}  ({runs} runs each) ──")

        # ── Jade bytecode VM ──
        vm_times = measure([str(JADE_BIN), "run", str(jade_src)], runs)
        report("Jade  (bytecode VM)", vm_times)
        print()

        # ── Jade LLVM native ──
        native_bin = os.path.join(tmpdir, f"jade_{jade_src.stem}")
        llvm_ok = compile_llvm(jade_src, native_bin)
        if llvm_ok:
            llvm_times = measure([native_bin], runs)
            report("Jade  (LLVM native)", llvm_times)
        else:
            llvm_times = []
            print("    Jade  (LLVM native)")
            print("      (compile failed)")
        print()

        # ── Python ──
        py_times = measure([PYTHON_BIN, str(suite["py"])], runs)
        report("Python 3           ", py_times)
        print()

        # ── Summary ──
        if vm_times and py_times:
            vm_mean = statistics.mean(vm_times)
            py_mean = statistics.mean(py_times)
            if vm_mean < py_mean:
                print(f"    VM vs Python   → Jade VM is {py_mean / vm_mean:.2f}x faster")
            else:
                print(f"    VM vs Python   → Python is {vm_mean / py_mean:.2f}x faster")

        if llvm_times and py_times:
            llvm_mean = statistics.mean(llvm_times)
            py_mean   = statistics.mean(py_times)
            if llvm_mean < py_mean:
                print(f"    LLVM vs Python → Jade LLVM is {py_mean / llvm_mean:.2f}x faster")
            else:
                print(f"    LLVM vs Python → Python is {llvm_mean / py_mean:.2f}x faster")

        if vm_times and llvm_times:
            vm_mean   = statistics.mean(vm_times)
            llvm_mean = statistics.mean(llvm_times)
            if llvm_mean < vm_mean:
                print(f"    LLVM vs VM     → LLVM is {vm_mean / llvm_mean:.2f}x faster than bytecode VM")
            else:
                print(f"    LLVM vs VM     → VM is {llvm_mean / vm_mean:.2f}x faster than LLVM")
