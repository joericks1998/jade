# `src/compiler/infer/`: whole-program static types

## What this subtree is

This is Jade 2.0's type checker. It works out one concrete type for every value in a program, with no annotations, so the static backend can compile an int to a register and a struct to a fixed layout with no type tags. It also decides which programs Jade 2.0 accepts. The rules it enforces are the decisions in [`design/v2-static-values.md`](../../../design/v2-static-values.md); read that first.

It runs on the whole merged program: after the per-file pass in `type_infer.rs`, after the import pass inlines every module, and after struct defaults are filled. Until the static backend replaces the bytecode backend, it is a gate. `aot::compile_with_mode` and `aot::would_build` call `check_program`, so a program it rejects neither builds nor passes `jade check`, even though the bytecode backend could still run it.

## Why it was built

The v1 checker in `type_infer.rs` is one forward pass with no type variables. A function's parameters are always `Unknown`, a call never looks at its arguments, and `Unknown` spreads into everything it touches. That was enough to pick `AddInt` over a dynamic add where it could, and left everything else to tags checked while the program ran. A backend with no tags needs every type known, so the inference had to change shape rather than get better at the edges.

## How it works

*A function is checked once per set of argument types.* `add(1, 2)` checks `add`'s body with both parameters `int`; `add(1.5, 2.0)` checks it again with both `float`. Each distinct set of argument types is an *instance*, keyed by `InstKey`, and the backend compiles one copy per instance. A method is checked per receiver type the same way, so an inherited method gets a copy for each child that calls it. A function nobody calls is never checked, and never compiled.

*Every value lives in a slot, and slots only widen.* `ty::Store` holds them. A variable, an array element, a dict value and a struct field are each a slot, so `let xs = []` makes an array whose element slot is empty, and a later `xs.push(1)` fills it. Two types meet through `Store::join`: `int` and `int` give `int`, a type and `nil` give an optional, two structs give their nearest common parent, and anything else is a mismatch. Joining never narrows a slot.

*The checker repeats its pass until nothing changes.* Types flow in every direction, so one walk cannot get them right. `run` checks `main` and every instance it reaches, over and over, until a whole pass leaves `Store::version` unchanged. Errors are collected only on the last pass, once every type has settled; a type that is merely not known yet on an early pass is not an error. A slot nothing ever constrained is `nil` by then, which is what an empty array nobody fills holds.

*Slots are cached by syntax node address.* Each instance keeps a map from the address of the node that made a slot to the slot, so a second pass reuses the first pass's slots instead of making new ones. That is what makes "nothing changed" mean the types have settled. The program outlives the checker, so an address is a stable identity for the whole run. Anything that makes a slot inside a body must go through `Checker::slot` for this reason; a `store.fresh()` there makes a new slot every pass and the passes never settle.

*Functions as values.* A named function, closure, builtin or bound method used as a value has the type `Ty::Func`, which is not yet given argument types. Calling one through a variable checks it at that call's types, the way calling it by name does. When two different functions meet in one place, such as an array literal of functions, they join into one `Ty::Fn` with shared parameter and return slots, and `Store::fn_members` remembers which functions flowed in. Once the parameter types settle, `settle_fn_members` checks each member at them.

*Narrowing.* Inside `if x != nil { … }`, and after `if x == nil { return }`, the variable is read as its non-optional type. `&&` narrows its right side by its left. Narrowing lives beside the scopes rather than in them, so assigning the variable still reaches its real slot.

## What each file does

- *`mod.rs`* holds `check_program`, the `Checker`, registration of every top-level definition, the struct hierarchy and field slots, the pass loop, `instantiate`, and the `Typed` answer: every instance, the resolved type of every expression by instance and node address, and what every call resolved to.
- *`ty.rs`* holds `Ty`, `FnTy`, `FuncRef`, and the `Store` of slots with `join`, `resolve`, the cycle check that refuses a type containing itself, and the common-ancestor walk that joins two structs.
- *`check.rs`* checks statements and expressions inside one instance: scopes, narrowing, calls by name, by value and by method, operators, struct literals, fields and indexing, `join` and `wait`.
- *`builtins.rs`* holds the types of everything a program calls without writing it: the bare globals, every std package function, the primitive methods, and the built-in structs that replaced v1's record-shaped dicts (`RuntimeError`, `HttpResponse`, `HttpBytesResponse`, `ShOutput`, `TimeParts`, and one `Settled` struct per `join(…, settle = true)` site).
- *`tests.rs`* pins each rule with one program that checks and, where the rule refuses something, one that does not.

## Who uses it

*Used by:* `aot::compile_with_mode` and `aot::would_build`, as the gate described above. `cli/check.rs` reaches it through `would_build`, which is also how the `every_example_matches_its_expected_check_result` test judges every fixture.

*Depends on:* `compiler::tir` for the merged program it reads, `frontend::ast` for struct field definitions, and `builtins` for the package and primitive-method name lists.

## Gotchas

*A slot made inside a body must be cached.* See "Slots are cached by syntax node address" above. The symptom of getting it wrong is the error "the program's types never settle", on a program that has nothing recursive in it.

*`join` must not make a type contain itself.* An array pushed into itself would need an element type that is its own array type, and `resolve` would recurse until the stack ran out. `Store::occurs` refuses that on both paths `join` can fill a slot, and it compares a slot before following what the slot holds; following first looked past the slot itself once it had been defaulted to `nil`.

*Native calls are not typed yet.* A call into a C library returns whatever its use makes it. Feeding the binding declarations in `jade.toml` to the checker is part of the package work in design decision 11.

## Building and testing

```sh
cargo test compiler::infer
./target/debug/jade check examples/arrays/mixed_error.jde    # one rule, from the CLI
```
