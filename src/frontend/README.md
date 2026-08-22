# `src/frontend/`: source text to AST

## What this subtree is

This is the front half of the Jade compiler. It takes raw `.jde` source text and turns it into a `Program`, the untyped abstract syntax tree every later stage builds on. Nothing here knows about types, bytecode, or LLVM. Its whole job is working out what the programmer wrote.

It is the first link in the one-way chain that defines the toolchain:

```
source text → frontend → compiler (TIR) → bytecode → vm | aot
```

No stage reaches backwards, so a change here ripples forward and never the other way.

## Why it was built this way

Two ideas shape the code.

*One concern per file.* The lexer only lexes and the parser only parses. When a language feature needs both, it gets a token in the lexer and a rule in the parser, rather than one helper doing a bit of each. That is what keeps these files readable at the size they have grown to.

*Every failure carries a span.* Every `JadeError` variant holds a `Span { line, col }`, because an error with no location is nearly useless to whoever is writing the program. That is also why there are no panics on the interpreter path. An `unwrap()` on anything derived from user input turns a good message into a backtrace.

A third choice is worth knowing before you add syntax: *prefer desugaring*. If the parser can rewrite new syntax into an AST node that already exists, the rest of the pipeline never learns about it. The `~>` operator is the model. It is a parser-level rewrite to the prompt-dereference node, so type inference, the emitter, and both backends were left untouched.

*But desugar only what the parser can actually decide.* `|>` is the counter-example, and it is worth reading before you reach for the rule above.

The parser used to rewrite `x |> f` straight into a call. Doing that meant deciding that `f` was a function, which is a question about what a name refers to, and the parser cannot answer it. So a second rule grew for `?p |> int`, where the stage is a type rather than a function. Then a `Parser::in_print_call` flag grew on top of that, to ban the one combination the two rules could not express together. The result was one operator with two parse paths, and a grammar whose legality depended on the name of the enclosing call.

In v1.2.0, `|>` became an ordinary `Expr::Pipe` node, and `compiler::type_infer::infer_pipe` now classifies the stage. That deleted both extra paths and the flag. The rule to take from it: if a rewrite needs to know what a name *means*, it is not a desugaring.

## What each file does

- *`lexer.rs`* turns source text into a `Vec<Token>`. It handles Jade's string forms, meaning plain, single-quoted, triple-quoted, and f-strings, plus numeric literals with overflow detection and the prompt-related sigils. It *strips comments*, which is why `cli/fmt.rs` formats source text directly rather than reprinting the token stream.
- *`parser.rs`* turns tokens into a `Program`. It is the largest file here. Operator precedence lives in its call chain, which runs `parse_or`, then `parse_and`, and on down to `parse_primary`. Add a precedence level by inserting a function at the right point in that chain, rather than special-casing inside an existing one.
- *`ast.rs`* holds the AST node types, plus the small enums shared with later stages: `BinOpKind`, `UnaryOpKind`, `StructFieldDef`, and `FStrPart`. All of them are serde-serializable, because the on-disk cache stores them.
- *`error.rs`* holds `JadeError`, `Span`, and the `Result` alias the whole crate uses. Every variant carries a span. `undefined_variable_hint` lives here rather than beside either engine, because the AOT backend reports the same thing about programs that type inference had to let through, and the two should word it the same way.
- *`mod.rs`* holds module declarations only.
- *`tests.rs`* holds the parser and lexer tests. Two helpers: `parse_src(src)` returns a `Program`, and `parse_src_err(src)` returns the `JadeError` a bad program produces.

## Who uses it

*Depends on:* nothing else in the crate. This is the bottom of the stack.

*Used by:* nearly everything. `compiler/type_infer.rs` consumes the `Program`, and `cache/` serializes it. `cli/check.rs`, `cli/run.rs`, `cli/build.rs`, and `cli/repl.rs` all begin by calling the lexer and parser. Every module in the crate uses `error::JadeError` and `Span`, including the VM, the AOT backend, and the built-in packages.

## Gotchas

*Bump `CACHE_FORMAT_VERSION`* in `src/cache/mod.rs` whenever you change the shape of an AST node. The AST is serde-serialized into the on-disk cache, so a new field means stale caches would deserialize into the wrong struct. A tripwire test pins the constant. If it fails after your change, that is the test doing its job.

*Error messages should name the fix.* A message reading `prefix '?' cannot be applied to a field — write 'obj.(?p)' or 'obj~>p' instead` is worth far more than `unexpected token`.

*Structural rules that need a counter are enforced here, not in the type checker.* `return` and `yield` need an enclosing function. `break` and `continue` need an enclosing loop. Two depth counters on the parser settle all four: `fn_depth` and `loop_depth`. The answer depends only on where the statement sits, and by the time type inference runs, the nesting has been flattened into a tree that no longer records it.

`loop_depth` is *reset* across a function boundary rather than merely saved, in all three places a body is parsed: `fn`, `async fn`, and a closure. A loop outside the enclosing function is not one its body can break out of, because leaving it would mean crossing a call frame, and `return` is the statement for that. Getting this wrong fails silently. The parse succeeds, and the emitter produces a jump to an address in another chunk.

*A rule that applies to `fn` almost certainly applies to `async fn`, and separate functions parse the two.* `parse_fn_with_decorators` rejected nesting while `parse_async_fn_with_decorators` did not, even though both increment `fn_depth` in the body. That was an omission, not a decision.

It cost users two surprises they could not connect to each other. A nested `async fn` failed at *run* time while reading the enclosing function's parameters, and a decorator on it was dropped in silence. Anything you add to one of the two belongs in the other, unless you have a reason it does not.

*A decorator means two different things depending on what it sits on, and only one of them lives here.* On a `let` or a `prompt`, the parser rewrites it into a call, so `@f let x = v` becomes `let x = f(v)`. Nothing downstream learns the syntax exists, and the feature costs one desugar with no new AST node.

On a `fn` it cannot work that way, because the value being wrapped is a function the emitter has yet to build. That path lives in `emit.rs` and runs at emit time. The two paths must agree on nesting order: the decorator written *first* is applied first, which is the reverse of Python's rule. Change one and you have to change the other.

## Building and testing

```sh
cargo test frontend::         # everything in this subtree
cargo test frontend::tests::  # the parser/lexer suite specifically
```
