# `src/providers/`: the provider registry

## What this subtree is

This handles the CLI side of managing inference provider packages. It is the *only writer* of the active slot that the runtime reads.

Everything lives per-user under `$HOME/.jade/`:

| Path | Contents |
|---|---|
| `provider/<name>.<ext>` | the installed pool, meaning every provider the user has added |
| `provider/active/<name>.<ext>` | exactly one `.so`: the active provider both engines load |
| `provider/active/config.json` | the active provider's opaque credential blob |
| `credentials/<name>.json` | per-provider key backups, so switching does not re-prompt |

Provider libraries are also found in two other places. One is where the toolchain ships them, at `<prefix>/lib/jade/providers/`, derived from the running binary. The other is `JADE_PROVIDERS_DIR`, meant for development, which takes highest priority.

## Why it is split from the runtime half

There are two questions here: *which provider is active*, and *where does the active slot live*. Both engines need the second answer at run time, so the paths live in `jade_runtime::provider`. The first is a management concern only the CLI has, so it lives here.

That split buys one useful property: *the runtime never learns a provider's name*. Selecting a provider copies it into the pool and then into `active/`, so the engines only ever see the one active library and load it blind. Adding a vendor is packaging work, not language work.

Credentials are backed up per provider rather than only stored in the active slot, so switching back and forth does not make the user re-enter an API key.

## What each file does

- *`mod.rs`* holds the pool and credential path helpers `pool_dir`, `credential_path`, and `bundled_provider_dir`. It also handles discovery across the three search locations, plus the add, select, and configure operations that write the active slot.
- *`tests.rs`* holds the registry tests.

## What a provider actually is

A provider is a compiled Jade `--lib` package, built outside this repo, exporting two functions.

- *`infer(request) -> [Frame]`* takes an `InferRequest`, whose fields are `input`, `grammar`, `anchor`, and `stop_anchor`. On success it returns some number of `Token` frames and then a `Done`, optionally with a leading `Meta` and any `Json` frames for tool calls. On failure it returns a single `Error`. Every request field is always present, and holds `nil` when the language has nothing to say.
- *`configure(opts)`* takes the parameters that can change at run time: `api_key`, `model`, `temperature`, `tools`, and `system`. It is optional, because a package may read its own environment variable instead.

Both shapes are declared once, outside this repo, in the `ovata-infer-protocol` submodule at `src/protocol/jade/infer.jde`. A package registers `jade/` as a `[lib]` and writes `use ovata::infer`, so it reads and returns those definitions rather than copies of them. The compiler keeps a hand-written copy of the names, and `src/llm/tests.rs` holds a tripwire test against that file.

A frame may be written as a struct, such as `Token { text: "hi" }`, where the type name is the tag. It may also be written as a dict, such as `{"type": "Token", "text": "hi"}`. Anything else *raises*.

The decoder used to skip what it could not read. So a provider that renamed `text`, or wrote `"token"` in lowercase, produced an empty reply with no error at any layer. The model appeared to have said nothing.

The language is provider-blind. It loads whatever single package sits in the active slot, through the same `jade_pkg_init` C ABI that `jade build --lib` emits. It calls `configure` with the stored credential, calls `infer`, and folds the frames into the response. It never learns a vendor detail. That is the point: a cloud path any machine can run, without the public language knowing anything about Anthropic or OpenAI.

One superseded design is worth knowing, because the reasoning still gets cited. Releases 1.1.21 through 1.1.29 also had an inference *daemon*, reached over a socket. That gave two ways to do one thing, and the daemon was the one carrying a serialization boundary, a framing layer, and a second process to keep running. v1.1.30 removed it, and a linked package has been the only path since.

## Who uses it

*Depends on:* `jade_runtime::provider` for `active_dir`, `is_provider_lib`, and `jade_home`.

*Used by:* `cli/register.rs`, which implements `jade register` and `jade use`. On the reading side, `llm/provider_backend.rs` in the VM and the C runtime's `jrt_native_*` functions in compiled binaries both load whatever is in the active slot.

## Gotchas

Nothing outside this module may write to `provider/active/`. If a second writer appears, the rule that exactly one active `.so` exists stops holding, and the engines have no way to detect that.

`install.sh` runs `jade register` right after installing, so this code path is often the very first thing a new user touches. Errors here should read as guidance.

## Building and testing

```sh
cargo test providers::
JADE_PROVIDERS_DIR=/path/to/built/providers ./target/debug/jade register
```

The section *What a provider actually is*, above, covers what a provider package is and why it has this shape.
