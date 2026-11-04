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

if (( fail > 0 )); then
  echo
  for f in "${failures[@]}"; do
    echo "--- $f"
  done
fi

echo
echo "golden: $pass ok, $skip skipped, $fail failed"
(( fail == 0 ))
