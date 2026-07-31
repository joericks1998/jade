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
# Inference is made deterministic by src/scripts/fake-provider.jde, a stand-in
# provider package answering every prompt with one canned reply. It is built once
# with `jade build --lib` and installed into a throwaway provider slot that
# JADE_PROVIDER_ACTIVE points at, so both engines load it the same way a released
# binary loads a real provider. examples/llm is therefore covered here rather than
# skipped — it was the largest blind spot in this gate, and the first thing it was
# pointed at turned up a live muting bug in the VM. An example supplies its own
# reply as `responses.txt` beside the .jde; without one it gets DEFAULT_REPLY.
#
# This used to be scripts/fake-jaded.py, a stand-in *daemon* on a Unix socket,
# restarted between the VM and AOT runs so each engine got the same script from
# the top. The provider is stateless — one reply, however many prompts — so
# there is nothing to reset between runs.
#
# Usage: src/scripts/backend-parity.sh [path-to-jade-binary]

set -uo pipefail

JADE="${1:-./target/debug/jade}"
WORK="$(mktemp -d)"
FAKE_PROVIDER="$(dirname "$0")/fake-provider.jde"
SLOT="$WORK/provider"
DEFAULT_REPLY="ok"

trap 'rm -rf "$WORK"' EXIT

# Build the stand-in provider once and install it as the active one. A slot holds
# exactly one library, and discovery takes whatever is in it, so the name is free.
# The stub imports the shared protocol definition through the [lib] entry in
# src/scripts/jade.toml, so this also proves that route works — it is how a real
# provider project reaches the same file.
mkdir -p "$SLOT"
if ! provider_err="$("$JADE" build "$FAKE_PROVIDER" --lib -o "$SLOT/fake.so" 2>&1)"; then
  echo "failed to build the stand-in provider:" >&2
  echo "$provider_err" >&2
  exit 1
fi
export JADE_PROVIDER_ACTIVE="$SLOT"

# The reply this example should get: the first non-comment, non-blank line of its
# responses.txt, or DEFAULT_REPLY when it has none.
reply_for() {
  local responses="$1"
  if [[ -n "$responses" ]]; then
    local line
    line="$(grep -v -e '^[[:space:]]*#' -e '^[[:space:]]*$' "$responses" | head -1)"
    if [[ -n "$line" ]]; then
      echo "$line"
      return
    fi
  fi
  echo "$DEFAULT_REPLY"
}

# Examples excluded from parity, each for a reason that is not backend drift.
# Keep this list short and justified — an entry here is a blind spot.
skip_reason() {
  case "$1" in
    # Reach the real network; output depends on the environment, not on the
    # backend. (examples/llm is NOT here — it runs against the stand-in provider.)
    examples/http/*|examples/uhttp/*)
      echo "needs the network" ;;
    # Fixtures that document rejected programs — they fail identically on both
    # backends by design, which `jade check` in CI already asserts.
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

  responses=""
  if [[ -f "$(dirname "$file")/responses.txt" ]]; then
    responses="$(dirname "$file")/responses.txt"
  fi

  JADE_FAKE_REPLY="$(reply_for "$responses")"
  export JADE_FAKE_REPLY

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
