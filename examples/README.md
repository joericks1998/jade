# `examples/`: the language fixture suite

## What this subtree is

Every `.jde` file here is a test fixture, not a demo. CI type-checks all of them on every pull request, and runs most of them through *both* execution engines, diffing the output. Together they are the closest thing Jade has to a specification of its own surface.

They are also where a language change should start. The established workflow is simple: *write the `.jde` fixture first, then make the Rust match it*. Working in that order tends to surface design problems before you have built on top of them.

## How the naming works

Two rules, both enforced by CI:

- `<name>.jde` must *pass* `jade check`.
- `<name>_error.jde` must *fail* `jade check`. These document programs the language deliberately rejects, so one that quietly starts passing is also a CI failure.

A third rule applies to the whole tree: it stays formatted, and CI runs `jade fmt --check examples` to hold it there. That is as much a test of the formatter as of the fixtures. `jade fmt` is the one command nothing else here exercises, and by v1.1.34 it had rotted badly enough to reindent the inside of multi-line strings, changing what a program printed. Run `jade fmt examples` before committing a new fixture. Four spaces per level.

## What is in here

There is one directory per language area, and one subdirectory inside it per case:

`arithmatic/` (arithmetic, bitwise, unary) · `arrays/` · `assignment/` · `async/` · `closures/` · `collections/` · `control_flow/` · `decorators/` · `dicts/` · `exceptions/` · `for_loop/` · `fs/` · `functions/` · `http/` · `imports/` · `interfaces/` · `llm/` · `llvm/` · `numbers/` · `pipe/` · `streams/` · `strings/` · `structs/` · `time/` · `trust/` · `uhttp/`

`async/max_tasks/` pins the v1.4.6 concurrency limit. It reads the default, clamps at both ends, and then times the same eight sleeping tasks at a limit of four and of sixteen, so a run that widened or narrowed the fan-out fails on the wave count rather than passing quietly. Its last case is a task awaiting another at `set_max_tasks(1)`, which is what a parked task keeping its slot would deadlock on.

`exceptions/raise_through_frames/` pins the v1.4.3 unwinding cleanup: a raise that passes through frames holding locals, a catch that keeps its own, and a rethrow of an unmatched type. Nothing there is visible in the output, which is the point — what it pins is that the values are still correct after the frames let go of what they held.

Five fixtures pin the v1.4.3 async work, and each fails on a build from before it. `async/deep_nesting/` nests `await` 2,000 deep, where a binary used to hang at about 512. `async/fn_values/` holds an `async fn` in a local, an array, a dict, and a `map`, none of which compiled at all. `async/imported/` awaits an `async fn` from a module, which failed on both engines. `streams/raise_midstream/` raises out of a `yield`ing function, which gave the caller the wrong buffer compiled. And `async/shared_state/` gained three error fixtures for the task-safety holes: a callback, a user method, and the spawner's own writes.

Three fixtures pin the v1.3.3 fixes, and each one fails on a build from before those fixes. `trust/sh_sinks/` feeds a shell command's own output back into all three `sh` functions, and `sh.output` used to run it. `dicts/dot_access/` reads dict entries with a dot, which raised "value has no fields" when compiled and worked when interpreted. `async/nested_async_error.jde` nests an `async fn`, which used to parse and then fail at run time on a variable the inner function could not see.

`decorators/` covers `@dec` on a `let` and on a `prompt`, both of which the parser rewrites into a call. In `prompt_tags/`, the wrapper prints what it built, because there is otherwise nothing to assert against. A prompt renders as `<prompt>`, so a fixture cannot observe its text any other way. Decorators on `fn`, `struct`, and `extend` are older, and unit tests in `src/frontend/tests.rs` exercise those rather than fixtures here.

`llm/` is worth calling out. Those fixtures do real prompt dereferences, and they still give the same answer every time in CI, because `src/scripts/backend-parity.sh` installs `src/scripts/fake-provider.jde` as a stand-in inference provider that answers with a canned reply. An example supplies its own reply as a `responses.txt` beside the `.jde`, and gets the default without one. Pointing the parity gate at these turned up a VM muting bug and an AOT segfault immediately.

`exceptions/error_values/` pins the *shape* of a caught error: a `RuntimeError` struct with a `message` field, matched by a typed catch. The compiled backend used to raise the bare message string, so `e.message` worked when interpreted and raised when compiled. The fixture checks messages by substring rather than by equality, because the interpreter adds a `[line:col]` prefix and a compiled binary has no source position at run time. That is the one difference that remains on purpose.

`functions/values/` covers what a call site does *not* know. Calling a name the compiler can see resolves to one function, so it can check the arguments and fill the ones the call left off; calling a *value* cannot, because the site has no idea which function the value holds. A compiled binary used to guess from the arguments it had, which dropped extras, read missing ones out of uninitialised memory, and never filled a default. The fixture calls the same function directly and through a value, with and without its default, and also covers a built-in as a value, a bound method through `map`, and how each kind of callable prints. It reads arity messages with `.contains` rather than `==`, for the `[line:col]` reason above.

`control_flow/break_continue/` and `exceptions/break_from_catch/` cover the two halves of `break`, added in v1.3.6. The first is the ordinary shape: leaving a `for`, leaving a `while true`, and leaving only the innermost of two nested loops. The second is the one with a trap in it: leaving through a `catch` arm, and leaving from the `try` body itself. Both have to pop the handler the jump escapes, so both end by raising again and catching it, which fails loudly if a stale frame was left installed.

Three more fixtures pin the AOT memory bugs fixed in v1.1.34, and each one *fails on the backend from before those fixes*. That is what makes them worth keeping.

`dicts/nested_get/` reads a module-level dict of dicts through `.get()` until a missing retain double-frees the inner dict. `collections/method_type_guard/` calls primitive methods on receivers of the wrong kind, which used to segfault rather than raise. `exceptions/return_inside_try/` returns out of a `try`, which leaked a handler frame pointing at dead stack. Before the fix, that fixture does not crash, it *spins forever*, so run it under a timeout if you ever test against an old binary.

All three are ordinary parity examples. The gate special-cases none of them.

`imports/missing_error/` names a module that does not exist. It is the fixture for the gap v1.1.33 closed. `jade check` used to accept it and let it fail at run time instead, because import resolution was not a compile stage. The `*_error` convention now covers imports too, because the harness in `cli/check.rs` checks a fixture *by path* rather than by source text. A `use` cannot be resolved without knowing which file asked for it.

`imports/project_lib/` is the other odd one. It carries its own `jade.toml`, which makes it a project inside the fixture tree, and that is deliberate.

The gate runs every example from the repository root. So a fixture whose imports depend on *its own* project root is the only way to catch the two engines disagreeing about where that root is. They did disagree until v1.1.31, with the VM reading it from the shell's directory and the AOT path reading it from the source file's.

Its importing file sits under `app/`, so the target directory is out of relative-path reach and the `[lib]` entry is genuinely exercised.

## Who uses it

*Used by:* `.github/workflows/ci.yml`, which runs `jade check` over every fixture and `jade fmt --check` over the tree. `src/scripts/backend-parity.sh` runs each fixture on the VM and on the AOT backend and diffs stdout. The `docs/` site draws on several of them for its examples.

*Depends on:* only the `jade` binary. Fixtures never import from the Rust tree.

## Gotchas

*The `jade check` gate type-checks fixtures and never runs them.* That is what lets them depend on a network or an API key at run time. The tradeoff is that a fixture can type-check happily while printing something its own comments contradict. When the *behavior* matters, write a Rust test as well. `run_src` and `run_src_with_mock` in `src/vm/tests.rs` are the helpers for that.

The parity script keeps a skip list for examples that cannot run identically on both engines. Check its header before assuming an example is covered.

## Running them

```sh
./target/debug/jade check examples/structs/prompt_fields/prompt_fields.jde
./target/debug/jade run   examples/strings/fstrings/fstrings.jde
./src/scripts/backend-parity.sh
```
