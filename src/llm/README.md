# `src/llm/` — the VM's inference backends

## What this subtree is

The seam between the language and whatever actually runs a model. When the VM evaluates `?p`, it calls an `InferenceBackend`; this directory holds the trait and its implementations.

There are two live paths and one test double:

- **`JadedBackend`** — talks to a local inference daemon over a Unix domain socket.
- **`ProviderPackageBackend`** — drives an installed *provider package*, which does its own HTTP to a vendor API. This is the daemon-free cloud path.
- **`MockBackend`** — canned responses, used by `run_src_with_mock` in the VM tests.

## Why it looks like this

The language used to contain OpenAI and Anthropic HTTP clients. They are gone. Everything vendor-specific moved out — either behind the daemon or into a provider package — so the language itself is a pure protocol client and never learns a vendor detail. The trait remains as the seam the mock implements and as the choice point between the two live paths.

The wire protocol lives once, in `jade_runtime::infer`, shared with compiled binaries. It used to exist twice — `runtime_aot/ipc/ipc.c` for AOT and `jaded.rs` for the VM, in two languages — and the two had already drifted on short `DONE` payloads, invalid UTF-8, connection reuse, and size ceilings. What stays in `jaded.rs` is only what is specific to running under the VM.

A provider package is a compiled Jade `--lib` exporting `infer(request) -> [Frame]` and optionally `configure(opts)`. Frames are dicts: `{"type":"Token","text":…}`, `{"type":"Done",…}`, `{"type":"Error","message":…}`. The package does the HTTP; the language decodes frames. See `design/provider-packages.md` for the full shape.

## What each file does

- **`mod.rs`** — the `InferenceBackend` trait (`infer` plus a defaulted `infer_stream`), `InferenceResponse`, `MockBackend`, the re-exported `InferenceRequest` wire type from the `ovata-infer-protocol` crate, and `select_backend`, which picks a path based on what the user registered.
- **`jaded.rs`** — the daemon backend. Builds request bodies, maps transport failures into catchable `JadeError`s with a source span, and does the stop-anchor trimming the streaming contract requires. **Each request gets its own connection**: compiled binaries hold one for the process, but the VM runs `async` prompts concurrently and a single serialized connection would turn those back into a sequence.
- **`provider_backend.rs`** — the provider-package backend. Loads the package through `native/`, hands it stored config via `configure`, calls `infer({prompt})`, and decodes the returned frames. Only plain `?p` is supported against a remote provider; constrained decoding needs the daemon.
- **`tests.rs`** — backend tests.

## Who uses it

*Depends on:* `native/` to load a provider package, `jade_runtime::infer` for the wire protocol, `jade_runtime::provider` for the active-slot paths, `frontend::error` for spans, and the external `ovata-infer-protocol` crate for the request type.

*Used by:* `vm/llm_prompt.rs` on every prompt dereference, and `vm/state.rs`, which holds the selected backend. `src/providers/` is the CLI-side counterpart that writes the slot this module reads.

## Gotchas

This is the *VM's* half only. Compiled binaries reach the same daemon through `runtime_aot/infer/` and `jade_runtime::infer`, and load provider packages through the C runtime's `jrt_native_*`. A change to inference behavior usually needs both.

Both engines honour `JADE_LLM_SOCK`, which is what lets `scripts/fake-jaded.py` make the LLM examples deterministic and parity-testable.

## Building and testing

```sh
cargo test llm::
./scripts/backend-parity.sh    # covers examples/llm via the fake daemon
```
