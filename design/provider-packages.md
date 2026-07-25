# Provider packages — cloud inference without a daemon

Status: shipped. VM path in v1.1.24; AOT path in v1.1.25.

## Why

The `1.1.21` daemon split made the language a pure wire-protocol client to the
private `jade-tree` daemon and deleted every language-side way to say "just use my
Anthropic key." That gated the language on local-model hardware. This restores a
cloud path any machine can run, **without teaching the public language anything
about Anthropic or OpenAI.**

## What a provider is

A provider is a **compiled Jade `--lib` package** (built in dovata: `anthropic`,
`openai`, plus local profiles like `qwen3-coder-30b`). It does its own HTTP to the
vendor API and exposes two Jade functions:

- **`infer(request) -> [Frame]`** — `request` is a dict (`{prompt, model?,
  max_tokens?, grammar?, rlm?}`); returns an **array of frame dicts**. Success →
  `[Token?, Json?(tool_calls), Done]`; failure → `[Error]`.
- **`configure(opts)`** — sets runtime-mutable params (api_key, model, temperature,
  tools, system, …). Optional; the package can also read its own env var.
- **Frames are Jade dicts**, not wire bytes: `{"type":"Token","text":…}`,
  `{"type":"Done","tokens_used":…}`, `{"type":"Error","message":…}`,
  `{"type":"Json","json":…}`.

The language loads it through the **native-package machinery** (the same
`jade_pkg_init` C-ABI that `jade build --lib` emits and `jade pkg add --c-abi`
consumes), calls `configure` with the stored credential, calls `infer({prompt})`,
and decodes the frame array into the response text. It is provider-blind: it loads
whatever single package is in the active slot and never learns a vendor detail.

Note: this is **not** `ovata-infer-protocol`'s `Provider` cdylib ABI. That ABI
(`ovata_provider_*`) is what the *daemon* hosts; the language briefly targeted it
by mistake (the packages dovata ships are Jade `--lib`s, which export
`jade_pkg_init` + `jade_export$infer`, not `ovata_provider_*`). The language hosts
providers as Jade packages.

## On-disk layout (all per-user, under `$HOME/.jade/`)

| Path | Owner | Contents |
|---|---|---|
| `provider/<name>.<ext>` | CLI | the installed pool |
| `provider/active/<name>.<ext>` | CLI | exactly ONE `.so`: the active provider both engines load |
| `provider/active/config.json` | CLI, `0600` | the `configure()` argument (`{"api_key":…}` + any extra params) |
| `credentials/<name>.json` | CLI, `0600` | per-provider key backups, so switching doesn't re-prompt |

Also discovered from where the toolchain ships them
(`<prefix>/lib/jade/providers/`, bundled from dovata's `providers-latest`) and
`JADE_PROVIDERS_DIR` (dev). `JADE_PROVIDER_ACTIVE` overrides the active-slot dir.
The slot is under `$HOME`, not beside the `jade` binary, because a compiled Jade
program is its own binary and can only find `$HOME` (like the daemon socket).

Discovery accepts both `.so` and `.dylib` — providers ship as `.so` on every
platform and `dlopen` ignores the extension.

## VM — `src/llm/provider_backend.rs`

An `InferenceBackend` that, on first prompt, loads the active package via
`crate::native::load_native_package`, calls `configure(config_dict)` (from
`active/config.json`), then per prompt calls `infer(request_dict)` via
`NativeLibFn::call` and decodes the `[Frame]` array. `configure`/`infer` cross the
package boundary through the v1.1.24 native-FFI dict/array marshalling
(`vm_to_ffi`/`ffi_to_vm`, deep-copied via a process-shared allocator).
`select_backend()` resolves **provider package → daemon socket → none**; the last
raises `NoInferenceBackend`, pointing at `jade register`.

## AOT — `runtime_aot/infer/infer.c`

Every `jrt_prompt*` routes through a one-branch check: *active provider? drive it :
daemon.* The provider drive is pure C reusing the existing native-package path —
`jrt_native_load` the active `.so` (path/config from jade-runtime's
`jrt_provider_active_lib_path`/`_config`), `jrt_native_call(handle,"configure",…)`
once, `jrt_native_call(handle,"infer",…)`, then walk the returned frame-dict array
with `jrt_coll_*`, accumulating `Token` text and raising a catchable Jade error on
`Error`. Same `jrt_native_call` marshalling as the VM, so no second decoder. The
request/config dicts are built with `jrt_kdict_*` + `jrt_json_parse_chunk`.

Only plain `?p` is supported remotely — `grammar` (typed `?p |> Type`) and `rlm`
are rejected by the package with an `Error` frame, since a cloud API can't enforce
a GBNF grammar.

## CLI — `src/providers/` + `jade register` / `jade use`

The sole writer of the active slot.

- `jade register [PROVIDER] [KEY]` — interactive when unnamed (list installed →
  pick → prompt); `KEY` is a plain positional. Copies the chosen `.so` into the
  pool and the active slot, and materializes the key to `active/config.json`. A key
  supplied only via `<PROVIDER>_API_KEY` is not written to disk — the package reads
  it itself.
- `jade use <provider>` — switch without re-entering a key.
- `jade env` — shows the active provider, whether a key is set, what's installed.
- Credentials write `0600` via a `create_new` temp + `rename` (mode-enforcing, atomic).

## Distribution

Providers are built by dovata, staged on this repo's `providers-latest` release,
served at `jadelang.org/<name>-<platform>.so` (for `jade pkg add --url`) and
bundled into the release tarball under `lib/providers/` → `<prefix>/lib/jade/providers/`
(idempotent — skips until `providers-latest` exists). `install.sh` installs the
bundled providers and offers `jade register`.

## Verification

Both engines, end-to-end, against the **real published `anthropic.so`**:

| | valid key | dummy key |
|---|---|---|
| `jade run` (VM) | completion | catchable `HTTP 401` |
| `jade build` → binary (AOT) | completion | catchable `HTTP 401` (genuine Anthropic `request_id`) |

`register`→`configure(api_key)`→`infer`→live HTTPS→frame decode confirmed; the
no-provider case falls back to the daemon in both engines.

## Known limits

- **Typed deref / grammar / rlm** are unsupported remotely (the package returns an
  `Error` frame).
- **Interactive key entry echoes** (no `rpassword` dep; env-var path is secret-free).
- **A compiled `?p` binary** depends on the active provider `.so` + config being
  present on the target machine.
