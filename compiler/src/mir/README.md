# `src/mir/`: the typed middle layer

## What this subtree is

MIR sits between Jade 2.0's type checker and its backend. The checker in [`src/typechecker/infer/`](../typechecker/infer/README.md) works out a type for every value and a target for every call. MIR writes that answer down as code a backend can lower without asking any more questions about types:

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

The v1 backend lowered bytecode, a register machine whose registers had no fixed type, so every operation read type tags while the program ran. The backend needs a representation where the types are already settled. The parser's tree, which the checker reads, is too close to the source for that: it has nested expressions, implicit conversions such as an `int` stored into an `int?`, and calls whose target depends on which instance is being checked.

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

- *`mod.rs`* defines MIR: `Program`, `Func`, `Block`, `Stmt`, `Rvalue`, `Operand` and `Term`, plus `Op`, the support operations every builtin and method lowers to. Its header states the ownership rules. `NumTy` names the type arithmetic, bitwise operators and negation work at, a sized one included, and `Op::Convert` is every converter where either side is a sized number type.
- *`lower.rs`* walks each checked instance's body with the checker's answers and writes MIR. It looks up expression types, slot types and call targets by instance and node address. It reads the parser's tree, `frontend::ast`, as the per-file pass left it, and refers to the tree's statements and program as `ast::Stmt` and `ast::Program` to keep them apart from MIR's own. It inserts conversions where a value meets a wider slot, and `coerce` turns a number literal into the sized constant it meets, `Const::Sized` or `Const::Float32`; a negated literal folds into a constant first, binds arguments to parameters with defaults and keywords, and lowers `for` loops over arrays, strings and dict keys. A copy-with literal reads each field it leaves out from the base, or, when the base's class lacks the field, lowers the field's declared default, kept in `Lowerer::defaults`, as the checker checked it. A `RawPipe`, which the per-file pass never leaves behind, is an internal error.
- *`pipe.rs`* lowers a pipe, which calls its stages once per piece of its source. Over one piece the stages are calls; over a stream that has arrived, a loop. Over a call to a generator, the stages become a function of their own and a copy of the generator calls it at each `yield`, taking what the stages read from the caller as extra parameters, so each value goes through the stages before the generator makes the next and no closure is needed. A raise in a stage leaves the copy without entering the generator's own `try`. The copies are made once per instance and pipe, in `Lowerer::pipes`, so a generator that pipes itself terminates, and their ids follow the instances' with the adapters'. MIR reads a stream as an array, so the lowering keeps the set of expressions the checker typed as streams.
- *`ops.rs`* lowers calls the checker resolved to a builtin: `print`, `len`, `str`, the type functions and converters, `join` and `wait`, and the methods on strings, arrays, dicts, bytes and futures. `toDict(text)` lowers to `Op::ToDict` carrying the dict type the checker chose from the program's use, so the backend generates one reader per dict type, as `Op::ToStr` carries the type it shows. Until 2.0.7 `Op::Json` and `JsonFn` covered `std::json` and its `Json` value.
- *`packages.rs`* lowers std package calls, `time.sleep` or `fs.read`, to their `Op`. Each package is one `Op`, often carrying an enum of its functions, such as `Op::Fs(FsFn::Read)`, and the argument types the checker settled pick the variant.
- *`taskcheck.rs`* refuses tasks that race on shared state: a task that writes a global, writes through something it was handed, or calls a function that does; and a spawner that assigns a global or changes a collection while a task it started is still using it. It was v1's rule over bytecode, ported in phase 6, when the bytecode went. MIR suits it better: each call names the instance it runs, each write through a reference is its own statement, and the types say which values live on the heap and so can be shared at all. Each heap local carries two marks, *from a parameter* and *from a global*, so a write through a global's collection counts as a global write even inside a helper called with no arguments, which v1 missed. Each block records a span per statement for its messages. Its tests, in `taskcheck/tests.rs`, compile programs through the real pipeline.
- *`display.rs`* prints MIR as text.
- *`tests.rs`* lowers small programs and checks the MIR they produce.

## Who uses it

*Used by:* `aot::compile`, which runs the checker, lowers to MIR, runs `taskcheck`, and hands the result to [`src/backend/`](../backend/README.md).

*Depends on:* `typechecker::infer` for `Typed` and `Ty`, and `frontend::ast` for the program it walks.

## Gotchas

*Anything a later phase adds stops here.* A prompt, or a C symbol whose signature cannot be derived, returns `JadeError::NotYetStatic`, naming the feature. Each phase of `design/v2-static-values.md` removes some of these errors.

*A prompt is its text.* `norm` turns the prompt type into `str`, so a prompt, a prompt literal and a prompt field lower as strings. `?p` itself never reaches MIR: `src/infer/rewrite.rs` turns it into a call into the inference prelude first, and what the prelude asks of the backend is `Op::Infer`, one `InferFn` per request. A dereference into a class or through a grammar is still refused here by name. `Program::infer` carries the model's plan into the backend, filled in by `aot::compile` when a program that runs a model is built.

*A call into a Rust package is `Op::Native`*, carrying the wrapper's symbol and the static types its arguments go as and its result comes back as, which are the package's declared ones (`typechecker::infer::native`). Each argument is coerced to its parameter's type first, so a literal meeting a sized parameter becomes a sized constant. `Program.rust` is the staticlib the backend links when the program calls one.

*A call to an async function is `Rvalue::Spawn`*, which makes a future; the function's own MIR body returns the plain value. `await`, `join`, `wait` and the future methods are `Op`s the backend implements in `backend/tasks.rs`.

*Scopes must mirror the checker's.* A top-level `let` in `main` is a global, a `let` inside a block is a local, and the checker decides which slot a variable gets by the same rule. If the two walks disagree about scoping, a variable's slot lookup fails with an internal error.

*A slot the checker never filled is `nil`.* `norm` replaces any leftover type variable with `Ty::Nil`, which is what an empty array nothing pushes into holds.

## Building and testing

```sh
cargo test mir::
./target/debug/jade build --emit mir examples/functions/basic/basic.jde
```
