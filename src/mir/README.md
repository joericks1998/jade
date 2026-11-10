# `src/mir/`: the typed middle layer

## What this subtree is

MIR sits between Jade 2.0's type checker and its backend. The checker in [`src/compiler/infer/`](../compiler/infer/README.md) works out a type for every value and a target for every call. MIR writes that answer down as code a backend can lower without asking any more questions about types:

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

## Why it was built

The v1 backend lowered bytecode, a register machine whose registers had no fixed type, so every operation read type tags while the program ran. The backend needs a representation where the types are already settled. TIR is too close to the source for that: it has nested expressions, implicit conversions such as an `int` stored into an `int?`, and calls whose target depends on which instance is being checked.

MIR resolves all three. Expressions become a flat sequence of statements on typed locals. Each conversion is written out as a `Wrap` or `Unwrap`. Each call names one function, or switches on a struct's type id when a method has a version per child. It is also the layer the GPU work will plug into, and the place where memory optimizations will happen.

## How ownership works

Every heap value (a string, array, dict or struct) carries a reference count, and every place that holds one owns one reference. The lowering follows a few rules, and the backend applies them:

- *An assignment* computes its value and retains it when the value was only borrowed, such as a copy of a variable, a field read or an array element. It then releases the place's old value and stores the new one.
- *A temporary that is stored* is handed over with `Operand::Move`, which clears the temporary instead of retaining.
- *Temporaries are dropped* when their statement ends.
- *Parameters are owned.* A function retains its heap parameters on entry and releases every local when it returns, so a parameter behaves like any other local. This is what gives a dict its value semantics: a callee that writes to a dict parameter sees a count above one and copies it first (`Stmt::DictSet`).

Releasing a cleared place does nothing. That is what lets a temporary be dropped on every path out of a statement without tracking which paths set it.

## Classes

A struct value carries its class id, so `x is C` lowers to `Rvalue::IsA` over the ids of `C` and its descendants, and a method with a version per child switches on `Rvalue::TypeId`. Each class's destructor chain arrives in `Program::drops`, as the functions the backend runs from the class's release. The lowering also frees a block's variables when `break` or `continue` leaves it, and releases the globals, newest first, when `main` ends normally, so a destructor runs when the language says it does.

## What each file does

- *`mod.rs`* defines MIR: `Program`, `Func`, `Block`, `Stmt`, `Rvalue`, `Operand` and `Term`, plus `Op`, the support operations every builtin and method lowers to. Its header states the ownership rules.
- *`lower.rs`* walks each checked instance's body with the checker's answers and writes MIR. It looks up expression types, slot types and call targets by instance and node address. It inserts conversions where a value meets a wider slot, binds arguments to parameters with defaults and keywords, and lowers `for` loops over arrays, strings and dict keys.
- *`ops.rs`* lowers calls the checker resolved to a builtin: `print`, `len`, `str`, the conversions, `join` and `wait`, and the methods on strings, arrays, dicts, bytes, futures and `Json` values.
- *`packages.rs`* lowers std package calls, `time.sleep` or `fs.read`, to their `Op`. Each package is one `Op`, often carrying an enum of its functions, such as `Op::Fs(FsFn::Read)`, and the argument types the checker settled pick the variant.
- *`taskcheck.rs`* refuses tasks that race on shared state: a task that writes a global, writes through something it was handed, or calls a function that does; and a spawner that assigns a global or changes a collection while a task it started is still using it. It was v1's rule over bytecode, ported in phase 6, when the bytecode went. MIR suits it better: each call names the instance it runs, each write through a reference is its own statement, and the types say which values live on the heap and so can be shared at all. Each heap local carries two marks, *from a parameter* and *from a global*, so a write through a global's collection counts as a global write even inside a helper called with no arguments, which v1 missed. Each block records a span per statement for its messages. Its tests, in `taskcheck/tests.rs`, compile programs through the real pipeline.
- *`display.rs`* prints MIR as text.
- *`tests.rs`* lowers small programs and checks the MIR they produce.

## Who uses it

*Used by:* `aot::compile`, which runs the checker, lowers to MIR, runs `taskcheck`, and hands the result to [`src/backend/`](../backend/README.md).

*Depends on:* `compiler::infer` for `Typed` and `Ty`, and `compiler::tir` for the program it walks.

## Gotchas

*Anything a later phase adds stops here.* A prompt, or a C symbol whose signature cannot be derived, returns `JadeError::NotYetStatic`, naming the feature. Each phase of `design/v2-static-values.md` removes some of these errors.

*A call into a native package is `Op::Native`*, carrying the function and the static types its arguments go as and its result comes back as. The types are the declared ones from `jade.toml` where there are any (`compiler::infer::native`), and otherwise the ones the call has; a result nothing settles is an error naming the declaration to add. `Program.natives` says where each package is found.

*A call to an async function is `Rvalue::Spawn`*, which makes a future; the function's own MIR body returns the plain value. `await`, `join`, `wait` and the future methods are `Op`s the backend implements in `backend/tasks.rs`.

*Scopes must mirror the checker's.* A top-level `let` in `main` is a global, a `let` inside a block is a local, and the checker decides which slot a variable gets by the same rule. If the two walks disagree about scoping, a variable's slot lookup fails with an internal error.

*A slot the checker never filled is `nil`.* `norm` replaces any leftover type variable with `Ty::Nil`, which is what an empty array nothing pushes into holds.

## Building and testing

```sh
cargo test mir::
./target/debug/jade build --emit mir examples/functions/basic/basic.jde
```
