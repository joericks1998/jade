---
id: changelog
title: Changelog
sidebar_label: Changelog
---

## v1.1.32

- **A provider package built for an older Jade is now refused by name.** v1.1.31 began sending the inference request as a struct, which a package built before it cannot read — so every provider published to date failed with `native function returned an unknown value tag`, raised from inside the call, naming neither the version nor the fix. Both engines now compare the package's value ABI against the runtime's at load and refuse a mismatch with a message that says what to do. `jade build --lib` stamps every package it emits with the ABI it was built against; packages predating that are read through the runtime version they re-export. A plain C library wrapped by `jade pkg add --c-abi` declares neither, has no value ABI to disagree about, and loads as before.
- **If `?p` stopped working after upgrading, reinstall your providers.** The bundled `anthropic`/`openai` packages that ship in the release tarball must be rebuilt against v1.1.31 or later. Until they are, the error above is what you will see — which is the point of it.
- **Fixed: a compiled binary printed a prompt's text where `jade run` printed `<prompt>`.** A prompt is a type you dereference, not text you read, and it displays opaquely the way a future displays as `<future>`. The AOT backend held a prompt as the bare string it wraps, on the reasoning that a prompt only ever reaches a dereference — but a prompt can also be printed, held in a collection, stored in a struct field, passed to a function, or returned from one, and each of those showed a string where the interpreter showed a prompt. A prompt is now its own heap kind on that side too.
- **`jade build` supports `prompt` struct fields.** They were rejected outright, since there was nothing to store that would read back as a prompt. `examples/structs/prompt_fields` was excluded from the backend parity gate for that reason and now passes it, along with a check that a prompt keeps its type through a struct field and a collection.
- **Fixed: a compiled binary leaked one object per prompt.** The new heap kind was not reference-counted at first, so a loop building prompts never reclaimed them. A prompt is counted like a future now: header-carrying, owning one child.
- **The repository root holds only the things that must live there.** The Cargo build script moved to `src/runtime_aot/build.rs`, beside the C runtime it compiles, declared through `build =` in `Cargo.toml` — it could not go to `src/build.rs`, since `src/build/` already claims that module name. The backend parity gate and its stand-in inference provider moved to `src/scripts/`. The `design/` tree is gone, its two notes rehomed beside the code they govern as `src/providers/design.md` and `src/compiler/design.md`. And the root `install.sh` is gone: it was a byte-for-byte duplicate of `docs/static/install.sh`, which is the copy actually served at `jadelang.org/install.sh`. Nothing synced the two, so they matched only as long as every edit remembered both. No change to how anything builds, installs, or is tested.
- **An array literal may hold values of different types.** `[1, "two"]` and `[Token { text: "hi" }, Done { count: 2 }]` are now legal. The element type is whatever every element agrees on, and unknown when they differ — the same rule a dict's value type has always used. Nothing changed at runtime, because arrays were never typed there: the check was a frontend gate over machinery both engines already had, which is why building the same array with `push` had always worked. Mixed numerics widen rather than promoting, so `[1, 2.0]` has an unknown element type instead of a float one; typing `a[0]` as float while it holds an int would send compiled code down a specialized path for a value that is not that type.
- **What that costs you is compile-time errors, not correctness.** With a mixed array the compiler knows nothing specific about `arr[i]`, so `mixed[0] + mixed[1]` is checked when it runs rather than when it compiles. A uniform array still carries its element type and is checked as before.
- **Fixed: `contains` on a mixed array answered in the VM and raised in a compiled binary.** Membership was using the `==` operator, which rejects a comparison across types by design. But `arr.contains(x)` cannot ask which elements match without walking past the ones that do not, so an element of another type answers `false` rather than raising. Both engines now share one rule for it. `1` and `1.0` remain different values, matching `1 == 1.0` being an error.
- **Fixed: a compiled binary described a cross-type comparison wrongly.** `1 == "x"` reported `'==' requires numeric operands` — misleading, since the trouble is that the types differ, not that they are non-numeric — where `jade run` named both types. Both now say `'==' cannot compare int and str`, for every comparison operator.
- **Fixed: `jade run` leaked a Rust name into arithmetic type errors.** `1 + "x"` reported `Add requires numeric operands`, naming an internal enum variant, where the same program compiled said `'+' requires numeric operands`. Both now use the operator symbol.
- **Removed `JadeError::HeterogeneousArray`**, which nothing can raise any more.

## v1.1.31

- **Structs now cross the native package boundary, carrying their type name.** The FFI carried `nil`, `int`, `float`, `bool`, `str`, arrays, and dicts; a struct became `nil`. It is now a tag of its own — a dict plus the struct's type name, fields in declaration order — mirrored byte for byte in the VM's marshaller and the C runtime's. The type name is the point: a dict with the wrong keys reads as a set of nils and fails silently, so two programs sharing a dict share a convention, while two sharing a struct share a type the receiver can check. Functions and futures still do not cross. `RUNTIME_ABI_VERSION` is 2.
- **A provider receives an `InferRequest` struct, defined once outside this repo.** `?p` used to hand a provider package an anonymous dict whose keys — `prompt`, `grammar`, `anchor`, `stop_anchor` — were string literals written out separately in Rust and in C, and read by string in the package. A renamed key reached the provider as a silent `nil` with no error at any layer. The shape now lives in `ovata-infer-protocol`'s `jade/infer.jde`, consumed here as the `src/protocol/` submodule (pinned to `v0.5.0`) and by provider packages as a `[lib]` they `use ovata::infer`. Every field is always present; an unset one is `nil`, where the dict omitted the key.
- **The prompt field is called `input`.** `prompt` is a Jade keyword and cannot name a struct field in any spelling, and `input` is the better word regardless — what a provider receives is text to complete, not a Jade prompt binding.
- **The response is declared too, and an unreadable frame now raises.** The shared definition covers both directions: `Token`, `Done`, `Error`, `Meta`, and `Json` sit alongside the request. Both engines used to match a frame by bare string literal and *skip* anything they did not recognise, so a provider that wrote `"token"` lowercase or renamed `text` produced an empty reply with no error at any layer — the model appearing to have said nothing. An unknown frame type, a `Token` without string text, or an element that is neither a struct nor a tagged dict now raises a catchable inference error naming what arrived.
- **A frame may be a struct or a dict.** `Token { text: "hi" }` and `{"type": "Token", "text": "hi"}` are the same frame: a struct's type name is what the dict spells under `"type"`. The dict form stays supported. One wrinkle with structs — a Jade array literal must be homogeneous, so `[Token {…}, Done {…}]` is a type error; build the array with `push`.
- **Fixed: a struct crossing the FFI carried its import-mangled name.** `aot/imports.rs` renames an imported module-global `Foo` to `Foo$2` while flattening imports, and that name was baked into the compiled library, so a provider built with `use ovata::infer` returned frames named `Token$0` and the caller rejected its own protocol. Both marshallers now strip a trailing `$<digits>`, which is never part of a name anyone wrote — `$` is not legal in a Jade identifier. Affects any native package that returns a struct, not just providers.
- **Tripwire tests bind the compiler to that definition.** The compiler cannot `use` a `.jde` in its own Rust and C sources, so it keeps a hand-written copy of the names. Tests parse `src/protocol/jade/infer.jde` with the compiler's own lexer and parser — not a regex, so a comment naming a field cannot fool them — and fail on any difference in the request's fields or the set of frame names, in either direction. Two more read the C engine's source text and assert it names every request field and every frame, since a Rust constant cannot reach C. A missing submodule is a hard failure rather than a skip: an absent definition is exactly when drift goes unnoticed.
- **Fixed: `jade run` and `jade build` disagreed about which project a file belongs to.** The VM found the project root by walking up from the *current directory*, the AOT backend from the *source file's* directory. So from a repo root, `jade build sub/app.jde` resolved a `[lib]` import that `jade run sub/app.jde` reported as a missing file — same program, same shell, different answer. Both now resolve from the source file: which project a file belongs to is a property of the file, not of where you happen to be standing. `jade test` picks up the same rule. `jade run` with no file argument, `jade env`, and `jade pkg` still read the current directory, which is their only input. A new fixture, `examples/imports/project_lib/`, carries its own `jade.toml` so the parity gate can see this class of bug at all; nothing in `examples/` previously had a project root of its own.
- **Breaking for provider packages, again.** A package written against the v1.1.30 dict receives a struct and reads `request["prompt"]` as nothing. Providers must read `request.input` and be rebuilt. A package whose frames were already `Token`/`Done`/`Error`/`Meta`/`Json` needs no response change; one that emitted anything else now fails loudly instead of silently.

## v1.1.30

- **The inference daemon and its Unix socket are gone; a provider package is the only way to reach a model.** `?p` used to pick between two backends: the provider package it loads in-process, or a local daemon on `$HOME/.jade/llm.sock`. They did the same job, and the daemon was the one with a serialization boundary in the middle — a wire format, a framing layer, and a second process to keep running for what a linked library does with a function call. Removed with it: `jade-runtime`'s `infer/` module (the socket client, frame decoder, and `jrt_ipc_*` C entry points), `src/llm/jaded.rs`, `runtime_aot/ipc/`, the hand-rolled JSON request builder in `infer.c`, the `ovata-infer-protocol` dependency in both crates, and the `JADE_LLM_SOCK` environment variable. Roughly 1,900 lines. If you were running the daemon, run `jade register` to install a provider instead; a `?p` with none installed raises `NoInferenceBackend` naming that command.
- **Constrained decoding is the provider's job now.** A typed dereference (`?p |> Type`) or an explicit `Grammar.new` puts `grammar`, `anchor`, and `stop_anchor` in the request the package receives. The three travel together — the anchors bound the span the grammar constrains, so sending the pattern alone would silently drop half of a `Grammar.new(pattern, anchor, stop)`. A package that cannot honour a grammar returns an `Error` frame, which surfaces as a catchable Jade error rather than an unconstrained reply. This needs a provider built to accept them; older packages reject a grammar outright.
- **Fixed: a compiled binary printed anchored output the VM suppressed.** On the AOT streaming path, a provider's reply went straight to stdout without passing through the anchor-muting scanner, so `stream(?p, mute_on=[g])` printed a region that `jade run` hid. Both engines now run the reply through the same scanner. This was invisible to the parity gate, which reached the daemon on that path rather than a provider.
- **`InferenceRequest` is four fields instead of eleven.** It stopped being a wire type, so what the language cannot express no longer exists on it: `prompt`, `grammar`, `anchor`, `stop_anchor`. `model`, `max_tokens`, `keep_anchors`, and `trust` were already pinned to fixed defaults; `count_only`/`stats_only`/`health_only` lost their callers when the `llm` package was removed in v1.1.21; and `rlm` was never set by the language at all.
- **The parity gate's stand-in daemon became a stand-in provider.** `scripts/fake-jaded.py` served canned responses over a socket and had to be restarted between the VM and AOT runs of each example. `scripts/fake-provider.jde` is a Jade `--lib` the gate builds once and installs into a throwaway slot, so `examples/llm` now runs through the exact path a released binary takes. All 59 parity examples pass on both engines.
- **The two global allocators moved into `src/alloc/`, with tests.** `src/pool_alloc.rs` and `src/alloc_profile.rs` were loose files at the crate root; they are now `alloc::pool` and `alloc::profile` under one module whose docs carry the rule they exist to enforce — a global allocator is declared in the binary, never in `jade-runtime`, because a package linking a second instance is what corrupted the heap when this was mimalloc. Ten unit tests were added where there were none: the pool wrapper is checked for alignment, for actually delegating to the pool free list (a freed block must come back at the same address), for `alloc_zeroed` clearing a recycled block's stale bytes, and for preserving contents when a realloc crosses a size class; the profiler is checked for its bucket arithmetic at both ends and for its alloc/free/live-byte accounting, including that a realloc is counted as a free plus an alloc. No behavior change — the allocators themselves are byte-for-byte what they were.
- **Contributor documentation: a README in every major directory.** No language-visible change. Each major subtree of the repo now carries a `README.md` explaining what it is, why it was built that way, what each file does, and which other parts of the tree depend on it — the compiler pipeline (`frontend`, `compiler`, `bytecode`, `vm`, `aot`, `build`), both runtimes (`runtime`, `runtime_aot`), the language surface (`builtins`, `native`), the LLM path (`llm`, `providers`), the project tooling (`cli`, `project`, `pkg`, `cache`), and the non-code trees (`examples`, `scripts`, `bench`, `docs`, `design`). The root `README.md` is unchanged and remains the entry point for people working on the compiler.

## v1.1.28

- **REPL stops echoing redundant/void output.** An expression that prints as it evaluates — a bare `?p` (already suppressed) and now `stream(...)` — no longer has its result echoed again after the live output. And a void result is no longer echoed: `print("hi")` used to print `hi` then a stray `nil`; bare `nil` and any nil-returning call now display nothing.

## v1.1.27

- **Fixed a REPL defect: a string result printed its internal representation.** In the REPL, a bare expression evaluating to a string echoed the Rust struct `JStr { text: "…", trust: 0 }` instead of the string — most visibly with `stream(?p)`, but any bare string result was affected. It now echoes the string quoted (e.g. `"hey there!"`), Debugging the string contents rather than the internal tagged-string struct.

## v1.1.26

- **Fixed `jade upgrade` — it never worked.** It pointed at a nonexistent repo (`joericks1998/jade-os`, which 404s, so it silently reported "no releases published yet"), looked for a wrongly-named asset (`jade-<pkg-platform-tag>` like `jade-darwin-aarch64`, not the published `jade-macos-arm64.tar.gz`), and would have written the downloaded tarball straight to the binary path without extracting it. It now targets the real repo, matches the published archive name (`macos-arm64`/`linux-x86_64`), and extracts + installs the binary **plus** the runtime archives (`libJadeRuntime.a`/`libjade_runtime.a`) and bundled providers into `<prefix>/lib/jade/`, mirroring the installer, with an atomic binary replace. Note: any jade older than this still carries the broken `upgrade`, so reinstall once via `jadelang.org/install.sh` to reach a version whose `jade upgrade` works.

## v1.1.25

- **Provider-package `?p` now works in AOT-compiled binaries, not just the VM.** A provider is a compiled Jade `--lib` package (dovata's `anthropic`/`openai`) that exposes `infer(request) -> [Frame]` / `configure(opts)` and does its own HTTP to the vendor API. `jade run` already drove these; now a `jade build` binary does too — the C runtime loads the active provider through the existing native-package machinery (`jrt_native_load`/`jrt_native_call`, reusing the v1.1.24 dict/array FFI marshalling), calls `configure` with the stored credential, calls `infer({prompt})`, and folds the returned frame dicts into the response text. Each prompt path routes to the provider when one is active, else the daemon. An `Error` frame (e.g. a cloud auth failure) raises a catchable Jade error in both engines. Verified end-to-end on VM and AOT against the live Anthropic API. (The earlier `ovata_provider_*` cdylib ABI the language briefly targeted is gone — that ABI is the daemon's; the language hosts providers as Jade packages.)

## v1.1.24

- **Cloud inference with no daemon — `?p` through a provider package.** The `1.1.21` split made the daemon the only way to run inference, which effectively gated the language on local-model hardware. Inference providers (Anthropic, OpenAI) are now installable `.so` packages the language loads **in-process**, so `?p` works on any machine with just an API key — no daemon, no GPU. A provider is a library implementing `ovata-infer-protocol`'s `Provider` ABI (the same one the daemon hosts); the runtime loads the single active provider from `$HOME/.jade/provider/active/`, hands it an opaque credential blob, and decodes the same wire frames as the daemon path. It is deliberately **provider-blind** — one library, one config, no vendor knowledge in the language or the compiler. Works identically under `jade run` and `jade build`: the driver is single-sourced in `jade-runtime` (a `jrt_provider_*` C surface mirrors the daemon's `jrt_ipc_*`), and each prompt path routes to the provider when one is active, else the daemon. Providers ship with the toolchain; if none is active and no daemon is running, `?p` raises `NoInferenceBackend` (renamed from `MissingApiKey`) pointing at `jade register`.
- **`jade register` / `jade use` — choose and configure a provider.** `jade register [provider]` picks an inference provider (interactively when unnamed) and stores its API key under `~/.jade` (`0600`); `jade use <provider>` switches the active one without re-entering the key. A key can also come from `<PROVIDER>_API_KEY` in the environment, which is never written to disk. Exactly one provider is active at a time. `jade env` now reports the active provider, whether a key is set, and what's installed. The installer (`jadelang.org/install.sh`) offers to run `jade register` at the end instead of the removed `jade configure`.

## v1.1.23

- **`std/uhttp` now works in AOT-compiled binaries, not just the VM.** HTTP-over-Unix-socket was a VM-only package — `jade build` on a program using `uhttp.*` failed to lower it. The request transport core moved down into `jade-runtime` (one copy, shared by both engines, mirroring how `std/http` is structured), and a `jrt_uhttp_{get,post,put,delete,head}` C-ABI surface was added so native binaries reach it directly. `uhttp.get`/`post`/`put`/`delete`/`head` return the same `{status, body}` dict under `jade run` and `jade build`, with identical output verified against a live socket; a transport failure raises in both engines. Streaming (`uhttp.stream`) stays VM-only — it invokes a Jade handler per line and so can't be a pure native symbol.
- **Native packages can now exchange dicts and arrays, and no longer corrupt memory.** The native FFI (`jade build --lib` packages) previously marshalled only scalars — a dict or array argument silently became `nil`, and a dict/array return was dropped. The `JadeVal` ABI now carries arrays and dicts as nested trees, deep-copied at the boundary through a process-shared allocator, so collections (including nested ones) round-trip in both directions under both `jade run` and `jade build`. Structs still cross as unsupported (`nil`). Two memory-safety bugs are fixed with it: loading a package under the VM used to hang on exit, and dict/array results used to come back as corrupted memory — both were the same root cause (see below).
- **Replaced the mimalloc global allocator with our own, host-only pool.** mimalloc was declared in `jade-runtime`, which is *also* statically linked into every native package, so a process that loaded a package held two allocator instances whose duplicate symbols interposed across the boundary — corrupting the heap and deadlocking tokio's shutdown. It's gone. In its place the `jade` VM binary now installs a segregated free-list pool (size classes 8–4096 B, system fallback) declared **in the binary, never in `jade-runtime`** — so it applies only to the interpreter process and can never reach a loaded package. It recovers the ~2× on allocation-heavy code (`bench/alloc_heavy.jde`: 0.26s → 0.13s) without the corruption. The pool is shared with the AOT object path (`gc::leak_obj`) too; a `--features alloc-profile` build adds a size-class allocation profiler.
- **AOT: region allocation for non-escaping arrays.** Allocation-bound compiled code was dominated by collection churn (a 3M-iteration array loop spent ~97% of its time allocating). A new type-aware escape analysis on the typed IR proves which array literals never leave their region, and the AOT backend bump-allocates those in a per-frame arena — reset in bulk at each loop iteration and function return, with no per-object `malloc`/`free` and no refcounting. Sound by construction: arena objects carry an `ObjHeader` flag that makes the refcount ops no-op on them, so an arena pointer can flow through refcounted registers and never be freed by the collector — only the region reset frees it. v1 targets arrays of immediate scalars (`[i, i+1, i+2]`); a 3M-iteration non-escaping array loop drops from **0.15s to 0.06s (~2.5×)**, verified leak- and double-free-free by the heap instrument, with identical VM/AOT output. Constant-index literals (`[1,2,3][0]`) still fold to their elements with zero allocation.
- **Performance.** Backend/runtime optimizations with no language-visible change:
  - **AOT scalar specialization.** The native backend treated every value as a tagged word, so integer-only code (recursive `fib`, whose parameters are untyped) paid a runtime call per operation. Added the LLVM `-O2` pipeline, inlined an `is_heap` guard around reference-count ops so ints/bools/nil skip the runtime call, and inlined an int fast-path into dynamic `add`/`sub`/`mul` and compare. `fib(40)` compiled: **17.0s → 2.3s (7.4×)**, and native now beats Python where it was 2× slower. Overflow still raises exactly as the VM does.
  - **VM: FxHash globals.** Hash the interpreter's `globals` map with `FxHash` instead of SipHash — variable names are short internal keys, not attacker-controlled inputs. `fib(34)` under `jade run`: ~25% faster.
  - **VM: borrow the callee.** `Call` now borrows the `Arc<CompiledFn>` out of its slot for plain-function calls instead of cloning the whole value, avoiding an atomic refcount bump+drop per call (~10% more).
  - Internal: `src/vm/mod.rs` was split from a 3119-line monolith into focused modules (`dispatch`, `call`, `coerce`, `llm_prompt`, `ops`, `value`, `state`, `chunk`, `async_tasks`, `exceptions`).

## v1.1.22

- **Unified module imports; removed quoted file imports and the `as` alias (breaking).** There is now one import form: a `use` statement names a **module** with `::` notation (or a bare name) and binds its last path segment. A bare name resolves to a **sibling `.jde` file** (`use utils` → `./utils.jde`), a `::` path descends into subdirectories (`use sub::helper` → `./sub/helper.jde`), and the first segment naming a registered `[lib]` or an installed dependency resolves that instead. The quoted-path form (`use "lib.jde" as lib`) and the `as` alias are rejected at compile time (`QuotedImport` / `ImportAlias`) with a message pointing at the new syntax. Parent/cross-directory imports (`../`) are no longer expressible as a module path — register those directories as a `[lib]`, which anchors resolution at the project root. Resolution is identical in the VM and the AOT build.

## v1.1.21

- **Moved all remaining inference config to the daemon; the language is a pure wire-protocol client.** It no longer counts tokens (`llm.count_tokens`, `llm.total_tokens`, and the `token_count` state are gone), no longer caps generation length (`llm.set_max_tokens` is gone; requests send `max_tokens: 0`, so the daemon owns the budget), no longer tracks or selects a model (`llm.model()` is gone; requests send an empty `model`, so the daemon uses its configured/loaded one), no longer toggles anchor visibility (`llm.keep_anchors` is gone; requests send `keep_anchors: false`), and no longer re-asks the model on a coercion miss. A typed dereference (`?p |> Type`) is now single-shot — grammar-constrained sampling already forces the reply into the target shape — and raises `PromptOverflow` immediately if it doesn't coerce, in both the VM and the AOT engine. **The `use llm` package is removed entirely** — its remaining function, `llm.health()`, and the earlier `llm.model`/`keep_anchors`/`set_max_tokens`/`count_tokens`/`total_tokens`/`profile`/`find_tool_call`/`find_tool_calls`/`tool_grammar` are all gone. Running inference is language syntax now (`?p`, `?p |> Type`); the model-specific pieces ship with each model as Jade packages on the daemon side. The `JADE_MAX_RETRIES` env var and `max_retries` `jade.toml` key are removed

## v1.1.20

- **Dropped Windows support.** Jade is now macOS and Linux only. The toolchain is built on Unix domain sockets — `jade build` talks to the build daemon and the `jade` inference provider talks to the LLM daemon that way — so a Windows build was only ever the language with its interesting half stubbed out. Building for a non-Unix target now fails immediately with an explanatory error rather than producing a degraded binary. The `jade-windows-x86_64.zip` release artifact is no longer published; on Windows, use WSL2
- **Removed the build daemon.** `jade build` now compiles in-process. The daemon existed to keep LLVM out of the `jade` binary while code generation lived in a separate repository; once `src/aot/` and the C runtime moved here, its only remaining job was forwarding a request to a function this crate already exported — and a daemon built from an older commit could resolve imports differently from the CLI calling it, silently. LLVM 18 is now a build-time requirement for the toolchain (`LLVM_SYS_180_PREFIX`); running a released binary needs nothing installed. The `codegen` Cargo feature is gone, and `jade env` no longer reports daemon reachability
- Linux releases are now **glibc** (`x86_64-unknown-linux-gnu`) rather than musl: LLVM's prebuilt distributions are glibc-based, so a static musl build would mean sourcing or building a musl LLVM
- **Added a package manager.** `[dependencies]` in `jade.toml`, pinned by `jade.lock`, installed into a project-local `libs/`. Dependencies are prebuilt native shared libraries sourced from a URL or a local path — there is no registry, and so no transitive resolution and no version ranges. `jade pkg add/remove/install/update/list`; `jade run` and `jade test` install anything missing. A `{platform}` URL records an artifact per platform in the lock so a lock committed from macOS installs and verifies on Linux CI, while only the matching artifact is ever downloaded. Every artifact is checksum-verified on every install
- Dependencies are imported by **bare name** — `use fastmath` — resolving through the same `[lib]` machinery, so behavior is identical in the VM and the AOT build. A name matching both a library and a sibling `.jde` file is a hard error naming both
- Plain **C libraries** can be dependencies: `abi = "c"` plus a symbol table generates and compiles a binding shim, so a library exporting no `jade_pkg_init` still works. Requires a C compiler at install time
- **`jade build --lib`** compiles a Jade file to a shared library exporting `jade_pkg_init` — a package other Jade projects can depend on. `--export` narrows the binding set; the default is every top-level function

## v1.1.19

- Added the **`std/uhttp`** package — an HTTP/1.1 client that speaks over a **Unix domain socket** rather than a TCP host, for talking to local daemons such as the Docker Engine API (`/var/run/docker.sock`) and other socket-backed OS services. Mirrors `std/http`: `uhttp.get`/`post`/`put`/`delete`/`head` return the same `{status, body}` dict and accept an optional trailing `headers` dict
- The target is a single pseudo-URL of the form `unix://<socket-path>:<request-path>` (e.g. `unix:///var/run/docker.sock:/v1.43/containers/json`); the socket path runs to the first `:` after the scheme, the rest is the request path (defaulting to `/`)
- The transport is hand-framed HTTP/1.1 written directly onto a `UnixStream` — no new dependencies. Response framing honors `Content-Length`, `Transfer-Encoding: chunked` (de-chunked), and read-to-EOF on `Connection: close`. Unix-only; a missing socket, malformed pseudo-URL, or connection failure raises an `IoError`
- Added **`uhttp.stream(url, handler, headers?)`** for long-lived streaming endpoints (Docker `/events`, `/logs?follow=1`, image-pull progress). A worker thread owns the socket and decodes the body incrementally; the VM invokes the Jade `handler` once per newline-delimited line (mirroring the LLM token-stream drain pattern). The handler returning `false` stops the stream and closes the socket; `stream` returns the HTTP status once the stream ends

## v1.1.12

- Expanded the built-in `llm` package to expose the inference daemon's model profiles, tool-call helpers, protocol controls, and health to Jade programs. The package stays decoupled from the daemon — the Unix socket (`~/.jade/llm.sock`) is the only contract; jadelang implements the wire format itself, drift-guarded by a golden-bytes test
- Added **model profile** introspection — `llm.model()` returns the active model name; `llm.profile()` returns the model's token/tool vocabulary (tool-call delimiters, name field, special-token spans) as a dict. Profiles are selected by the model name the daemon reports
- Added **tool-call helpers** — `llm.find_tool_call(text)` returns the first tool call in a response as `{name, args}` (or `nil`); `llm.find_tool_calls(text)` returns all of them; `llm.tool_grammar()` returns the canonical tool-call GBNF. All resolve tool-call delimiters from the active model's profile, so they work across models. The canonical grammar is checked in at `grammars/tool_call.gbnf`
- Added **protocol controls** — the wire request now carries `keep_anchors` (toggle via `llm.keep_anchors(b)`, making tool-span boundaries observable in-band) and `trust` (prompt provenance), matching the daemon's request schema
- Added **daemon lifecycle** — `llm.health()` returns a daemon health snapshot (`status`, `model`, `model_loaded`, `uptime_secs`, `protocol_version`) via a new `health` op and structured-JSON response frame

## v1.1.11

- Improved type inference for values read out of a dict. A `let`-bound homogeneous dict literal now records its value type, so indexing it (`d["k"]`) infers that concrete type instead of `Unknown`. This lets the native (AOT) backend pick the right print/format codegen for, e.g., `bool` values stored in a dict; the VM is unaffected (it dispatches on runtime tags)
- Fixed a regression in unary `!` type inference. The v1.1.10 logical-operator fix typed *every* `!expr` as `bool`, which incorrectly accepted `!` on a known non-`bool` operand such as `!1` (this should be a `TypeError`). `!x` now short-circuits to `bool` only when the operand type is `Unknown` (e.g. `!method_call(x)` on an untyped value, where native codegen emits an `i1`); a known non-`bool` operand once again reports a `TypeError`. `&&` and `||` are unaffected — they continue to yield `bool` whenever an operand is `Unknown`

## v1.1.10

- Fixed a native build failure (LLVM verification error) when a function returns a logical expression with an untyped operand — `!x`, `a && b`, and `a || b` are now always typed as `bool` (matching the `i1` codegen emits), even when an operand is `Unknown` such as a method call on an untyped parameter. Previously these inferred `int`, mismatching the generated function signature. Mirrors the earlier comparison-operator fix

## v1.1.9

- **Breaking:** module-path imports now use `::` as the separator instead of `.` — `use std::math`, `from std::math use floor`, `use utils::math` for `[lib]` libraries. The `.` form is no longer accepted in module-path position (`.` is reserved for field and method access on values); `use std.math` is now a parse error
- Namespaced decorators also use `::` — `@tools::register` instead of `@tools.register`
- Quoted file-path imports (`use "lib.jde" as lib`) are unchanged
- Added `null` as a third spelling of `nil` — `nil`, `None`, and `null` are interchangeable aliases for the same value; they compare equal and may be used as literals, default parameter values, and type annotations

## v1.1.8

- Native code generation moved out of the `jade` binary — `jade build` now runs the language frontend (lex → parse → type-infer → typed IR) and hands the typed program to the **build daemon** over `$HOME/.jade/build.sock`, which performs import resolution, code generation, and linking. The in-process LLVM backend and the `llvm` Cargo feature were removed; `jade env` now reports build-daemon reachability instead of LLVM status
- Stdlib package imports must now use dot notation — `use std.math`, `use std.fs`, etc.; string-literal forms (`use "std/math"`) are now a compile-time error. Applies to both `use` and `from … use` forms
- File-path imports now require an alias — `use "lib.jde" as lib`; bare string imports without `as name` are now a compile-time error
- Native packages declared in `jade.toml [native]` now require an `alias` field specifying the global binding name
- Fixed: functions exported from imported modules can now access stdlib packages the module imported (e.g. `use std.fs` in a module is visible when module functions are called in the parent scope)
- Improved error messages — type errors now include the actual type of the offending value; heterogeneous array literals, nested function definitions, and non-string prompt struct fields each emit a dedicated error
- Added empty struct test coverage (`struct Unit {}`)

## v1.1.7

- Added `std/sh` package — execute shell commands from Jade via `sh.exec`, `sh.run`, and `sh.output`
- Added `std/json` package — parse JSON strings into Jade values and serialize Jade values back to JSON with `json.parse`, `json.stringify`, and `json.stringify_pretty`
- Added `std/env` package — read and write environment variables (`env.get`, `env.set`), inspect command-line arguments (`env.args`), and get the working directory (`env.cwd`)
- Added `std/path` package — cross-platform path manipulation: `path.join`, `path.basename`, `path.dirname`, `path.ext`, `path.stem`, `path.abs`, `path.is_abs`
- Added `std/random` package — random number generation with `random.int`, `random.float`, `random.choice`, `random.shuffle`, and a seedable global RNG via `random.seed`

## v1.1.6

- Added `input(prompt?)` built-in — reads a line from stdin; the optional `prompt` argument prints to stdout without a trailing newline before reading. Returns an empty string on EOF.
- Added `write(str)` built-in — prints to stdout without a trailing newline and flushes immediately (complements `print`, which adds `\n`)
- Fixed array mutation semantics — mutations to an array are now visible through all aliases (reference semantics); previously mutations did not propagate to other variables pointing at the same array
- Added `llm.set_max_tokens(n)` via `use "llm"` — configure the maximum token limit for LLM inference at runtime
- Extended LLVM native codegen: typed `try`/`catch` arms and struct method calls (`obj.method(args)`) now compile and run correctly in native binaries

## v1.1.5

- Added single-quote string literals — `'hello'` and `'''triple'''` are now equivalent to their double-quote forms; `f'…{expr}…'` f-strings work too
- Fixed `jade.toml` config loading — a config-only file with only a `[model]` section (no `[project]`) is now correctly picked up
- Added `jade upgrade` command — downloads and atomically replaces the binary from the latest GitHub release

## v1.1.4

- Added `async fn` definitions and `await` expressions — concurrent LLM inference via `await` on prompt dereferences
- Added Jade OS as a supported LLM backend provider
- Added comprehensive error handling for async tasks — panics from spawned tasks produce `AsyncPanic` errors with source location
- Switched TLS backend to `rustls` (no OpenSSL dependency)

## v1.1.3

- Added official install script at `https://jadelang.org/install.sh` — detects OS and architecture, downloads the correct prebuilt binary, and installs to `/usr/local/bin/jade`
- Added Windows prebuilt binary: `jade-windows-x86_64.exe` available from the GitHub Releases page
- Updated documentation installation page to document the install script and Windows download path

## v1.0.9

- Added `try`/`catch`/`raise` exception handling — raise any value as an exception, catch by struct type name or with a catch-all arm, nested `try`/`catch` blocks, built-in runtime errors (division by zero, type errors, etc.) are automatically catchable
- Upgraded CLI to full subcommand structure: `jade run`, `jade check`, `jade build`, `jade repl`, `jade test`, `jade fmt`, `jade env`, `jade cache`, `jade model`, `jade new`, `jade init`
- Fixed implicit function return: the last bare expression in a function body is now returned automatically without needing an explicit `return` keyword

## v1.0.8

- Added anonymous closures: `|x| x * 2` (inline expression body) and `|x| { … }` (block body) with environment capture at creation time
- Added `for` loops: `for x in array { … }` iteration over arrays (via bytecode VM)
- Added `dict` type: dictionary literals (`{"key": value}`), key access (`d["key"]`), key assignment, and `len` support
- Added `use "path.jde"` for multi-file imports
- Added bytecode compiler and VM — programs now run through type inference, bytecode emission, and a register-based VM
- Added multi-level AST and TIR caching to skip redundant compilation passes

## v1.0.7

- Added `str` type: string literals, triple-quoted strings, concatenation with `+`, character indexing, equality and lexicographic ordering
- Added f-string interpolation: `f"…{expr}…"` and `f"""…{expr}…"""`
- Added array literals (`[1, 2, 3]`), index access (`arr[i]`), and index assignment (`arr[i] = expr`)
- Added `print` and `len` built-in functions
- Added pipe operator `|>` for chaining function calls
- Added `interface` definitions and `extend Type: Interface` conformance checking
- Added `elif` clause for chained conditionals
- Added `jade configure` command for LLM backend configuration
- Added `prompt` declarations and `?` dereference for LLM inference

## v1.0.6

- Added `struct` definitions with named fields, field access, and field mutation
- Added `extend` blocks for attaching methods, with `self` binding
- Added bare variable assignment (`x = expr`)
- Added `while` loops with boolean condition

## v1.0.5

- Added `struct` definitions with named fields
- Added struct instantiation with `TypeName { field: value, … }` literals
- Added field access (`obj.field`) and field mutation (`obj.field = expr`)
- Added `extend` blocks for attaching methods to struct types
- Added method calls (`obj.method(args)`) with automatic `self` binding
- Added bare variable assignment (`x = expr`) as an alternative to `let` rebinding

## v1.0.4

- Added `while` loops with boolean condition

## v1.0.3

- Added `fn` definitions with parameter lists and `return`
- Added function calls as first-class expressions
- Added first-class function values — functions can be assigned to variables and passed as arguments
- Added recursion — functions can call themselves
- Added `if`/`else` control flow

## v1.0.2

- Modulus operator: `%`
- Bitwise operators: `&`, `|`, `^`, `<<`, `>>`
- Unary bitwise NOT: `~`
- Float literals (`f64`) and unary negation for floats
- Boolean literals: `true`, `false`
- Logical operators: `&&`, `||`, `!` with short-circuit evaluation
- Comparison operators: `==`, `!=`, `<`, `>`, `<=`, `>=`
- Runtime errors: remainder by zero, invalid shift amount (negative or ≥ 64)

## v1.0.1

- Initial interpreter release written in Rust
- `let` variable declarations with arithmetic expressions
- Operators: `+`, `-`, `*`, `/`
- Automatic semicolon insertion — no semicolons required
- Runtime errors: undefined variable, division by zero
- CLI: `jade <file>`, `--verbose`, `--help`
