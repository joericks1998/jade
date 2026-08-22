# `src/llm/`: the VM's inference backend

## What this subtree is

This is the seam between the language and whatever actually runs a model. When the VM evaluates `?p`, it calls an `InferenceBackend`. This directory holds that trait and its implementations.

There is one live path and one test double:

- *`ProviderPackageBackend`* drives an installed *provider package*, which makes its own HTTP calls to a vendor API.
- *`MockBackend`* returns canned responses. `run_src_with_mock` in the VM tests uses it.

## Why it looks like this

The language used to contain OpenAI and Anthropic HTTP clients. They are gone. Everything vendor-specific moved out into a provider package, so the language never learns a vendor detail. The trait remains as the seam the mock implements.

There used to be a second live path called `JadedBackend`, which reached a local inference daemon over a Unix socket at `$HOME/.jade/llm.sock`. v1.1.30 removed it, along with the socket, the shared wire protocol in `jade_runtime::infer`, and the `ovata-infer-protocol` dependency. A provider package is a linked library the engine calls directly, so the daemon was a second way to do the same thing, with a serialization boundary in the middle.

A provider package is a compiled Jade `--lib` that exports `infer(request) -> [Frame]`, and optionally `configure(opts)`. The package makes the HTTP calls, and the language decodes the frames. See `src/providers/README.md` for the full shape.

Both directions are declared once, outside this repo, in the `ovata-infer-protocol` submodule at `src/protocol/jade/infer.jde`. The request is `InferRequest`, and the reply is the set of frames `Token`, `Done`, `Error`, `Meta`, and `Json`.

A frame may be written as a struct, whose type name is the frame name, or as a dict carrying that name under `"type"`. Anything else raises. Skipping unrecognised frames is what let a renamed key or a miscased tag read as an empty reply, with no error at any layer.

## What each file does

- *`mod.rs`* holds the `InferenceBackend` trait, meaning `infer` plus a defaulted `infer_stream`. It also holds `InferenceRequest`, `InferenceResponse`, `MockBackend`, and `select_backend`, which returns the active provider or `None`. The compiler's copy of the shared names lives here too, as `REQUEST_TYPE`, `REQUEST_FIELDS`, and `FRAME_TYPES`.
- *`provider_backend.rs`* is the provider-package backend. It loads the package through `native/`, hands it the stored config through `configure`, calls `infer` with a request built by `request_value`, and folds the reply together in `decode_frames`. Constrained decoding travels on the same call, because `grammar`, `anchor`, and `stop_anchor` are request fields and the package enforces them.
- *`tests.rs`* covers the shape of the request, how strictly the response is read, and the tripwire that checks both against the shared definition. It used to hold golden tests pinning the exact JSON bytes sent to the daemon. There is no wire left to pin.

## Who uses it

*Depends on:* `native/` to load a provider package, `jade_runtime::provider` for the active-slot paths, and `frontend::error` for spans.

*Used by:* `vm/llm_prompt.rs` on every prompt dereference, and `vm/state.rs`, which holds the selected backend. `src/providers/` is the CLI-side counterpart that writes the slot this module reads.

## Gotchas

This is the *VM's* half only. A compiled binary loads and drives the same package through `runtime_aot/infer/` and the C runtime's `jrt_native_*` functions. A change to inference behavior usually needs both halves, and the two must agree on the shapes in each direction. `request_value` and `provider_request` build the request, while `decode_frames` and `provider_infer_text` read the reply. The tripwire in `tests.rs` reads the source text of `infer.c`, because a Rust constant cannot reach C.

`JADE_PROVIDER_ACTIVE` overrides the slot directory. That is what lets `src/scripts/fake-provider.jde` make the LLM examples repeatable, so the parity gate can test them.

## Building and testing

```sh
cargo test llm::
./src/scripts/backend-parity.sh    # covers examples/llm via the stand-in provider
```
