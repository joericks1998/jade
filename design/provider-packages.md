# Provider packages — cloud inference without a daemon

Status: increments 1 (VM) + 2 (AOT) shipped on `1.1.24`; increment 3 (bundling +
installer) pending.

## Why

The `1.1.21` daemon split made the language a pure wire-protocol client to the
private `jade-tree` inference daemon and deleted every language-side way to say
"just use my Anthropic key." That optimized for the local-inference/privacy
story and threw out the *lightweight cloud path* with it — and local inference
needs hardware most machines don't have. If the daemon is the only door, the
language is effectively gated on a GPU.

This restores a cloud path any machine can run, **without re-teaching the public
language anything about Anthropic or OpenAI.** The provider ABI already exists in
`ovata-infer-protocol` (`PROVIDER_ABI_VERSION`): a provider `.so` exports
`ovata_provider_{abi_version,new,infer,free}` and owns its *own* HTTP to the
cloud inside `infer`. The daemon already hosts that ABI; we make **the language a
second host for it.** HTTP, endpoints, and key formats stay inside the provider
package (built in dovata).

## The core idea: the runtime is provider-blind

The runtime never enumerates providers, matches a name, or parses a selection. It
loads **the one `.so` sitting in a fixed slot** plus **one opaque config blob**,
and drives the ABI. Because every provider is byte-identical at the ABI, neither
the runtime nor the compiler learns a single vendor fact. All the "which
provider, what key" intelligence lives in the CLI — the only thing that writes
the slot.

## On-disk layout (all per-user, under `$HOME/.jade/`)

| Path | Owner | Contents |
|---|---|---|
| `provider/<name>.<ext>` | CLI | the installed pool — every provider the user has added |
| `provider/active/<name>.<ext>` | CLI | exactly ONE `.so`: the active provider the runtime loads |
| `provider/active/config.json` | CLI, `0600` via the credential file | the active provider's opaque credential blob |
| `credentials/<name>.json` | CLI, `0600` | per-provider key backups, so switching doesn't re-prompt |

Provider libraries are also discovered from where the toolchain ships them
(`<prefix>/lib/jade/providers/`, increment 3) and from `JADE_PROVIDERS_DIR`
(dev). `JADE_PROVIDER_ACTIVE` overrides the active-slot dir (testing), the same
shape as `JADE_LLM_SOCK`.

The slot is under `$HOME`, not beside the `jade` binary, because a compiled Jade
program is its *own* binary and can't find where `jade` was installed — but it
can find `$HOME`, exactly as the daemon socket (`$HOME/.jade/llm.sock`) already
does.

## Runtime — `jade-runtime::provider` (shared by both engines)

The driver is single-sourced in `jade-runtime` so the VM (rlib) and AOT-compiled
binaries (staticlib + C ABI) drive providers through identical code — no second
implementation to drift, the same principle that moved the daemon IPC out of C
in `1.1.20`.

- `active_lib_path()` / `active_config()` / `is_active()` — resolve the slot.
- `ProviderLib::load` + `run()` — dlopen the `.so` (via `libloading`, the one new
  `jade-runtime` dep), check `PROVIDER_ABI_VERSION`, `ovata_provider_new(config)`
  once (cached process-wide), drive `ovata_provider_infer` with a `FrameCallback`
  that decodes the same wire `Frame`s the daemon emits.
- `ffi`: `jrt_provider_{available,request,request_streaming}` — mirror the
  daemon's `jrt_ipc_*` signatures byte-for-byte.

**FFI safety** (audited): the provider side `catch_unwind`s every shim; our
`FrameCallback` is `catch_unwind`-wrapped too (unwinding across the `cdylib`
boundary is UB). The handle is shared across concurrent `infer` calls, which the
ABI permits (`Provider: Sync`, shared handle); each call has its own sink.

### AOT dispatch — `runtime_aot/infer/infer.c`

Every prompt path routes through a one-branch helper: *active provider? drive it
in-process (`jrt_provider_*`) : talk to the daemon (`jrt_ipc_*`).* Same
request/response shapes, so it's pure routing. `provider.h` declares the entry
points (implemented in Rust, linked via `libjade_runtime.a`).

## VM — `src/llm/provider_backend.rs`

A thin async facade over `jade_runtime::provider::run` (`spawn_blocking` +
mapping the driver's `String` errors to `JadeError` with the `?p` span).
`select_backend()` resolves **provider package → daemon socket → none**; the last
raises `NoInferenceBackend` (renamed from `MissingApiKey`), whose message points
at `jade register`.

## CLI — `src/providers/` + `jade register` / `jade use`

The sole writer of the active slot.

- `jade register [provider] [--key K] [--list] [--remove]` — interactive when no
  provider is named (list installed → pick → prompt for key). Copies the chosen
  `.so` into the pool and then the active slot; materializes the key to
  `active/config.json`. A key supplied only via `<PROVIDER>_API_KEY` is *not*
  written to disk — the provider reads it itself at runtime.
- `jade use <provider>` — switch the active provider without re-entering a key.
- `jade env` — shows the active provider, whether a key is set, and what's installed.
- Credentials: `store_credential` writes `0600` via a `create_new` temp +
  `rename` (enforces the mode even over a pre-existing loose file; atomic).
- "One active provider" is enforced by clearing the slot before each activation.

## Increment 3 — bundling + installer (pending)

- `.github/workflows/release.yml` — bundle the provider `.so`s from dovata's
  `providers-latest` release into the tarball under `lib/jade/providers/`, so a
  fresh install ships them.
- `install.sh` + `docs/static/install.sh` — drop the dead `jade configure` call
  and invoke `jade register` interactively (from `/dev/tty`), degrading to a
  printed hint when there's no tty.

## Verification

Both engines, end-to-end, against a real mock provider `.so`:

| | active provider | no provider + no daemon |
|---|---|---|
| `jade run` (VM) | `echo:…` ✓ | `NoInferenceBackend`, exit 1 ✓ |
| `jade build` → binary (AOT) | `echo:…` ✓ | falls back to daemon, exit 1 ✓ |

The credential blob provably reaches `from_config`; the compiled binary goes
`jrt_provider_available` → `jrt_provider_request` → dlopen → decode; the
no-provider case falls back to the daemon branch. `jrt_provider_*` verified as
defined in the Rust staticlib and referenced by the C runtime.

## Open items / risks

- **Env-only keys at register time.** A key passed as `<PROVIDER>_API_KEY` is
  snapshotted into `active/config.json` only if it was stored; otherwise the
  provider must read its own env var at runtime (kept off disk deliberately).
- **Interactive key entry echoes.** No `rpassword`/termios dep pulled in; the
  env-var path is the secret-free route. Hidden entry is a possible follow-up.
- **Provider `.so` at AOT runtime.** A compiled binary using `?p` depends on the
  active provider `.so` + config being present on the target — a deployment note
  for OS images (ship the `.so`, register it, or set the env key).
- **FrameSink ABI stability.** The language is now a second consumer of the
  provider ABI; the `ovata-infer-protocol` tag pin guards a bump.
