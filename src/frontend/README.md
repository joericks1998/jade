# `src/frontend/` — source text to AST

## What this subtree is

The front half of the Jade compiler. It takes raw `.jde` source text and turns it into a `Program`: the untyped abstract syntax tree that every later stage builds on. Nothing here knows about types, bytecode, or LLVM. Its whole job is deciding what the programmer wrote.

It is the first link in the one-way chain that defines the toolchain:

```
source text → frontend → compiler (TIR) → bytecode → vm | aot
```

No stage reaches backwards, so a change here ripples forward but never the other way.

## Why it was built this way

Two ideas shape the code.

*One concern per file.* The lexer only lexes, the parser only parses. When a language feature needs both, it gets a token in the lexer and a rule in the parser rather than a helper that does a bit of each. That keeps each file readable at the size it has grown to.

*Every failure carries a span.* `JadeError` variants all hold a `Span { line, col }`, because an error without a location is nearly useless to whoever is writing the program. This is also why there are no panics on the interpreter path: `unwrap()` on anything derived from user input turns a good message into a backtrace.

A third choice worth knowing before you add syntax: *prefer desugaring*. If new syntax can be rewritten in the parser into an AST node that already exists, the rest of the pipeline never learns about it. The `~>` operator is the model — it is a parser-level rewrite to the prompt-dereference node, so type inference, the emitter, and both backends were untouched.

*But desugar only what the parser can actually decide.* `|>` is the counter-example, and it is worth reading before you reach for the rule above. The parser used to rewrite `x |> f` straight into a call, which meant deciding that `f` was a function — a question about what a name refers to, which the parser cannot answer. So a second rule grew for `?p |> int`, where the stage is a type rather than a function, and a `Parser::in_print_call` flag grew on top of that to ban the combination the two rules could not express together. One operator, two parse paths, and a grammar whose legality depended on the name of the enclosing call. In v1.2.0 `|>` became an ordinary `Expr::Pipe` node and `compiler::type_infer::infer_pipe` classifies the stage, which deleted both extra paths and the flag. If a rewrite needs to know what a name *means*, it is not a desugaring.

## What each file does

- **`lexer.rs`** — source text to `Vec<Token>`. Handles Jade's string forms (plain, single-quoted, triple-quoted, f-strings), numeric literals with overflow detection, and the prompt-related sigils. It *strips comments*, which is why `cli/fmt.rs` formats source text directly rather than reprinting the token stream.
- **`parser.rs`** — tokens to `Program`. The largest file here. Operator precedence lives in its call chain (`parse_or` → `parse_and` → … → `parse_primary`); add a precedence level by inserting a function at the right spot rather than special-casing inside an existing one.
- **`ast.rs`** — the AST node types, plus the small enums (`BinOpKind`, `UnaryOpKind`, `StructFieldDef`, `FStrPart`) shared with later stages. These are serde-serializable because the on-disk cache stores them.
- **`error.rs`** — `JadeError`, `Span`, and the `Result` alias used across the whole crate. Every variant carries a span.
- **`mod.rs`** — module declarations only.
- **`tests.rs`** — parser and lexer tests. Helpers: `parse_src(src)` returns a `Program`, `parse_src_err(src)` returns the `JadeError` a bad program produces.

## Who uses it

*Depends on:* nothing else in the crate. This is the bottom of the stack.

*Used by:* essentially everything. `compiler/type_infer.rs` consumes the `Program`; `cache/` serializes it; `cli/check.rs`, `cli/run.rs`, `cli/build.rs`, and `cli/repl.rs` all start by calling the lexer and parser. `error::JadeError` and `Span` are used by every module in the crate, including the VM, the AOT backend, and the built-in packages.

## Gotchas

**Bump `CACHE_FORMAT_VERSION`** in `src/cache/mod.rs` whenever you change the shape of an AST node. The AST is serde-serialized into the on-disk cache, so a new field means stale caches would deserialize into the wrong struct. A tripwire test pins the constant — if it fails after your change, that is the test working.

**Error messages should name the fix.** `prefix '?' cannot be applied to a field — write 'obj.(?p)' or 'obj~>p' instead` is worth much more than `unexpected token`.

**A decorator means two different things depending on what it sits on, and only one of them is here.** On a `let` or a `prompt` the parser rewrites it into a call — `@f let x = v` becomes `let x = f(v)` — so nothing downstream learns the syntax exists, and the feature costs one AST-free desugar. On a `fn` it cannot work that way, because the value being wrapped is a function the emitter has yet to build, so that path lives in `emit.rs` and runs at emit time. The two must agree on nesting order: the decorator written *first* is applied first, which is the reverse of Python's rule. Change one and you have to change the other.

## Building and testing

```sh
cargo test frontend::         # everything in this subtree
cargo test frontend::tests::  # the parser/lexer suite specifically
```
