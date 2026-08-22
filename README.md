# Jade

*The AI-native programming language.*

Most languages treat a model call like any other library call. You get a string back, and then you parse it, validate it, and write the retry loop yourself. Jade moves that work into the language. A prompt is a type. Dereferencing it calls the model. The type you ask for is a contract the compiler enforces.

```jade
prompt p = "How many moons does Mars have? Reply with just the number."

let n = ?p |> int      // n is an int, or the program raises. Never a string to parse.
```

That `|> int` runs in two stages. First, naming a type builds a grammar that limits how the model generates its reply. A reply shaped like prose or wrapped in a code fence is ruled out before a single token of it exists. Second, whatever comes back is coerced to the type you named. If it still does not fit, the program raises `PromptOverflow` instead of handing you a string to parse. The glue code you would otherwise write by hand is the runtime's job.

Prompts live wherever values live, including inside structs. You dereference them right where you use them.

```jade
struct Agent {
    prompt system = "You are a careful code reviewer."
}

let a = Agent {}
let review = a~>system        // or the explicit a.(?system)
```

*This README is for people working on the compiler.* If you want to use Jade, install it, learn the syntax, or browse the standard library, go to [jadelang.org](https://jadelang.org).

---

## Where to start reading

Every directory in this repo has its own `README.md`. Read them in the order the compiler runs, because almost every change makes more sense once you know which stage it belongs to.

1. [`src/frontend/`](src/frontend/README.md) turns source text into tokens and then an AST. Start here if you are touching syntax.
2. [`src/compiler/`](src/compiler/README.md) infers types, produces TIR, and emits bytecode. Start here if you are touching types.
3. [`src/bytecode/`](src/bytecode/README.md) defines the instruction set. Read it before adding an opcode.
4. [`src/vm/`](src/vm/README.md) and [`src/codegen/`](src/codegen/README.md) are the two engines. They are peers, not layers. `jade run` uses the VM; `jade build` uses codegen.
5. [`src/runtime/`](src/runtime/README.md) holds value semantics once, so both engines agree on what a value does.
6. [`examples/`](examples/README.md) is the fixture suite. A language change starts here.

Three more that come up often:

- [`src/builtins/`](src/builtins/README.md) if you are adding something to `std`.
- [`src/pkg/`](src/pkg/README.md) if you are working on packages or C bindings.
- [`src/cli/`](src/cli/README.md) if you are adding or changing a `jade` subcommand.

[`CLAUDE.md`](CLAUDE.md) has the full map of all thirty directories with a one-line description of each.

---

## Build from source

You need two things installed.

*Rust 1.85 or later.* Install it from [rustup.rs](https://rustup.rs). The crate uses edition 2024, so an older toolchain will not build it.

*LLVM 18.* `jade build` compiles in-process rather than shelling out to a separate compiler, so LLVM is a build dependency of the toolchain itself. A released `jade` binary needs nothing installed; only building from source does.

```sh
brew install llvm@18                                              # macOS
sudo apt-get install llvm-18-dev libpolly-18-dev libzstd-dev      # Debian and Ubuntu
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

Then clone and build:

```sh
git clone https://github.com/joericks1998/jade
cd jade
cargo build            # debug build, at target/debug/jade
cargo test
```

For a build with optimizations, use `cargo build --release` and look in `target/release` instead. Both work; debug builds are faster to produce and are what you want while developing.

### Keep the checkout

`jade build` links two runtime archives into every executable it emits: `libJadeRuntime.a` and `libjade_runtime.a`. The same `cargo build` leaves both in the same `target` directory as the binary, and a locally built `jade` remembers that path. So do not delete the checkout after building.

To move a locally built toolchain to another machine, copy the archives along with the binary into `<prefix>/lib/jade`. That is the layout the release tarball uses, and it is where `jade` looks for them relative to itself.

### Confirm it works

```sh
./target/debug/jade run examples/strings/fstrings/fstrings.jde     # prints "hello, Jade!"
./target/debug/jade check examples/structs/prompt_fields/prompt_fields.jde
```

If both behave, your setup is good. Welcome aboard.

---

## How it fits together

Data flows one way through the compiler, and no stage reaches backwards:

```
source text
  → frontend/lexer.rs      → Vec<Token>
  → frontend/parser.rs     → Program (AST)
  → compiler/type_infer.rs → TProgram (TIR)
  → compiler/emit.rs       → CompiledProgram (bytecode)
  → vm/                    → execution           (jade run)
  → codegen/               → LLVM IR             (jade build)
  → aot/                   → object → binary     (jade build)
```

The VM and the AOT backend are peers. Neither wraps the other. The language is whatever the two of them agree on, which is why `src/scripts/backend-parity.sh` runs every example on both engines and diffs the output, and why value semantics live once in the shared `jade-runtime` crate instead of twice.

`jade build` needs no build daemon. The frontend produces TIR, `src/aot/` lowers it through LLVM 18, and the linker runs. A daemon used to exist because codegen lived in a separate repository. Once codegen moved here, the daemon's only remaining job was forwarding a request to a function this crate already exported, so it was removed.

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
5. Emission in `compiler/emit.rs`
6. Execution in `vm/dispatch.rs`, and lowering in `codegen/`
7. Error variant in `frontend/error.rs`, if the feature can fail

Not every feature needs all seven steps. Syntax that desugars to an AST node you already have stops at step 3, which costs nothing downstream. Reach for that when you can. The `~>` operator is a good example. It is a parser-level rewrite to a node that already existed, so type inference, the emitter, and both engines never had to learn about it.

Operator precedence lives in the parser's call chain, which runs `parse_or` then `parse_and` and on down to `parse_primary`. To add a precedence level, insert a new function at the right point in that chain rather than special-casing inside an existing one.

### Adding a built-in

Built-ins are native Rust functions with the signature `fn(&[VmValue]) -> Result<VmValue>`, held in a central registry. To add one, write a `BuiltinFn` constant in the package's `mod.rs`, register it in that package's `fns` slice, and add its type information in `register_types`. Global built-ins go in `CORE_BUILTINS` instead of a package slice.

A whole new package needs four things: `src/<name>/mod.rs`, a `pub mod <name>;` line in `lib.rs`, a `use crate::<name>;` line in `builtins/mod.rs`, and an entry in `PACKAGES`. If anything else needs changing, the registry boundary has sprung a leak. Fix the leak rather than working around it.

### Errors

Every failure returns a `JadeError`, and every variant carries a `Span`.

*No panics on the interpreter path.* That includes `unwrap()` on anything derived from user input.

Type errors belong in `compiler/type_infer.rs`, not in the VM or in codegen. By the time bytecode runs, types are already settled.

Good messages name the fix, not just the problem. Prefer `prefix '?' cannot be applied to a field, write 'obj.(?p)' or 'obj~>p' instead` over `unexpected token`.

### Tests

Tests live in `#[cfg(test)] mod tests` blocks, either inline or in a sibling `tests.rs` wired up with `mod tests;`. There is no top-level `tests/` directory.

Three helpers cover most cases. `run_src(src)` runs the full pipeline. `run_src_with_mock(src, responses)` stubs the LLM backend so a test needs no API key. `parse_src(src)` and `parse_src_err(src)` cover the parser alone.

*Write the `.jde` fixture in `examples/` first, then make the Rust match it.* That is the established workflow here, and it tends to surface design problems before you have built on top of them.

CI and `cargo test` type-check every fixture:

- `<name>.jde` must pass `jade check`.
- `<name>_error.jde` must fail. These fixtures document programs the language rejects, so one that quietly starts passing is also a failure.

Fixtures are only type-checked, never run, so they are free to depend on a network or an API key at runtime. The tradeoff is that a fixture can type-check while printing something its own comments contradict. When behavior matters, write a Rust test as well.

### Two things that will bite you

Both have already caused real bugs, so they are worth knowing before you hit them.

*Do not mutate process-global state in tests.* `cargo test` is heavily parallel, so `std::env::set_var` races against every other thread calling `getenv`. That is a genuine data race, and it is why `set_var` is `unsafe` as of the 2024 edition. It made the cache tests fail intermittently for a while. Inject a path instead, or use a `#[cfg(test)]` thread-local with an RAII guard so cleanup survives a failing assertion.

*Bump `CACHE_FORMAT_VERSION` when you change a serialized shape.* The AST and TIR are serialized with serde into the on-disk cache in `src/cache/mod.rs`. Adding a field to either one means a stale cache would deserialize into the wrong struct. A tripwire test pins the constant. If that test fails after your change, it is doing its job, not flaking.

---

## Sending a change

Open an issue before starting anything substantial. Bug fixes and small improvements can go straight to a pull request.

Run these before you open it:

```sh
cargo build
cargo test
cargo clippy --all-targets
cargo fmt --all --check
```

CI is defined in `.github/workflows/ci.yml` and runs on every pull request against `main`. It runs the four commands above, then `jade check` and `jade fmt --check` over every fixture in `examples/`, then two gates: `src/scripts/backend-parity.sh`, which runs every example on both engines and diffs the results, and `src/scripts/ffi-gate.sh`, which builds and runs real C bindings.

Clippy runs without `-D warnings`, so existing warnings do not fail the build. Do not add new ones.

One rule to know before you touch `Cargo.toml`. *A version bump ships a release.* On merge to `main`, CI reads `version` from `Cargo.toml`, and if no matching tag exists it creates one and starts the release build. Only bump the version when you mean to publish. The full release procedure is in [`CLAUDE.md`](CLAUDE.md).

---

## License

MIT. See [LICENSE](LICENSE).
