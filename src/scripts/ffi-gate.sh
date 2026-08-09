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
# Both are skipped rather than failed when what they need is absent, so this is
# safe to run anywhere. Skips are reported, never silent.
#
# Usage: src/scripts/ffi-gate.sh [path-to-jade-binary]

set -uo pipefail

JADE="${1:-./target/debug/jade}"
# Absolute, because every step below runs from a scratch directory.
case "$JADE" in /*) ;; *) JADE="$PWD/${JADE#./}" ;; esac
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
FIXTURE="$ROOT/src/scripts/glib-fixture.jde"
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
    echo $? > add.status
  )
  if [ "$(cat "$proj/add.status")" != 0 ]; then
    echo "  FAIL  glib does not install:"
    head -5 "$proj/add.err" | sed 's/^/        /'
    fail=$((fail + 1))
  else
    bound="$(grep -m1 -oE '^[0-9]+ bound' "$proj/add.txt" || echo '? bound')"
    ( cd "$proj" && "$JADE" run main.jde > vm.txt 2>&1 )
    ( cd "$proj" && "$JADE" build main.jde -o app > build.txt 2>&1 && ./app > aot.txt 2>&1 )
    if [ ! -s "$proj/vm.txt" ] || grep -q "error" "$proj/vm.txt"; then
      echo "  FAIL  glib under the VM:"
      head -5 "$proj/vm.txt" | sed 's/^/        /'
      fail=$((fail + 1))
    elif [ ! -f "$proj/aot.txt" ]; then
      echo "  FAIL  glib would not compile:"
      tail -5 "$proj/build.txt" | sed 's/^/        /'
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

echo "ffi: $pass ok, $skip skipped, $fail failed"
[ "$fail" -eq 0 ]
