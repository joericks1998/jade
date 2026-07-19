# Jade

**The AI-native programming language.**

Every other language treats a model call as a library call: you get a string back, then you parse it, validate it, and write the retry loop yourself. Jade moves that into the language. A prompt is a type. Dereferencing it calls the model. And the type you ask for is a contract the compiler enforces:

```jade
prompt p = "How many moons does Mars have? Reply with just the number."

let n = ?p |> int      // n is an int, or the program raises. Never a string to parse.
```

That `|> int` is doing real work. If the model returns prose, a code fence, or nonsense, Jade re-asks — three times by default, configurable — until the value coerces or it raises `PromptOverflow`. The glue code you would have written by hand is the runtime's job.

Prompts live wherever values live, including inside structs, dereferenced right where you use them:

```jade
struct Agent {
    prompt system = "You are a careful code reviewer."
}

let a = Agent {}
let review = a~>system        // or the explicit a.(?system)
```

**This README is for people working on the compiler.** If you want to *use* Jade — install it, learn the syntax, browse the standard library — everything is at **[jadelang.org](https://jadelang.org)**.

---

## Getting started

You'll need Rust 1.70+ ([rustup.rs](https://rustup.rs)).

```sh
git clone https://github.com/joericks1998/jade
cd jade
cargo build
cargo test
```

Then run something to confirm it all works:

```sh
./target/debug/jade run examples/strings/fstrings/fstrings.jde     # prints "hello, Jade!"
./target/debug/jade check examples/structs/prompt_fields/prompt_fields.jde
```

If both of those behave, you have a working setup. Welcome aboard.

---

## How it fits together

Almost every change makes more sense once you know where it sits in this chain. Data flows one way, and no stage reaches backwards:

```
source text
  → frontend/lexer.rs      → Vec<Token>
  → frontend/parser.rs     → Program (AST)
  → compiler/type_infer.rs → TProgram (TIR)
  → compiler/emit.rs       → CompiledProgram (bytecode)
  → compiler/vm.rs         → execution          (jade run)
  → build/mod.rs           → codegen/           (jade build)
```

`jade build` compiles in-process: the frontend produces TIR, then `src/codegen/` lowers it through LLVM 18 and links the result. There is no build daemon — it used to exist because codegen lived in another repository, and once that moved here its only remaining job was forwarding a request to a function this crate already exported.

That makes **LLVM 18 a build-time requirement for the toolchain** (locate it with `LLVM_SYS_180_PREFIX`). It is linked into the binary, so a released `jade` needs nothing installed.

```sh
cargo build    # needs LLVM 18
cargo test     # needs LLVM 18
```

---

## Working on the compiler

### One concern per file

The lexer lexes, the parser parses, and each compiler pass turns one IR into the next. A feature that cuts across layers should touch each one on its own terms rather than smearing logic across the boundaries.

### Adding a language feature

Follow the pipeline in order:

1. Token(s) in `frontend/lexer.rs`
2. AST node(s) in `frontend/ast.rs`
3. Parse rule(s) in `frontend/parser.rs`
4. Lowering in `compiler/type_infer.rs`
5. Emission in `compiler/emit.rs`
6. Execution in `compiler/vm.rs`
7. Error variant(s) in `frontend/error.rs`, if it can fail

Not everything needs all seven. Syntax sugar that desugars to an existing AST node stops at step 3 — reach for that when you can, since it costs nothing downstream. The `~>` operator is a good example: it's a parser-level rewrite to a node that already existed, so type inference, the emitter, and the VM never had to learn about it.

Operator precedence lives in the parser's call chain (`parse_or` → `parse_and` → … → `parse_primary`). Add a level by inserting a function at the right spot, rather than special-casing inside an existing one.

### Adding a built-in

Built-ins are native Rust (`fn(&[VmValue]) -> Result<VmValue>`) in a central registry. Add a `BuiltinFn` constant to the package's `mod.rs`, register it in that package's `fns` slice (or `CORE_BUILTINS` for globals), and add type info in `register_types`.

A new package needs `src/<name>/mod.rs`, a `pub mod <name>;` in `lib.rs`, a `use crate::<name>;` in `builtins/mod.rs`, and an entry in `PACKAGES`. If anything *else* needs changing, the registry boundary has sprung a leak — worth fixing rather than working around.

### Errors

- Every failure returns a `JadeError`, and every variant carries a `Span`. **No panics on the interpreter path** — that includes `unwrap()` on anything derived from user input.
- Type errors belong in `compiler/type_infer.rs`, not the VM or codegen. By the time bytecode runs, types are settled.
- Good messages name the fix, not just the problem. Prefer `prefix '?' cannot be applied to a field — write 'obj.(?p)' or 'obj~>p' instead` over `unexpected token`.

### Tests

Tests live in `#[cfg(test)] mod tests` blocks, either inline or in a sibling `tests.rs` wired up with `mod tests;`. There's no top-level `tests/` directory.

Useful helpers: `run_src(src)` runs the full pipeline, `run_src_with_mock(src, responses)` stubs the LLM backend, and `parse_src(src)` / `parse_src_err(src)` cover the parser.

**Write the `.jde` fixture in `examples/` first, then make the Rust match it.** That's the established workflow here, and it tends to surface design problems before you've built on top of them.

Every fixture is type-checked by CI and `cargo test`:

- `<name>.jde` must pass `jade check`.
- `<name>_error.jde` must **fail** — these document rejected programs, so one that quietly starts passing is a failure too.

Fixtures are only type-checked, never run, so they're free to depend on a network or an API key at runtime. The flip side: a fixture can type-check while printing something its own comments contradict. If behavior matters, write a Rust test as well.

### Two things that will bite you

Both of these have already caused real bugs, so they're worth knowing up front.

**Don't mutate process-global state in tests.** `cargo test` is heavily parallel, so `std::env::set_var` races against every other thread calling `getenv` — a genuine data race, and the reason `set_var` is `unsafe` as of the 2024 edition. This made the cache tests fail intermittently for a while. Inject a path or use a `#[cfg(test)]` thread-local instead, with an RAII guard so cleanup survives a failing assertion.

**Bump `CACHE_FORMAT_VERSION` when you change a serialized shape.** The AST and TIR are serde-serialized into the on-disk cache, so adding a field to either means stale caches would deserialize into the wrong struct. A tripwire test pins the constant — if it fails after your change, that's it doing its job, not a flake to silence.

---

## Sending a change

Open an issue before starting anything substantial; bug fixes and small improvements can go straight to a PR.

Before you open it:

```sh
cargo build
cargo test
```

CI (`.github/workflows/ci.yml`) runs those two on every PR against `main`, plus `jade check` over every fixture in `examples/`. There's no formatting or clippy gate, so match the style around you and try not to reformat code you didn't otherwise touch — it makes reviews much easier.

One thing to know: **version bumps ship releases.** On merge to `main`, CI reads `version` from `Cargo.toml` and, if there's no matching tag, creates one and kicks off the release build. So only bump it when you actually mean to publish.

---

## License

MIT — see [LICENSE](LICENSE).
