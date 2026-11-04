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
# Usage: src/scripts/golden.sh [path-to-jade-binary]

set -uo pipefail

JADE="${1:-./target/debug/jade}"
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
  if ! build_err="$("$JADE" build "$file" -o "$bin" 2>&1)"; then
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
done < <(find examples -name '*.jde' | sort)

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
    cat > "$HANDLE_DIR/jade.toml" <<'TOML'
[project]
name = "handle-fixture"

[lib.handlefix]
path  = "."
files = ["handlefix.so"]
TOML
    if ! h_build="$("$JADE" build "$HANDLE_DIR/handles.jde" -o "$HANDLE_DIR/handles.bin" 2>&1)"; then
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

if (( fail > 0 )); then
  echo
  for f in "${failures[@]}"; do
    echo "--- $f"
  done
fi

echo
echo "golden: $pass ok, $skip skipped, $fail failed"
(( fail == 0 ))
