# `src/mir/lower/`: the checked program to MIR

## What this subtree is

This is the step that turns the program into MIR. It reads two things: the tree the parser made, `frontend::ast`, after imports are merged, and the checker's answer, `typechecker::infer::Typed`, which holds the type of every expression and the target of every call, keyed by instance and the address of the syntax node. It walks each checked instance's body once with those answers in hand and writes one MIR function per instance. There is no tree in between: the AST plus `Typed` is the whole input.

```text
fn add(a, b) { return a + b }       add(1, 2) and add(1.5, 2.0)
→ fn f2 add(int, int)  and  fn f3 add(float, float)
```

## Why it was built

The checker settles every type, but the tree still has nested expressions, implicit conversions and calls whose target depends on the instance. Lowering writes each of those out, so codegen never asks a question about types. It was `mir/lower.rs` with its three helpers beside it until 2.0.11, when the lowering files moved into a directory of their own and `src/mir/` kept MIR itself.

## What each file does

- *`mod.rs`* walks each checked instance's body with the checker's answers and writes MIR. It looks up expression types, slot types and call targets by instance and node address. It reads the parser's tree, `frontend::ast`, as the per-file pass left it, and refers to the tree's statements and program as `ast::Stmt` and `ast::Program` to keep them apart from MIR's own. It inserts conversions where a value meets a wider slot, and `coerce` turns a number literal into the sized constant it meets, `Const::Sized` or `Const::Float32`; a negated literal folds into a constant first, binds arguments to parameters with defaults and keywords, and lowers `for` loops over arrays, strings and dict keys. A copy-with literal reads each field it leaves out from the base, or, when the base's class lacks the field, lowers the field's declared default, kept in `Lowerer::defaults`, as the checker checked it. A `RawPipe`, which the per-file pass never leaves behind, is an internal error.
- *`pipe.rs`* lowers a pipe, which calls its stages once per piece of its source. Over one piece the stages are calls; over a stream that has arrived, a loop. Over a call to a generator, the stages become a function of their own and a copy of the generator calls it at each `yield`, taking what the stages read from the caller as extra parameters, so each value goes through the stages before the generator makes the next and no closure is needed. A raise in a stage leaves the copy without entering the generator's own `try`. The copies are made once per instance and pipe, in `Lowerer::pipes`, so a generator that pipes itself terminates, and their ids follow the instances' with the adapters'. MIR reads a stream as an array, so the lowering keeps the set of expressions the checker typed as streams.
- *`ops.rs`* lowers calls the checker resolved to a builtin: `print`, `len`, `str`, the type functions and converters, `join` and `wait`, and the methods on strings, arrays, dicts, bytes and futures. `toDict(text)` lowers to `Op::ToDict` carrying the dict type the checker chose from the program's use, so codegen generates one reader per dict type, as `Op::ToStr` carries the type it shows. Until 2.0.7 `Op::Json` and `JsonFn` covered `std::json` and its `Json` value.
- *`packages.rs`* lowers the calls that are left to std packages codegen writes: `array.sort` and `array.reverse`, which copy, `std::bytes`, what the inference prelude asks of `__infer`, and what library code asks of `__sys`, as `Op::Sys(SysFn)`, each argument coerced to the hook's declared type. `SysFn::params` and `SysFn::result` in `mod.rs` are the one place a hook's type is written; the checker reads them too, and `SysFn::may_raise` says which hooks refuse tainted text. Every other package, `std::fs` among them, is written in Jade and never reaches it: its functions are the program's own. Until 2.0.8 `Op::Fs`, `Op::Sh`, `Op::Env`, `Op::Random`, `Op::Http` and `Op::UhttpStream`, with an enum of each package's functions, covered the six packages phase B of part 3 moved.

## Who uses it

*Used by:* `build::compile`, through `mir::lower_program_with`, and tests through `mir::lower_program`. `codegen/func/mod.rs` calls `norm` to read a function type the same way.

*Depends on:* `../mod.rs` for MIR's types, `typechecker::infer` for `Typed`, `Target` and `Ty`, and `frontend::ast` for the tree.

## Gotchas

*Scopes must mirror the checker's.* A top-level `let` in `main` is a global and a `let` inside a block is a local, by the same rule the checker uses; if the two walks disagree, a slot lookup fails with an internal error.

## Building and testing

```sh
cargo test mir::
./target/debug/jade build --emit mir examples/functions/basic/basic.jde
```
