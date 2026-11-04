# `examples/`: the language fixture suite

## What this subtree is

Every `.jde` file here is a test fixture, not a demo. CI type-checks all of them on every pull request, and builds and runs most of them, comparing each binary's output with the output recorded beside it. Together they are the closest thing Jade has to a specification of its own surface.

They are also where a language change should start. The established workflow is simple: *write the `.jde` fixture first, then make the Rust match it*. Working in that order tends to surface design problems before you have built on top of them.

## How the naming works

Two rules, both enforced by CI:

- `<name>.jde` must *pass* `jade check`.
- `<name>_error.jde` must *fail* `jade check`. These document programs the language deliberately rejects, so one that quietly starts passing is also a CI failure.

Each runnable fixture also has a `<name>.expected` beside it: what it printed on v1.4.8, recorded from a run where the VM and the compiled backend agreed. v2 removes the VM, so there is no second engine to diff against, and `src/scripts/golden.sh` compares each built binary to that file instead. A fixture that should exit non-zero records its status in `<name>.expected-exit`. A new fixture needs its `.expected` written by hand, and checked by reading it.

v2 turned a set of runtime type errors into compile errors, since every value's type is known when the program is checked. The fixtures that raised and caught one of those kept their other cases, and each error moved into a `*_error.jde` beside its category: `arithmatic/bitwise_non_int_error`, `arrays/mixed_error`, `async/join_non_future_error`, `bytes/write_non_int_error`, `bytes/construct_types_error`, `collections/dict_contains_error`, `collections/int_len_error`, `collections/method_type_guard_error`, `collections/self_reference_error`, `dicts/dot_access_error`, `exceptions/catch_parent_reads_child_error`, `functions/value_arity_error`, `functions/return_paths_error` and `streams/yield_mixed_error`. Each names the rule it pins in its opening comment.

A third rule applies to the whole tree: it stays formatted, and CI runs `jade fmt --check examples` to hold it there. That is as much a test of the formatter as of the fixtures. `jade fmt` is the one command nothing else here exercises, and by v1.1.34 it had rotted badly enough to reindent the inside of multi-line strings, changing what a program printed. Run `jade fmt examples` before committing a new fixture. Four spaces per level.

## What is in here

There is one directory per language area, and one subdirectory inside it per case:

`arithmatic/` (arithmetic, bitwise, unary) · `arrays/` · `assignment/` · `async/` · `bytes/` · `closures/` · `collections/` · `control_flow/` · `decorators/` · `dicts/` · `exceptions/` · `ffi/` · `for_loop/` · `fs/` · `functions/` · `http/` · `imports/` · `interfaces/` · `json/` · `llm/` · `llvm/` · `numbers/` · `pipe/` · `streams/` · `strings/` · `structs/` · `time/` · `trust/` · `uhttp/`

`async/max_tasks/` pins the v1.4.6 concurrency limit. It reads the default, clamps at both ends, and then times the same eight sleeping tasks at a limit of four and of sixteen, so a run that widened or narrowed the fan-out fails on the wave count rather than passing quietly. Its last case is a task awaiting another at `set_max_tasks(1)`, which is what a parked task keeping its slot would deadlock on.

`exceptions/raise_through_frames/` pins the v1.4.3 unwinding cleanup: a raise that passes through frames holding locals, a catch that keeps its own, and a rethrow of an unmatched type. Nothing there is visible in the output, which is the point — what it pins is that the values are still correct after the frames let go of what they held.

Five fixtures pin the v1.4.3 async work, and each fails on a build from before it. `async/deep_nesting/` nests `await` 2,000 deep, where a binary used to hang at about 512. `async/fn_values/` holds an `async fn` in a local, an array, a dict, and a `map`, none of which compiled at all. `async/imported/` awaits an `async fn` from a module, which failed on both engines. `streams/raise_midstream/` raises out of a `yield`ing function, which gave the caller the wrong buffer compiled. And `async/shared_state/` gained three error fixtures for the task-safety holes: a callback, a user method, and the spawner's own writes.

Three fixtures pin the v1.3.3 fixes, and each one fails on a build from before those fixes. `trust/sh_sinks/` feeds a shell command's own output back into all three `sh` functions, and `sh.output` used to run it. `dicts/dot_access/` reads dict entries with a dot, which raised "value has no fields" when compiled and worked when interpreted. `async/nested_async_error.jde` nests an `async fn`, which used to parse and then fail at run time on a variable the inner function could not see.

`decorators/` covers `@dec` on a `let` and on a `prompt`, both of which the parser rewrites into a call. In `prompt_tags/`, the wrapper prints what it built, because there is otherwise nothing to assert against. A prompt renders as `<prompt>`, so a fixture cannot observe its text any other way. Decorators on `fn`, `struct`, and `extend` are older, and unit tests in `src/frontend/tests.rs` exercise those rather than fixtures here.

`llm/` is worth calling out. Those fixtures do real prompt dereferences. Until v1.4.8 they gave the same answer every time in CI, because the parity script installed a stand-in inference provider that answered with a canned reply. An example supplied its own reply as a `responses.txt` beside the `.jde`, and got the default without one. Pointing the parity gate at these turned up a VM muting bug and an AOT segfault immediately. v2 dropped the provider packages, so `src/scripts/golden.sh` skips every example that dereferences a prompt until the compiler emits inference itself.

`exceptions/error_values/` pins the *shape* of a caught error: a `RuntimeError` struct with a `message` field, matched by a typed catch. The compiled backend used to raise the bare message string, so `e.message` worked when interpreted and raised when compiled. The fixture checks messages by substring rather than by equality, because the interpreter added a `[line:col]` prefix and a compiled binary has no source position at run time. That was the one difference between the two engines that remained on purpose.

`functions/values/` covers what a call site does *not* know. Calling a name the compiler can see resolves to one function, so it can check the arguments and fill the ones the call left off; calling a *value* cannot, because the site has no idea which function the value holds. A compiled binary used to guess from the arguments it had, which dropped extras, read missing ones out of uninitialised memory, and never filled a default. The fixture calls the same function directly and through a value, with and without its default, and also covers a built-in as a value, a bound method through `map`, and how each kind of callable prints. It reads arity messages with `.contains` rather than `==`, for the `[line:col]` reason above.

`control_flow/break_continue/` and `exceptions/break_from_catch/` cover the two halves of `break`, added in v1.3.6. The first is the ordinary shape: leaving a `for`, leaving a `while true`, and leaving only the innermost of two nested loops. The second is the one with a trap in it: leaving through a `catch` arm, and leaving from the `try` body itself. Both have to pop the handler the jump escapes, so both end by raising again and catching it, which fails loudly if a stale frame was left installed.

Three more fixtures pin the AOT memory bugs fixed in v1.1.34, and each one *fails on the backend from before those fixes*. That is what makes them worth keeping.

`dicts/nested_get/` reads a module-level dict of dicts through `.get()` until a missing retain double-frees the inner dict. `collections/method_type_guard/` calls primitive methods on receivers of the wrong kind, which used to segfault rather than raise. `exceptions/return_inside_try/` returns out of a `try`, which leaked a handler frame pointing at dead stack. Before the fix, that fixture does not crash, it *spins forever*, so run it under a timeout if you ever test against an old binary.

All three are ordinary golden examples. The gate special-cases none of them.

`imports/missing_error/` names a module that does not exist. It is the fixture for the gap v1.1.33 closed. `jade check` used to accept it and let it fail at run time instead, because import resolution was not a compile stage. The `*_error` convention now covers imports too, because the harness in `cli/check.rs` checks a fixture *by path* rather than by source text. A `use` cannot be resolved without knowing which file asked for it.

`imports/project_lib/` is the other odd one. It carries its own `jade.toml`, which makes it a project inside the fixture tree, and that is deliberate.

The gate runs every example from the repository root. So a fixture whose imports depend on *its own* project root is the only way to catch a build reading that root from the wrong place. The two engines disagreed about it until v1.1.31, with the VM reading it from the shell's directory and the AOT path reading it from the source file's.

Its importing file sits under `app/`, so the target directory is out of relative-path reach and the `[lib]` entry is genuinely exercised.

## Who uses it

*Used by:* `.github/workflows/ci.yml`, which runs `jade check` over every fixture and `jade fmt --check` over the tree. `src/scripts/golden.sh` builds each fixture, runs the binary, and compares its output with the `.expected` file beside it. The `docs/` site draws on several of them for its examples.

*Depends on:* only the `jade` binary. Fixtures never import from the Rust tree.

## Gotchas

*The `jade check` gate type-checks fixtures and never runs them.* That is what lets them depend on a network or an API key at run time. The tradeoff is that a fixture can type-check happily while printing something its own comments contradict. When the *behavior* matters, record it in the fixture's `.expected` file, which `src/scripts/golden.sh` holds the built binary to. Until v1.4.8 the helpers for a Rust test of behavior were `run_src` and `run_src_with_mock` in `src/vm/tests.rs`, and they went with the VM.

`src/scripts/golden.sh` keeps a skip list for examples it does not run: the ones that need the network, the `_error` fixtures, and the 14 that dereference a prompt. Check it before assuming an example is covered.

## Running them

```sh
./target/debug/jade check examples/structs/prompt_fields/prompt_fields.jde
./target/debug/jade run   examples/strings/fstrings/fstrings.jde
./src/scripts/golden.sh
```
