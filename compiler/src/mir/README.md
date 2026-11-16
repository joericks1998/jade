# `src/mir/`: the typed middle layer, and lowering into it

## What this subtree is

MIR sits between Jade 2.0's type checker and codegen. The checker in [`src/typechecker/infer/`](../typechecker/infer/README.md) works out a type for every value and a target for every call. MIR writes that answer down as code [`src/codegen/`](../codegen/README.md) can turn into LLVM IR without asking any more questions about types:

- one function per checked instance, so `add(1, 2)` and `add(1.5, 2.0)` become `add(int, int)` and `add(float, float)`;
- every local with its type;
- control flow as basic blocks;
- reference counting as explicit statements.

```text
fn f2 add(int, int)(_0: int, _1: int) -> int
  let _2: int
  bb0:
    _2 = add.int _0, _1
    return move _2
```

`jade build --emit mir file.jde` prints a program's MIR in this form.

This directory holds MIR and the two steps that produce it, in the order a build runs them:

```
one file's tree → imports.rs: every imported file merged into one tree
                → typechecker/infer/: each value's type, each call's target
                → lower/: the tree and the checker's answer → MIR
                → taskcheck.rs: refuses tasks that race
```

The type checker runs between the two, from its own directory. Until 2.0.11 the import pass lived in `src/aot/`, with the driver, which is now [`src/build/`](../build/README.md).

## Why it was built

The v1 backend lowered bytecode, a register machine whose registers had no fixed type, so every operation read type tags while the program ran. Codegen needs a representation where the types are already settled. The parser's tree, which the checker reads, is too close to the source for that: it has nested expressions, implicit conversions such as an `int` stored into an `int?`, and calls whose target depends on which instance is being checked.

MIR resolves all three. Expressions become a flat sequence of statements on typed locals. Each conversion is written out as a `Wrap` or `Unwrap`. Each call names one function, or switches on a struct's type id when a method has a version per child. It is also the layer the GPU work will plug into, and the place where memory optimizations will happen.

## How ownership works

Every heap value (a string, array, dict or struct) carries a reference count, and every place that holds one owns one reference. The lowering follows a few rules, and codegen applies them:

- *An assignment* computes its value and retains it when the value was only borrowed, such as a copy of a variable, a field read or an array element. It then releases the place's old value and stores the new one.
- *A temporary that is stored* is handed over with `Operand::Move`, which clears the temporary instead of retaining.
- *Temporaries are dropped* when their statement ends.
- *Parameters are owned.* A function retains its heap parameters on entry and releases every local when it returns, so a parameter behaves like any other local. This is what gives a dict its value semantics: a callee that writes to a dict parameter sees a count above one and copies it first (`Stmt::DictSet`).

Releasing a cleared place does nothing. That is what lets a temporary be dropped on every path out of a statement without tracking which paths set it.

## Classes

A struct value carries its class id, so `x is C` lowers to `Rvalue::IsA` over the ids of `C` and its descendants, and a method with a version per child switches on `Rvalue::TypeId`. Each class's destructor chain arrives in `Program::drops`, as the functions codegen runs from the class's release. The lowering also frees a block's variables when `break` or `continue` leaves it, and releases the globals, newest first, when `main` ends normally, so a destructor runs when the language says it does.

## What each file does

- *`mod.rs`* defines MIR: `Program`, `Func`, `Block`, `Stmt`, `Rvalue`, `Operand` and `Term`, plus `Op`, the support operations every builtin and method lowers to. Its header states the ownership rules. `NumTy` names the type arithmetic, bitwise operators and negation work at, a sized one included, and `Op::Convert` is every converter where either side is a sized number type.
- *`imports.rs`* handles import resolution and module namespacing. Each imported file is parsed and run through the per-file pass, as the entry file was. Jade gives every imported file its own namespace, while a program has one symbol table. So this file mangles imported symbols to keep two modules that both define `greet` distinct. One renamer walks every expression, a class field's default included, since a default is the same tree as any other expression; a `RawPipe`, which the per-file pass never leaves behind, is reported as an internal error. A `use` naming a Rust package builds the project's Rust packages the first time (`rustpkg::build`): each exported struct becomes a class with public, required fields, and each function's signature is recorded for the checker; a name the package does not export is refused, which the gotcha below explains. A file inside a source package resolves its imports against the package's own `jade.toml` first, and `[package] exports` limits what an importing project can name.

  A `use std::path`, or any std package written in Jade (`src/lib/`, `crate::stdlib`), loads that module once per program, `load_library`, namespaced under the package's reserved id, so `path.join(a, b)` becomes `join$<id>(a, b)` and no name in the module can meet the program's. The `use` binds the package's public functions, its top-level `fn`s without a leading `_`; `from std::path use join` binds one. Its classes stay its own, so a program's class of the same name never meets one. A library module may `use` another, and then reaches all of its top-level functions, private ones included, since `Registry::library_values` keeps each loaded module's names: `uhttp.jde` calls `http._fetch` and returns `http.jde`'s `HttpResponse`. A call to a function the registry says folds, `path.join`, with more than two arguments becomes nested calls of two, `fold_long_call`, as v1's variadic join computed it. The hook package `std::__sys` is dropped like the packages codegen writes, and only library modules get that far with it.
- *[`lower/`](lower/README.md)* lowers each checked instance into MIR: `mod.rs` walks the tree with the checker's answers, `pipe.rs` lowers pipes, `ops.rs` calls to builtins and methods, and `packages.rs` calls to the std packages codegen writes and to the `__sys` and `__infer` hooks.
- *`taskcheck.rs`* refuses tasks that race on shared state: a task that writes a global, writes through something it was handed, or calls a function that does; and a spawner that assigns a global or changes a collection while a task it started is still using it. Library code is checked like any other, so since 2.0.8 a task that shuffles a global array with `random.shuffle` is refused, naming `random.shuffle` as the program calls it. It was v1's rule over bytecode, ported in phase 6, when the bytecode went. MIR suits it better: each call names the instance it runs, each write through a reference is its own statement, and the types say which values live on the heap and so can be shared at all. Each heap local carries two marks, *from a parameter* and *from a global*, so a write through a global's collection counts as a global write even inside a helper called with no arguments, which v1 missed. Each block records a span per statement for its messages. Its tests, in `taskcheck/tests.rs`, compile programs through the real pipeline.
- *`display.rs`* prints MIR as text.
- *`tests.rs`* lowers small programs and checks the MIR they produce.

## The std library written in Jade

Since 2.0.8 every std package but the value types' own is Jade source, `src/lib/`, that the import pass adds to a program that uses it, so its functions lower like the program's own. Three things are particular to them.

- *A built-in type test folds.* Library code may write `if (x is int)`, which the checker answers for each instance it checks. `Fx::static_answer` reads the type the checker recorded for `x` there, narrowing included, and lowers only the branch the checker checked; an `is` used as a value becomes a constant. A test on a `T?` is asked while the program runs, as a nil test, and the branch reads the value as a `T`.
- *A library function does not count against the recursion limit.* `Func::library` says so, and codegen skips the count, as it never counted a built-in, so a one-instruction function such as `math.sqrt` inlines to that instruction.
- *Hooks are `Op::Sys`*, as above. Only the four that refuse tainted text raise, `read_text`, `read_file`, `shell` and `spawn`, so only they are followed by an error check; every other failure is left in the thread's error slot for library code to raise.

`Rvalue::may_raise` also says a division by a constant other than 0 and -1 cannot raise, so no error check follows it; Hinnant's calendar arithmetic in `time.jde` divides by constants throughout.

## Who uses it

*Used by:* `build::compile`, the driver, which merges imports here, runs the checker, lowers to MIR, runs `taskcheck`, and hands the result to [`src/codegen/`](../codegen/README.md). `infer/rewrite.rs` and many tests call `imports::namespace_program` and `namespace_source` to load the std library the way a build does.

*Depends on:* `typechecker::infer` for `Typed` and `Ty`, `typechecker::resolve` for the per-file pass on each imported file, `frontend` to parse them, `project/` and `pkg/` to resolve a `use`, `rustpkg` to build a Rust package, `stdlib` for the library's source, and `frontend::ast` for the program it walks.

## Gotchas

*Anything a later phase adds stops here.* A prompt, or a C symbol whose signature cannot be derived, returns `JadeError::NotYetStatic`, naming the feature. Each phase of `design/v2-static-values.md` removes some of these errors.

*A prompt is its text.* `norm` turns the prompt type into `str`, so a prompt, a prompt literal and a prompt field lower as strings. `?p` itself never reaches MIR: `src/infer/rewrite.rs` turns it into a call into the inference prelude first, and what the prelude asks of codegen is `Op::Infer`, one `InferFn` per request. A dereference into a class or through a grammar is still refused here by name. `Program::infer` carries the model's plan into codegen, filled in by `build::compile` when a program that runs a model is built.

*A call into a Rust package is `Op::Native`*, carrying the wrapper's symbol and the static types its arguments go as and its result comes back as, which are the package's declared ones (`typechecker::infer::native`). Each argument is coerced to its parameter's type first, so a literal meeting a sized parameter becomes a sized constant. `Program.rust` is the staticlib `src/link/` links when the program calls one.

*A call to an async function is `Rvalue::Spawn`*, which makes a future; the function's own MIR body returns the plain value. `await`, `join`, `wait` and the future methods are `Op`s codegen implements in `codegen/support/tasks.rs`.

*A mistyped native name has to be caught in `imports.rs`.* Before 2.0.5, `gfx.jade_gfx_key_presed` could pass `jade check`, build, ship, and fail the first time that line ran, because nothing linked the name. A Rust package's library lists everything it exports, so `Renamer::ref_native_qual`, the one place `alias.field` becomes `__native$<pkgid>$<field>`, checks the field against that list and suggests the closest name. Its `.jde` sibling `ref_value_qual` checks whether a module exports the name in the same way. A struct's name is left to `ref_type`, which names its class.

*Scopes must mirror the checker's.* A top-level `let` in `main` is a global, a `let` inside a block is a local, and the checker decides which slot a variable gets by the same rule. If the two walks disagree about scoping, a variable's slot lookup fails with an internal error.

*A slot the checker never filled is `nil`.* `norm` replaces any leftover type variable with `Ty::Nil`, which is what an empty array nothing pushes into holds.

## Building and testing

```sh
cargo test mir::
cargo test mir::imports    # the import pass
./target/debug/jade build --emit mir examples/functions/basic/basic.jde
```
