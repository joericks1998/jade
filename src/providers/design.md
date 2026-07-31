# Provider packages — how the language reaches a model

Status: shipped. VM path in v1.1.24; AOT path in v1.1.25. Sole path since v1.1.30,
when the inference daemon and its socket were removed.

## Why

The `1.1.21` daemon split made the language a pure wire-protocol client to the
private `jade-tree` daemon and deleted every language-side way to say "just use my
Anthropic key." That gated the language on local-model hardware. This restored a
cloud path any machine can run, **without teaching the public language anything
about Anthropic or OpenAI.**

For four releases both paths existed and `?p` chose between them. They were two
ways to do one thing, and the daemon was the one with a serialization boundary in
the middle: a linked package is a function call, a daemon is a socket, a wire
format, a framing layer, and a second process to keep running. v1.1.30 removed
the daemon path — see *What the daemon removal changed* below.

## What a provider is

A provider is a **compiled Jade `--lib` package** (built in dovata: `anthropic`,
`openai`, plus local profiles like `qwen3-coder-30b`). It does its own HTTP to the
vendor API and exposes two Jade functions:

- **`infer(request) -> [Frame]`** — `request` is an `InferRequest` struct
  (`input`, `grammar`, `anchor`, `stop_anchor`); returns an **array of frames**.
  Success → `Token*` then `Done`, with an optional leading `Meta` and any `Json`
  (tool calls); failure → a single `Error`. Every request field is always present,
  `nil` when the language has nothing to say.
- **`configure(opts)`** — sets runtime-mutable params (api_key, model, temperature,
  tools, system, …). Optional; the package can also read its own env var.

Both shapes are declared once, outside this repo, in the `ovata-infer-protocol`
submodule at `src/protocol/jade/infer.jde`. A package registers `jade/` as a
`[lib]` and `use ovata::infer`, so it reads and returns those definitions rather
than copies of them. The compiler keeps a hand-written copy of the names, tripwired
against that file in `src/llm/tests.rs`.

A frame may be written two ways, and the language accepts either:

```jade
Token { text: "hi" }              // the struct form — its type name is the tag
{"type": "Token", "text": "hi"}   // the dict form — the tag is under "type"
```

Anything else **raises**. The decoder used to skip what it could not read, so a
provider that renamed `text` or wrote `"token"` lowercase produced an empty reply
with no error at any layer — the model appearing to have said nothing.

One wrinkle with the struct form: a Jade array literal must be homogeneous, so
`[Token {…}, Done {…}]` is a type error. Build the array with `push`. The dict form
has no such restriction, since every dict is one type.

The language loads a package through the **native-package machinery** (the same
`jade_pkg_init` C-ABI that `jade build --lib` emits and `jade pkg add --c-abi`
consumes), calls `configure` with the stored credential, calls `infer` with an
`InferRequest`, and folds the frames into the response text. It is provider-blind:
it loads whatever single package is in the active slot and never learns a vendor
detail.

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
program is its own binary and can only find `$HOME`.

Discovery accepts both `.so` and `.dylib` — providers ship as `.so` on every
platform and `dlopen` ignores the extension.

## VM — `src/llm/provider_backend.rs`

An `InferenceBackend` that, on first prompt, loads the active package via
`crate::native::load_native_package`, calls `configure(config_dict)` (from
`active/config.json`), then per prompt builds an `InferRequest` in `request_value`,
calls `infer` via `NativeLibFn::call`, and folds the returned frames in
`decode_frames`. `configure`/`infer` cross the package boundary through the native
FFI (`vm_to_ffi`/`ffi_to_vm`, deep-copied via a process-shared allocator), which
carries structs as of v1.1.31. `select_backend()` resolves **provider package →
none**; the second raises `NoInferenceBackend`, pointing at `jade register`.

A struct crosses the boundary under its *source* name. `aot/imports.rs` renames an
imported module-global `Foo` to `Foo$2` while flattening imports, and that name is
baked into the compiled library — so `native::abi_type_name` strips the suffix on
the way out, and `ffi_strdup_abi_type` does the same in `runtime_aot/native.c`.
Without it, a provider built with `use ovata::infer` returns frames named `Token$0`
and the caller does not recognise its own protocol.

## AOT — `runtime_aot/infer/infer.c`

Every `jrt_prompt*` drives the provider. This is pure C reusing the existing
native-package path —
`jrt_native_load` the active `.so` (path/config from jade-runtime's
`jrt_provider_active_lib_path`/`_config`), `jrt_native_call(handle,"configure",…)`
once, `jrt_native_call(handle,"infer",…)`, then walk the returned frame array with
`jrt_coll_*`, accumulating `Token` text and raising a catchable Jade error on
`Error` or on any frame it cannot read. Same `jrt_native_call` marshalling as the
VM, so no second decoder. The request is built with `jrt_kstruct_*`, the config
with `jrt_json_parse_chunk`.

Constrained decoding rides the same path: `grammar`, `anchor`, and `stop_anchor`
are request fields and enforcing them is the package's job. They travel
together — the anchors bound the span the grammar constrains, so sending the
pattern alone would silently drop half of an explicit
`Grammar.new(pattern, anchor, stop)`. A package that cannot honour a grammar
returns an `Error` frame, which is a catchable Jade error rather than an
unconstrained reply.

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
no-provider case raises `NoInferenceBackend` in both engines.

## What the daemon removal changed (v1.1.30)

Deleted: `jade-runtime`'s `infer/` module (the socket client, framing, and its C
entry points), `src/llm/jaded.rs`, `runtime_aot/ipc/`, the hand-rolled JSON
request builder in `infer.c`, the `ovata-infer-protocol` dependency in both
crates, and `JADE_LLM_SOCK`.

`InferenceRequest` stopped being a wire type and became four fields —
`prompt`, `grammar`, `anchor`, `stop_anchor`. The rest went with the wire:
`model`, `max_tokens`, `keep_anchors`, and `trust` were already pinned to fixed
defaults, `count_only`/`stats_only`/`health_only` lost their callers when the
`llm` package was removed, and `rlm` was never set by the language at all.

Two behaviours moved rather than disappeared. Grammar enforcement is now the
package's, which is why the packages had to accept `grammar` before this could
land. And the AOT streaming path now runs a provider's reply through the same
anchor-muting scanner the VM uses — it previously wrote provider replies straight
to stdout, so an anchored region the VM suppressed was printed by a compiled
binary.

The parity gate changed with it: `scripts/fake-jaded.py`, a stand-in daemon on a
socket, became `src/scripts/fake-provider.jde`, a stand-in package built with
`jade build --lib` into a throwaway slot. It exercises the path a released binary
actually takes.

## Known limits

- **Interactive key entry echoes** (no `rpassword` dep; env-var path is secret-free).
- **A compiled `?p` binary** depends on the active provider `.so` + config being
  present on the target machine.
