---
id: changelog
title: Changelog
sidebar_label: Changelog
---

## Unreleased

- **Unified module imports; removed quoted file imports and the `as` alias (breaking).** There is now one import form: a `use` statement names a **module** with `::` notation (or a bare name) and binds its last path segment. A bare name resolves to a **sibling `.jde` file** (`use utils` → `./utils.jde`), a `::` path descends into subdirectories (`use sub::helper` → `./sub/helper.jde`), and the first segment naming a registered `[lib]` or an installed dependency resolves that instead. The quoted-path form (`use "lib.jde" as lib`) and the `as` alias are rejected at compile time (`QuotedImport` / `ImportAlias`) with a message pointing at the new syntax. Parent/cross-directory imports (`../`) are no longer expressible as a module path — register those directories as a `[lib]`, which anchors resolution at the project root. Resolution is identical in the VM and the AOT build.
- **Moved all remaining inference config to the daemon; the language is a pure wire-protocol client.** It no longer counts tokens (`llm.count_tokens`, `llm.total_tokens`, and the `token_count` state are gone), no longer caps generation length (`llm.set_max_tokens` is gone; requests send `max_tokens: 0`, so the daemon owns the budget), no longer tracks or selects a model (`llm.model()` is gone; requests send an empty `model`, so the daemon uses its configured/loaded one), no longer toggles anchor visibility (`llm.keep_anchors` is gone; requests send `keep_anchors: false`), and no longer re-asks the model on a coercion miss. A typed dereference (`?p |> Type`) is now single-shot — grammar-constrained sampling already forces the reply into the target shape — and raises `PromptOverflow` immediately if it doesn't coerce, in both the VM and the AOT engine. **The `use llm` package is removed entirely** — its remaining function, `llm.health()`, and the earlier `llm.model`/`keep_anchors`/`set_max_tokens`/`count_tokens`/`total_tokens`/`profile`/`find_tool_call`/`find_tool_calls`/`tool_grammar` are all gone. Running inference is language syntax now (`?p`, `?p |> Type`); the model-specific pieces ship with each model as Jade packages on the daemon side. The `JADE_MAX_RETRIES` env var and `max_retries` `jade.toml` key are removed
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
