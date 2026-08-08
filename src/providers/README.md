# `src/providers/` — the provider registry

## What this subtree is

The CLI-side management of inference provider packages. It is the **only writer** of the active slot that the runtime reads.

Everything lives per-user under `$HOME/.jade/`:

| Path | Contents |
|---|---|
| `provider/<name>.<ext>` | the installed pool — every provider the user has added |
| `provider/active/<name>.<ext>` | exactly one `.so`: the active provider both engines load |
| `provider/active/config.json` | the active provider's opaque credential blob |
| `credentials/<name>.json` | per-provider key backups, so switching does not re-prompt |

Provider libraries are also discovered from where the toolchain ships them (`<prefix>/lib/jade/providers/`, derived from the running binary) and from `JADE_PROVIDERS_DIR` for development, which takes highest priority.

## Why it is split from the runtime half

There are two questions: *which provider is active* and *where does the active slot live*. The second is needed by both engines at run time, so the paths live in `jade_runtime::provider`. The first is a management concern that only the CLI has, so it lives here.

The consequence of that split is the useful property: **the runtime never learns a provider's name.** Selecting a provider copies it into the pool and then into `active/`, so the engines only ever see the one active library and load it blind. Adding a vendor is packaging work, not language work.

Credentials are backed up per provider rather than only stored in the active slot, so switching back and forth does not make the user re-enter an API key.

## What each file does

- **`mod.rs`** — pool and credential path helpers (`pool_dir`, `credential_path`, `bundled_provider_dir`), discovery across the three search locations, and the add / select / configure operations that write the active slot.
- **`tests.rs`** — registry tests.

## What a provider actually is

A compiled Jade `--lib` package, built outside this repo, exporting two functions:

- **`infer(request) -> [Frame]`** — takes an `InferRequest` (`input`, `grammar`, `anchor`, `stop_anchor`) and returns frames: `Token*` then `Done` on success, with an optional leading `Meta` and any `Json` for tool calls; a single `Error` on failure. Every request field is always present, `nil` when the language has nothing to say.
- **`configure(opts)`** — runtime-mutable parameters (api_key, model, temperature, tools, system). Optional; a package may read its own env var instead.

Both shapes are declared once, outside this repo, in the `ovata-infer-protocol` submodule at `src/protocol/jade/infer.jde`. A package registers `jade/` as a `[lib]` and does `use ovata::infer`, so it reads and returns those definitions rather than copies. The compiler keeps a hand-written copy of the names, tripwired against that file by `src/llm/tests.rs`.

A frame may be written as a struct (`Token { text: "hi" }`, where the type name is the tag) or as a dict (`{"type": "Token", "text": "hi"}`). Anything else **raises**. The decoder used to skip what it could not read, so a provider that renamed `text` or wrote `"token"` lowercase produced an empty reply with no error at any layer — the model appearing to have said nothing.

The language is provider-blind. It loads whatever single package is in the active slot through the same `jade_pkg_init` C-ABI that `jade build --lib` emits, calls `configure` with the stored credential, calls `infer`, and folds the frames into the response. It never learns a vendor detail. That is the point: a cloud path any machine can run, without the public language knowing anything about Anthropic or OpenAI.

Superseded, and worth knowing because the reasoning still gets cited: releases 1.1.21 through 1.1.29 also had an inference *daemon*, reached over a socket. Two ways to do one thing, and the daemon was the one with a serialization boundary, a framing layer and a second process to keep running. It was removed in v1.1.30, and a linked package has been the sole path since.

## Who uses it

*Depends on:* `jade_runtime::provider` for `active_dir`, `is_provider_lib`, and `jade_home`.

*Used by:* `cli/register.rs`, which implements `jade register` and `jade use`. On the reading side, `llm/provider_backend.rs` (VM) and the C runtime's `jrt_native_*` (AOT binaries) load whatever is in the active slot.

## Gotchas

Nothing outside this module may write `provider/active/`. If a second writer appears, the invariant that there is exactly one active `.so` stops holding, and the engines have no way to detect that.

`install.sh` runs `jade register` right after installing, so this code path is often the very first thing a new user touches. Errors here should read as guidance.

## Building and testing

```sh
cargo test providers::
JADE_PROVIDERS_DIR=/path/to/built/providers ./target/debug/jade register
```

Background on what a provider package is and why it is shaped this way is under *What a provider actually is*, above.
