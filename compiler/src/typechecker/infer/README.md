# `src/typechecker/infer/`: whole-program static types

## What this subtree is

This is Jade 2.0's type checker. It works out one concrete type for every value in a program, with no annotations, so the backend can compile an int to a register and a struct to a fixed layout with no type tags. It also decides which programs Jade 2.0 accepts. The rules it enforces are the decisions in [`design/v2-static-values.md`](../../../../design/v2-static-values.md); read that first.

It runs on the whole merged program: after the per-file pass in `type_infer.rs`, after the import pass inlines every module, and after struct defaults are filled. Its answer is the input to the rest of the build: [`src/mir/`](../../mir/README.md) lowers it into typed code. A program it rejects neither builds nor passes `jade check`.

## Why it was built

The v1 checker in `type_infer.rs` is one forward pass with no type variables. A function's parameters are always `Unknown`, a call never looks at its arguments, and `Unknown` spreads into everything it touches. That was enough to pick `AddInt` over a dynamic add where it could, and left everything else to tags checked while the program ran. A backend with no tags needs every type known, so the inference had to change shape rather than get better at the edges.

## How it works

*A function is checked once per set of argument types.* `add(1, 2)` checks `add`'s body with both parameters `int`; `add(1.5, 2.0)` checks it again with both `float`. Each distinct set of argument types is an *instance*, keyed by `InstKey`, and the backend compiles one copy per instance. A method is checked per receiver type the same way, so an inherited method gets a copy for each child that calls it. A function nobody calls is never checked, and never compiled.

*Every value lives in a slot, and slots only widen.* `ty::Store` holds them. A variable, an array element, a dict value and a struct field are each a slot, so `let xs = []` makes an array whose element slot is empty, and a later `xs.push(1)` fills it. Two types meet through `Store::join`: `int` and `int` give `int`, a type and `nil` give an optional, two structs give their nearest common parent, and anything else is a mismatch. Joining never narrows a slot.

*The checker repeats its pass until nothing changes.* Types flow in every direction, so one walk cannot get them right. `run` checks `main` and every instance it reaches, over and over, until a whole pass leaves `Store::version` unchanged. Errors are collected only on the last pass, once every type has settled; a type that is merely not known yet on an early pass is not an error. A slot nothing ever constrained is `nil` by then, which is what an empty array nobody fills holds.

*Slots are cached by syntax node address.* Each instance keeps a map from the address of the node that made a slot to the slot, so a second pass reuses the first pass's slots instead of making new ones. That is what makes "nothing changed" mean the types have settled. The program outlives the checker, so an address is a stable identity for the whole run. Anything that makes a slot inside a body must go through `Checker::slot` for this reason; a `store.fresh()` there makes a new slot every pass and the passes never settle.

*Functions as values.* A named function or builtin used as a value has the type `Ty::Func`, which is not yet given argument types. Calling one through a variable checks it at that call's types, the way calling it by name does. When two different functions meet in one place, such as an array literal of functions, they join into one `Ty::Fn` with shared parameter and return slots, and `Store::fn_members` remembers which functions flowed in. Once the parameter types settle, `settle_fn_members` checks each member at them. Closures and bound methods were removed in 2.0.1; a method used without a call is refused with a message saying to call it.

*Visibility.* A class member is private unless its class marks it `@public`. `check_visible` refuses reading or setting a private field, or calling a private method, from code outside the class that owns it and its descendants. The code being checked belongs to a class when it is one of that class's methods, or its constructor, the function the parser names `Class::new`. Built-in structs such as `RuntimeError` have only public members. A call to a method the class declares but nothing defines is refused here, at the call, so a file can declare methods its importers define.

*Narrowing.* Inside `if x != nil { … }`, and after `if x == nil { return }`, the variable is read as its non-optional type. Inside `if x is Dog { … }` it is read as a `Dog`. `&&` narrows its right side by its left.

*Classes.* `Class(x)` views a value as a class it is or descends from; a class decorator on a `let` becomes one. `Parent::m(self, …)` inside a method resolves to that parent's version of `m`, with `self` put first by the type pass. A field's type decorator joins into its slot when classes are registered. Nothing calls a destructor, so `instantiate_drops` checks each constructed class's `drop` chain, its own then its ancestors', and `Typed::drops` hands the chains to the backend; calling `drop`, or giving it arguments, is refused. Narrowing lives beside the scopes rather than in them, so assigning the variable still reaches its real slot.

## What each file does

- *`mod.rs`* holds `check_program`, the `Checker`, registration of every top-level definition, the struct hierarchy and field slots, the pass loop, `instantiate`, and the `Typed` answer: every instance, the resolved type of every expression and every slot by instance and node address, what every call resolved to, and every struct's layout.
- *`members.rs`* checks the class rules that need what a program wrote rather than what the compiler filled in, so `aot` runs it before struct defaults are filled: a literal outside a class sets only public fields, a private field it leaves out needs a default or a constructor, a method defined outside its class is declared in the body, and no method is defined twice. It reads each literal's `given` list, the fields the source wrote.
- *`ty.rs`* holds `Ty`, `FnTy`, `FuncRef`, and the `Store` of slots with `join`, `resolve`, the cycle check that refuses a type containing itself, and the common-ancestor walk that joins two structs.
- *`check.rs`* checks statements and expressions inside one instance: scopes, narrowing, calls by name, by value and by method, operators, struct literals, fields and indexing, `join` and `wait`.
- *`builtins.rs`* holds the types of everything a program calls without writing it: the bare globals, every std package function, the primitive methods, and the built-in structs that replaced v1's record-shaped dicts (`RuntimeError`, `HttpResponse`, `HttpBytesResponse`, `ShOutput`, `TimeParts`, and one `Settled` struct per `join(…, settle = true)` site).
- *`tests.rs`* pins each rule with one program that checks and, where the rule refuses something, one that does not.

## Who uses it

*Used by:* `aot::compile` and `aot::would_build`, and `mir::lower_program`, which reads `Typed`. `cli/check.rs` reaches it through `would_build`, which is also how the `every_example_matches_its_expected_check_result` test judges every fixture.

*Depends on:* `typechecker::tir` for the merged program it reads, `frontend::ast` for struct field definitions, and `builtins` for the package and primitive-method name lists.

## Gotchas

*A slot made inside a body must be cached.* See "Slots are cached by syntax node address" above. The symptom of getting it wrong is the error "the program's types never settle", on a program that has nothing recursive in it.

*`join` must not make a type contain itself.* An array pushed into itself would need an element type that is its own array type, and `resolve` would recurse until the stack ran out. `Store::occurs` refuses that on both paths `join` can fill a slot, and it compares a slot before following what the slot holds; following first looked past the slot itself once it had been defaulted to `nil`.

*Native calls are typed from `jade.toml`.* `native.rs` reads a `[lib.<name>.symbols.<fn>]` declaration, written in Jade types such as `handle<Thing>` or `fn(int, str) -> int`, and the import resolver derives one from each C dependency's symbol table. `check_program_with` takes them by `__native$<pkg>$<fn>`. A declared call checks its arguments and gives its result; an undeclared one takes its arguments as they are and its result from how it is used. A result its use never settles would default to `nil` like any open slot, so it is recorded first in `Typed.unsettled_natives`, and MIR lowering refuses it by name. A declared callback type gets a return slot of its own at the call, which is how the store recognizes it from pass to pass.

*An `==` between a char and a string waits for its unknown side.* A char compares equal to the one-character string spelling it, so when one side of `==` or `!=` is a string or char and the other is not known yet, the checker does not make them one type on that pass: the unknown side may come from a call checked later, such as `let c = first(s)` where `first` returns a char, and forcing it to `str` first made the program fail. If the passes settle with such a side still unknown, `deferred_eq` turns on `force_eq` for one more pass, which joins the two sides as before.

*A function only C calls is still checked.* A named function passed where a function type is declared is checked at that type when its function type settles, and each pass keeps draining its worklist until settling adds nothing new. Otherwise the final pass would leave such a function out of the answer.

## Building and testing

```sh
cargo test typechecker::infer
./target/debug/jade check examples/arrays/mixed_error.jde    # one rule, from the CLI
```
