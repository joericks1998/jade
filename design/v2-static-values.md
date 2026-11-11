# Jade 2.0: static values

Status: decided, 2026-11-04. Joe chose the recommendation in every decision below, including the four that change what programs the language accepts: dicts are maps and records are structs (3), mixed values join to a parent struct (4), the whole standard library stays (10), and Jade packages are compiled from source (11). The three open questions at the end are still open. Phases 1 and 2 of the order of work are built; see Progress.

## Summary

Jade 2.0 compiles every value to a plain machine value whose type the compiler knows before the program runs. An int lives in a register, a struct has a fixed layout, and a string is a pointer and a length. Nothing in a compiled program checks a type tag, and there is no runtime library: the compiler emits only the support code each program uses, and the program links against the operating system's C library and nothing else.

Jade keeps inferring types rather than asking for annotations. The inference gets stronger instead: the compiler type-checks each function once for every set of argument types it is called with, and compiles a copy for each. A few dynamic patterns that v1 allowed become type errors, and this document lists them and proposes what replaces each one.

The first eleven sections below are decisions. Each says what the problem is, what was decided, what a program looks like under it, and what stops compiling. The rest describes how the compiler changes and in what order the work lands.

## Why

Today every compiled value is a 64-bit word with a tag in its low bits. Adding two values calls into the runtime, which reads both tags and then adds:

```
fn add(a, b) { return a + b }
add(1, 2)

v1:   call jrt_add_any(word_a, word_b)    check both tags, add, check overflow, retag
v2:   %r = call {i64, i1} @llvm.sadd.with.overflow.i64(i64 %a, i64 %b)
```

The runtime that does this is about 16,600 lines: 11,300 of Rust in `src/runtime/` and 5,300 of C in `src/runtime_aot/`. Codegen calls 237 distinct functions in it. Floats are heap-boxed, so every float result is a `malloc`, and the boxes are never freed. Inference kernels for the GPU, the next milestone, cannot sit on top of a tag-dispatching runtime, which is why this work comes first.

## Where the type checker stands

`src/typechecker/type_infer.rs` is one forward pass with no type variables. A function's parameters are always `Unknown`, a call never looks at its arguments, and `Unknown` spreads quietly into everything it touches. The checker then picks typed instructions such as `AddInt` where it can, and leaves the rest to runtime tags. About half the bytecode instruction set is typed, but only arithmetic, comparison and unary operators use the types; every collection, struct, call and formatting instruction dispatches on tags.

Reading the checker also turned up five places where it gives a value a type it may not have, so a typed instruction runs on the wrong kind of value:

1. An assignment in one branch: `let x = "s"; if c { x = 1 }; x + 1` types `x` as `int`.
2. A loop body is checked once, so a change of type on the second pass is never seen.
3. A struct field is typed from its default, not from what a literal puts there: `struct P { let x = 0 }` then `P { x: "s" }.x + 1`.
4. `push` does not widen: `let a = [1]; a.push("s"); a[1] + 1`.
5. A decorated function keeps the type of the undecorated one.

All five are closed by decision 2.

## Decisions

### 1. Types come from each call

*Problem.* A function has no parameter types, so the checker cannot know what `a + b` means inside it.

*Recommendation.* Check a function's body when it is called, with its parameters bound to the argument types at that call, and compile one copy per distinct set of argument types. Reuse the result when the same function is called again with the same types.

```
fn add(a, b) { return a + b }
add(1, 2)          // checks and compiles add(int, int) -> int
add(1.5, 2.0)      // checks and compiles add(float, float) -> float
add("a", "b")      // checks and compiles add(str, str) -> str
add(1, "b")        // type error at this call: int + str
```

- *Recursion* finds its return type by repeating the check until it stops changing. Recursion that keeps changing its own argument types, such as `f(x)` calling `f([x])`, never settles and is a type error.
- *A function nobody calls* is still checked, with each parameter an unknown type that only has to be consistent, so a mistake in it is still reported. It is not compiled.
- *Calls before the definition* stop mattering, because checking follows calls rather than source order.
- *Methods* are checked per receiver type the same way, so an inherited method gets a copy for each child struct that calls it.

### 2. One type per variable, and `nil` means "maybe absent"

*Problem.* v1 lets a variable change kind (`let v = "text"` then `v = 7`, documented in `variables.md`), and `nil` is a type of its own that never mixes with anything.

*Recommendation.* Every variable, array element, dict value and struct field has one type for its whole life. Joining two types that differ is a type error, with one exception: joining `T` with `nil` gives "`T` or nothing", written `T?` in messages. There is no syntax for it; it is inferred.

```
let first = nil
first = 5               // first: int?

fn maybe(x) {           // maybe(int) -> int?
    if x > 0 { return x }
}

let port = env.get("PORT")      // str?
if port != nil {
    print(port.len())           // port is str inside this branch
}
print(port.len())               // type error: port may be nil here; check it first
```

- `== nil` and `!= nil` are allowed on any `T?`, and an `if` on them narrows the variable inside the branch.
- Using a `T?` where a `T` is needed, without a check, is a type error that says to check it.
- A function that returns nothing returns `nil`, and printing one prints `nil`, as today.
- `d.get(k)`, `env.get`, `path.ext`, `random.choice` and a function that falls off its end without returning all give `T?`.

*What stops compiling.* Changing a variable's kind (`variables.md:70-76` documents it as a feature; one fixture does it, through `nil`, and that one keeps working). `functions/return_paths` returns a string on one path and an int on another.

### 3. A dict is a map from strings to one type; a record is a struct

*Problem.* Dicts carry no types at all today, and 8 fixtures use a dict as a record with values of different kinds: `{"name": "jade", "version": 1}`. The standard library returns record-shaped dicts too: `http.get` gives `{status, body}`, `sh.output` gives `{stdout, stderr, code}`, `join(..., settle = true)` gives `{ok, value}` or `{ok, error}`.

*Recommendation.* A dict is `{str: V}`: string keys, as every fixture already uses, and one value type. A value with named fields of different kinds is a struct. The standard library's record-shaped results become named structs with the same field names, so `r.status` and `r["status"]` both keep reading the same way they do now.

```
let ages = {"ana": 31, "bo": 28}          // {str: int}
ages["cy"] = 40
let a = ages.get("dee")                    // int?

let d = {"name": "jade", "version": 1}     // type error: a dict holds one kind of value;
                                           // use a struct for named fields of different kinds
struct Release { name, version }
let r = Release { name: "jade", version: 1 }

let resp = http.get(url)                   // HttpResponse { status, body }
print(resp.status)
```

*The alternative* is to infer a record type from any dict literal with fixed keys, so `{"name": "jade", "version": 1}` keeps compiling. It saves rewriting those 8 fixtures, but it gives Jade two things that both look like dicts and behave differently: adding a key later (`d["stable"] = true`, which `dicts/basic` does) would break a record and not a map.

*What stops compiling.* `dicts/basic`, `dicts/dot_access`, `dicts/nested_get`, `collections/edge_cases`, and dot access on a dict (`d.name`), which becomes a struct feature only.

### 4. Values of different kinds: arrays hold one kind, and a parent struct holds any child

*Problem.* Mixed arrays have been legal since v1.1.32 (`[1, 2.0, true, "hello", nil]`), added so the provider protocol could hold `[Token, Token, Done]`. Under static types, an element of a mixed array has no single type.

*Recommendation.* An array holds one type, as it did before v1.1.32. The case that motivated mixed arrays, several kinds of record in one list, is what struct inheritance already describes:

```
struct Frame {}
struct Token(Frame) { text }
struct Done(Frame) { reason }

let frames = [Token { text: "hi" }, Done { reason: "stop" }]   // [Frame]
```

When two struct types meet in one array, variable or return value, the compiler joins them to their nearest common parent. A value of that parent type carries a small type id, its methods dispatch on that id through a table, and only the parent's fields can be read from it. This is the one place a value carries a tag, and it is a struct's own type id, not a runtime-wide tag.

To act on the actual kind, a program can call a method each child defines, or use a typed `catch`-style test. See open question 1 for the syntax of that test.

*The alternative* is automatic union types: `[1, "a"]` infers `[int | str]`, and code narrows an element before using it. It keeps mixed arrays legal, but every operation on an element then needs a narrowing step, and Jade has no syntax for one today.

*What stops compiling.* `arrays/mixed`, `collections/self_reference` (a recursive array type), the mixed stream in `streams/yield_basic`, and `wait`/`join` over futures of different result types in `async/waiting` and `async/basic`. For `wait` and `join`, see decision 7.

### 5. JSON is its own type

*Problem.* `json.parse` can return anything, and `uhttp/stream` reads `event["Action"]` straight off the result.

*Recommendation.* Two ways to read JSON:

- `json.parse(s)` returns a `Json` value: null, bool, number, string, array of `Json`, or object of `Json`. Indexing one gives another `Json`, and `str(j)`, `int(j)`, `float(j)`, `bool(j)` convert it, raising if it is the wrong kind. Printing one prints it. Like decision 4's parent structs, a `Json` value carries a tag, and it is the language's one built-in value that does.
- `json.parse(s) |> T` decodes straight into a struct or primitive, reusing the machinery `?p |> City` already has for model replies.

```
let event = json.parse(line)
print(str(event["Action"]))

struct Container { Id, Image }
let c = json.parse(body) |> Container
print(c.Image)
```

`json.stringify` takes any value whose type the compiler can write out: primitives, arrays, dicts, structs and `Json`.

### 6. `raise` throws a struct

*Problem.* `raise` accepts any value today, and the catch-all `catch e` binding has no single type: one fixture prints `e` as a string and reads `e.message` as a struct field in the same file.

*Recommendation.* `raise` takes a struct. `raise "message"` stays legal and means `raise RuntimeError { message: "message" }`, the same struct every runtime error already is. A raised struct carries its type id, which is what lets a typed `catch` match a child of the type it names, as it does now.

```
struct NotFound(Failure) { path }

try { load(p) }
catch NotFound e { print(e.path) }      // e: NotFound
catch e { print(e.message) }            // e: Error
```

The catch-all `e` has type `Error`, which every raisable value converts to: printing it prints what v1 prints, and `e.message` is the message for a `RuntimeError` or a raised string, and the struct's printed form for anything else.

*What stops compiling.* Raising an int or another non-struct (`exceptions.md:11` documents it; no fixture does it). A `catch Parent e` arm reading a child's field (`exceptions/catch_inherited` does, twice); the arm must name the child.

### 7. Function values have one type each

*Problem.* A function value carries no static type today, so calling one with the wrong number of arguments is caught at run time (`functions/values`, `async/fn_values`).

*Recommendation.* A function value has a concrete type: its parameter types, its return type, and whether it is async. A closure's parameter types come from where it is used, so `apply(|x| x * x, 6)` gives `x` the type `int`. A generic builtin used as a value, such as `len` or `str`, gets a copy for the type it is used with. Calling a function value with the wrong arguments is a compile error.

`wait` takes futures that may have different result types, because it answers an index and never reads a result. `join` over different result types returns a struct per result rather than an array, for the same reason a dict cannot mix kinds.

### 8. Integers are full 64-bit

*Problem.* A tagged word spends a bit on the tag, so a Jade integer is 63-bit, and a literal past 2^62 is a lexer error.

*Recommendation.* An `int` is a full signed 64-bit integer. Overflow still raises `integer overflow`, checked with LLVM's overflow intrinsics. Floats are plain `f64` values in registers, which also ends the float leak.

### 9. Memory: reference counting, emitted per type

*Problem.* Strings, arrays, dicts and structs need freeing, and v1 counts references through runtime calls on every value.

*Recommendation.* Keep reference counting, which keeps v1's behavior, but emit it per type: an int needs nothing, a string needs one decrement, and each struct type gets its own generated release function that knows its fields. Counts stay atomic because tasks share one heap. Cycles leak, as they do now; a cycle collector is not part of 2.0. Values that the compiler can prove never escape their function live on the stack instead, which replaces v1's narrow arena.

Dicts keep value semantics (copy on write) and arrays and structs keep reference semantics, as today.

### 10. The standard library

*Problem.* Every std package is runtime code today. Some of it is small (env, path, random, time), and some is large: the task pool is about 1,750 lines, `uhttp` about 700, float printing leans on Rust's formatter, and string case mapping on Rust's Unicode tables. HTTPS works only because `std::http` runs `curl`.

*Recommendation.* Keep the whole surface, and land it in order of cost. The core language comes first, then the small packages, then tasks, then `json`, `http`, `uhttp` and `sh`. Each package's support code is emitted only when a program uses it.

Two pieces need care to match v1 exactly, and both have a way that needs no runtime:

- *Printing a float* gives the shortest digits that read back to the same number, in positional form with `.0` on whole numbers, as v1 prints. The emitted code finds those digits by formatting with increasing precision through the C library's `snprintf` until `strtod` reads the same value back.
- *`upper`, `lower` and `capitalize`* use the full Unicode case tables, including multi-character mappings such as ß to SS. The compiler is written in Rust, so it builds those tables from Rust's own and emits them into a program only when the program uses one of the three.

*The alternative* the earlier plan raised is dropping `http`, `uhttp` and `sh` from 2.0 to shorten the work.

### 11. Packages and C libraries

*Problem.* A Jade package built with `--lib` is a shared library carrying its own copy of the whole runtime, loaded with `dlopen`, and called through a tagged value format. Generic functions cannot be precompiled under per-call checking, because the argument types are not known until a program calls them.

*Recommendation.*

- A Jade package becomes a source dependency, compiled into each program that uses it, so its functions are checked and copied per call like any other.
- A C library is linked at build time. The bindings in `jade.toml` already declare each function's types, and those declarations now feed the type checker, so a `Handle("sqlite3")` is a real type.
- The 16-byte `JadeVal` format stays as the boundary for C code that calls back into Jade, with the conversion generated per static type instead of dispatched on tags.

This breaks the Jade-level FFI entry point `jade-graphics` uses today, and needs that owner's input.

## Prompts

Prompts keep their current types: `?p` is a `str`, `?p |> T` decodes into a primitive or a struct, and streaming gives a stream of strings. Inference itself is the next milestone; this one only gives the prompt types static layouts. The struct coercion behind `?p |> City` becomes generated per struct type, and decision 5's typed JSON decoding shares it.

*Inference on Metal is done* (2026-11-09), unoptimized on purpose; `src/infer/README.md` describes it. A prompt lowers as its text, and `?p` and `?p |> int`, `float`, `bool`, `char` and `str` become calls into a Jade prelude that tokenizes and decodes; `?p |> City` and grammars are not compiled yet. Replies run to `max_tokens()`, 256 unless `set_max_tokens(n)` says otherwise.

## How the compiler changes

```
v1:  AST → type_infer → TIR (partly Unknown) → emit → bytecode → codegen → LLVM, + runtime
v2:  AST → infer → TIR (fully typed) → MIR → LLVM
```

- *`infer`* replaces `type_infer.rs`'s single pass with checking per call, type variables and unification, `T?`, typed dicts and struct fields, and parent-struct joins. `Unknown` becomes an error, and the lenient `opaque_imports` mode goes away: the whole program is checked after imports are merged, at the point `aot/mod.rs` already re-checks struct defaults.
- *MIR* is new: one function per checked copy, every value's type explicit, control flow as basic blocks, and reference counting written out as instructions. It is the layer the GPU work later plugs into, and the place memory optimizations happen.
- *Codegen from MIR* replaces `src/codegen/`'s bytecode lowering. Support code for strings, collections, printing and so on is generated as LLVM functions with internal linkage, once per program, only when used. System calls go through the C library, which macOS ships as libSystem and Linux as libc.
- *Deleted at the end:* `src/bytecode/`, `compiler/emit.rs`, the bytecode lowering in `src/codegen/`, `src/runtime/`, `src/runtime_aot/`, and the `jade_pkg_init` loader. `CACHE_FORMAT_VERSION` bumps, since TIR changes shape.

## What happens to the examples

About 80 of the 123 runnable fixtures type-check under these rules unchanged. The rest fall into three groups.

*Fixtures that test a runtime type error on purpose* turn into compile-error fixtures (`*_error.jde`), since the error now happens at build time: `collections/method_type_guard`, `collections/method_receivers`, the bad-operand halves of `arithmatic/shift_bounds`, `bytes/buffer` and `bytes/construct`, `exceptions/error_values`, and the arity mismatches in `functions/values` and `async/fn_values`.

*Fixtures that rely on a pattern decided above* are rewritten to its replacement, with their `.expected` output kept wherever the output itself does not change:

- mixed arrays and streams, to parent structs (decision 4): `arrays/mixed`, `collections/self_reference`, `streams/yield_basic`
- mixed-value dicts, to structs (decision 3): `dicts/basic`, `dicts/dot_access`, `dicts/nested_get`, `collections/edge_cases`, `async/settle`
- JSON reads (decision 5): `json/parse`, `uhttp/basic`, `uhttp/stream`
- mixed return paths and parent-arm field reads (decisions 2 and 6): `functions/return_paths`, `exceptions/raise_through_frames`, `exceptions/catch_inherited`

*Fixtures that need a default:* an empty collection nothing ever fills (`tricks = []` in `structs/inheritance`) gets an element type of `nil`, which prints the same.

Two fixture comments already claim arrays are homogeneous and are wrong today (`arrays/basic:11-14`, `collections/edge_cases:27-28`). They become right.

## Holes found on the way, to close rather than carry over

- Float boxes are never freed, and Grammar objects are never released. Both go away with unboxed floats and per-type release.
- `d.keys()` returns trusted strings, which launders taint from a dict built out of tainted input.
- `infer.c` gives a model reply the prompt's trust, while the README says replies are tainted.
- `json.parse` silently truncates integers wider than 63 bits.
- The checker holes listed under "Where the type checker stands".

## Order of work

Each phase ends with the golden gate green for every example it covers. The new backend sits behind `jade build --backend=static` until it covers the whole suite, then becomes the only one.

1. *Inference.* The new checker, run over every example in `jade check`, with the fixture changes above made as it rejects them. Nothing is compiled differently yet.
2. *MIR and core codegen.* Ints, floats, bools, strings, arrays, dicts, structs, control flow, functions, printing, and integer overflow. The examples that use only these pass under `--backend=static`.
3. *Exceptions, function values and closures, parent-struct joins, streams.*
4. *Tasks:* `async`, `await`, `join`, `wait`, `cancel`, `max_tasks`.
5. *The standard library*, in the order of decision 10, then C libraries and Jade packages per decision 11.
6. *Switch over and delete* the bytecode, the old codegen and both runtimes.

## Progress

*Phase 1 is done* (2026-11-04): `src/typechecker/infer/` checks the whole merged program and gates `jade build` and `jade check`. Every runnable example checks, the 19 fixtures listed above were rewritten, and each runtime type error they tested is now a `*_error.jde` compile-error fixture. Building it settled four details this document had left open:

- *A function's trailing bare expression is its return value*, as v1 ran it: `fn double(x) { x * 2 }` returns an int.
- *An early exit narrows too.* After `if x == nil { return }`, the rest of the block reads `x` as its non-optional type, as inside `if x != nil { … }`.
- *JSON absorbs nil.* A `Json` value already has a null, so `let first = nil` later assigned `json.parse(line)` is `Json`, not an optional.
- *An array cannot contain itself.* Its element type would have to contain its own type. A structure that points back at itself goes through a struct field, whose type is a name.

*Phase 2 is done* (2026-11-04): `src/mir/` lowers the checker's answer into MIR, and `src/backend/` compiles MIR to a binary that links libc and nothing else, behind `jade build --backend=static`. The 52 examples that use only the core language print exactly their recorded output under `golden.sh --backend=static`, with no leaks under macOS's `leaks` tool. The list is `src/scripts/static-examples.list`. Everything a later phase adds stops with an error that names the feature. On one benchmark the static binary ran about 30 times faster than v1's and was a hundredth of its size.

Building it settled five details:

- *`==` compares the contents of arrays and dicts*, and the identity of structs, which are references. v1 refused `==` on two arrays at run time; the checker accepts it, so it needed a meaning.
- *Popping an empty array is a runtime error*, `pop from an empty array`. v1 returned `nil`, which an `[int]` cannot hold.
- *Runtime errors stop the program* with v1's `jade: <message>` and exit status 1 until exceptions land in phase 3. They all go through one function, `F::fail`, which phase 3 turns into a raise.
- *A parameter is owned by the function*, which retains it on entry. That keeps a dict parameter a copy under copy-on-write.
- *Integer literals were limited to 2^62* by the lexer, because the tagged backend could not hold more. The limit lifted at the switchover in phase 6.

*Phase 3 is done* (2026-11-04), without closures, which `design/v2-classes.md` removed. Exceptions are a thread-local error slot that MIR checks after every operation that can raise, with no unwinding library. A named function used as a value is called directly; a value of a joined function type is a pointer to a generated adapter. A stream runs its generator to the end into an array, which is what v1 printed. Parent-struct joins became classes.

*Phase 4 is done* (2026-11-04). A call to an async function queues a job that is also its future, and worker threads up to `max_tasks()` run them. Two choices keep it simple:

- *Awaiting a job that has not started runs it on the awaiting thread.* A chain of 2,000 tasks each awaiting the next runs on one thread, and `set_max_tasks(1)` with a task awaiting another cannot deadlock.
- *`join(…, settle = …)` needs a written `true` or `false`*, because it decides whether the result is an array of values or of `Settled` records. v1 decided at run time.

With phases 3 and 4, 94 examples pass under `--backend=static`.

*Phase 5's standard library is done* (2026-11-04): math, time, bytes, fs, sh, env, path, random, json, http and uhttp, with the trust model. Every example that does not dereference a prompt, 112 of them, passes under `--backend=static`, and ad hoc programs exercising each package printed what v1 printed, byte for byte, apart from the choices below. Building it settled these:

- *Strings carry a trust word in their header*, and a char taken from a tainted string carries the taint in bit 31. Bytes have the string layout, so they carry it too.
- *`http` and `uhttp` both run `curl`.* v1 ran `curl` for `http` and framed HTTP by hand over a Unix socket for `uhttp`; `curl --unix-socket` does the second as well, so one mechanism serves both, and `uhttp.stream` calls the handler through a function pointer once a line.
- *`json.parse` follows serde_json's grammar and messages*, line and column included, and a `Json` value is the one tagged value: null is a null pointer, and arrays and objects are a `[json]` and a `{json}`.
- *`math.pow` of two ints with a negative exponent raises*, since the result is an int; v1 returned a float. `random.choice` of an empty array raises; v1 returned `nil`.
- *`json.stringify` keeps the taint of the strings it writes*, where v1's result was trusted, which was a way around `sh.exec`'s check. It writes a class value as an object of its fields; v1 wrote `null`.
- *`http.head` runs `curl --head`.* v1's `-X HEAD` left curl waiting for the body a HEAD response's length promised.

*Decision 11's C libraries are done* (2026-11-04). A native package keeps v1's `JadeVal` boundary, so a library built for v1 loads unchanged, and the static backend marshals each call by its static types. The types come from `jade.toml`:

- *A Jade-ABI library declares its functions in its `[lib]` entry*, as `[lib.<name>.symbols.<fn>]` with `args` and `ret` in Jade types: `handle<Thing>`, `handle` for any handle, `fn(int, str) -> int` for a callback, `[T]`, `{T}`, `T?`. This is new, and it is what keeps a native call from being the one untyped thing left: such a library has no types of its own.
- *A C dependency's Jade signature is derived from its symbol table*, through the same parse the shim generator uses. A C struct crossing either way becomes a class under its C name, `box.box_t`, with public fields typed from the `structs` table and a zero default where the type has one, so a literal may leave a field out as C leaves it zero. Several results become a class named `<symbol>_result`, keyed as the shim keys them: `ret` for the C return, then each out-parameter by its name. glib's fixture, the `alloc_str` long run and `src/scripts/struct-fixture.jde` pass under `--backend=static`. A Jade-ABI library declares the records it passes as `[lib.<name>.structs.<T>]` with `fields` in Jade types.
- *An undeclared function still checks*: its arguments go as they are, and its result takes the type its use gives it. Where nothing settles the result, the build names the declaration to add. A callback always needs one, since nothing else says what C will pass it.

This is the part of decision 11 that needed the input it names: a Jade-ABI package such as jade-graphics now builds under `--backend=static` once its `[lib]` entry declares what its functions take and return, and its calls whose results the program uses are typed even without that.

*Jade packages as source dependencies are done* (2026-11-09), which completes decision 11. A dependency with `abi = "source"` is a directory, git repository or `.tar.gz` archive holding a project with a `[package]` section, and its files join the program's import graph like any module, so a package's generic functions are checked per call. The details, in `src/pkg/source.rs`:

- *`use geo` names the package's entry module*, and `[package] exports`, when present, is the list of its top-level names another project may use. Anything else is a compile error naming the list.
- *A package's own imports resolve from the package*: relative imports from its files, then its own `[lib]` entries, then the project's dependencies. A `jade.toml` with `[package]` and no `[project]` is therefore a project root too.
- *Dependencies are flat*, as they were for prebuilt packages: adding a package adds what it declares to the project's manifest, and one copy of each package is compiled in. A path a package names is rewritten relative to the project when the package is itself a path; from a git or archive package it cannot be followed and is named for the user to add.
- *The lock pins a hash of the source*, `jade.toml` and every `.jde` file by path and content, for all three kinds, plus the commit for git. Installing checks the unpacked tree against it, and a path package is re-pinned when it changes, as a path library always was.

`jade build --lib` went away in phase 6, below. A native library a program calls still exports `jade_pkg_init`, since that is the boundary C code speaks.

*C structs and several results are done* (2026-11-09): see the C dependency bullet above. *musl* is too: on a Linux system with no glibc, such as Alpine, the backend links against musl's loader, which is its C library, and the one `_start` the backend emits serves both.

*Phase 6 is done* (2026-11-09). The static backend is the only one: `jade build`, `jade run`, `jade test` and `jade check` all go through `aot::compile` or `aot::would_build`, and the `--backend` flag and the list of covered examples are gone. Deleted: `src/bytecode/`, `src/codegen/`, `compiler/emit.rs` with its escape analysis and grammar builder, `src/runtime/` (`jade-runtime`), `src/runtime_aot/`, and `src/alloc/`, the `jade` binary's own pool allocator. The root `build.rs` builds only the LLD bridge, and a release ships one binary. Building it settled these:

- *The task-safety check moved to MIR* (`src/mir/taskcheck.rs`). It ran inside bytecode emission, so the static backend had been building programs whose tasks race; it now runs on every build. MIR blocks record a span per statement for its messages. It is a little more precise than v1's: only heap values can be shared, and a write into a global's collection counts as a global write even inside a helper called with no arguments, which v1 missed.
- *Integers are full 64-bit in source too.* The lexer reads up to `i64::MAX`, and `-9223372036854775808` folds to `i64::MIN`.
- *Prompts wait for inference.* A prompt, a prompt field or a `Grammar` stops with `jade cannot compile prompts yet`; `jade check`'s fixture test counts those examples as pending, and `golden.sh` skips them. The Metal inference kernel is the next milestone.
- *The value ABI number moved* from `jade_runtime::RUNTIME_ABI_VERSION` to `backend::native::VALUE_ABI_VERSION`, still 5, so a package built for v1 loads unchanged.

## Open questions

`design/v2-classes.md` answers the first two, and both are built: an `is` test (its decision 8) and support for every parent through a field table (its decision 9).

1. *How does a program test which child struct it holds?* Methods cover most uses. For the rest, the choices are a new `if x is Token { ... }` test, which narrows `x` inside the branch, or nothing beyond methods and typed `catch`.
2. *Multiple inheritance and layout.* A child's fields follow its first parent's, so a child can stand in for its first parent with no adjustment. Standing in for a second parent needs a per-type field table. That is a cost only programs using a second parent pay, but it is worth confirming that both parents should support it.
3. *The recursion limit.* v1 counts calls and raises `recursion limit exceeded` at 10,000, a number chosen to match the VM. A check of the remaining stack at function entry gives the same error without a counter, and allows deeper recursion than 10,000 when frames are small.
