# `src/future/`: the one thing you can ask a task without waiting

## What this subtree is

One method, `ready()`, on the value an `async fn` call returns. It answers whether the task has finished, and it does not block.

That is the whole module. It is a top-level module rather than a few lines inside `builtins/` because that is the shape every other primitive-method provider has — `src/string/` supplies the `str` methods, `src/dict/` supplies the `dict` methods — and a future is now one more receiver kind with a table of its own.

## Why it was built

`await` blocks. That is the right default: a task is work you asked for, and asking for the answer should hand it to you. It is also the whole problem for anything with a loop it cannot stop.

A screen redrawing at frame rate has no point at which it is willing to freeze. Before this it could start work concurrently — that part always worked, and putting a render loop on its own task keeps the screen alive — but the only way to *read* the result was to block the reader. So a program could fire off a connection attempt and then have no way to notice it had succeeded, short of picking an arbitrary moment to stop drawing. That is the gap this closes, and it came out of building an OS on top of the language rather than out of a test.

The shape it enables:

```jade
let pending = connect(ssid)
let shown = false
while running {
    draw_frame()
    if !shown && pending.ready() {
        show_banner(await pending)
        shown = true
    }
}
```

The `await` after a true `ready()` costs nothing — measured at 7µs compiled, 39µs interpreted, on a future that has already finished. It falls straight through the wait. That is what makes the shape work rather than merely read well: without it, the poll would only move the block rather than remove it.

## What it holds now

`ready()` came first and answers one question without blocking. `cancel()` joined it in v1.4.5, and the pair is the whole surface a future has: *is it done*, and *stop waiting for it*.

`cancel` does not stop the work. A compiled task is a real thread running straight-line code with no point at which the runtime could interrupt it, so a cancel that claimed to kill the task would be a promise only one engine could keep. It stops the caller: `await` raises at once, and one already blocked wakes and raises. A task that wants to give up early checks `cancelled()`, which is the only thing that actually stops work, and which is the task's own choice.

Cancelled outranks a result that arrived anyway. The caller said it had stopped waiting, and handing it the answer after that would make `cancel` mean nothing on a task that was about to finish.

## What each file does

- *`mod.rs`* holds `find_future_method`, `register_future_method_types`, and the `ready` implementation. The interpreter reads the future's join handle; a handle that has already been taken means the task is certainly over.
- *`tests.rs`* covers the registry entries. The behaviour itself is pinned by `examples/async/ready/`, which the parity gate runs on both engines.

## What `ready` means

*Finished, however it ended.* A task that raised is finished, and the `await` that follows re-raises. A future whose result has already been taken is finished, and the second `await` reports the double await on its own terms. Neither is this method's business to hide, and a caller that wants to know which happened can `try` around the `await`.

*Not a guarantee about the next instant.* False means "not yet", not "not soon". Nothing here promises a task will still be unfinished by the time the caller acts on the answer, which is the ordinary property of asking about another thread.

## Where the rest of the async surface lives

Nothing about running tasks is a package, and this module is one piece of that. `async fn` and `await` are keywords, `join` is an opcode, `f.ready()` and `f.cancel()` are here, and `cancelled()`, `wait()`, `max_tasks()`, and `set_max_tasks()` are bare globals in `src/core/`. A program never imports anything to use tasks.

`max_tasks` and `set_max_tasks` are the newest of those and the only ones that are not about a single future: they say how many tasks may run at once, defaulting to 32. See `src/core/README.md` for what the number means and `src/runtime/README.md` for how each engine enforces it.

## Why there is no package form

Every other primitive method has one: `s.upper()` is `string.upper(s)`, and `src/builtins/README.md` makes registering both a rule. That rule is about *package* functions, which have a receiver-first spelling to mirror. A future is not a package and has no other functions, so a `std/future` holding one name would be a package invented for the symmetry rather than for anything a program wants to import.

## Who uses it

*Depends on:* `builtins/` for `BuiltinFn`, `vm/` for `VmValue` and the future handle, and `compiler/type_infer` for the method's type.

*Used by:* `builtins::find_primitive_method` through `PrimType::Future`, and `builtins::register_primitive_method_types`. The interpreter reaches it from the `VmValue::Future` arm of `GetField` in `vm/dispatch.rs`. The compiled backend does not come through here at all: `codegen` lowers `ready` to `jade_future_ready`, which sits on `jrt_future_ready` in `jade-runtime`'s `task.rs`, so both engines answer from the same `done` flag.

## Gotchas

*A polling loop is free inside something that was already looping.* A render loop wakes at frame rate whether or not it asks. It is not free for a program with nothing else to do — `while !f.ready() { }` burns a core. What that case wants is to block until *any* of several futures resolves, with a timeout, so the CPU can idle. That primitive does not exist yet, and this one is not a substitute for it.

*The receiver guard belongs at the call site.* `codegen` emits `jrt_require_kind(recv, JRT_WANT_FUTURE, "ready")` before the call, which is what gets the interpreter's exact wording — `int has no method 'ready'` — for free. `jade_future_ready` checks again anyway, because it is the boundary and a value arriving from elsewhere is not the guard's business.
