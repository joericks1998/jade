#!/usr/bin/env bash
#
# Build a real Rust package and run it.
#
# The FFI is the one part of the toolchain whose correctness depends on code we
# do not write: someone else's crate, someone else's C library, and cargo's and
# rustc's opinion of the bridge #[jade::export] generates. Unit tests and the
# examples cover the shapes we thought of. This covers what an ordinary package
# from crates.io brings with it.
#
# Two gates, and they catch different classes:
#
#   1. The end-to-end run. ffi-gate/ is a Rust package wrapping `regex`, a large
#      crate from crates.io, and the machine's own zlib through `libz-sys`, which
#      links it from the system rather than carrying it. So the run covers cargo
#      fetching and building a dependency graph we did not write, a staticlib
#      that imports a system library, and the link that finds it: Apple's SDK on
#      macOS, a shared object beside the C library on Linux.
#
#   2. The long run. Step 1 calls each function once, which proves the answer is
#      right and nothing else. What a Rust function returns is Rust's memory, and
#      the program frees it through the function's `__free` companion, so a long
#      run should hold no more memory than a short one. One call cannot tell a
#      correct release from a leak, and cannot tell either from a crash that only
#      shows up under sustained churn.
#
# Until 2.0.5 this gate bound glib's C header whole through the C shim; Rust
# packages replaced that path (design/v2-rust-packages.md).
#
# Both are skipped rather than failed when cargo is absent, so this is safe to
# run anywhere. Skips are reported, never silent.
#
# Usage: compiler/src/scripts/ffi-gate.sh [path-to-jade-binary]

set -uo pipefail

JADE="${1:-./target/debug/jade}"
# Absolute, because every step below runs from a scratch directory.
case "$JADE" in /*) ;; *) JADE="$PWD/${JADE#./}" ;; esac
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
CRATE="$ROOT/src/scripts/ffi-gate"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

pass=0; skip=0; fail=0

proj="$WORK/gate"
loop_ready=0; loop_skip="cargo not installed"

# ── 1. A crates.io crate and a system library, built and run ──────────────────
if ! command -v cargo >/dev/null 2>&1; then
  echo "  skip  Rust package built and run                     (cargo not installed)"
  skip=$((skip + 1))
else
  mkdir -p "$proj"
  cp "$CRATE/gate.jde" "$proj/main.jde"
  cat > "$proj/jade.toml" <<TOML
[project]
name = "ffi-gate"

[dependencies.gate]
path = "$CRATE"
abi = "rust"
TOML
  # Every run keeps its exit status, and that is not a detail. A process killed
  # by a signal *after* it has printed everything leaves output that looks
  # perfectly correct; the glib gate this replaced reported a SIGSEGV as `ok`
  # for five releases that way. Each runs through an inner shell so the shell
  # that waits on a signalled child does not print a line of its own.
  bash -c 'cd "$1" && "$2" build main.jde -o app > build.txt 2>&1' _ "$proj" "$JADE" 2>/dev/null
  build_rc=$?
  run_rc=0
  if [ "$build_rc" -eq 0 ]; then
    bash -c 'cd "$1" && ./app > run.txt 2>&1' _ "$proj" 2>/dev/null
    run_rc=$?
  fi
  if [ "$build_rc" -ne 0 ]; then
    echo "  FAIL  the Rust package would not build:"
    tail -20 "$proj/build.txt" | sed 's/^/        /'
    fail=$((fail + 1))
    loop_skip="the package does not build"
  elif [ "$run_rc" -ne 0 ] || ! diff -q "$proj/run.txt" "$CRATE/gate.expected" >/dev/null; then
    echo "  FAIL  the program did not print what gate.expected records (exit $run_rc):"
    diff "$proj/run.txt" "$CRATE/gate.expected" | head -10 | sed 's/^/        /'
    fail=$((fail + 1))
    loop_ready=1
  else
    echo "  ok    Rust package built and run                     (regex, system zlib)"
    pass=$((pass + 1))
    loop_ready=1
  fi
fi

# ── 2. Two functions, called until something breaks ───────────────────────────
#
# Two runs of the same program at different call counts. Surviving is the
# assertion that matters and is checked always; comparing what the two runs held
# needs a peak-RSS tool, and skips when there is none.
LOOP_BASE=200000
LOOP_BIG=600000
# Each iteration returns a string and an array of three strings: four Rust
# allocations of a few dozen bytes each, plus the array's own. Leaking them would
# cost roughly 100 bytes an iteration, so the 400,000 extra iterations of the big
# run would hold some 40 MB more. Ordinary allocator churn is a few MB. 16 MB sits
# between the two with room on both sides.
LOOP_MARGIN_KB=$((16 * 1024))

# Peak RSS in KB from whichever /usr/bin/time this platform has, or nothing.
# GNU coreutils prints kbytes under -v; BSD and macOS print bytes under -l.
peak_rss_kb() {
  local kb
  kb="$(sed -n 's/.*Maximum resident set size (kbytes): \([0-9]*\).*/\1/p' "$1")"
  if [ -n "$kb" ]; then echo "$kb"; return 0; fi
  kb="$(sed -n 's/^ *\([0-9]*\)  *maximum resident set size.*/\1/p' "$1")"
  [ -n "$kb" ] && echo $((kb / 1024))
}

# Neither flag is guaranteed to exist, so ask rather than guess from `uname`.
time_flag=""
if [ -x /usr/bin/time ]; then
  for f in -l -v; do
    /usr/bin/time "$f" true >/dev/null 2>"$WORK/timeprobe.txt"
    if [ -n "$(peak_rss_kb "$WORK/timeprobe.txt")" ]; then time_flag="$f"; break; fi
  done
fi

# Runs the loop binary at $1 calls, output to $2 and the timer's report to
# $2.time. Exits with the binary's own status, which is the whole point.
run_loop() {
  if [ -n "$time_flag" ]; then
    JADE_FFI_LOOP_ITERS="$1" bash -c \
      '/usr/bin/time "$1" "$2" > "$3" 2> "$4"' \
      _ "$time_flag" "$proj/looper" "$2" "$2.time" 2>/dev/null
  else
    JADE_FFI_LOOP_ITERS="$1" bash -c \
      '"$1" > "$2" 2>&1' _ "$proj/looper" "$2" 2>/dev/null
  fi
}

if [ "$loop_ready" -eq 0 ]; then
  printf '  skip  %-47s(%s)\n' "Rust calls, $LOOP_BIG times" "$loop_skip"
  skip=$((skip + 1))
  printf '  skip  %-47s(%s)\n' "Rust calls, memory held" "$loop_skip"
  skip=$((skip + 1))
else
  cp "$CRATE/loop.jde" "$proj/loop.jde"
  ( cd "$proj" && "$JADE" build loop.jde -o looper > loopbuild.txt 2>&1 )
  if [ ! -x "$proj/looper" ]; then
    echo "  FAIL  the loop would not compile:"
    tail -5 "$proj/loopbuild.txt" | sed 's/^/        /'
    fail=$((fail + 1))
    printf '  skip  %-47s(%s)\n' "Rust calls, memory held" "loop would not compile"
    skip=$((skip + 1))
  else
    run_loop "$LOOP_BASE" "$proj/loop-base.txt"; base_rc=$?
    run_loop "$LOOP_BIG"  "$proj/loop-big.txt";  big_rc=$?

    ok_out=1
    grep -qE "^calls +=  *$LOOP_BIG\$" "$proj/loop-big.txt" || ok_out=0
    grep -qE "^mismatched +=  *0\$"    "$proj/loop-big.txt" || ok_out=0

    if [ "$base_rc" -ne 0 ] || [ "$big_rc" -ne 0 ]; then
      bad=$LOOP_BIG; rc=$big_rc; out="$proj/loop-big.txt"
      if [ "$base_rc" -ne 0 ]; then
        bad=$LOOP_BASE; rc=$base_rc; out="$proj/loop-base.txt"
      fi
      echo "  FAIL  the compiled binary did not survive $bad iterations (exit $rc):"
      [ -s "$out" ] && tail -3 "$out" | sed 's/^/        /'
      fail=$((fail + 1))
      printf '  skip  %-47s(%s)\n' "Rust calls, memory held" "the loop did not survive"
      skip=$((skip + 1))
    elif [ "$ok_out" -eq 0 ]; then
      echo "  FAIL  a Rust function gave back the wrong answer over $LOOP_BIG iterations:"
      head -3 "$proj/loop-big.txt" | sed 's/^/        /'
      fail=$((fail + 1))
      printf '  skip  %-47s(%s)\n' "Rust calls, memory held" "the loop gave wrong answers"
      skip=$((skip + 1))
    else
      echo "  ok    Rust calls, $LOOP_BIG times"
      pass=$((pass + 1))
      if [ -z "$time_flag" ]; then
        printf '  skip  %-47s(%s)\n' "Rust calls, memory held" "no /usr/bin/time reports peak memory"
        skip=$((skip + 1))
      else
        base_kb="$(peak_rss_kb "$proj/loop-base.txt.time")"
        big_kb="$(peak_rss_kb "$proj/loop-big.txt.time")"
        grew=$((big_kb - base_kb))
        if [ "$grew" -gt "$LOOP_MARGIN_KB" ]; then
          echo "  FAIL  memory grew with the call count: ${base_kb} KB at $LOOP_BASE, ${big_kb} KB at $LOOP_BIG"
          echo "        what a Rust function returns is not being freed"
          fail=$((fail + 1))
        else
          echo "  ok    Rust calls, memory held                        (${base_kb} KB, then ${big_kb} KB)"
          pass=$((pass + 1))
        fi
      fi
    fi
  fi
fi

echo
echo "ffi gate: $pass ok, $skip skipped, $fail failed"
(( fail == 0 ))
