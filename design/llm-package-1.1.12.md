# `llm` package overhaul — 1.1.12

Status: **shipped in 1.1.12** (daemon `health` op + `0x05` frame landed; tool-call
helpers, model profiles, protocol controls, and health all wired and tested)
Goal: make the inference daemon's contract *consumable in `llm`* so model
profiles, protocol controls, token/stats, and daemon lifecycle are exposed to
Jade users — **decoupled from the daemon's source** (no shared crate, no git dep).

Cross-repo: the daemon lives at `../jade-inference-daemon`
(`github.com/joericks1998/jade-inference-daemon`). Its wire and per-model
vocabulary are factored into `jade-protocol` and `jade-model-profile` crates, but
jadelang **does not depend on them** — the socket is the only contract.

---

## 0. The core decision: the socket is the contract, no crate dependency

A well-defined socket *is* the interface. jadelang already implements the wire by
hand (`src/llm/jade_os.rs`: `encode_request`, `decode_frame`), the way any
protocol client does — independent of the server's source. Importing the daemon's
crates would only collapse two implementations of the same spec into one; it buys
drift-proofing at the cost of a git dep + CI reachability. We take the decoupled
path instead and drift-proof with a **spec + golden-bytes test**.

- **No `jade-protocol` dep.** Keep the hand-rolled wire; bring it up to spec.
- **No `jade-model-profile` dep.** Profiles are language-layer data that never
  cross the socket (§3); phase-1 carries the data itself, phase-2 decides
  reimplement-vs-vendor.
- **No CI change, no daemon-tag blocker** for the wire work.

### Current drift to fix

jadelang's hand-rolled request has fallen behind the daemon's `InferenceRequest`:

- missing `keep_anchors` → cannot do observable tool-span delimiting;
- missing `trust` → no prompt provenance.

Fix = add those fields + the new `health_only` flag + the `0x05` frame decode,
then lock it with a golden-bytes conformance test so any future drift fails CI.

---

## 1. Socket (unchanged)

`~/.jade/llm.sock`, confirmed on both sides (`jaded/src/main.rs:26`,
`jade_os.rs:11`). The `/run/jade` / `/dev/jade` mentions in daemon doc comments
are stale. No path or discovery change.

---

## 2. Wire spec (authoritative — this is what jadelang implements by hand)

### 2.1 Request: length-prefixed JSON `[u32 LE len][JSON]`

Mirror the daemon's `InferenceRequest` field-for-field (omit `Option`/`false`
defaults on the wire):

| field | type | notes |
|---|---|---|
| `prompt` | string | |
| `model` | string | `""` = active model |
| `max_tokens` | u32 | |
| `grammar` | string? | GBNF; omit when unconstrained |
| `anchor` | string? | grammar-enforcement span start |
| `stop_anchor` | string? | grammar-enforcement span end |
| `keep_anchors` | bool | **add** — make span boundary observable in-band; default false |
| `trust` | u8 | **add** — 0 TRUSTED / 1 TAINTED; default 0 |
| `count_only` | bool | tokenize only, return count in DONE |
| `stats_only` | bool | cumulative token counter |
| `health_only` | bool | **add** — daemon health snapshot (§2.3) |

### 2.2 Response frames `[u8 type][u16 LE len][payload]`

| byte | name | payload | |
|---|---|---|---|
| `0x01` | TOKEN | utf8 text chunk | existing |
| `0x02` | DONE | 8 bytes u64 LE tokens_used | existing |
| `0x03` | ERROR | utf8 message | existing |
| `0x04` | META | utf8 model name | existing |
| `0x05` | JSON | utf8 JSON structured result | **add** — accumulate to DONE |

### 2.3 `health_only` → `0x05 JSON` then `DONE { tokens_used: 0 }`

```jsonc
{
  "status":           "ok",                 // ok | degraded | loading | error
  "model":            "Qwen3-Coder-30B",
  "model_loaded":     true,
  "uptime_secs":      12345,
  "protocol_version": "0.2.0"
}
```

Serde-default everything so either side evolves freely. `queue_depth` /
`max_parallel` only if `jaded` actually tracks them.

> **Daemon-side (owner: you):** implement `health_only` + the `0x05` frame in
> `jade-protocol`/`jaded`. This is the *only* thing the runtime work blocks on —
> and only the `llm.health()` call, not the rest.

---

## 3. Surfaces → mechanism

| Surface | How it's exposed | Blocks on daemon? |
|---|---|---|
| **Protocol controls** | hand-rolled request gains `keep_anchors`, `trust`; expose from Jade | No |
| **Token & stats** | existing `count_only` / `stats_only`, surfaced as `llm` fns | No |
| **Model profile** | phase-1: carry profile data in jadelang, introspect via `llm.model()`/`llm.profile()`; phase-2: tool-call streaming (reimplement-or-vendor `ToolStreamParser`) | No (never on the wire) |
| **Daemon lifecycle** | new `health_only` + `0x05` frame (§2.3) | **Yes (health only)** |

Why model profiles aren't a socket concern: the wire is deliberately
model-agnostic — it carries opaque text + anchored spans and never knows `<tool>`
means "tool call." Profiles (delimiters, the stream parser) are language-layer
data; they don't travel over the socket, so "socket vs crate" doesn't apply —
it's reimplement-vs-vendor, deferred to phase 2.

---

## 4. Jade-facing API (fixture-first → `jade_evals/llm/package_controls.jde`)

### Phase 1 — ship in 1.1.12

```jade
use llm

llm.model()                    // → "Qwen3-Coder-30B"   (from Meta frame)
llm.profile()                  // → dict: tool_call {open, close, name_field}, spans[...]

llm.set_max_tokens(512)        // existing, kept
llm.keep_anchors(true)         // observable tool-span boundaries (new request field)

llm.count_tokens("…")          // → int   (count_only)
llm.total_tokens()             // → int   (stats_only)

let h = llm.health()           // → dict: status, model, model_loaded, uptime_secs, … (new op)
```

### Phase 2 — follow-up: tool-call streaming

Surface content/tool-call events (the `ToolStreamParser` concept, reimplemented
or vendored) so `?p` over a tool-enabled prompt yields structured
`tool_call {name, args}` instead of raw text. Language-design lift; later release.

---

## 5. Implementation order

Nothing here blocks on the daemon except step 6.

1. **`src/llm/mod.rs`:** add `keep_anchors`, `trust`, `health_only` to the
   request type; add `health()` to `InferenceBackend` (default `{status:"ok"}` for
   API backends); add an `0x05` JSON frame variant to the decoder enum.
2. **`src/llm/jade_os.rs`:** extend `encode_request` (new fields) and
   `decode_frame` (`0x05`); implement `health_blocking` over the socket.
3. **Golden-bytes test:** encode a fixed request, assert exact wire bytes; lock
   the frame decode against fixed byte inputs. Drift now fails CI.
4. **`src/compiler/vm.rs`:** new `NativeFnId` (`LlmHealth`, `LlmModel`,
   `LlmProfile`, `LlmKeepAnchors`); async dispatch; `keep_anchors`/`trust` as
   `VmState` session fields merged into each request.
5. **`builtins/llm_pkg.rs` + `type_infer`:** register `model`/`profile`/`health`/
   `keep_anchors` alongside existing `set_max_tokens`/`count_tokens`/`total_tokens`.
   Carry phase-1 profile data (the TOML vocabulary) in jadelang.
6. **Daemon (owner: you):** `health_only` + `0x05` in `jade-protocol`/`jaded`.
   Unblocks `llm.health()` end-to-end.
7. **Fixture + docs + changelog:** make `package_controls.jde` pass; bump to 1.1.12.
```
