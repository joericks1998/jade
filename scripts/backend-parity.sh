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
# Inference is made deterministic by scripts/fake-jaded.py, a stand-in daemon
# serving canned responses over the real wire protocol. Both engines honour
# JADE_LLM_SOCK, so examples/llm is covered here rather than skipped — it was
# the largest blind spot in this gate, and the first thing it was pointed at
# turned up a live muting bug in the VM. An example supplies its own script as
# `responses.txt` beside the .jde; without one it gets DEFAULT_REPLY.
#
# The daemon is restarted between the VM and AOT runs of each example. Its
# responses are consumed in order, so a shared daemon would hand the second
# engine a different script than the first and manufacture failures.
#
# Usage: scripts/backend-parity.sh [path-to-jade-binary]

set -uo pipefail

JADE="${1:-./target/debug/jade}"
WORK="$(mktemp -d)"
FAKE_JADED="$(dirname "$0")/fake-jaded.py"
SOCK="$WORK/llm.sock"
DAEMON_PID=""
DEFAULT_REPLY="ok"

stop_daemon() {
  if [[ -n "$DAEMON_PID" ]] && kill -0 "$DAEMON_PID" 2>/dev/null; then
    kill "$DAEMON_PID" 2>/dev/null
    wait "$DAEMON_PID" 2>/dev/null
  fi
  DAEMON_PID=""
}

# Start the stand-in daemon and block until it is actually listening. It prints
# "ready" once bound, so there is no connect race to sleep around.
start_daemon() {
  local responses="$1"
  stop_daemon
  rm -f "$SOCK"
  local args=("$FAKE_JADED" "$SOCK")
  if [[ -n "$responses" ]]; then
    args+=(--responses "$responses")
  else
    args+=(--reply "$DEFAULT_REPLY")
  fi
  python3 "${args[@]}" > "$WORK/daemon.log" 2>&1 &
  DAEMON_PID=$!
  for _ in $(seq 1 100); do
    if grep -q ready "$WORK/daemon.log" 2>/dev/null; then return 0; fi
    if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
      echo "fake-jaded failed to start:" >&2
      cat "$WORK/daemon.log" >&2
      return 1
    fi
    sleep 0.05
  done
  echo "fake-jaded did not become ready" >&2
  return 1
}

trap 'stop_daemon; rm -rf "$WORK"' EXIT

export JADE_LLM_SOCK="$SOCK"

# Examples excluded from parity, each for a reason that is not backend drift.
# Keep this list short and justified — an entry here is a blind spot.
skip_reason() {
  case "$1" in
    # Reach the real network; output depends on the environment, not on the
    # backend. (examples/llm is NOT here — it runs against the stand-in daemon.)
    examples/http/*|examples/uhttp/*)
      echo "needs the network" ;;
    # Fixtures that document rejected programs — they fail identically on both
    # backends by design, which `jade check` in CI already asserts.
    *_error.jde)
      echo "intentional-error fixture" ;;
    # Known AOT lowering gap: prompt struct fields are unsupported in lower.rs.
    # Remove this entry when that lands.
    examples/structs/prompt_fields/*)
      echo "AOT gap: prompt struct fields unsupported" ;;
    # llm.model()/llm.profile() resolve against the *configured* model and the
    # jade-model-profile crate's table, so this reads the developer's own
    # config rather than anything the stand-in daemon controls. Environment
    # dependence, not backend drift.
    examples/llm/package_controls/*)
      echo "depends on the configured model profile" ;;
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

  if ! start_daemon "$responses"; then
    printf '  FAIL  %-52s (stand-in daemon failed to start)\n' "$file"
    failures+=("$file: fake-jaded failed to start")
    fail=$((fail + 1))
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

  # Fresh daemon: responses are consumed in order, so the AOT run has to start
  # from the same place the VM run did.
  if ! start_daemon "$responses"; then
    printf '  FAIL  %-52s (stand-in daemon failed to restart)\n' "$file"
    failures+=("$file: fake-jaded failed to restart")
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
