# `src/llm/` — the VM's inference backend

## What this subtree is

The seam between the language and whatever actually runs a model. When the VM evaluates `?p`, it calls an `InferenceBackend`; this directory holds the trait and its implementation.

There is one live path and one test double:

- **`ProviderPackageBackend`** — drives an installed *provider package*, which does its own HTTP to a vendor API.
- **`MockBackend`** — canned responses, used by `run_src_with_mock` in the VM tests.

## Why it looks like this

The language used to contain OpenAI and Anthropic HTTP clients. They are gone. Everything vendor-specific moved out into a provider package, so the language never learns a vendor detail. The trait remains as the seam the mock implements.

There used to be a second live path: `JadedBackend`, which reached a local inference daemon over a Unix socket at `$HOME/.jade/llm.sock`. It was removed in v1.1.30, along with the socket, the shared wire protocol in `jade_runtime::infer`, and the `ovata-infer-protocol` dependency. A provider package is a linked library the engine calls directly, so the daemon was a second way to do the same thing with a serialization boundary in the middle.

A provider package is a compiled Jade `--lib` exporting `infer(request) -> [Frame]` and optionally `configure(opts)`. Frames are dicts: `{"type":"Token","text":…}`, `{"type":"Done",…}`, `{"type":"Error","message":…}`. The package does the HTTP; the language decodes frames. See `design/provider-packages.md` for the full shape.

## What each file does

- **`mod.rs`** — the `InferenceBackend` trait (`infer` plus a defaulted `infer_stream`), `InferenceRequest`, `InferenceResponse`, `MockBackend`, and `select_backend`, which returns the active provider or `None`.
- **`provider_backend.rs`** — the provider-package backend. Loads the package through `native/`, hands it stored config via `configure`, calls `infer(request)`, and decodes the returned frames. Constrained decoding rides the same call: `grammar`, `anchor`, and `stop_anchor` go in the request dict and the package enforces them.
- **`tests.rs`** — the shape of the request dict a package receives. This file used to hold golden tests pinning the exact JSON bytes sent to the daemon; there is no wire left to pin.

## Who uses it

*Depends on:* `native/` to load a provider package, `jade_runtime::provider` for the active-slot paths, and `frontend::error` for spans.

*Used by:* `vm/llm_prompt.rs` on every prompt dereference, and `vm/state.rs`, which holds the selected backend. `src/providers/` is the CLI-side counterpart that writes the slot this module reads.

## Gotchas

This is the *VM's* half only. Compiled binaries load and drive the same package through `runtime_aot/infer/` and the C runtime's `jrt_native_*`. A change to inference behavior usually needs both, and the two must build the same request dict — `provider_backend.rs`'s `request_value` and `infer.c`'s `provider_request` are the pair to keep in step.

`JADE_PROVIDER_ACTIVE` overrides the slot directory, which is what lets `scripts/fake-provider.jde` make the LLM examples deterministic and parity-testable.

## Building and testing

```sh
cargo test llm::
./scripts/backend-parity.sh    # covers examples/llm via the stand-in provider
```
