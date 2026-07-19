#!/usr/bin/env bash
#
# Every example must behave identically on both backends.
#
# Jade has two independent execution paths — the bytecode VM (`jade run`) and
# the AOT LLVM backend (`jade build`) — and they have drifted three times:
# the build daemon resolving imports against stale code, imported `extend`
# methods reaching AOT but not the VM, and imported field defaults likewise.
# Each was invisible because nothing ever ran the same program both ways and
# compared. This does that.
#
# Usage: scripts/backend-parity.sh [path-to-jade-binary]

set -uo pipefail

JADE="${1:-./target/debug/jade}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Examples excluded from parity, each for a reason that is not backend drift.
# Keep this list short and justified — an entry here is a blind spot.
skip_reason() {
  case "$1" in
    # Reach the network or the inference daemon; output depends on the
    # environment, not on the backend.
    examples/llm/*|examples/http/*|examples/uhttp/*)
      echo "needs network or the inference daemon" ;;
    # Fixtures that document rejected programs — they fail identically on both
    # backends by design, which `jade check` in CI already asserts.
    *_error.jde)
      echo "intentional-error fixture" ;;
    # Known AOT lowering gap: prompt struct fields are unsupported in lower.rs.
    # Remove this entry when that lands.
    examples/structs/prompt_fields/*)
      echo "AOT gap: prompt struct fields unsupported" ;;
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

  vm_out="$("$JADE" run "$file" 2>&1)"; vm_rc=$?

  bin="$WORK/$(echo "$file" | tr '/' '_').bin"
  if ! build_err="$("$JADE" build "$file" -o "$bin" 2>&1)"; then
    printf '  FAIL  %-52s (AOT build failed)\n' "$file"
    failures+=("$file: AOT build failed: $build_err")
    fail=$((fail + 1))
    continue
  fi

  aot_out="$("$bin" 2>&1)"; aot_rc=$?

  if [[ "$vm_out" == "$aot_out" && "$vm_rc" == "$aot_rc" ]]; then
    printf '  ok    %s\n' "$file"
    pass=$((pass + 1))
  else
    printf '  FAIL  %-52s (backends disagree)\n' "$file"
    failures+=("$file: VM(rc=$vm_rc) vs AOT(rc=$aot_rc)
--- vm
$vm_out
--- aot
$aot_out")
    fail=$((fail + 1))
  fi
done < <(find examples -name '*.jde' | sort)

echo
echo "parity: $pass ok, $skip skipped, $fail failed"

if (( fail > 0 )); then
  echo
  for f in "${failures[@]}"; do
    echo "=== $f"
    echo
  done
  exit 1
fi
