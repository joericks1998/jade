#!/usr/bin/env bash
#
# Bind a real C library and run it on both engines.
#
# The FFI is the one part of the toolchain whose correctness depends on code we
# do not write: someone else's header, someone else's macros, and a C compiler's
# opinion of the shim we generate from them. Unit tests cover the shapes we
# thought of. This covers the ones we did not.
#
# Two gates, and they catch different classes:
#
#   1. The fortify check. glibc's `realpath` writes up to PATH_MAX bytes into the
#      buffer it is given and aborts the process when that buffer is smaller —
#      but only in an optimised build, which is why `cargo test` and the parity
#      gate both miss it. Every FFI package in a compiled binary died at startup
#      on Linux for two releases. glibc says so at compile time, so compiling the
#      C runtime optimised and refusing the warning is enough, and takes seconds
#      rather than the minutes a release build of the toolchain would.
#
#      It only bites on glibc. Apple's headers carry no such attribute, so on a
#      Mac this step passes on code that aborts on Linux — which is how the bug
#      shipped in the first place. CI is the one that counts.
#
#   2. The end-to-end run. glib is the library because it is big and ordinary:
#      1890 exported symbols written the way real libraries are written. Binding
#      it turned up two bugs the seven-library survey never did — a callback
#      parameter checked against the typedef's name instead of its category, and
#      a function-like macro shadowing the symbol we bound. Each one refused the
#      whole dependency, so glib bound 1357 symbols and could not be used at all.
#
#   3. The long run. Step 2 calls each binding once, which proves the answer is
#      right and nothing else. `alloc_str` is a claim about many calls — the
#      shim copies the string out and hands the original to the library's free
#      function, so a long run holds no more memory than a short one. One call
#      cannot tell a correct release from a leak, and cannot tell either from a
#      crash that only shows up under sustained churn. A user hit exactly that:
#      SIGSEGV at roughly 300,000 iterations over `g_uri_escape_string`.
#
# All three are skipped rather than failed when what they need is absent, so
# this is safe to run anywhere. Skips are reported, never silent.
#
# Usage: src/scripts/ffi-gate.sh [path-to-jade-binary]

set -uo pipefail

JADE="${1:-./target/debug/jade}"
# Absolute, because every step below runs from a scratch directory.
case "$JADE" in /*) ;; *) JADE="$PWD/${JADE#./}" ;; esac
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
FIXTURE="$ROOT/src/scripts/glib-fixture.jde"
LOOP_FIXTURE="$ROOT/src/scripts/alloc-str-loop.jde"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

pass=0; skip=0; fail=0

# ── 1. The C runtime, compiled the way a release build compiles it ────────────
#
# `-Werror=attribute-warning` is the whole point: glibc marks the fortified
# entry points with a warning attribute saying what is wrong with the call, and
# `build.rs` compiles with warnings off, so the message existed and nobody saw
# it. Anything else that trips at -O2 shows up here too.
if command -v cc >/dev/null 2>&1; then
  cerr="$WORK/fortify.txt"
  : > "$cerr"
  for f in "$ROOT"/src/runtime_aot/*.c "$ROOT"/src/runtime_aot"/infer/"*.c; do
    cc -O2 -D_FORTIFY_SOURCE=3 -Wall -Werror=attribute-warning \
       -I "$ROOT/src/runtime_aot" -I "$ROOT/src/runtime_aot/infer" \
       -c -o "$WORK/o.o" "$f" 2>>"$cerr"
  done
  if grep -qE '\berror\b' "$cerr"; then
    echo "  FAIL  the C runtime does not compile clean at -O2 with fortify:"
    grep -E '\berror\b' "$cerr" | head -10 | sed 's/^/        /'
    fail=$((fail + 1))
  else
    echo "  ok    C runtime, -O2 with _FORTIFY_SOURCE=3"
    pass=$((pass + 1))
  fi
else
  echo "  skip  C runtime fortify check                        (no C compiler)"
  skip=$((skip + 1))
fi

# ── 2. glib, bound and run on both engines ────────────────────────────────────
#
# pkg-config rather than a hard-coded path: the library sits in an
# architecture-named directory on Linux and under the Homebrew prefix on macOS,
# and neither is worth guessing.
glib_lib=""; glib_hdr=""; incs=()
# Set by step 2 once glib is installed, so step 3 can reuse the same project
# rather than bind the header a second time. Binding glib is nearly all of this
# script's runtime; doing it twice would roughly double it.
loop_proj=""; loop_skip="glib not installed"
if command -v pkg-config >/dev/null 2>&1 && pkg-config --exists glib-2.0 2>/dev/null; then
  libdir="$(pkg-config --variable=libdir glib-2.0)"
  for ext in so dylib; do
    [ -e "$libdir/libglib-2.0.$ext" ] && glib_lib="$libdir/libglib-2.0.$ext" && break
  done
  glib_hdr="$(pkg-config --variable=includedir glib-2.0)/glib-2.0/glib.h"
  for flag in $(pkg-config --cflags-only-I glib-2.0); do incs+=(--include "${flag#-I}"); done
else
  # Homebrew installs glib without necessarily installing pkg-config, and a
  # developer's own machine is where this gate is most worth running.
  for prefix in /opt/homebrew /usr/local; do
    if [ -e "$prefix/lib/libglib-2.0.dylib" ] && [ -e "$prefix/include/glib-2.0/glib.h" ]; then
      glib_lib="$prefix/lib/libglib-2.0.dylib"
      glib_hdr="$prefix/include/glib-2.0/glib.h"
      # glibconfig.h is generated per install and lives beside the library.
      incs=(--include "$prefix/include/glib-2.0" --include "$prefix/lib/glib-2.0/include")
      break
    fi
  done
fi

if [ -z "$glib_lib" ] || [ ! -e "$glib_hdr" ]; then
  echo "  skip  glib bound and run on both engines             (glib not installed)"
  skip=$((skip + 1))
else
  hdr="$glib_hdr"

  proj="$WORK/glib"
  mkdir -p "$proj"
  cp "$FIXTURE" "$proj/main.jde"
  (
    cd "$proj" || exit 1
    "$JADE" init >/dev/null 2>&1
    # The whole header, not a narrowed slice: a slice would bind only the
    # shapes we already handle, which is the opposite of what this is for.
    "$JADE" pkg add glib --path "$glib_lib" --header "$hdr" "${incs[@]}" > add.txt 2> add.err
    status=$?
    # A string the caller owns is the one shape a header cannot express:
    # `g_basename` points into its argument and `g_strdup` mallocs, and both are
    # written `gchar *`. So the generator refuses all 125 of them and names the
    # spelling, and this is a user writing that spelling — by hand, in jade.toml,
    # exactly as the message says to.
    cat >> jade.toml <<'TOML'

[dependencies.glib.symbols.g_strdup]
args = ["str"]
ret = "alloc_str"
frees_with = "g_free"

[dependencies.glib.symbols.g_ascii_strup]
args = ["str", "int"]
ret = "alloc_str"
frees_with = "g_free"
TOML
    "$JADE" pkg install >> add.txt 2>> add.err || status=$?
    echo $status > add.status
  )
  if [ "$(cat "$proj/add.status")" != 0 ]; then
    echo "  FAIL  glib does not install:"
    head -5 "$proj/add.err" | sed 's/^/        /'
    fail=$((fail + 1))
    loop_skip="glib does not install"
  else
    loop_proj="$proj"
    bound="$(grep -m1 -oE '^[0-9]+ bound' "$proj/add.txt" || echo '? bound')"
    # Every one of these keeps its exit status, and that is not a detail.
    #
    # A process killed by a signal *after* it has printed everything leaves an
    # output file that looks perfectly correct, and the two engines then agree
    # about it because both printed the same correct thing. This step used to
    # check only that the file was non-empty and free of the word "error", so a
    # SIGSEGV in the VM was reported as `ok` from v1.3.19 until v1.3.24 — the
    # crash was visible in the CI log the whole time, as a line the *shell*
    # printed, while the gate below it said 4 ok, 0 failed.
    #
    # Each runs through an inner shell for the reason `run_loop` documents: the
    # shell that waits on a signalled child announces it, and that line is noise
    # on top of a FAIL that already says the same thing.
    bash -c 'cd "$1" && "$2" run main.jde > vm.txt 2>&1' _ "$proj" "$JADE" 2>/dev/null
    vm_rc=$?
    bash -c 'cd "$1" && "$2" build main.jde -o app > build.txt 2>&1' _ "$proj" "$JADE" 2>/dev/null
    build_rc=$?
    aot_rc=0
    if [ "$build_rc" -eq 0 ]; then
      bash -c 'cd "$1" && ./app > aot.txt 2>&1' _ "$proj" 2>/dev/null
      aot_rc=$?
    fi
    if [ "$vm_rc" -ne 0 ] || [ ! -s "$proj/vm.txt" ] || grep -q "error" "$proj/vm.txt"; then
      echo "  FAIL  glib under the VM (exit $vm_rc):"
      head -5 "$proj/vm.txt" | sed 's/^/        /'
      fail=$((fail + 1))
    elif [ "$build_rc" -ne 0 ] || [ ! -f "$proj/aot.txt" ]; then
      echo "  FAIL  glib would not compile:"
      tail -5 "$proj/build.txt" | sed 's/^/        /'
      fail=$((fail + 1))
    elif [ "$aot_rc" -ne 0 ]; then
      echo "  FAIL  the compiled glib program did not survive its own run (exit $aot_rc):"
      tail -5 "$proj/aot.txt" | sed 's/^/        /'
      fail=$((fail + 1))
    elif ! diff -q "$proj/vm.txt" "$proj/aot.txt" >/dev/null; then
      echo "  FAIL  glib: the two engines disagree:"
      diff "$proj/vm.txt" "$proj/aot.txt" | head -10 | sed 's/^/        /'
      fail=$((fail + 1))
    else
      echo "  ok    glib bound and run on both engines             ($bound)"
      pass=$((pass + 1))
    fi
  fi
fi

# ── 3. An alloc_str binding, called until something breaks ────────────────────
#
# Two runs of the same program at different call counts. Surviving is the
# assertion that matters and is checked always; comparing what the two runs held
# needs a peak-RSS tool, and skips when there is none.
#
# Compiled only, not both engines. Two reasons. The VM is far slower per FFI
# call, so matching these counts there would cost minutes rather than seconds.
# And the failure being chased is a compiled one — the user's report is a
# compiled binary, and step 2 already covers the two engines agreeing about
# alloc_str. Nothing is lost by leaving the VM out of the long run.
#
# The counts: the smaller is a baseline, the larger has to clear the ~300,000
# from the user's report by enough to be worth calling a gate. 800,000 calls
# between them run in about two seconds compiled.
LOOP_BASE=200000
LOOP_BIG=600000
# A leaked string costs about 100 bytes: the v1.3.14 measurement was 62 MB held
# before the release was added and 42 MB after, over 200,000 calls. So the
# 400,000 extra calls in the big run cost roughly 40 MB if they leak. Ordinary
# allocator churn over the same 400,000 is a few MB. 16 MB sits between the two
# with room on both sides, which is why it is not a number tuned to one machine.
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
#
# Through an inner shell because the crash being looked for kills the process
# with a signal, and the shell that waits on it announces that with a
# "Segmentation fault" line of its own. Down here that line is noise on top of a
# FAIL that already says the same thing, so the inner shell absorbs it and hands
# back a plain exit status.
run_loop() {
  if [ -n "$time_flag" ]; then
    JADE_FFI_LOOP_ITERS="$1" bash -c \
      '/usr/bin/time "$1" "$2" > "$3" 2> "$4"' \
      _ "$time_flag" "$loop_proj/looper" "$2" "$2.time" 2>/dev/null
  else
    JADE_FFI_LOOP_ITERS="$1" bash -c \
      '"$1" > "$2" 2>&1' _ "$loop_proj/looper" "$2" 2>/dev/null
  fi
}

if [ -z "$loop_proj" ]; then
  printf '  skip  %-47s(%s)\n' "alloc_str called $LOOP_BIG times" "$loop_skip"
  skip=$((skip + 1))
  printf '  skip  %-47s(%s)\n' "alloc_str loop, memory held" "$loop_skip"
  skip=$((skip + 1))
else
  cp "$LOOP_FIXTURE" "$loop_proj/loop.jde"
  ( cd "$loop_proj" && "$JADE" build loop.jde -o looper > loopbuild.txt 2>&1 )
  if [ ! -x "$loop_proj/looper" ]; then
    echo "  FAIL  the alloc_str loop would not compile:"
    tail -5 "$loop_proj/loopbuild.txt" | sed 's/^/        /'
    fail=$((fail + 1))
    printf '  skip  %-47s(%s)\n' "alloc_str loop, memory held" "loop would not compile"
    skip=$((skip + 1))
  else
    run_loop "$LOOP_BASE" "$loop_proj/loop-base.txt"; base_rc=$?
    run_loop "$LOOP_BIG"  "$loop_proj/loop-big.txt";  big_rc=$?

    # A wrong answer is as much a failure as a crash: releasing the string
    # before it is copied out gives back garbage of the right length, so the
    # fixture compares every result and reports how many did not match.
    ok_out=1
    grep -qE "^calls +=  *$LOOP_BIG\$" "$loop_proj/loop-big.txt" || ok_out=0
    grep -qE "^mismatched +=  *0\$"    "$loop_proj/loop-big.txt" || ok_out=0

    if [ "$base_rc" -ne 0 ] || [ "$big_rc" -ne 0 ]; then
      bad=$LOOP_BIG; rc=$big_rc; out="$loop_proj/loop-big.txt"
      if [ "$base_rc" -ne 0 ]; then
        bad=$LOOP_BASE; rc=$base_rc; out="$loop_proj/loop-base.txt"
      fi
      echo "  FAIL  the compiled binary did not survive $bad alloc_str calls (exit $rc):"
      [ -s "$out" ] && tail -3 "$out" | sed 's/^/        /'
      fail=$((fail + 1))
      printf '  skip  %-47s(%s)\n' "alloc_str loop, memory held" "the loop did not survive"
      skip=$((skip + 1))
    elif [ "$ok_out" -eq 0 ]; then
      echo "  FAIL  alloc_str gave back the wrong string over $LOOP_BIG calls:"
      head -3 "$loop_proj/loop-big.txt" | sed 's/^/        /'
      fail=$((fail + 1))
      printf '  skip  %-47s(%s)\n' "alloc_str loop, memory held" "the loop gave wrong answers"
      skip=$((skip + 1))
    else
      printf '  ok    %-47s(%s)\n' "alloc_str called $LOOP_BIG times, compiled" "every answer correct"
      pass=$((pass + 1))

      if [ -z "$time_flag" ]; then
        printf '  skip  %-47s(%s)\n' "alloc_str loop, memory held" \
          "no /usr/bin/time -l or -v"
        skip=$((skip + 1))
      else
        base_kb="$(peak_rss_kb "$loop_proj/loop-base.txt.time")"
        big_kb="$(peak_rss_kb "$loop_proj/loop-big.txt.time")"
        grew=$((big_kb - base_kb))
        if [ "$grew" -gt "$LOOP_MARGIN_KB" ]; then
          echo "  FAIL  alloc_str holds memory in proportion to the calls made:"
          echo "        $LOOP_BASE calls held $((base_kb / 1024)) MB, $LOOP_BIG held $((big_kb / 1024)) MB"
          echo "        $((grew / 1024)) MB more for $((LOOP_BIG - LOOP_BASE)) more calls, over the $((LOOP_MARGIN_KB / 1024)) MB allowed"
          fail=$((fail + 1))
        else
          printf '  ok    %-47s(%s)\n' "alloc_str loop, memory held" \
            "$((base_kb / 1024)) MB then $((big_kb / 1024)) MB"
          pass=$((pass + 1))
        fi
      fi
    fi
  fi
fi

echo "ffi: $pass ok, $skip skipped, $fail failed"
[ "$fail" -eq 0 ]
