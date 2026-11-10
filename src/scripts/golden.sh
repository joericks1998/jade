#!/usr/bin/env bash
#
# Every example must print what it printed before the VM was removed.
#
# Up to v1.4.8, src/scripts/backend-parity.sh ran each example on the bytecode VM
# and on the compiled backend and diffed the two. v2 has one engine, so there is
# nothing left to diff against. Before the VM went, each example's output was
# recorded beside it as `<name>.expected`, from a run where both engines agreed.
# This builds each example, runs the binary, and compares it to that file.
#
# An example that should exit non-zero records its status in `<name>.expected-exit`.
# An example with no `.expected` file is reported as missing, never as passing.
#
# With --backend=static, it builds with Jade 2.0's static backend instead, and
# only the examples listed in src/scripts/static-examples.list, which grows phase
# by phase until it holds every example.
#
# Usage: src/scripts/golden.sh [--backend=static] [path-to-jade-binary]

set -uo pipefail

BACKEND_FLAGS=()
LIST=""
if [[ "${1:-}" == "--backend=static" ]]; then
  BACKEND_FLAGS=(--backend=static)
  LIST="$(dirname "$0")/static-examples.list"
  shift
fi

JADE="${1:-./target/debug/jade}"
# Absolute, since the C struct check runs `jade pkg install` from its own directory.
JADE="$(cd "$(dirname "$JADE")" && pwd)/$(basename "$JADE")"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Examples excluded from the gate, each for a reason that is not a regression.
# Keep this list short and justified: an entry here is a blind spot.
skip_reason() {
  case "$1" in
    # Reach the real network; output depends on the environment.
    examples/http/*|examples/uhttp/*)
      echo "needs the network" ;;
    # Fixtures that document rejected programs. `jade check` in CI asserts they fail.
    *_error.jde)
      echo "intentional-error fixture" ;;
    # Dereference a prompt. v2 dropped the provider packages these ran against,
    # and inference returns when the compiler emits it over dovata's memory.
    examples/decorators/prompt_tags/*|examples/llm/async_prompts/*|\
    examples/llm/basic_prompt/*|examples/llm/fstring_interp/*|\
    examples/llm/multi_prompt/*|examples/llm/single_quote_strings/*|\
    examples/llm/stream_aot/*|examples/llm/typed_bool/*|examples/llm/typed_char/*|\
    examples/llm/typed_int/*|examples/llm/typed_struct/*|\
    examples/llm/typed_struct_messy/*|examples/pipe/deref_chain/*|\
    examples/pipe/deref_in_print/*)
      echo "dereferences a prompt" ;;
    # Behavior the static backend has and the tagged one never will, since it
    # is deleted at the end of v2: destructors run only there, and only its ints
    # are 64-bit.
    examples/classes/destructors/*|examples/numbers/int_bounds/*)
      if [[ -z "$LIST" ]]; then echo "static backend only"; else echo ""; fi ;;
    *) echo "" ;;
  esac
}

pass=0; skip=0; fail=0
failures=()

while IFS= read -r file; do
  reason="$(skip_reason "$file")"
  if [[ -n "$reason" ]]; then
    printf '  skip  %-52s (%s)\n' "$file" "$reason"
    skip=$((skip + 1))
    continue
  fi

  stem="${file%.jde}"
  if [[ ! -f "$stem.expected" ]]; then
    printf '  FAIL  %-52s (no .expected file)\n' "$file"
    failures+=("$file: no $stem.expected; record one before relying on this example")
    fail=$((fail + 1))
    continue
  fi
  want_rc=0
  if [[ -f "$stem.expected-exit" ]]; then
    want_rc="$(cat "$stem.expected-exit")"
  fi

  bin="$WORK/$(echo "$file" | tr '/' '_').bin"
  if ! build_err="$("$JADE" build ${BACKEND_FLAGS[@]+"${BACKEND_FLAGS[@]}"} "$file" -o "$bin" 2>&1)"; then
    printf '  FAIL  %-52s (build failed)\n' "$file"
    failures+=("$file: build failed: $build_err")
    fail=$((fail + 1))
    continue
  fi

  # Command substitution drops trailing newlines, so compare through the same
  # printf the recording used.
  got="$("$bin" 2>&1)"; got_rc=$?
  if [[ "$(printf '%s\n' "$got")" == "$(cat "$stem.expected")" && "$got_rc" == "$want_rc" ]]; then
    printf '  ok    %s\n' "$file"
    pass=$((pass + 1))
  else
    printf '  FAIL  %-52s (output differs)\n' "$file"
    failures+=("$file: rc=$got_rc, want $want_rc
$(diff <(printf '%s\n' "$got") "$stem.expected")")
    fail=$((fail + 1))
  fi
done < <(if [[ -n "$LIST" ]]; then grep -v '^#' "$LIST" | grep .; else find examples -name '*.jde' | sort; fi)

# ── Native handles ───────────────────────────────────────────────────────────
#
# Handles are the one value kind `examples/` cannot reach: a handle only ever
# comes from a native C package, and a Jade package built with `--lib` has no
# way to mint one. So this runs its own fixture, against its own recorded
# output, rather than leaving the tag uncovered. The same blind spot let the
# bytes marshaller stay broken for three releases.
#
# Needs `cc`, which the C shim path already requires. Where there is none the
# check reports itself skipped rather than silently passing.
HANDLE_C="$(dirname "$0")/handle-fixture.c"
HANDLE_JDE="$(dirname "$0")/handle-fixture.jde"
HANDLE_WANT="$(dirname "$0")/handle-fixture.expected"
HANDLE_DIR="$WORK/handles"

if ! command -v cc >/dev/null 2>&1; then
  printf '  skip  %-52s (%s)\n' "native handle round-trip" "no C compiler"
  skip=$((skip + 1))
else
  mkdir -p "$HANDLE_DIR"
  case "$(uname -s)" in
    Darwin) shared=(-dynamiclib) ;;
    *)      shared=(-shared) ;;
  esac
  # One source of truth for the value ABI: the fixture reads it from the runtime
  # rather than carrying a literal someone has to remember to bump.
  ABI="$(sed -n 's/.*RUNTIME_ABI_VERSION: u32 = \([0-9]*\).*/\1/p' \
        "$(dirname "$0")/../runtime/src/lib.rs" | head -1)"
  if [ -z "$ABI" ]; then
    printf '  FAIL  %-52s (cannot read RUNTIME_ABI_VERSION)\n' "native handle round-trip"
    failures+=("native handle fixture: RUNTIME_ABI_VERSION not found")
    fail=$((fail + 1))
  elif ! cc_err="$(cc "${shared[@]}" -fPIC -DJADE_PKG_ABI="$ABI" "$HANDLE_C" -o "$HANDLE_DIR/handlefix.so" 2>&1)"; then
    printf '  FAIL  %-52s (fixture library did not build)\n' "native handle round-trip"
    failures+=("native handle fixture: $cc_err")
    fail=$((fail + 1))
  else
    # The module name is the file stem, so the library is named for the `use`
    # that imports it.
    cp "$HANDLE_JDE" "$HANDLE_DIR/handles.jde"
    # The `symbols` tables give the library's functions their static types,
    # which a Jade-ABI library cannot carry itself (design decision 11 in
    # design/v2-static-values.md). `tag_of` takes a handle of any type.
    cat > "$HANDLE_DIR/jade.toml" <<'TOML'
[project]
name = "handle-fixture"

[lib.handlefix]
path  = "."
files = ["handlefix.so"]

[lib.handlefix.symbols.open]
args = ["str"]
ret = "handle<Thing>"

[lib.handlefix.symbols.name]
args = ["handle<Thing>"]
ret = "str"

[lib.handlefix.symbols.tag_of]
args = ["handle"]
ret = "str"

[lib.handlefix.symbols.cursor]
args = ["handle<Thing>"]
ret = "handle<Cursor>"

[lib.handlefix.symbols.close]
args = ["handle<Thing>"]
ret = "nil"

[lib.handlefix.symbols.live]
args = []
ret = "int"

[lib.handlefix.symbols.visit]
args = ["handle<Cursor>", "fn(int, str) -> int"]
ret = "int"

[lib.handlefix.symbols.store]
args = ["fn(int) -> int"]
ret = "nil"

[lib.handlefix.symbols.pump]
args = ["int"]
ret = "int"
TOML
    if ! h_build="$("$JADE" build ${BACKEND_FLAGS[@]+"${BACKEND_FLAGS[@]}"} "$HANDLE_DIR/handles.jde" -o "$HANDLE_DIR/handles.bin" 2>&1)"; then
      printf '  FAIL  %-52s (build failed)\n' "native handle round-trip"
      failures+=("native handle round-trip: build failed: $h_build")
      fail=$((fail + 1))
    else
      h_got="$("$HANDLE_DIR/handles.bin" 2>&1)"; h_rc=$?
      if [[ "$(printf '%s\n' "$h_got")" == "$(cat "$HANDLE_WANT")" && "$h_rc" == 0 ]]; then
        printf '  ok    %s\n' "native handle round-trip"
        pass=$((pass + 1))
      else
        printf '  FAIL  %-52s (output differs)\n' "native handle round-trip"
        failures+=("native handle round-trip: rc=$h_rc
$(diff <(printf '%s\n' "$h_got") "$HANDLE_WANT")")
        fail=$((fail + 1))
      fi
    fi
  fi
fi

# ── C structs and several results ───────────────────────────────────────────
#
# A C dependency whose functions pass structs both ways and return several
# values at once, each of which the shim packs into a record. Like the handle
# fixture, examples/ cannot reach it, since it needs a C library; it needs `cc`
# to build that library and its shim.
STRUCT_C="$(dirname "$0")/struct-fixture.c"
STRUCT_H="$(dirname "$0")/struct-fixture.h"
STRUCT_JDE="$(dirname "$0")/struct-fixture.jde"
STRUCT_WANT="$(dirname "$0")/struct-fixture.expected"
STRUCT_DIR="$WORK/structs"

if ! command -v cc >/dev/null 2>&1; then
  printf '  skip  %-52s (%s)\n' "C struct round-trip" "no C compiler"
  skip=$((skip + 1))
else
  mkdir -p "$STRUCT_DIR/vendor"
  case "$(uname -s)" in
    Darwin) lib="libbox.dylib"; shared=(-dynamiclib) ;;
    *)      lib="libbox.so"; shared=(-shared) ;;
  esac
  cp "$STRUCT_H" "$STRUCT_DIR/vendor/struct-fixture.h"
  cp "$STRUCT_JDE" "$STRUCT_DIR/main.jde"
  cat > "$STRUCT_DIR/jade.toml" <<TOML
[project]
name = "struct-fixture"

[dependencies.box]
path = "vendor/$lib"
abi = "c"
headers = ["struct-fixture.h"]
include_dirs = ["vendor"]

[dependencies.box.symbols.box_make]
args = ["int", "int", "out_struct:box_t"]
ret = "nil"

[dependencies.box.symbols.box_area]
args = ["in_struct:box_t"]
ret = "float"

[dependencies.box.symbols.box_grow]
args = ["inout_struct:box_t@b", "int"]
ret = "scalar:int"

[dependencies.box.symbols.divmod]
args = ["int", "int", "out_scalar:int64_t@q", "out_scalar:int64_t@r"]
ret = "scalar:int"

[dependencies.box.symbols.pt_unit]
args = []
ret = "struct:pt_t"

[dependencies.box.structs.box_t]
fields = [["w", "int"], ["h", "int"], ["scale", "float"], ["name", "array<char>:16"]]

[dependencies.box.structs.pt_t]
fields = [["x", "int"], ["y", "int"]]
TOML
  if ! s_err="$(cc "${shared[@]}" -fPIC "$STRUCT_C" -I"$(dirname "$0")" -o "$STRUCT_DIR/vendor/$lib" 2>&1)"; then
    printf '  FAIL  %-52s (fixture library did not build)\n' "C struct round-trip"
    failures+=("C struct fixture: $s_err")
    fail=$((fail + 1))
  elif ! s_err="$(cd "$STRUCT_DIR" && "$JADE" pkg install 2>&1)"; then
    printf '  FAIL  %-52s (pkg install failed)\n' "C struct round-trip"
    failures+=("C struct fixture: $s_err")
    fail=$((fail + 1))
  elif ! s_err="$("$JADE" build ${BACKEND_FLAGS[@]+"${BACKEND_FLAGS[@]}"} "$STRUCT_DIR/main.jde" -o "$STRUCT_DIR/main.bin" 2>&1)"; then
    printf '  FAIL  %-52s (build failed)\n' "C struct round-trip"
    failures+=("C struct round-trip: build failed: $s_err")
    fail=$((fail + 1))
  else
    s_got="$("$STRUCT_DIR/main.bin" 2>&1)"; s_rc=$?
    if [[ "$(printf '%s\n' "$s_got")" == "$(cat "$STRUCT_WANT")" && "$s_rc" == 0 ]]; then
      printf '  ok    %s\n' "C struct round-trip"
      pass=$((pass + 1))
    else
      printf '  FAIL  %-52s (output differs)\n' "C struct round-trip"
      failures+=("C struct round-trip: rc=$s_rc
$(diff <(printf '%s\n' "$s_got") "$STRUCT_WANT")")
      fail=$((fail + 1))
    fi
  fi
fi

if (( fail > 0 )); then
  echo
  for f in "${failures[@]}"; do
    echo "--- $f"
  done
fi

echo
echo "golden: $pass ok, $skip skipped, $fail failed"
(( fail == 0 ))
