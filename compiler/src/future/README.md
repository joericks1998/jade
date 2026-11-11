# `src/future/`: the one thing you can ask a task without waiting

## What this subtree is

One method, `ready()`, on the value an `async fn` call returns. It answers whether the task has finished, and it does not block.

That and `cancel()`, covered below, are the whole module. It is a top-level module rather than a few lines inside `builtins/` because that is the shape every other primitive-method provider has — `src/string/` supplies the `str` methods, `src/dict/` supplies the `dict` methods — and a future is now one more receiver kind with a table of its own.

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

`cancel` does not stop the work. A compiled task runs straight-line code on a worker thread with no point at which anything could interrupt it, so a cancel that claimed to kill the task would be a promise the program could not keep. It stops the caller: `await` raises at once, and one already blocked wakes and raises. A task that wants to give up early checks `cancelled()`, which is the only thing that actually stops work, and which is the task's own choice.

Cancelled outranks a result that arrived anyway. The caller said it had stopped waiting, and handing it the answer after that would make `cancel` mean nothing on a task that was about to finish.

## What each file does

- *`mod.rs`* holds `FUTURE_METHODS`, which names `ready` and `cancel`, and `register_future_method_types`, which gives the checker their types. The behavior lives in `src/backend/tasks.rs` and the `Op::Ready` and `Op::Cancel` arms of `src/backend/func.rs`, and it is pinned by `examples/async/ready/`, which `src/scripts/golden.sh` builds and compares with its recorded output.

## What `ready` means

*Finished, however it ended.* A task that raised is finished, and the `await` that follows re-raises. A future whose result has already been taken is finished, and the second `await` reports the double await on its own terms. Neither is this method's business to hide, and a caller that wants to know which happened can `try` around the `await`.

*Not a guarantee about the next instant.* False means "not yet", not "not soon". Nothing here promises a task will still be unfinished by the time the caller acts on the answer, which is the ordinary property of asking about another thread.

## Where the rest of the async surface lives

Nothing about running tasks is a package, and this module is one piece of that. `async fn` and `await` are keywords, `join` is an operation in MIR, `f.ready()` and `f.cancel()` are here, and `cancelled()`, `wait()`, `max_tasks()`, and `set_max_tasks()` are bare globals in `src/core/`. A program never imports anything to use tasks.

`max_tasks` and `set_max_tasks` are the newest of those and the only ones that are not about a single future: they say how many tasks may run at once, defaulting to 32. See `src/core/README.md` for what the number means and `src/backend/tasks.rs` for how a compiled program enforces it.

## Why there is no package form

Every other primitive method has one: `s.upper()` is `string.upper(s)`, and `src/builtins/README.md` makes registering both a rule. That rule is about *package* functions, which have a receiver-first spelling to mirror. A future is not a package and has no other functions, so a `std/future` holding one name would be a package invented for the symmetry rather than for anything a program wants to import.

## Who uses it

*Depends on:* `typechecker/type_infer` for `TypeContext`, which the method types register into.

*Used by:* `builtins::primitive_method_tables`, which lists `FUTURE_METHODS` under `future`, and `builtins::register_primitive_method_types`. The compiled backend does not come through here at all: `mir/ops.rs` lowers `ready` to `Op::Ready`, and `src/backend/func.rs` answers it from the job's state and cancel words under the task queue's lock.

## Gotchas

*A polling loop is free inside something that was already looping.* A render loop wakes at frame rate whether or not it asks. It is not free for a program with nothing else to do — `while !f.ready() { }` burns a core. What that case wants is to block until *any* of several futures resolves, with a timeout, so the CPU can idle. That primitive is `wait()`, a bare global, with a `time.after(secs)` among the futures for the timeout, and this one is not a substitute for it.

*The receiver is checked before the program runs.* The static checker knows every value's type, so `n.ready()` on an int stops the build with `int has no method 'ready'`, from `typechecker/infer/builtins.rs`. Until phase 6, v1's backend emitted a run-time guard before the call and the runtime checked again; a v2 binary needs neither.
