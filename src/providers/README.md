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
- **`design.md`** — a design note rather than code: what a provider package is, the `$HOME/.jade` layout, how both engines drive one, and how providers are built and distributed. It spans this directory, `src/llm/`, `src/runtime_aot/infer/`, and `jade_runtime::provider`, so it lives here rather than in any one of them.

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

Background on what a provider package is and why the design is shaped this way: `design.md`, beside this file.
