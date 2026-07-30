# `src/llm/` — the VM's inference backend

## What this subtree is

The seam between the language and whatever actually runs a model. When the VM evaluates `?p`, it calls an `InferenceBackend`; this directory holds the trait and its implementation.

There is one live path and one test double:

- **`ProviderPackageBackend`** — drives an installed *provider package*, which does its own HTTP to a vendor API.
- **`MockBackend`** — canned responses, used by `run_src_with_mock` in the VM tests.

## Why it looks like this

The language used to contain OpenAI and Anthropic HTTP clients. They are gone. Everything vendor-specific moved out into a provider package, so the language never learns a vendor detail. The trait remains as the seam the mock implements.

There used to be a second live path: `JadedBackend`, which reached a local inference daemon over a Unix socket at `$HOME/.jade/llm.sock`. It was removed in v1.1.30, along with the socket, the shared wire protocol in `jade_runtime::infer`, and the `ovata-infer-protocol` dependency. A provider package is a linked library the engine calls directly, so the daemon was a second way to do the same thing with a serialization boundary in the middle.

A provider package is a compiled Jade `--lib` exporting `infer(request) -> [Frame]` and optionally `configure(opts)`. The package does the HTTP; the language decodes frames. See `design/provider-packages.md` for the full shape.

Both directions are declared once, outside this repo, in the `ovata-infer-protocol` submodule at `src/protocol/jade/infer.jde`: the request as `InferRequest`, the reply as the `Token`/`Done`/`Error`/`Meta`/`Json` frames. A frame may be written as a struct — whose type name is the frame name — or as a dict carrying that name under `"type"`. Anything else raises. Skipping unrecognised frames is what let a renamed key or a miscased tag read as an empty reply, with no error at any layer.

## What each file does

- **`mod.rs`** — the `InferenceBackend` trait (`infer` plus a defaulted `infer_stream`), `InferenceRequest`, `InferenceResponse`, `MockBackend`, `select_backend` (the active provider or `None`), and the compiler's copy of the shared names: `REQUEST_TYPE`/`REQUEST_FIELDS` and `FRAME_TYPES`.
- **`provider_backend.rs`** — the provider-package backend. Loads the package through `native/`, hands it stored config via `configure`, calls `infer` with a request built by `request_value`, and folds the reply in `decode_frames`. Constrained decoding rides the same call: `grammar`, `anchor`, and `stop_anchor` are request fields and the package enforces them.
- **`tests.rs`** — the shape of the request, the strictness of the response, and the tripwire that checks both against the shared definition. It used to hold golden tests pinning the exact JSON bytes sent to the daemon; there is no wire left to pin.

## Who uses it

*Depends on:* `native/` to load a provider package, `jade_runtime::provider` for the active-slot paths, and `frontend::error` for spans.

*Used by:* `vm/llm_prompt.rs` on every prompt dereference, and `vm/state.rs`, which holds the selected backend. `src/providers/` is the CLI-side counterpart that writes the slot this module reads.

## Gotchas

This is the *VM's* half only. Compiled binaries load and drive the same package through `runtime_aot/infer/` and the C runtime's `jrt_native_*`. A change to inference behavior usually needs both, and the two must agree on the shapes in each direction: `request_value`/`provider_request` build the request, `decode_frames`/`provider_infer_text` read the reply. The tripwire in `tests.rs` reads `infer.c`'s source text, since a Rust constant cannot reach C.

`JADE_PROVIDER_ACTIVE` overrides the slot directory, which is what lets `scripts/fake-provider.jde` make the LLM examples deterministic and parity-testable.

## Building and testing

```sh
cargo test llm::
./scripts/backend-parity.sh    # covers examples/llm via the stand-in provider
```
