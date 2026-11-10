# compiler: the Jade toolchain

This directory is `jade`: the lexer, parser, type checker, LLVM ahead-of-time backend, package manager and CLI, written in Rust. It is one of two crates in the repo's Cargo workspace, with its own `Cargo.toml`, version and releases; the other is [`dovata/`](../dovata/README.md), the process that holds a model's memory. What Jade is, and the rest of the repo, are in the [root README](../README.md).

The commands below run from the repo root, where the workspace, `target/` and `examples/` are.

---

## Where to start reading

Every directory under `src/` has its own `README.md`. Read them in the order the compiler runs, because almost every change makes more sense once you know which stage it belongs to.

1. [`src/frontend/`](src/frontend/README.md) turns source text into tokens and then an AST. Start here if you are touching syntax.
2. [`src/compiler/`](src/compiler/README.md) infers types and produces TIR, and its `infer/` gives every value one static type. Start here if you are touching types.
3. [`src/mir/`](src/mir/README.md) lowers the checker's answer into MIR: typed locals, basic blocks, and explicit reference counts.
4. [`src/backend/`](src/backend/README.md) turns MIR into LLVM IR and links a binary against the C library alone. What v1 called the runtime is support code here, emitted into each program only where it is used.
5. [`examples/`](../examples/README.md), at the repo root, is the fixture suite. A language change starts here.

Three more that come up often:

- [`src/builtins/`](src/builtins/README.md) if you are adding something to `std`.
- [`src/pkg/`](src/pkg/README.md) if you are working on packages or C bindings.
- [`src/cli/`](src/cli/README.md) if you are adding or changing a `jade` subcommand.

[`CLAUDE.md`](../CLAUDE.md) at the repo root has the full map of every directory with a one-line description of each.

---

## Build from source

You need two things installed.

*Rust 1.85 or later.* Install it from [rustup.rs](https://rustup.rs). The crate uses edition 2024, so an older toolchain will not build it.

*LLVM 18.* `jade build` compiles and links in-process rather than shelling out to a separate compiler, so LLVM and its linker, LLD, are build dependencies of the toolchain itself. `build.rs`, beside this README, compiles `src/backend/lld.cpp` against LLD's libraries from the same install. A released `jade` binary needs nothing installed; only building from source does.

```sh
brew install llvm@18                                              # macOS
sudo apt-get install llvm-18-dev libpolly-18-dev libzstd-dev liblld-18-dev   # Debian and Ubuntu
```

`cargo build` and `cargo test` need nothing else. The FFI gate, `src/scripts/ffi-gate.sh`, binds against real glib, so running that one script also needs glib, pkg-config, and clang:

```sh
brew install glib pkg-config                                # macOS
sudo apt-get install libglib2.0-dev pkg-config clang        # Debian and Ubuntu
```

The `llvm-sys` crate finds LLVM through the `LLVM_SYS_180_PREFIX` environment variable. `.cargo/config.toml` sets it to the Apple Silicon Homebrew path as a default. Cargo will not overwrite a variable you have already exported, so on any other host set it yourself:

```sh
export LLVM_SYS_180_PREFIX=/opt/homebrew/opt/llvm@18    # macOS
export LLVM_SYS_180_PREFIX=/usr/lib/llvm-18             # Debian and Ubuntu
```

Then clone and build. Run cargo from the repo root, where the workspace is; a bare `cargo build` there builds the compiler alone:

```sh
git clone https://github.com/joericks1998/jade
cd jade
cargo build            # debug build, at target/debug/jade
cargo test
```

For a build with optimizations, use `cargo build --release` and look in `target/release` instead. Both work; debug builds are faster to produce and are what you want while developing.

### The binary stands alone

A program `jade build` emits links the C library and nothing else, through the LLD built into `jade`. There are no runtime archives to keep beside the binary, so a locally built `jade` works after you delete the checkout, and you can copy it to another machine on its own. Until Jade 2.0 every program linked two runtime archives, and the checkout had to stay.

### Confirm it works

```sh
./target/debug/jade run examples/strings/fstrings/fstrings.jde     # prints "hello, Jade!"
./target/debug/jade check examples/classes/basic/basic.jde         # prints "ok"
```

If both behave, your setup is good. Welcome aboard.

---

## How it fits together

Data flows one way through the compiler, and no stage reaches backwards:

```
source text
  → frontend/lexer.rs      → Vec<Token>
  → frontend/parser.rs     → Program (AST)
  → compiler/type_infer.rs → TProgram (TIR), per file
  → aot/imports.rs         → one merged TProgram
  → compiler/infer/        → Typed: one static type for every value
  → mir/                   → MIR, checked by mir/taskcheck.rs
  → backend/               → LLVM IR → object → binary, linked by LLD
```

Jade has one engine, the compiler. `jade build` runs the whole chain, `jade run` and `jade test` build into a scratch directory and run the binary, and `jade check` runs everything short of LLVM. Every example's output is recorded beside it as `<name>.expected`, and `src/scripts/golden.sh` builds each one and compares. Until v1.4.8 there was also a bytecode VM, and until phase 6 of `design/v2-static-values.md` compiled programs linked a shared runtime; both are gone.

`jade build` needs no build daemon and no system linker. `src/backend/` lowers MIR through LLVM 18, and `src/backend/link.rs` links the object with LLD inside the same process. A daemon used to exist because code generation lived in a separate repository. Once it moved here, the daemon's only remaining job was forwarding a request to a function this crate already exported, so it was removed.

---

## Working on the compiler

### One concern per file

The lexer lexes. The parser parses. Each compiler pass turns one IR into the next. A feature that spans several layers should touch each layer on its own terms rather than smearing logic across the boundaries.

### Adding a language feature

Follow the pipeline in order:

1. Token or tokens in `frontend/lexer.rs`
2. AST node or nodes in `frontend/ast.rs`
3. Parse rule or rules in `frontend/parser.rs`
4. Lowering in `compiler/type_infer.rs`
5. Static types in `compiler/infer/`
6. Lowering to MIR in `mir/`
7. LLVM code in `backend/`
8. Error variant in `frontend/error.rs`, if the feature can fail

Not every feature needs all eight steps. Syntax that desugars to an AST node you already have stops at step 3, which costs nothing downstream. Reach for that when you can. The `~>` operator is a good example. It is a parser-level rewrite to a node that already existed, so nothing past the parser ever had to learn about it.

Operator precedence lives in the parser's call chain, which runs `parse_or` then `parse_and` and on down to `parse_primary`. To add a precedence level, insert a new function at the right point in that chain rather than special-casing inside an existing one.

### Adding a built-in

A built-in is a name in a central registry plus code the backend emits. To add one, put its name in the package's `fns` slice in `src/<name>/mod.rs` and its type in `register_types`; give it a static type in `compiler/infer/builtins.rs`; lower it in `mir/packages.rs`; and emit its code from the matching file in `src/backend/`. Global built-ins go in `core::GLOBAL_NAMES` instead of a package slice. [`src/builtins/README.md`](src/builtins/README.md) has the detail.

A whole new package also needs `src/<name>/mod.rs`, a `pub mod <name>;` line in `lib.rs`, a `use crate::<name>;` line in `builtins/mod.rs`, and an entry in `PACKAGES`.

### Errors

Every failure returns a `JadeError`, and every variant carries a `Span`.

*No panics in the compiler.* That includes `unwrap()` on anything derived from user input.

Type errors belong in `compiler/type_infer.rs` and `compiler/infer/`, not in MIR or the backend. By the time MIR is lowered, types are already settled.

Good messages name the fix, not just the problem. Prefer `prefix '?' cannot be applied to a field, write 'obj.(?p)' or 'obj~>p' instead` over `unexpected token`.

### Tests

Tests live in `#[cfg(test)] mod tests` blocks, either inline or in a sibling `tests.rs` wired up with `mod tests;`. There is no top-level `tests/` directory.

Three sets of helpers cover most cases. `run(src)` in `backend/tests.rs` builds a program, runs it, and returns its output and exit status. `lower(src)` and `text(src)` in `mir/tests.rs` lower a program to MIR. `parse_src(src)` and `parse_src_err(src)` in `frontend/tests.rs` cover the parser alone.

*Write the `.jde` fixture in `examples/` first, then make the Rust match it.* That is the established workflow here, and it tends to surface design problems before you have built on top of them.

CI and `cargo test` type-check every fixture:

- `<name>.jde` must pass `jade check`.
- `<name>_error.jde` must fail. These fixtures document programs the language rejects, so one that quietly starts passing is also a failure.

A fixture that uses a feature v2 does not compile yet, such as a prompt, fails `jade check` with `jade cannot compile … yet` and is reported as pending rather than as a failure.

CI also runs the fixtures. `src/scripts/golden.sh` builds every example, runs the binary, and compares what it prints with the `<name>.expected` file beside it. It skips the few that reach the network or dereference a prompt, and lists each skip with its reason at the top of the script. An example with no `.expected` file is reported as missing, never as passing.

### Two things that will bite you

Both have already caused real bugs, so they are worth knowing before you hit them.

*Do not mutate process-global state in tests.* `cargo test` is heavily parallel, so `std::env::set_var` races against every other thread calling `getenv`. That is a genuine data race, and it is why `set_var` is `unsafe` as of the 2024 edition. It made the cache tests fail intermittently for a while. Inject a path instead, or use a `#[cfg(test)]` thread-local with an RAII guard so cleanup survives a failing assertion.

*Bump `CACHE_FORMAT_VERSION` when you change a serialized shape.* The AST and TIR are serialized with serde into the on-disk cache in `src/cache/mod.rs`. Adding a field to either one means a stale cache would deserialize into the wrong struct. A tripwire test pins the constant. If that test fails after your change, it is doing its job, not flaking.
