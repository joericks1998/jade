---
id: changelog
title: Changelog
sidebar_label: Changelog
---

## v1.4.6

*How many tasks run at once is now something a program says, and both engines listen.* It was the machine's core count, overridable only through a `JADE_MAX_TASKS` environment variable that appeared in no documentation and reached the compiled engine only. `jade run` ignored it outright, so the same fan-out ran sixteen requests in one wave built and two waves interpreted.

```jade
print(max_tasks())           // 32
print(set_max_tasks(8))      // 8
```

Two bare globals, next to `cancelled()` and `wait()`, because nothing else about async is imported either. The default is a flat 32 rather than the core count: a task usually waits on a model or a socket rather than using a core, so sizing the limit to the machine measured the wrong resource. `set_max_tasks` answers with what took effect, since a request outside `1` to `512` is clamped rather than refused.

*The interpreter honors it too, which it could not before.* `jade run` ran one task per core whatever the limit said, because a task that blocks on a socket held the runtime worker it landed on. The calls that wait on the outside world — `http`, `uhttp`, `sh`, `time.sleep`, `input`, and `fs.read_stdin_bytes` — now hand that worker off for the duration, so sixteen requests take one wave under `jade run` exactly as they do compiled. Inference already did, through the provider backend. A task parked in `await` gives its slot back, so a task awaiting another cannot deadlock against the limit even at `set_max_tasks(1)`.

*Fixed on the way: the compiled pool ignored the limit on threads it already had.* It refused to grow whenever any worker was idle, which made eight tasks submitted in a loop onto four idle workers run four at a time under a limit of sixteen — every submit saw an idle worker because none had woken yet. And a worker now checks the limit before claiming queued work, not only before starting a thread, so lowering the limit after a wide fan-out binds on the threads that fan-out left behind.

`JADE_MAX_TASKS` is gone. Nothing in the documentation referenced it.

## v1.4.5

*Four things a task could not do.* `ready()` in v1.4.4 let a loop that was already running check on a task without blocking. These are the rest of what a program needs before it can be built on tasks rather than around them.

*`time.after(secs)` is a deadline you can wait on.* A future that finishes with nil when it expires, which is what lets a timeout be an ordinary member of a list of things to wait for rather than a parameter on the waiting. One timer thread serves every deadline: a task that sleeps holds a pool worker without announcing itself as blocked, so a redraw loop arming a 16ms timer every frame would fill the pool with sleepers.

*`wait(futures)` blocks until one of them is settled, and answers which.* `ready()` suits a loop that was already running and burns a core in one that was not; this is what a program with nothing else to do wants.

```jade
let which = wait([wifi, input, time.after(0.016)])
```

It answers the index and consumes nothing, so the caller then awaits the one that is ready — which costs nothing, because it is. That keeps the resolve-once rule intact and composes with `ready()` rather than duplicating it. The lowest ready index when several are, so a program that puts its timeout last sees real work first.

*`f.cancel()` stops waiting for a task, and `cancelled()` is how a task agrees to stop.* Cancelling does not stop the work, and saying so plainly is the design rather than a shortcoming: a compiled task is a real thread running straight-line code with no point at which the runtime could interrupt it, so a cancel that claimed to kill the task would be a promise only one engine could keep. What it changes is the caller's side — `await` raises at once instead of blocking, and one already blocked wakes and raises. Cancelled outranks a result that arrived anyway.

A task that wants to give up early checks `cancelled()`, which is the only thing that actually stops work. Both engines agree down to the iteration a polite worker stops on, and a task that never looks runs to completion on both.

*`join(..., settle = true)` reports what every task did.* Plain `join` raises the first failure and throws away the ones that worked, which is the wrong answer for a fan-out: eight requests with one failure should still hand back the seven that succeeded. `settle` gives one dict per task instead, `{ok: true, value: v}` or `{ok: false, error: e}`, so nothing is lost and nothing raises. `error` holds the value the task raised rather than its text, so a struct arrives as a struct.

It is a mode rather than data — written `true` or `false`, because it changes the shape of what comes back and a caller that cannot tell which it is getting cannot use either. And it covers what the *tasks* did: a member that is not a future still raises, since calling `join` wrongly is not an outcome.

*Fixed on the way: `time.sleep(0)` crashed a compiled binary* with a null dereference. Codegen unboxed a float straight from the argument word, and an int literal is not a boxed float. `math` never had the problem because its cores take the tagged word and coerce; sleep and after do that now.

Still missing, and the next thing worth building: a channel, so tasks can stream to each other instead of resolving once. It fits the concurrency model, where values move rather than being shared.

## v1.4.4

*You can ask a task whether it has finished, without waiting for it.* `await` blocks, which is the right default and also the whole problem for anything with a loop it cannot stop. A screen redrawing at frame rate has no point at which it is willing to freeze, so it could start work concurrently and then never find out the answer.

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

`ready()` is a plain bool and the only thing you can ask a future that does not block. The `await` after a true one costs nothing — 7µs compiled, 39µs interpreted, on a task that has already finished — which is what makes the shape work rather than merely read well. Without that, a poll would move the block rather than remove it.

Finished means finished however it ended. A task that raised is finished, and the `await` re-raises; a future whose result has already been taken is finished, and the second `await` reports the double await on its own terms. `ready()` hides neither, so a caller that wants to know which can `try` around the `await`.

One thing it is not: a way to wait cheaply. `while !f.ready() { }` burns a core. Inside a loop that was already running it costs nothing extra, which is the case it was built for. A program with nothing else to do wants to block until *any* of several futures resolves, with a timeout, and that primitive does not exist yet.

*A nested loop no longer leaks what it reads.* Reading a value declared outside an inner loop leaked it, once per pass of the loop around it:

```jade
while i < 1000 {
    let a = [1, 2, 3]
    let seen = 0
    while seen < 3 { let x = a[0]  seen = seen + 1 }
    i = i + 1
}
```

left 1000 live objects at exit. A read takes a reference for the destination, and the store that follows releases whatever the destination held — except that the release is skipped when the destination already holds that very object. That skip is right for an in-place write-back, which hands back the object the slot already has and takes no reference, and wrong for a read, whose reference is then unbalanced. A single pass balanced by luck, because the next store released it, which is why it took a nested loop to show and went unnoticed until `while !f.ready()` made it easy to write.

Older than the async work — it reproduces on every build that has the current refcounting.

*A method checks its receiver, even when the work behind it would not.* `x.len()` on a number printed `-1` compiled where the interpreter raised, because the method spelling handed the value to the length dispatcher and tagged its "no length" sentinel as an int. The function spelling, `len(x)`, was right all along.

`d.contains(k)` was worse: it answered `true`. The method reused the `in` operator, which does take a dict, but `contains` is not a dict method — the interpreter raises and points at `has`. A wrong answer rather than a wrong message.

## v1.4.3

*Async, one layer at a time.* v1.4.2 stopped a deeply nested `await` from aborting a compiled binary, and left it hanging instead. That is the worse failure of the two, and it needed a design decision rather than a patch, because the ceiling it hit was not the real problem.

*An `await` chain can now be as deep as the call stack allows.* `await` blocks a whole OS thread, so a chain of N nested awaits pins N threads. Every pool has a last thread: past it the innermost task had nobody left to run it and every level above waited for it forever. Raising the ceiling moves the wall without removing it.

So an awaiting thread runs the task itself. A future carries its own body until somebody claims it, and a thread about to park on one takes it and calls it inline instead. The depth then costs stack rather than threads, and it spends the same budget ordinary recursion spends: an inline body keeps counting against the recursion limit, so the chain is bounded by the number that already bounds every other call chain rather than by the pool. Past it a binary says "recursion limit exceeded" where the interpreter, whose tasks live on the heap rather than the stack, keeps going. That is a real difference between the two and a loud one; before this, the binary hung at 512 and said nothing.

Tasks that do not wait on each other still run side by side. Running one inline is what a thread does instead of idling, not a replacement for the pool. The pool's ceiling is now a cap on parallelism alone and can no longer stop a program, and a nested fan-out gives the same answer at `JADE_MAX_TASKS=1` as at 512, where it used to deadlock.

*Nested `try` is no longer capped at 64.* The compiled handler stack was a fixed 64-slot array, so a recursive function with a `try` in it reported "exception stack overflow" for a program `jade run` finished. The interpreter's handlers belong to the call frame and have no ceiling. This one grows now.

*A caught raise closes the generator buffers it skipped past.* A `yield`ing function opens a buffer when it starts and hands it back when it returns, and a raise in between never reaches the return:

```jade
fn inner()  { yield 10
              raise Stop { message: "no more" } }

fn outer()  { yield 1
              try { inner() } catch Stop e { }
              yield 2 }

print(outer())        // [1, 2] interpreted, [10, 2] compiled
```

`outer`'s second yield went into the buffer `inner` abandoned, and `outer` handed back the one on top. Whoever stops a raise is the one that knows how far to unwind, so the catch now truncates the generator stack the same way it already truncates the handler stack and the recursion counter. A generator that catches its own raise still keeps its own buffer.

*A task no longer inherits the thread it runs on.* Three things leaked across that boundary, invisibly while each task had a thread to itself and visibly once a bounded pool started reusing workers. A generator that raised inside a task left its half-filled buffer behind, and the next `yield` on that thread landed in it. A `try` left open by a `return` did the same to the handler stack. And the recursion budget carried over, so a long chain of awaits tripped a limit the interpreter never trips. A task is a fresh call chain, as it already was in the interpreter.

*A task gets the stack the main body gets.* Pool workers ran on the 2 MB Rust hands a thread by default, so the same function succeeded at top level and overflowed inside an `async fn`, taking the process down with no Jade error to read. They get 256 MB now, matching both `jade run` and the compiled main body.

*An `async fn` is a value now, everywhere.* Holding one refused to compile at all, and `map` over one ran it inline:

```jade
async fn double(n) { return n * 2 }

let f = double
await f(21)                       // build error: spawn of a non-static function
[1, 2].map(double)                // ran inline, gave [2, 4] rather than futures
```

Whether a call starts a task was decided at the call site, from the static type of what it was calling. A value carries no static type, so a function reached through a local, a collection, `map`, or an import was an ordinary call. The function carries the fact itself now, and every call reads it. Storing one in an array or a dict, returning one, passing one to `map`, and calling one with a default parameter all work on both engines, and a wrong argument count raises where it used to refuse to build.

The same gap is why `await lib.work(3)` never worked. The type checker cannot see inside an imported module — its names only exist once the importer merges the two, which is long after checking — so an imported `async fn` was called synchronously and the `await` failed with "applied to a non-Future value". That was true of both engines, and needed nothing at the call site to change.

*Three ways a task could reach the spawner's data with nothing to say so.* Jade refuses a program whose task mutates state the spawner can still reach, and three shapes walked straight past that check.

A callback. `async fn run(f) { f() }` calls a parameter, and a parameter resolves to no definition, so `let record = || readings.push(3)` reached the task completely unexamined. It is still an ordinary value at the spawn, where its body is as visible as the task's, and that is where it is caught now.

A function value read back from a global. A closure is bound under the name of the *variable*, not of any definition, so looking it up by definition name found nothing. What the global was last assigned answers it instead.

A user method. `arr.push(x)` was rejected and `batch.grow()` was not, because a method reaches its receiver as `self` rather than as an argument — so an `extend` method mutating the receiver had an empty argument list and looked harmless. The receiver counts now, the same way it does for the built-in methods.

*And the spawner is now held to the same rule as the task.* Every check in this pass asked what a task does. None asked what the spawner does *while the task is running*, which is the same race from the other side:

```jade
let limit = 2
async fn read() { return limit }
let f = read()
limit = 10                        // ← refused here
print(await f)
```

The two engines did not even agree on what that means: the interpreter gives each task a snapshot of the globals and answers 2, a compiled binary shares one cell and answers 10. Refusing the program is the answer rather than picking one. The same goes for mutating a collection a running task was handed. The window opens at the spawn and closes at the await, so both writes are fine once the task has finished.

*Two more leaks on the task paths.* A future nobody awaited never released its result, so a fire-and-forget task returning a collection leaked one object per spawn: 5,000 of them left 5,001 live objects, and now leave 2. And a `join` whose task raised abandoned the results it had already collected, because the rethrow is a jump and the caller's collect-into-an-array loop never runs; those are released before the jump now.

*A compiled binary's output streams, and does not tear.* Two things `jade run` got right that `jade build` did not, both invisible at a terminal and both serious for a service.

stdio picks full buffering for a stream that is not a terminal, so a binary whose output went to a file or a supervisor produced nothing until 4 KB had piled up, and lost whatever was still buffered when it was told to stop. The interpreter never had this, because Rust line-buffers stdout whether or not it is a terminal. A binary does now too.

And a print was several writes with nothing between them, so concurrent tasks spliced their text together: four tasks printing 200 lines each produced 56 corrupt lines out of 800. A print is one write under one lock now, and the same run produces none.

*`join` reports a second failure instead of escaping the `try`.* This one is the interpreter's. `join` dispatches a caught error by popping a handler and jumping, and the jump was written inside the loop over the tasks — where it bound to that loop instead. So the first failure popped a handler and quietly carried on, and the second found the handler stack empty and escaped the enclosing `try` altogether:

```jade
try { join(bad(1), bad(2), good(3)) } catch E1 e { print(e.message) }
```

reported an unhandled exception rather than running the catch arm. Every task is awaited first now and the first failure is reported once, at the end, which is what the compiled backend already did and what `join` is documented to do.

*A double `await` is a `RuntimeError` compiled, as it already was interpreted.* Awaiting a future twice, or awaiting something that is not a future, raised a bare string from a binary, so `catch e` bound a string and `catch RuntimeError e` did not match at all. A program that handled it interpreted died on it compiled.

*`join` no longer leaks its results.* Collecting a task's result into the joined array took a reference for the array without giving back the one the join handed over, so every result that lived on the heap leaked one object. 10,000 iterations of `join(mk(i))` left 10,001 live objects where the same loop written with `await` left 1, and a service that joined in its request loop grew without bound: 400,000 joins peaked at 80 MB, and now hold flat at 3 MB.

*Running out of threads no longer wedges the process.* When the OS refused a worker thread, the pool tried to undo its own bookkeeping while still holding the lock it needed to do it, and a `spawn` deadlocked against itself with nothing to wake it. The success path hid the bug completely, since the new thread is what runs the code that would have re-locked, so it only ever fired when the machine was out of threads and the recovery mattered most.

A binary now finishes correctly even when *every* thread creation fails: the work runs on whichever thread awaits it, so a machine out of threads does its tasks one after another instead of hanging. The pool reports how often that happened rather than counting it silently.

## v1.4.2

*Seven ways a compiled binary could crash or quietly disagree with the interpreter, all fixed.* Every one of them ran correctly under `jade run` and misbehaved under `jade build`, which is the class of bug `src/scripts/backend-parity.sh` exists to catch and none of these were caught, because no fixture happened to combine the right pieces.

*A caught raise no longer undoes the writes that led to it.* This is the big one, because ordinary code reaches it:

```jade
fn f() {
    let a = 0
    try {
        a = 1
        raise "stop"
    } catch e {
    }
    return a          // 1 interpreted, 0 compiled
}
```

A compiled binary returns to a handler with `longjmp`, which restores the callee-saved registers to what they held when the matching `setjmp` ran. Any local the optimizer had kept in one of those registers reverted. When the local held a heap value, the reverted word named a value it no longer owned, and releasing it a second time was a double free — a `for` loop with a `try` in the body was enough to abort the process. Slots in a function containing a `try` now stay in memory, which is the same rule C states as needing `volatile` on a local modified between `setjmp` and `longjmp`. Nothing else pays for it, and a `try` in a loop measures the same as before.

*A method used as a value keeps its receiver alive.* `let read = thing.read` holds the thing it will pass as `self`, but it took no reference to it, so a binding that outlived the frame that made it read freed memory:

```jade
fn make(n) {
    let t = Tag { id: n }
    return t.read
}
print(make(42)())     // 42 interpreted, a crash compiled
```

Bound methods are now reference counted like anything else that owns a value. They used to leak as well, one per binding plus its receiver; a loop making 200,000 of them went from 54 MB to 2.4 MB.

*A task keeps its arguments alive.* Calling an async function starts it and hands back a future, so the task usually outlives the frame that spawned it. It was borrowing its arguments from that frame, and reading them after the frame released them.

*Values are checked before they are dereferenced.* Inference is not always precise enough to be trusted with a pointer — a function whose branches return different types takes the first branch's type — so the compiled code now checks the word it actually has, the way the interpreter always did. These were all crashes and are now the interpreter's own error message: calling something that is not a function, `{true: "y"}`, `{"a": 1}.slice(0, 1)`, `[1].decode()`, and arithmetic on a value whose inferred type was wrong.

*Releasing a deeply nested structure no longer overflows the stack.* The destructor walked children by recursing; a chain of arrays about 30,000 deep, which a loop builds by wrapping one array each time, ran out of stack. It walks a worklist now, so depth costs heap instead.

*Two error messages now match the interpreter.* A dict names both a key and a method when neither is there, since `d.name` is a key lookup first.

*Calling a function value works properly.* A call site that jumps at a value does not know which function the value holds, so it cannot know how many parameters that function has. A compiled binary used to guess from the arguments it had:

```jade
fn scale(n, by = 10) { return n * by }
fn apply(f) { return f(3) }

print(apply(scale))    // 30 interpreted; garbage compiled, `by` never filled
```

Extra arguments were dropped, missing ones were read out of uninitialised memory, and a default was never filled. Every callable now carries an entry that knows its own parameter list and does the checking and filling there, so the wrong argument count raises the interpreter's message instead of returning a wrong answer.

*A core builtin is a value.* `let count = len`, `xs.map(str)`, and passing `print` to a function all work; the name used to read as nil, so printing it said `nil` and calling it crashed. Callable values also name themselves the way the interpreter does — `<fn>`, `<builtin len>`, `<type str>`, `<bound method>` — rather than `<object>`. A bound method can now be handed to `map`, to `filter`, and to a C package as a callback.

`func` and `input` still have no compiled form. Both now decline the build for reading the name as well as for calling it, rather than one erroring and the other quietly yielding nil.

*A keyword argument no longer blanks the defaults you did not name.* This one was the interpreter's, not the compiler's:

```jade
fn tag(name, prefix = "<", suffix = ">") { return prefix + name + suffix }

print(tag("x", suffix = "]"))    // "<x]" compiled; a type error interpreted
```

Naming any argument made every parameter you did not name arrive as `nil`, defaults included. A parameter with no default and no argument now says which one is missing instead of arriving as `nil` too.

*Grammar values print as `<grammar>`* in a compiled binary rather than `<object>`.

*A method call checks the receiver.* Resolving `obj.go()` picked the one `go` in the program by name and argument count, which says nothing about what `obj` actually is — bytecode carries no types. So a receiver of another type ran the wrong method and answered with a number computed from another type's fields, and one whose type had no such method jumped through a null pointer:

```jade
struct A { n }
extend A { fn go(self) { return self.n * 10 } }
struct B { n }

fn call(o) { return o.go() }
print(call(B { n: 2 }))    // an error interpreted; 20 compiled
```

The call site now checks the receiver's type first and dispatches on the real one when it does not match. A data field holding a function wins over a method of the same name, which is the order the interpreter uses, and a struct method named like a built-in one no longer takes `[1, 2].contains(x)` with it.

*Assigning to a parameter no longer frees the caller's value.* A parameter slot borrowed its argument, which held until the callee wrote to it: the overwrite released a reference the frame did not own, so two calls with the same array freed it twice.

```jade
fn f(s) { s = 0 }
let a = [1, 2]
f(a)
f(a)                       // aborted here
```

*A compiled program gets the stack the interpreter gets.* `jade run` executes on a thread given 256 MB; a binary ran on the 8 MB a process starts with. Printing an array nested 2,000 deep was enough to segfault one and print fine from the other. A binary now runs its body on a thread of its own, sized to match.

*Values are checked in three more places.* `len` of something with no length raises rather than answering with whatever sat in the object header — `len(some_fn)` was 1 and `len(5)` was 0. `array.map` and `array.filter` reject a first argument that is not an array instead of walking a string as one, and `filter` requires its predicate to answer a bool rather than reading a tag bit as the truth value and quietly dropping every element. `map` also stopped leaking one object per element.

*Anything can be a condition.* `if x` read bit 4 of the word, which is the answer only when `x` really is a bool; a bound method took the false branch while `bool(x)` said true.

*`print(x, end)` works compiled.* The second argument replaces the newline. Fixing `print` at one argument made a compiled call raise where the interpreter printed.

*`math.floor` of a value too large to be an integer raises* instead of aborting. A non-finite float saturates to a whole number no tagged word can hold; the same goes for `ceil`, `round` and `trunc`.

*Awaiting deeply no longer aborts.* The task pool computed its runnable population by subtracting blocked threads from its own workers, which underflows once a thread that is not one of its workers blocks — the main thread awaiting at the top level is exactly that.

*A shadowed builtin reads as the builtin until it is assigned.* `print(len)` before `let len = 5` printed `nil` compiled where the interpreter still had the builtin.

*A caught raise no longer leaks the value it raised.* A `longjmp` runs nothing on the way out, so a frame it skips never releases what its registers hold — and for the frame that raised, one of those is the raised value itself:

```jade
fn c() { raise E { message: "x" } }
let i = 0
while i < 1000 { try { c() } catch E e { }  i = i + 1 }
```

leaked one object a pass, and 5,000 of them left 5,001 live objects at exit. Not an async bug and not a new one; the interpreter never had it, because its frames are Rust values that unwinding drops.

The raiser is the one frame that knows it is leaving, so it now cleans up before it goes, exactly as a `return` does. That also settles who owns a thrown value: the throw takes a reference of its own, and the `catch` binding takes that reference rather than a second one. A raise caught in the same frame is left alone, since that frame is staying.

A frame merely *unwound past* is covered too: `fn b() { let junk = [1, 2, 3]  c() }` sitting between the raise and the catch used to drop `junk` on the floor. Every frame now leaves behind a copy of what its registers hold, and the throw releases the copies belonging to the frames it erases.

The copy is a separate array from the registers, which is the whole trick. Handing the runtime the registers themselves is the obvious design and it costs 41% on `bench/extreme.jde`, because a register file whose address escapes cannot be promoted out of memory and every register access turns into a load. A copy escapes instead, the registers stay in machine registers, and it is written only where a store has already worked out that a heap value is involved — so arithmetic pays nothing for it.

Nor does anything outside a `try`. A frame can only be skipped by a handler that already existed when the frame was entered, since a handler installed deeper cannot unwind past something shallower and a raise with no handler ends the process. Registering is gated on that, which is what takes the remaining cost to 6% on a benchmark that is nothing but calls, and to nothing measurable on the three that resemble real programs.

*Two leaks on the `yield` path, found by the same measurement.* `yield x` took two references where the value needed one, because the call site retained and `jrt_karr_push` retained again, so `yield [1, 2]` leaked its array every pass. And a `yield`ing function's `return` retained the buffer it was handing back — right for a value read out of a register, wrong for one the generator already owns — so every call to a generator leaked its stream.

Known and not fixed: an *uncaught* error still prints `jade: <message>` from a binary against `<file>: runtime error: [line:col] <message>` from the interpreter, and caught errors agree apart from that same `[line:col]` prefix on `.message`. A stdlib module function or a primitive method used as a *value* (`let f = math.sqrt`, `let f = s.upper`) still has no compiled form. `await` nests about 512 deep in a binary before the task pool runs out of threads, where the interpreter keeps going. And a callback that mutates the array it is mapping over sees its own writes compiled, where the interpreter walks a snapshot.

## v1.4.1

*A struct literal can copy another struct.* Write `...base` first inside the braces and every field the type declares that you did not name is read from it.

```jade
struct Config { host, let port = 80, let tls = false, let retries = 3 }

let base = Config { host: "a", port: 1234, tls: true }
let staging = Config { ...base, host: "b" }   // port 1234, tls true, retries 3
```

A field you name beats the base, and the base beats that field's declared default. The second half is the part that matters: filling `port` with its default of `80` because the literal did not mention it would reset a copy to the defaults for everything it left out, which is the opposite of copying. A field the base does not carry does fall back to the default, so copying across two related types works.

The base is an expression, evaluated exactly once no matter how many fields come across. One per literal, and it comes first, since the fields after it are the ones overriding it.

Inside a method, `...self` is how a struct returns a changed version of itself:

```jade
extend Counter {
    fn bump(self) { return Counter { ...self, count: self.count + 1 } }
}
```

The copy runs when the program does, rather than being expanded while it compiles, and that is not an implementation detail. `self` has no static type, so nothing earlier in the pipeline knows what to copy. Neither does a struct that inherits a parent from another file: it does not have its own full field list until the engine merges the import, which each engine does at a different moment. Both know by the time the instruction runs.

When the base's type is known, every field is settled while the program is checked: the checker sees which fields the base supplies, folds the declared default into the rest, and still refuses a copy whose base cannot supply a required field. So any default expression works, not only the few literal shapes an engine can rebuild while it runs, and a default is still evaluated only where it is needed — `S { ...a }` does not call a `let id = nid()` default that `a` already answered.

*Fixed, and found by building the above: a compiled binary could not make a non-scalar field default.* `let tags = []` and `let seen = {}` were only ever materialized while a program was checked, folded into every struct literal before either backend saw one, so the gap never showed. A copy skips that fold, since a default must not overwrite the base, and asking the compiled side to produce one at run time turned up a table that held scalars only: the field came out missing and reading it ended the program, where `jade run` gave `[]`. Both engines build a fresh collection now, one per struct rather than one shared between them.

The values are copied, not the objects behind them, so a copy and its base share any array a field holds. The task-safety check knows: reaching a shared collection through a copy is refused the same way reaching it directly is.

*Fixed while wiring that up: putting a shared collection into a struct field hid it from the task check.* A struct literal counted as an allocation with nothing shared inside it, which is true of the object and not of what it holds, so `Box { items: shared }` inside a task compiled clean and `b.items.push(3)` reached the spawner's array on both engines. A field takes a value, and a collection put into one is the same collection; it is treated that way now, whether it is named or copied from a base. Older than this release, and worth knowing if a program that compiled before now names a data race.

## v1.4.0

*A struct can inherit.* Name parents in parentheses after the struct's own name, and it takes their fields, their defaults, and their `extend` methods as if it had written them itself.

```jade
struct Animal {
    name,
    let legs = 4
}

struct Dog(Animal, Trainable) {
    breed
}

Dog { name: "rex", breed: "corgi" }.legs   // 4
```

You may name as many parents as you like, inheritance is transitive, and a parent may live in another file: write it qualified, `struct Dog(creatures.Animal)`, the way you write any imported type.

*A field name may appear once across a struct and everything it inherits.* Two parents declaring the same field is an error, and so is a child redeclaring one it already has. Two fields with one name would mean two storage slots and nothing to say which a literal meant.

*Methods go the other way, because there is something to decide between them.* A child's own method overrides the one it would have inherited, and the nearest declaration wins, so a grandchild beats its parent, which beats its grandparent. Two *parents* supplying one method name is still an error: they are the same distance away, so neither is nearer.

*A typed `catch` arm follows inheritance.* `catch Failure e` catches anything inheriting `Failure`, so one arm handles a family of errors and adding a member does not mean editing every caller. It does not run in reverse, and arms are still tried in written order.

There is no `super`, so a child's method cannot call the version it replaced.

### Three things were removed

Each for a reason that stands on its own, and all three are refused by name rather than dropped in silence.

*`interface` is gone.* It was a compile-time conformance check and nothing else. `extend Point: Displayable` verified that Point declared every method Displayable listed, and then no part of the runtime ever heard about it. Method calls already resolved by name on the value, so the same polymorphic loop ran identically with the interface deleted. It could not annotate a parameter, a field, or a return either, so it was not a type. A shared parent covers what it was reached for and means something at run time. A plain `extend Type { … }` is untouched.

*A decorator on a `struct` is gone.* It ran in the VM as a post-construction hook and was never lowered by the compiled backend at all, so `@bump struct Point` gave one answer under `jade run` and another under `jade build`, silently, with no error either way. No fixture covered a decorated struct, which is why the parity gate stayed green over it. There is no replacement hook: call the function where you build the value. Decorators on `let`, `prompt`, `fn` and `async fn` are untouched, since those are rewrites rather than runtime hooks.

*`@route` is gone*, for the identical reason. It reached only the VM, the compiled backend had no lowering for it, and no example exercised it. Removing struct decorators over that and keeping `@route` would not have held up.

### Notes

Inheritance resolves in the type checker, which is what keeps it out of the rest of the toolchain: a child's field list is already complete by the time bytecode is emitted, so `MakeStruct`, field access, and both backends go on treating a struct as a flat list of fields. The one thing either engine learns at run time is a struct's ancestry, and only a typed `catch` arm reads it.

A parent from another file is folded in later, because each file is checked on its own and an imported struct is deliberately out of reach at that point. The compiled backend folds when it inlines every module into one stream; the VM folds when the import lands, since it has no earlier moment. Writing the cross-file fixture turned up a real divergence between the two, which is what fixtures are for.

Inheritance does not work across REPL snippets: the REPL re-runs type inference from a fresh context each time and does not carry struct definitions between them.

## v1.3.27

*A program can build a `bytes` value now, and write into one.* Until this release the only ways to get a blob were `str.encode()` and reading one off a disk or a socket. Neither builds an arbitrary buffer: a Jade string is UTF-8 and NUL-terminated, so a zero byte truncates it and any value above 127 encodes as two octets rather than one. Everything downstream already worked, including the FFI shim's pointer-and-length mapping, so a Jade program could receive a pixel buffer from a C library and never make one.

`std::bytes` is the new package, with three functions:

```jade
use std::bytes

let buf = bytes.zeros(1024)
let mask = bytes.from_ints([255, 128, 0, 255])
let atlas = bytes.concat(buf, mask)
```

Writing one octet is spelled `b[i] = v`, the same way an array works, and the value is an int from 0 to 255. Construction is a package rather than three more methods because a constructor has no receiver, and because the method surface is three on purpose. See [`std/bytes`](stdlib#stdbytes).

*A blob is now reference-semantic.* Two names for one buffer see the same write, and a function that writes into its argument changes what the caller still holds. That is how an array already behaves and it is what makes a buffer useful, but it is a change to an existing type: a blob taken out of a dict or an array is shared with what it came from rather than copied. `slice` still copies.

*Two data races in the concurrency checker are closed.* Both are older than this release, and both got much easier to reach now that a buffer is the obvious thing to write into. `SetIndex` was reading the wrong taint set, so `async fn f(arr) { arr[0] = 9 }` compiled clean while `arr.push(9)` beside it was correctly rejected. `SetIndexGlobal` had no check at all, so a task writing into a global collection was never refused even though rebinding that same global was. And a write to a global collection reached a caller only when a shared value was passed with it, so the same write behind a zero-argument helper was invisible. All three are now rejected at compile time. A program that was relying on any of them will stop compiling, which is the point.

A task may still write into a buffer it allocated itself. The three `std::bytes` constructors return storage nothing else points at, so the checker treats them the way it already treats an array literal. It recognises them by the module they came from rather than by the spelling, so a user file named `bytes.jde` exporting its own `concat` gets no exemption.

Two shapes that used to compile no longer do, both inside a task and neither of them about blobs in particular: rebinding a parameter to a fresh collection and then writing into it, and writing into a blob that came from `str.encode()` or `b.slice()`. Both really are freshly allocated, and the checker cannot prove it, so it refuses rather than guess. Joining to an empty blob with `bytes.concat` gives you one it can prove. Nothing changes outside a task.

*Fixed: a caught built-in error was a different type on each engine.* `JadeError::Exception` means a `raise` the program wrote, and the VM answers one by handing the catch block the value that was raised, which a built-in never sets. `bytes.decode()` was raising it, so catching an invalid-UTF-8 failure bound the bare string `"unknown exception"` under `jade run` and a `RuntimeError` struct under `jade build`. Reading `e.message` worked on one engine and failed on the other. It now raises what every other built-in raises, and a test pins the rule for the whole type.

*Also:* `s.encode().len()` types as `int` rather than `unknown`. The table of `bytes` method signatures had been filled in from the start and nothing ever read it. And `bytes.concat` refuses a result past the 4,294,967,295 octet limit `bytes.zeros` already refused, since the object header holds a length in 32 bits and the two engines read that length from different places.

## v1.3.26

*Rewrote every page of this site, and every `README.md` in the repository, for clarity.* The content is the same. What changed is how it reads.

The target is a 10th-grade reading level: sentences of 15 to 20 words on average, one idea per sentence, active voice, and the simplest verb that works. Punctuation is now limited to periods, commas, apostrophes, colons, and semicolons, so a long sentence held together by an em dash became two or three sentences instead. Emphasis is italics rather than bold. Every code block, table, heading, and link is unchanged, and so is every technical claim.

*The repository `README.md` gained the two things it was missing.* It now opens with a reading order, naming which directory README to read first and in what sequence, because every directory in the repo has one and nothing said where to start. And its build-from-source section is now complete: both the debug and release builds, the extra Debian packages, what `LLVM_SYS_180_PREFIX` is for, and why the checkout has to stay after building, since `jade build` links two runtime archives out of `target/`.

Three stale facts in that README were corrected along the way. Its pipeline diagram named `compiler/vm.rs` and `build/mod.rs`, which are now `vm/` and `aot/`. Its feature checklist repeated the same stale path. And it claimed CI runs no formatting or clippy gate, when CI runs `cargo clippy --all-targets`, `cargo fmt --all --check`, and `jade fmt --check examples`.

*One real defect fixed.* `docs/plugins/llms-txt.js` never listed the packages page in its `ORDER` array, so `llms.txt` appended it after the changelog instead of placing it between imports and exceptions. It is now in sidebar order.

Past changelog entries are left as they were written. A shipped release is a record of what shipped, and rewriting one changes what a version is documented to have contained.

## v1.3.25

**Fixed: a native package crashed the VM at exit on Linux.** A program that called into a C dependency printed every one of its answers correctly and then died with SIGSEGV during shutdown. It only happened under `jade run`, and only on Linux.

`NativeLibFn` keeps a library mapped while any of its functions are alive, which is the right rule for a call and the wrong one for a process. When the last binding dropped at shutdown, `dlclose` unmapped the library — while a thread that had not finished exiting still had that library's thread-local destructors queued against it. glibc runs those from `__nptl_deallocate_tsd` as the thread winds down, jumping into an address no longer mapped. glib registers such a destructor, which is why the FFI gate's fixture hit it.

A loaded package is now kept until the process exits. The compiled runtime never had the bug because it never unloads — there is no `dlclose` anywhere in `runtime_aot/native.c` — so this is the VM adopting the rule the other engine already followed. Nothing is given up: Jade has no API to unload a package, so an image released at shutdown could not be re-loaded by anything.

**Fixed: the FFI gate reported `ok` on a segfault.** A process killed by a signal *after* it has printed everything leaves an output file that looks perfectly correct, and both engines then agree about it because both printed the same correct thing. The gate's glib step asserted only that the VM's output was non-empty and free of the word "error", never its exit status — so a SIGSEGV was reported as a pass from v1.3.19 to v1.3.24, visible in every CI log as a `Segmentation fault` line the *shell* printed, directly above `4 ok, 0 failed`. Every run in that step now keeps its status, and a crash fails the gate.

What was crashing was the fixture, not the toolchain. It called `g_intern_static_string`, which does not copy: glib's global intern table keeps *the caller's pointer*, and its documentation says the string must never be freed. Jade owns the buffer it passes into a native call and frees it afterwards, so the table was left holding a dangling pointer into reused memory and glib faulted walking it at exit. It surfaced only on Linux and only under the VM — compiled, the literal sits in read-only static data and satisfies the contract by accident. The fixture now calls `g_intern_string`, which copies. Worth knowing when binding a library by hand: a binding has no way to say "the callee keeps this pointer forever", so a function with that contract is one Jade's argument ownership cannot satisfy.

## v1.3.24

**Fixed: a mistyped FFI symbol reached run time.** `gfx.jade_gfx_key_presed` passed `jade check`, built, linked, packaged and shipped, then failed the first time that line executed with "dict has no key or method". It was not a link error, because nothing links the name — the generated shim binds the symbols in the manifest's table and no others, so a name that is not in it is simply absent when the runtime looks it up.

The project's own `jade.toml` had the answer the whole time. An `abi = "c"` dependency must declare a `[symbols]` table, and that table is the complete list of what the shim binds. `jade check` and `jade build` now check every call against it:

```
main.jde: [4:7] 'gfx' has no symbol 'jade_gfx_key_press' — did you mean 'jade_gfx_key_pressed'?
```

When nothing is close enough to name, the message points at the manifest rather than guessing, since a confidently wrong suggestion is worse than none. `from gfx use <name>` is checked too, and reported at the import line.

The check lives where `alias.field` becomes a native reference, so it inherits that rewrite's scope rules for free: a local named `gfx` shadows the import and is not checked, exactly as it is not rewritten. Two things switch it off, both because there is nothing to check against rather than because checking would be inconvenient — a package with no declared table (a Jade-ABI package declares its exports in its own project, which this manifest cannot see, and an empty set would reject every call it has ever served), and a `[lib]` shadowing a dependency of the same name, whose table then describes a library the build is not using.

**Why `check` was silent when the machinery was already there.** The rewrite that catches this runs inside the build probe `jade check` performs — and that probe discarded *every* error import resolution produced, on the reasoning that an unresolved import means an uninstalled dependency, which `check_imports` already reports in better words. True for that case, and it swallowed this one with it. Resolution failures now carry which kind they are: an unresolved import is still dropped, a wrong program is reported.

**Added: a monotonic clock and UTC calendar conversion in `std::time`.** The package could read the wall clock and sleep, and that was all — so a program that wanted to know what day a timestamp fell on had to shell out to `date` and parse the text back.

**`time.monotonic()`** returns seconds from a fixed point in the process, as a float. It is the clock to measure a duration with, and `time.now_ms()` is not: the wall clock moves while a program runs, since NTP corrects it and a person can set it, so subtracting two readings of it can hand you a negative duration. The monotonic clock only moves forward. Its absolute value means nothing on its own.

**`time.parts(ts)`** breaks a timestamp into a dict of eight UTC fields — `year`, `month`, `day`, `hour`, `minute`, `second`, `weekday`, `yearday`. `weekday` is 0 for Sunday and `yearday` starts at 1, matching what `date +%w` and `date +%j` print, so nothing here needs a second numbering learned.

**`time.stamp(y, mo, d[, h[, mi[, s]]])`** is the exact inverse, and the time-of-day arguments default to zero. Out-of-range fields **carry** rather than fail, which is what turns date arithmetic into a single call: month 13 is next January, day 0 is the last day of the previous month, and `time.stamp(2026, 8, 16 + 45)` is September 30th.

**`time.utc(ts)`** formats a timestamp as ISO 8601 — `2026-08-16T14:03:22Z`. Fixed width and sortable as text, which is why it is here alongside `time.local`, whose human format is neither. Unlike `local` it is *trusted*: it is computed from an integer in process rather than read back from a subprocess.

All three calendar functions are UTC, deliberately. A local calendar needs the IANA timezone database, which is a dependency the runtime does not carry; `time.local(tz)` remains the local-time answer, and it gives a formatted string rather than fields.

The conversion is Howard Hinnant's `civil_from_days` — integer arithmetic on the proleptic Gregorian calendar, with no dependency and no `libc` call. It is tested against dates checked with `date -u`, over a round trip across four centuries on both sides of the epoch, and on the three leap-year cases that separate a correct implementation from a plausible one: 2024 has a February 29th, 2100 does not, and 2000 does.

`time.sleep` was already present and is unchanged.

## v1.3.23

**Added: 42 functions across `std::math`, `std::string`, `std::fs` and `std::array`.**

**`std::math`** gains `round`, `trunc`, `sign`, `clamp`; `ln`, `log2`, `log10`, `exp`; `sin`, `cos`, `tan`, `asin`, `acos`, `atan`, `atan2`, `hypot`; `is_nan`, `is_inf`; and the constants `pi`, `e`, `tau`, `inf`, `nan`. Two rules worth knowing without running the code: `round` breaks ties away from zero, so `round(2.5)` is 3, and `clamp` on a reversed range answers `hi` rather than aborting the way the standard library's does. The constants are spelled as calls — `math.pi()` — because a package namespace is only ever read as a field a call immediately consumes. `inf` and `nan` are reachable no other way at all, since the lexer caps a numeric literal.

**`std::string`** gains `trim_start`, `trim_end`, `capitalize`, `is_empty`, `index_of`, `last_index_of`, `count`, `repeat`, `slice`, `pad_start`, `pad_end` and `lines` — each in both spellings, so `s.index_of(x)` and `string.index_of(s, x)` are the same function.

- **Every index is a character index**, the same unit `len()` counts and `s[i]` walks. `"café!".index_of("!")` is 4. Rust's own `find` answers a byte offset, so the obvious implementation would have agreed with `len` on ASCII and disagreed on everything else.
- **`lines` is not `split("\n")`.** A trailing newline yields no empty final element, which matters because a file read off disk almost always ends in one.

**`std::fs`** gains `is_file`, `is_dir`, `size`, `copy`, `rename` and `rmdir`. `list_dir` gave you names with no way to tell a file from a directory, and there was no way to ask how big something was or to move it. `is_file` and `is_dir` answer questions, so an absent path is `false` rather than an error, matching `fs.exists`. `rmdir` removes an empty directory only — deliberately not recursive, since `fs.delete` is non-recursive on files and a recursive delete behind a five-character name is a foot-gun.

**`std::array`** gains `join`, in both spellings. Non-string elements render the way `print` renders them, so `[1, 2].join("-")` is `"1-2"`. It lives on `std::array` rather than `std::string` because a package function's first argument is the type the package is named for.

**Added: a test that stops half of a stdlib function from shipping.** Nothing tied a package's function table to the compiled backend's lowering, and a module call the backend declines is a hard build error rather than a fallback — so a function present in one and absent from the other is a program `jade run` accepts and `jade build` refuses. A dozen sat in that state until 1.3.21. The new test walks every package and names each function with no lowering; it went red for all 23 math functions before their arms were written, which is how we know it works.

**Fixed: two lines of `stdlib.md` that described the code wrongly.** `string.replace` replaces every occurrence, not the first, and `math.pow` returns an int when both operands are ints and the exponent is non-negative.

## v1.3.22

**Fixed: dicts were quadratic to build and linear to look up.** A dict is a value in Jade — assigning one, passing one to a function, or reading one out of another gives you an independent copy — and paying for that was costing far more than the rule itself. Three separate things, each O(n) where it should have been O(1). A dict now behaves like the hash map it always looked like: **O(n) to build, O(1) to look up.**

| | before | after |
|---|---|---|
| build a dict of 8,000 keys (`jade run`) | ~9s | 0.04s |
| build a dict of 8,000 keys (compiled) | ~5.4s | 0.21s |
| pass a 400-key dict to a function 20,000 times | 8.1s | 0.15s |
| 200,000 lookups, dict of 8,000 keys (compiled) | grew with the dict | flat |

- **Lookup was a linear scan.** Entries lived in one vector and every `get` and `set` walked it, so a lookup was O(n) and building a dict was O(n²). It is a compact hash map now: the same insertion-ordered vector of entries, plus an open-addressed table from a key's hash to its position. Small dicts skip the table entirely, because below a handful of entries scanning a contiguous vector wins and most dicts really are that size.
- **Reading a dict deep-copied it.** The interpreter took "value" literally, so `GetGlobal`, `SetGlobal` and every argument copied every entry. A dict now sits behind a copy-on-write handle: sharing it is a refcount bump, and the copy happens only when something writes through a handle another holder can see — exactly when a copy could be observed.
- **Every write copied, even with nobody else holding the dict.** Both engines copied on each `d[k] = v` to stay safe against an alias that usually was not there. Compiled code now asks the refcount first and writes in place when it is the sole owner. And index-assignment takes ownership of its variable for the duration — a local is handed to `SetIndex` directly, and a global goes through the new `SetIndexGlobal` — so the binding is not itself the second holder that forces the copy.

**None of the semantics moved.** A dict is still a value, an array and a struct are still references, and `examples/collections/container_semantics/` pins all three on both engines.

**Changed: the docs now cover the two shapes that actually catch people out.** The value-versus-reference rule was written down, but only for plain assignment.

- **Passing a dict to a function that writes to it.** The caller does not see the write, while the same code on an array or a struct does. There is a table of all three containers now, and the answer when a function has to hand a change back: use a struct, which is the shape the language is built for.
- **Reading a dict out of a dict.** That is an assignment too, so it copies, and writing to what you read back does not reach the outer dict.

## v1.3.21

**Fixed: a stdlib function you could call one way but not the other.** Most collection functions have two spellings — `string.upper(s)` and `s.upper()` are the same function — and several combinations did not work. `array.map(a, f)` ran and compiled, but `a.map(f)` existed on neither engine. `string.upper(s)`, `dict.keys(d)`, `array.sort(a)` and a dozen more ran under `jade run` and were refused by `jade build` as an *unsupported module call*, though nothing was missing: the symbol each one needs is the symbol its method spelling was already calling. Both spellings of every one of them now work on both engines.

- **`map` and `filter` are methods now.** They were the only array functions without a method spelling, because they run a Jade function per element and the method path could reach pure builtins only. `nums.filter(is_odd).map(double)` reads the way people expect it to.
- **One pair stays deliberately different, and now says so.** `std/array`'s package functions are the functional style: `array.sort(a)` answers with a sorted copy and leaves `a` alone, while `a.sort()` sorts in place. Compiled code had neither, and lowering the two together would have made a built binary mutate an array the interpreter does not touch.

**Fixed: error messages that named the wrong type, or no type at all.** Three of them, all reached by an ordinary typo.

- **A missing function on a stdlib module named neither the module nor the function's real home.** `math.round(2.5)` reported `struct 'dict' has no field 'round'` — a package is a dict at run time, and that leaked. It is refused at compile time now, on both engines: `std::math has no function 'round'`, followed by the list of what it does provide. There is no registry to go and read, so the list is the answer.
- **A missing method said "struct" whatever it was called on.** `a.map(f)` reported `struct 'array' has no field 'map'`, which is wrong three times over: an array is not a struct, it has no fields, and `map` is a method. It reads `array has no method 'map'` now, `dict has no key or method 'k'` for a dict, and still `struct 'Point' has no field 'z'` for a real struct. Which one applies is carried rather than guessed from the name, because `struct array {}` is a legal declaration.
- **The compiled runtime had its own copy of that wording**, so the fix landed twice. The parity fixture that asserts the two engines word it identically is what caught it.

## v1.3.20

**Fixed: `exit()` compiled clean, did nothing, and left the program running.** Jade has no `exit`. Calling one anyway was accepted, so `jade build` printed `built: ./prog` for a program that could not work — and what happened next pointed everywhere except the cause. The call was silently skipped, so a top-level `exit(main())` looked like `main` had returned and execution had carried on, which reads like a `return` failing to leave a loop rather than a name that was never there.

- **A single import used to turn off the undefined-variable check for the whole file.** Any `use` at all, on the theory that an import might be what defines the name. A stdlib import defines nothing of the sort — `use std::env` contributes `env` and nothing else — so the check now stays on for one, and every undefined name in such a file is a compile error again, not just `exit`.
- **Where leniency is still needed, the backend catches it instead.** A user module's top-level names really are invisible to type inference, and so are the bare names a `from … use` binds. By the time code generation runs every import is inlined, so reading a global that nothing anywhere in the program binds is a build error there, naming the line. It used to lower to a read of a nil cell, which the runtime then called — a binary that built cleanly and died with no message at all.
- **The error says what to write instead.** An uncaught `raise` at the top level exits 1, and that is the whole exit-code mechanism the language has.
- **The two engines agree again.** `jade run` has always raised `undefined variable` for these programs; only `jade build` accepted them.

## v1.3.19

**Changed: code generation moved to its own place in the source tree.** The half of `jade build` that translates bytecode into LLVM IR used to sit inside the half that resolves imports and runs the linker, as `src/aot/lower/`. Those are not related that way — lowering is where the interpreter and the compiled path have to agree on what an opcode means, so it is a peer of the VM rather than a detail of linking. It is now `src/codegen/`, one level flatter and named for what it produces.

- **A handful of build errors stopped naming `lower.rs`**, a file that no longer exists. They carry a `codegen:` prefix instead, continuing the cleanup 1.3.16 started.
- **Nothing about a compiled program changed.** Every example that emits IR emits byte-identical IR, and the backend parity gate is unmoved at 86 ok, 10 skipped, 0 failed.

## v1.3.18

**Fixed: a dozen `ar` warnings on every macOS build, including the release job.** `cc` does not know in advance whether the archiver takes `-D` (deterministic mode), so it probes: it runs `ar cqD`, and when that fails it retries with `ZERO_AR_DATE=1 ar cq`. Apple's `ar` rejects the flag, so the archive was always built correctly — but the failed probe printed its usage text into the build log twelve lines at a time, which buries anything worth reading.

- **The runtime archive is built with `llvm-ar`**, which takes the flag, so the probe succeeds first time and there is nothing to print. Not a new dependency: `jade build` links LLVM 18 in-process, so `LLVM_SYS_180_PREFIX` already has to point at an install for the crate to compile. Where it does not resolve, or `AR` is set, `cc` is left to probe as before rather than being handed a path that is not there.

## v1.3.17

**Fixed: an f-string could double-free, aborting a compiled binary inside the allocator.** A regression from 1.3.16, reported from the userland as `malloc(): unaligned tcache chunk detected` on glibc — and reproducible on macOS too, as a silent abort. `f"{x}"` has no literal text, so nothing is concatenated and the template produces the value's own string; rendering handed back that very pointer, so the destination became a second owner of one allocation. Inert while strings were never freed, a double free once they were.

- **The cause was an ownership contract that depended on the value's type.** `jrt_str_of_any` allocated for an int and borrowed for a string, so a fold over parts of both kinds had to get one of them wrong — and did, both ways: the borrowed case double-freed, and the allocated case leaked one string per interpolation. It always copies now, which is what `runtime.h` has always said it does, and trust travels with the copy.
- **The shape is an example now**, so the parity gate runs it on both engines every time. One round trip cannot tell a shared pointer from an owned one, so it runs in a loop.

**Fixed: a method called with the wrong number of arguments was reported as a method that does not exist.** `"abc".upper(1, 2, 3)` answered *no method named `upper`* — and `upper` plainly does exist, it takes no arguments. The predicate the compiler asks returns false both for a name no type defines and for a real method called wrongly, and 1.3.16 read every false as the first. It asks the arity table now, so the two mistakes are told apart: `` `upper` takes 0 arguments, but 3 were given ``.

- **A macOS system library can be bound.** 1.3.16 recognised a `.tbd` and explained why it could not be used; it can be used now. A stub is not Mach-O and never will be, but it is exactly what a linker wants — linking the shim against it records the real library as the shim's own dependency, which dyld resolves from the shared cache at load time. Nothing is copied and nothing is opened by hand. `jade pkg add zlib --path <sdk>/usr/lib/libz.tbd --header <sdk>/usr/include/zlib.h` binds and runs on both engines. The macOS install-name fixup skips a stub, because the name it carries is precisely the one the shim should keep asking for.
- **A build error names its line.** `[7:3] no method named \`nosuchmethod\`` — the same position the interpreter reports. The chunk has always carried a span per instruction; it never reached the resolver, so a large file had nothing to search for. Threading it *removed* a parameter rather than adding one, since `lower_body` was taking a chunk's `code` and `fn_defs` separately and can take the chunk.
- **Fixed: `-D`/`--define` was accepted and then ignored.** 1.3.16 added the flag, validated its shape, and never passed it to anything — so a header raising `#error` without `PCRE2_CODE_UNIT_WIDTH` still would not parse, having accepted the flag that exists to get past it. It now reaches clang when the header is read, is recorded in `jade.toml` beside `include_dirs`, and reaches `cc` when the shim is built — which matters, because the shim includes the same header and would otherwise hit the same `#error` one stage later. A fresh clone re-binds with the macros it was bound with.
- **`jade check` looks at calls now.** It ran the frontend and `emit` and stopped, so an unknown method, the wrong arity, and a surplus argument to a builtin all reported `ok` and then failed to build — the wrong way round for the command whose job is to predict the build. It runs the same throwaway-module probe `jade build` already runs before touching the real one, including the import inlining that precedes it, so a file with imports is not reported as an unsupported opcode. It costs nothing measurable: `check` and a full `build` are both about 25 ms on this repo's examples, dominated by process startup.
- **Four more build errors stopped naming `lower.rs`.** Calling a function with too many arguments, omitting one that has no default, and the two `spawn` equivalents all reported an internal Rust source file for what is an ordinary mistake in the program. The omitted-argument case now names *which* argument and what it is called — `this call omits argument 2 (\`b\`), which has no default.`

## v1.3.16

**Twenty bugs the userland found, mostly bindings that looked like they worked.** JADE OS keeps a list of what it has hit in the toolchain; this is most of it. The common shape is a binding `jade pkg add` wrote by itself that ran, returned a plausible value, and exited 0 — with the wrong answer.

**Binding a real library.**

- **A versioned ELF symbol is matched by its plain name.** A library built with a version script exports `lzma_version_number@@XZ_5.0`, and the header's plain name was compared against that string verbatim. 56 of an ordinary Linux image's libraries would not bind at all — `libc`, `libcrypto`, `libcurl`, `libxml2`, `libsystemd` among them — and 14 more, zlib included, bound *successfully* while silently dropping exactly their versioned half. Names are cut at the first `@` now, whatever the format.
- **A leading underscore is Mach-O's, and only Mach-O's.** Applying it to ELF broke GMP's entire public API (`__gmpz_*`) in one direction, and in the other bound a symbol that does not exist — a clean `add` with nothing skipped, and an `undefined symbol` only when the program ran. The object format is read from the file's magic bytes rather than assumed from the host.
- **A macOS SDK `.tbd` is recognised as a linker stub** rather than reported as a corrupt file, and names the library it stands for. Loading one is still not supported: the real library lives in the dyld shared cache with no file on disk, and Jade materializes every dependency as a file.

**Inferences that were wrong, and silent about it.**

- **A lone `void *` is a deallocator, whatever it returns.** The guard existed and required the return to be `void`, so `int cap_free(void *)` — the shape most free functions actually have — was bound as a buffer the call revises in place. The shim allocated a copy, handed it over to be freed, read it back, and freed it again: four valgrind errors, from a program that printed its result and exited 0.
- **`inout_struct:<Type>` exists**, and a struct parameter's direction is no longer guessed in silence. `const` binds as an input outright; the rest still default to `out_struct` but say so, naming both alternatives. It needed saying — read as out-only the parameter takes no argument at all, so libusb's `struct timeval` became a zero timeout that busy-spun while reporting success, and libsodium's streaming SHA-256 digested a state that had never been initialised.
- **A `void *` beside a callback is a context slot, so it is filled.** Two callbacks registered through one symbol now stay apart instead of both resolving to whichever was passed last.
- **A returned string is copied before the wrapper frees anything.** `uriEscapeA` returns a pointer *into* the caller's own out-buffer, which the header cannot disclose — handing that back read freed memory.
- **`nil` marshals to `NULL`**, so `g_uri_escape_string(s, NULL, TRUE)` can be written at all, and a failed tag check names the parameter and what was expected.

**The two engines agree about three more programs.** Float division by zero raised under `jade run` and returned `inf` compiled, so a guard written against the interpreter was absent from the binary that ships; both raise now, catchably. A call chain past ~700 frames aborted the interpreter outright — uncatchable, with no Jade file or line — while the compiled engine ran to 10,000; both enforce one limit now, raised as an ordinary catchable error. And a C `int64_t` outside Jade's 63-bit range came back correct from `jade run` and **± 2^63** from the compiled binary; both refuse it at the boundary rather than one truncating.

**Messages that were wrong rather than merely thin.** A malformed `jade.toml` was reported as a missing one, and `jade init` then refused to create the file it had just been told did not exist. A misspelled method compiled to `lower.rs: method call (GetField result) is unsupported`, which named an internal Rust file and read as "Jade cannot compile method calls". The "supported types" list omitted six types the generator itself writes. A refusal claimed it had recorded the header when it had recorded nothing.

**A compiled program reclaims its strings.** It never did — a string was left out of reference counting on the grounds that it is a leaf and cannot form a cycle, so a loop building one grew without bound whether or not any FFI was involved, and an FFI call returning a string leaked Jade's own copy of it on every call. A retain-nothing loop of two million iterations held 162 MB; it holds 1 MB now, and `leaks` reports none at all where it reported 40,001. Codegen has always emitted balanced retain and release for every heap word including strings, so honouring them in the runtime was the whole of it — the count lives in four bytes of the string header that were already padding, and a literal carries an immortal marker because it sits in read-only memory where a count could not be written.

**Smaller things.** `jade pkg add` grew `--only`, and both `add` and `bind` grew `-D`/`--define`, without which a header wanting `-DPCRE2_CODE_UNIT_WIDTH=8` could not be read. A bound header's own directory no longer goes on the include path, where `netlink/errno.h` shadowed the shim's own `<errno.h>` and made the compiler advise an include that was already there. The lock now covers the artifact that actually loads, and `jade pkg install` compares it rather than regenerating it. The interpreter no longer accepts surplus arguments to a builtin method. And the generated shim source no longer ships in the bundle that goes to the device.

## v1.3.15

**Fixed: a hand-written binding with no header declared C functions at Jade's widths, not the library's.** With no header the shim writes its own `extern` for each bound symbol, and `int` there became `int64_t`, `float` became `double`, and `bool` became `uint8_t`. Someone binding `g_uri_escape_string` by hand got `extern char* g_uri_escape_string(const char*, const char*, int64_t)` against glib's real third parameter, a 32-bit `gboolean`. Nothing caught it: the manifest is valid, the shim compiles, and the program runs.

- **The return is the dangerous half.** Passing a value that is too wide usually survives, because the callee reads only the part it wants. Reading one is the reverse — the shim believes its own declaration and reads eight bytes where the function wrote four, so the upper half is whatever was left in the register. A `float` declared as a `double` is worse still and worse everywhere: the two are different representations, so the answer is not slightly wrong but meaningless, on every machine rather than on unlucky ones.
- **`scalar:<ctype>` names the library's own C type** — `scalar:int`, `scalar:size_t`, `scalar:float`. The shim declares that type and converts to and from Jade's width at the boundary. Your side of the call does not change: an argument is still an ordinary Jade int, float or bool, and the conversion is the shim's job, which is what a shim is for. Spelled to match `out_scalar:<ctype>` and `inout_scalar:<ctype>`, and resolved through the same table, so the three cannot come to mean different things.
- **`int`, `float` and `bool` are now refused in a headerless binding**, in `args` and in `ret`, and the message leads with `jade pkg bind <name> --header <path>` — a header answers this for every symbol at once and cannot be got wrong. It then offers the explicit spelling with the symbol's own arguments already filled in, so what is left to supply is exactly what Jade could not work out.
- **Only those three.** Every other type in the vocabulary crosses as an address, and an address is one width. An `out_scalar`, an `out_buffer` and a `callback` were never exposed to this, because each already carries the library's own C type.
- **Nothing changes when a header is present.** The header's prototype governs, so `int` there is only a marshalling tag and is correct as it stands.
- **The two hints that tell you what to write now agree with what is accepted.** `jade pkg add` and the "no signature yet" error both showed `args = ["int", "int"]` as the shape to replace a `"?"` with — and a `"?"` means no header was read, so following the hint landed on a second error.
- **`fails_when = "negative"` is refused on a return the library declares unsigned.** Naming the C type makes the combination reachable for the first time, and `(r) < 0` on an unsigned type compiles to `false` — the symbol would bind, run, and hand every failure back as an ordinary result. Plain `char` is refused for the same test, because its signedness is the platform's choice and the test would fire on x86 Linux and not on ARM macOS.

**Fixed: a compiled binary that called into C in a loop ran out of stack and died.** Every FFI call took a few bytes of scratch space to lay its arguments out in and never gave them back — that space is only reclaimed when the surrounding function returns, and a loop does not return until it has finished. One argument cost 16 bytes and three cost 32, so an 8 MB stack ran out after 524,288 calls or 262,144 of them. The count was fixed and the callee was irrelevant: a binding that returns an int and allocates nothing on the C side died in exactly the same place as one returning a freshly allocated string.

- **The same mistake was in four other places, and `spawn` and `join` are the two that matter as much.** A loop that spawned tasks died the same way, with nothing to do with the FFI at all. All five now take their buffer once in the function's entry block and reuse it, which is what a `try` handler's `jmp_buf` has always done — the rule was written down and these five missed it.
- **None of the five sizes was ever dynamic.** Every one is the length of a list known while lowering, so a static buffer serves; `llvm.stacksave` and a heap fallback were both considered and neither was needed.
- **`join` keeps two buffers rather than sharing one**, because `jade_join_words` reads one while writing the other. Sharing everywhere else is sound because a buffer is filled from register slots with nothing in between, so two sites cannot interleave in one frame.

**Fixed: every FFI call in a compiled binary leaked 48 bytes.** Naming a bound function built a small heap object standing for the function itself, separate from calling it, on the assumption that the optimiser would delete it once the call resolved to a direct one. It does not: the value is stored into the register file, so it is dead to Jade and alive to LLVM. Nothing could free it either, since the object deliberately carries `ObjKind::Fn` where a reference count would go, so the reference counter steps over it by design. The leak was that decision's unpaid cost.

- **It is a constant now, not an allocation.** A native function value depends only on its package and its name, so it lives in read-only data as a pair of globals per binding and is never built at runtime. A loop of 1,600,000 calls holds exactly what a loop of 100,000 holds, and a compiled FFI program now ends with one fewer outstanding block than a program that makes no FFI call at all.
- **The environment holds the address of the package cell rather than the handle**, which is what makes the whole thing a link-time constant with nothing to initialise — and therefore nothing to race on when two threads evaluate the same reference.
- **`jade run` never had this**, so it is compiled binaries only.

**The FFI gate calls a binding 600,000 times and checks what it holds.** `glib-fixture.jde` calls each binding once, which proves the answer is right and nothing else. Both defects above only appear over many calls, and one call cannot tell a correct release from a leak. The new step fails if the binary does not survive the loop, and fails again if peak memory tracks the number of calls; it is what turned up the stack exhaustion.

**Two fixes to `alloc_str` itself, from reading the code that shipped in v1.3.14.**

- **A copy that could not be made now says so.** The shim reported a bare failure with no message attached, so a caller was told the call "returned a non-zero status" when the truth was that it ran out of memory.
- **The buffer a copied string lands in is released when its thread exits.** The comment claimed this already happened. It did not — a `_Thread_local` pointer has no destructor — so every worker that retired took its buffer with it, and workers retire after ten seconds idle for as long as a program runs. Now held through a `pthread_key` destructor, which cut 3,200 threads' worth from 511 MB to 5 MB.

## v1.3.14

**A string a C library allocated for you can now be bound, and it is released rather than leaked.** This was the largest gap left in the FFI: 125 of glib's symbols come back as a `gchar *` — `g_strdup`, `g_uri_escape_string`, `g_find_program_in_path` — and none of them could be bound at all. `curl_easy_escape` is the same shape.

- **`ret = "alloc_str"` is the new spelling**, and like `out_alloc_str` it requires `frees_with` naming the library's own free function. The shim copies the string out and hands the original straight back to that function, so the answer is right and nothing accumulates. Measured on 200,000 calls returning an 80-byte string: 62 MB held before, 42 MB after, which is exactly the figure for a call that allocates nothing.
- **Jade asks rather than guesses, because the header does not say.** `g_basename` points into its argument and `g_strdup` mallocs, and both are written `gchar *`. Reading one as the other either leaks on every call or frees a static string, so a non-const `char *` return is refused with both spellings named — the message says what to write.
- **Writing `ret = "str"` for one of these was the only way to reach it before**, and it leaked the allocation on every call. That is the defect this release fixes.
- **A copied string is no longer truncated.** The buffer it lands in was a fixed 4096 bytes and silently cut anything longer, which affected `out_alloc_str` as well. It grows to fit now, and is reused by the next call on that thread rather than held.
- **`frees_with` does not have to name a bound symbol.** It usually cannot: a call taking a lone `void *` and reporting nothing is refused as a binding, since that is the shape of a call that frees what it is given — which is precisely `g_free`. Without a header the shim declares it itself.
- **The FFI gate runs one end to end on both engines.** Two glib symbols are declared by hand and reinstalled, which is the other half of the workflow the refusal message describes.

## v1.3.13

**glib binds and runs, and CI binds it on every push.** Pointed at glib — 1890 exported symbols, written the way widely-used C libraries actually are — the binding generator produced a table of 1357 symbols that could not be used at all. Two separate faults, each of which refuses the whole dependency rather than the symbol.

- **Fixed: a callback's context slot was checked against the typedef's name, not its category.** glib spells every one of them `gconstpointer`, so a `void*` that the trampoline should accept and not forward was read as a type the FFI cannot carry. Every glib callback was unbindable.
- **Fixed: a function-like macro could intercept the call.** glib declares `g_atomic_pointer_add` and then defines a macro of the same name whose `_Static_assert` rejects the pointer the shim holds. The macro won and the shim would not compile. Calls are parenthesised now, so the symbol the artifact exports is the one that gets called — which is the one that was bound.

**`src/scripts/ffi-gate.sh` runs in CI, and it is why the two above were found.** The parity gate covers the language; this covers the part of the toolchain whose correctness depends on someone else's header, someone else's macros, and a C compiler's opinion of what we generate from them.

- **It compiles the C runtime the way a release build does**, with `-O2 -D_FORTIFY_SOURCE=3` and the warning refused. That is the check that would have caught the `realpath` abort above in seconds, rather than after two releases. It only bites on glibc, since Apple's headers carry no such attribute — so for this one step the Linux run is the one that counts.
- **It binds glib whole and runs a program on both engines.** The whole header, never a narrowed slice: a slice would cover only the shapes already handled, which is the opposite of the point. Missing glib or a missing C compiler is a reported skip, so the script is safe to run anywhere.

**Fixed: every FFI package aborted at startup in a compiled binary on Linux.** `*** buffer overflow detected ***`, before the program's first line ran. `realpath` on glibc writes up to `PATH_MAX` bytes into the buffer it is handed, and the fortified build aborts the process when that buffer is smaller — whatever the path being resolved turns out to be. The runtime handed it 1024 bytes, which is exactly `PATH_MAX` on macOS and a quarter of it on Linux.

- **Two things kept it hidden.** The check only runs in optimised builds, so a debug toolchain never tripped it, and the C runtime is compiled with warnings off — glibc says so at compile time, in as many words: *second argument of realpath must be either NULL or at least PATH_MAX bytes long buffer*.
- **Every buffer holding a path is `PATH_MAX` now**, from one definition in `runtime.h` with a 4096 fallback for systems that set no limit.
- **A failure names the whole path.** The error messages carrying paths were 512 and 900 bytes, so the sentence explaining *where* the runtime looked could be cut off mid-path — which is the one part worth reading. The single join that can still overflow says so, rather than truncating into a "not found" that sends you after the wrong thing.
- **Swept the whole C runtime** with `-O2 -D_FORTIFY_SOURCE=3 -Wall` on glibc. This was the only abort-class hazard, and no truncation warnings remain.

## v1.3.12

**Fixed: every FFI package aborted at startup in a compiled binary on Linux.** `*** buffer overflow detected ***`, before the program's first line ran. `realpath` on glibc writes up to `PATH_MAX` bytes into the buffer it is handed, and the fortified build aborts the process when that buffer is smaller — whatever the path being resolved turns out to be. The runtime handed it 1024 bytes, which is exactly `PATH_MAX` on macOS and a quarter of it on Linux.

- **Two things kept it hidden.** The check only runs in optimised builds, so a debug toolchain never tripped it, and the C runtime is compiled with warnings off — glibc says so at compile time, in as many words: *second argument of realpath must be either NULL or at least PATH_MAX bytes long buffer*.
- **Every buffer holding a path is `PATH_MAX` now**, from one definition in `runtime.h` with a 4096 fallback for systems that set no limit.
- **A failure names the whole path.** The error messages carrying paths were 512 and 900 bytes, so the sentence explaining *where* the runtime looked could be cut off mid-path — which is the one part worth reading. The single join that can still overflow says so, rather than truncating into a "not found" that sends you after the wrong thing.
- **Swept the whole C runtime** with `-O2 -D_FORTIFY_SOURCE=3 -Wall` on glibc. This was the only abort-class hazard, and no truncation warnings remain.

**Fixed: malformed JSON did nothing in a compiled program.** `json.parse` raises under `jade run` and answered `nil` under `jade build`, so a compiled binary took the success branch on input the interpreter rejected and every `try`/`catch` written around a parse stopped running. Nothing warned; the value was simply nil and the program carried on with it.

- **The wording matches too**, down to serde's own complaint: `I/O error: json.parse: EOF while parsing a string at line 1 column 5`. Only the `[line:col]` prefix still differs, which a compiled binary cannot carry and which every other raise already omits.
- **A good parse after a bad one is still good.** The failure travels on a channel the raising forwarder drains, and a stale one left there would be raised by whichever call came next — a valid parse reporting an error about input it never saw.
- **The provider config keeps the old behaviour on purpose.** It is read where no Jade frame exists to catch anything, so a malformed one leaves the provider unconfigured rather than aborting.
- **`examples/json/parse/` covers it now.** No example parsed invalid JSON, which is why the parity gate never saw this.

## v1.3.11

**Fixed: the Linux build had been broken since v1.3.9, so nothing shipped.** `jade_image_dir` asks the loader which image it is running in, using `dladdr` — which glibc hides unless `_GNU_SOURCE` is defined, while macOS declares it unconditionally. So every build on a developer's Mac passed and every build in CI failed, at the first step, inside the C compiler. v1.3.9 and v1.3.10 were merged and never tagged; their contents ship here.

**A library that splits its API across headers is now bound whole.** `ares.h` declares seventy-odd symbols itself and includes `ares_dns_record.h`, which declares sixty-three more — the entire modern DNS record API, and none of it was ever looked at. The rule was all-or-nothing: a header declaring nothing of its own was an umbrella and bound everything it included that the library exports, and a header declaring anything bound only its own. Plenty of libraries do both.

The export table decides for every header now, not only umbrellas. It is an exact test rather than a guess about which paths count as system ones — `fopen` rides in on every header and belongs to nobody, so it is not bound. The named header's own declarations are kept either way, so no library loses a symbol.

- **c-ares goes from 57 bound symbols to 114, of the 136 it exports and declares.** A Jade program now takes a real DNS answer, parses it with `ares_dns_parse`, and walks the records: `A for example.com`, on both engines.
- **zstd reaches all 68.** Across the seven libraries measured it is 410 of 444, from 352.

**A C library can keep a Jade function and call it back later.** Every async C API works this way — register now, called when the answer arrives — and none of them worked: the whole mechanism was scoped to a single call, so `ares_search` registered a callback, returned, and it never fired. Nothing errored. A real DNS query through c-ares now delivers its answer to a Jade function, on both engines.

- **Three things were per-call and are now per-VM:** the channel the callback posts on, which used to close when the registering call returned; the Jade function itself, which used to be freed then, leaving the library holding a dead pointer; and the shim's registration slot, which was cleared on return *and* thread-local — the sharper half, since each native call runs on its own worker thread.
- **Any call in flight can deliver a callback**, not only one that passes a function. `ares_process` passes none, and without that a callback fired from it would have found nobody listening.
- **A raise inside a stored callback surfaces from the call that pumped it**, since the call that registered is no longer the one that was running.
- **A callback fired with nothing in flight gets a neutral answer** rather than blocking forever. A library that calls back from a thread of its own is still unsupported, and this is where it says so.
- **A registration lasts until the program ends.** Nothing in C says when a library is finished with a stored callback, so there is no safe moment to release one. One small allocation per call that passes a function, not per invocation.
- **`callback_data` routes each registration through the library's own context slot**, so calling one symbol twice with different functions does not send both answers to the second. Where a library offers no such parameter there is one registration per symbol, and the binding report says so.
- **Fixed: a symbol taking two callbacks refused the whole library.** brotli's decoder has one, and the slot and trampoline were named for the symbol alone — so the shim defined the same C function twice and would not compile. They are named for where the callback sits in the symbol's own argument list now, and each is registered separately. `callback_data` is refused beside two, because the library hands the same context value to both and it cannot tell them apart.
- **A spawned task has its own registrations.** Sharing them would let one task run a callback against another task's variables — user code executing somewhere nobody chose, and invisible to the parity gate since both engines would do it.

## v1.3.10

**A bound library that cannot tell you anything is not bound.** capstone reported 19 of 20 symbols and was not a disassembler: `cs_insn` arrived as an id, an address and a size, because the mnemonic and operands are `char[32]` and `char[160]` and fixed-size arrays were dropped. A Jade program now disassembles x86 and prints `push rbp`, `mov rbp, rsp`, `ret`.

- **`char` crosses the FFI.** It is a first-class Jade type that could not travel in any position — not as an argument, not as a return, not in a struct. That is what made a `char[32]` field look like an encoding problem; it was not, an array of characters simply wanted characters. `RUNTIME_ABI_VERSION` moves to 5, so a package built against 4 is refused by name and must be rebuilt.
- **Fixed-size array fields are carried as a row**, with the element type deciding what is in it: plain `char` is characters, everything else numbers. `int reserved[4]` and `uint8_t bytes[24]` come along under the same rule rather than as special cases.
- **Nothing is trimmed.** A `char[32]` holding `push` arrives as thirty-two characters with the NUL padding intact, because trimming guesses where the text stops. `int(c)` and `char(n)` are new for exactly this, and `char(n)` refuses anything that is not a Unicode scalar rather than substituting a replacement character.
- **Writing a row back is bounded.** Longer than the field is an error naming it, not a silent truncation; shorter zero-fills, which is what an omitted field already does. A character that does not fit in a byte is refused — every byte is a character, not every character is a byte.
- **`<T>_at(handle, i)` reads one of a row of structs**, so a call that produces many is not limited to its first. The index is not checked against the count, which came back on the Jade side.
- **A handle written through `T**` of a struct the header defines is now readable.** It used to hand back a pointer nothing in the package could look inside unless some other function happened to take the same type by value.

- **Fixed: a count returned beside a handle was read as a status.** `size_t cs_disasm(…, cs_insn **insn)` returns how many instructions it wrote, so a successful disassembly of three raised. The discrimination is the C spelling — a status is an `int`, a count is a `size_t` — and both collapse to Jade's `int` before the old test saw them. Enums still read as statuses, which is right for `cs_err` and `lzma_ret`. `fails_when = "zero"` is spellable now, so a report that declines to guess can name something that works.
- **Fixed: a handle out-parameter no longer swallows the return unconditionally.** It does when a failure convention is testing it; otherwise the value comes back beside the handle.
- **Fixed: `fd_set` would have started arriving empty.** Making `int[4]` carryable stopped it being lossy, which turned it from a struct held by handle into an out-parameter — a zeroed local every call, so `ares_process` would have received nothing of what `ares_fds` filled and quietly done nothing. A struct holding nothing but rows is a buffer rather than a record, and stays held.
- **Fixed: c-ares had stopped installing entirely.** A callback parameter was checked after expanding its typedef and emitted before, so the generator produced a spelling the shim refuses — and the shim refuses the whole dependency. Expanding it instead is worse in a subtler way: an enum typedef is a distinct type for function-pointer compatibility, so the trampoline no longer matches what the library declares and the shim will not compile. The check now uses the type as written.
- **A callback signature may name a typedef.** The trampoline is declared with the spelling the library used, because a typedef expanded to its underlying type makes a function pointer C considers incompatible — and the shim then fails to compile, refusing the whole dependency. The category it marshals as is carried separately. Without this, every callback whose signature mentions a typedef was unbindable, which was most of c-ares.
- **A callback can deliver a blob.** A pointer beside a length arrives as one `bytes`, the same idiom an argument list already uses. `ares_callback` is how every DNS answer comes back, so c-ares could register a query and never see the result.
- **A callback now says it only lives for the call.** The Jade function is registered while the call runs and forgotten when it returns — right for a comparator or a visitor, wrong for a library that stores it and calls back later. Nothing in C tells the two apart, so the binding is generated and the report warns. c-ares reaches 57 bound symbols, and brotli's decoder 12.

- **Fixed: the compiled engine accepted invalid characters the interpreter refused.** A package returning a surrogate raised under `jade run` and produced a corrupt char under `jade build`. No example moved a char across the FFI, so the parity gate never ran the path; there is one now.

## v1.3.9

**A compiled program now runs somewhere other than the machine that built it.** `jade build` wrote the build machine's absolute path to every dependency into the artifact and loaded from there, so a binary worked in the directory it was produced in and nowhere else — and said so only at run time, on someone else's computer, naming a path that never existed there. The same was true of `jade build --lib`. Artifacts now name their dependencies by a path relative to a libraries directory, and `jade build` writes that directory beside the artifact. Move the pair and it works.

- **`-o` now produces a directory's worth of files when your program has dependencies.** `jade build main.jde -o dist/app` writes `dist/app` and `dist/libs/`. The `built:` line names what it wrote. A program with no dependencies is still a single file.
- **A dependency two packages share is loaded once, not once per package.** This is a correctness rule rather than an optimisation: a second copy has its own globals and runs its own module top level again, so for a library that owns a device or a graphics context two copies are two devices. One libraries directory is chosen by the program's host before anything loads, and every package resolves against that one — rather than each looking beside itself, which is what would produce two.
- **A dependency that cannot be found says where it looked and where that came from.** A bare loader error names the file and nothing else, which reads the same whether the bundle is incomplete or `JADE_LIBS` points somewhere wrong.
- **`JADE_LIBS` points a program at a different libraries directory, and always wins.** Nothing overwrites a value you set. That is the only way to give a single root to a process with no Jade program in it — a C or Python host that loads a Jade package. It also has to be right: a `JADE_LIBS` missing a dependency fails rather than falling back, because a fallback is a second directory and a second directory is a second copy.
- **A package now says what it depends on, and `jade pkg add` installs it.** A `jade build --lib` artifact carries the lock it was built against, so adding a package brings its dependencies with it instead of leaving you to read its documentation. They go into your `jade.toml` as ordinary entries, because a transitive dependency is a real dependency. When two packages name different versions of one dependency, the higher wins, which is Go's rule and the only one available with no registry to fetch a third from. It is always reported, because one of the two packages is then running against something other than what it asked for. Two versions are only ordered when both come from a URL and both are dotted numbers; anything else is refused, naming both. This is a choice between two, not version solving — solving needs ranges and a registry to search, and Jade has neither. Only a `url` dependency travels; a `path` names a file on the machine that built the package, so those are named for you to add yourself. Reading the record runs none of the package's code.
- **Fixed: the VM opened a dependency by whatever path resolution produced, without canonicalizing.** A symlinked `libs/` was two spellings of one file, and therefore two copies of it.

## v1.3.8

**Binding a C library reaches 91% of what a real library exports, up from 59%.** Measured across seven Homebrew libraries — liblzma, zstd, libfdt, capstone, c-ares and both halves of brotli — counting only what the header declares *and* the artifact exports. 348 of 381 symbols, from 223. A full lzma compress and decompress round trip now runs in Jade through nothing but the generated binding. Several of the old 223 were bindings that ran and did nothing; those are fixed too, and the notes below say which.

- **A struct the library only reads can be passed in.** `int f(const S* s)` was refused outright: the shim could fill a struct and hand it back, but not take one. `in_struct:<Type>` is the mirror — Jade builds the struct, the shim copies it into a real local of the library's own type and passes its address. Nothing owns anything across the boundary, because the library reads it and forgets it.
- **A field you leave out of one is zero, and a field you misspell is an error.** That is what the C it stands in for does: declare, zero, set what matters. `lzma_stream_flags` carries fifteen reserved fields the library requires to be zero, and demanding all seventeen would have made the shape unusable. The mistake worth catching is the other one — without the check a misspelling is indistinguishable from an omission, and silently becomes a zero you believed you had set.
- **A struct passed in must be one every field of which survives the trip.** The asymmetry with `out_struct`, which tolerates dropping a field it cannot carry: losing an output is visible in what comes back, losing an input is not.
- **A struct pointer beside an unrelated integer is no longer read as an array.** `cs_op_count(csh, const cs_insn *insn, unsigned op_type)` has the same shape as an array and its count, and was refused as one. The parameter's own name breaks the tie, and only a name with no count-like word in it decides — guessing the other way would hand a library one struct and tell it there were twenty.

- **A read-only blob with no length beside it can be passed.** Some libraries take a blob whose extent is written *inside* it: every `libfdt` call takes `const void *fdt` alone and reads the length out of the device tree's own header. There is nowhere to pass a size, so the shape was refused, and with it most of the library. `bytes_ptr` borrows the pointer for the call exactly as `bytes` does, without the count. libfdt goes from 51 bound symbols to 64. It is listed as an assumption, because Jade cannot check the extent and a truncated blob reads past the end.

- **A returned pointer whose length arrives through a parameter can be bound.** The mirror of `out_buffer`: there the return value is the count and the bytes go in through a parameter, here the bytes are the return value and the count comes back through one. `const void *fdt_getprop(const void *fdt, int off, const char *name, int *lenp)` is the main read call in libfdt and had no other spelling — you could walk a device tree but never read a property. Only recognised when the header *names* the parameter like a length, because nothing in the types tells `int *lenp` from the second value a call happens to write back.
- **A blob the library revises in place can be bound.** Every `libfdt` writer takes `void *fdt` and edits the device tree where it sits. A Jade blob is immutable, so `inout_bytes` copies the caller's bytes into scratch the shim owns, lets the library work on that, and hands the result back as a fresh blob. Your value is untouched, which is what immutable has to mean, and the edit is visible as a return rather than as a mutation nothing declared.
- **Which integer counts something is now decided by its name as well as its type.** A pointer followed by an int was read as a buffer and its length. Plenty are not: `fdt_getprop(const void *fdt, int nodeoffset, …)` takes a blob and a position, and `nodeoffset` is the single most common name to follow a byte pointer in these headers. The two mistakes are not symmetric — reading a real length as an ordinary argument costs nothing, since the int is still passed and you supply it, while reading an offset as a length *drops* it and hands the library a size it never computed.
- **libfdt goes from 51 bound symbols to 70, of 79 the library actually exports.**

- **A struct the caller allocates and the library keeps between calls can be bound.** The largest refusal in the set, and the reason liblzma sat last. `lzma_stream`, `ZSTD_outBuffer`, `fd_set`: these cannot be passed by value in either direction, because the shim would declare a fresh local every call and the pointers a codec keeps its position in would be dropped. So Jade holds one instead. `held = true` on a struct table makes the generator write `<T>_new`, `<T>_free`, `<T>_get` and `<T>_set` alongside the library's own symbols. The struct is allocated once on the C heap and every call gets the same pointer, so the fields that cannot travel stay exactly where the library put them.
- **A held struct's buffer fields can be filled.** Those uncarryable pointers are the point of the shape, so a held struct with no way to set them would be a handle you can make and never feed. The shim owns the memory they point at for the life of the handle, because the library expects it to still be there on the next call and a Jade blob makes no such promise. A read-only field is *set* from a blob you have; a writable one is *allocated* to a size and then *taken* from once the library has filled it. Two calls rather than one, because how much became real is something only you can work out — lzma counts down through `avail_out`, zstd counts up through `pos`, and no rule reads both.
- **A full lzma compress and decompress round trip now runs in Jade**, through nothing but the generated binding.
- **liblzma goes from 42 bound symbols to 95, capstone from 12 to 18, zstd from 60 to 65, c-ares from 39 to 50.**

- **A struct handed back by value can be bound.** `ZSTD_bounds ZSTD_cParam_getBounds(ZSTD_cParameter)` returns the struct itself, not a pointer to one. Nothing crosses the boundary but the value — it arrives in registers or on the stack, whichever the ABI says — so there is no allocation and no ownership to settle. It needs the header for the same reason the other struct shapes do: only the declaration settles how it arrives.
- **A write whose extent only the documentation gives can be bound, with you supplying the extent.** `lzma_stream_header_encode(const lzma_stream_flags *, uint8_t *out)` writes exactly twelve bytes and says so nowhere a generator can read. `sized_buffer:<ctype>` takes the count as its own argument, allocates that many, and hands the whole buffer back. Stating the size is what the C underneath required of you anyway, and the alternative was that eighteen symbols could not be called at all. It is listed as an assumption, and passing less than the library writes corrupts memory.
- **liblzma reaches 111 bound symbols of the 114 it exports, and libfdt 76 of 84.**

- **A callback's user-data slot no longer makes it unbindable.** C has no closures, so a callback that needs context takes a `void *` and the caller passes it back through whatever registered the callback. A Jade function already carries its own environment, so there is nothing to put there: the trampoline accepts the parameter, because the library will pass one, and does not forward it. The `void *` beside the callback in the outer call is the same slot, and is passed as null. Refusing the whole callback over the one parameter Jade has no use for made every c-ares callback unbindable.
- **`null_ptr` for a pointer that genuinely cannot be carried.** Brotli's allocator hooks hand back `void *`, which Jade cannot produce, and passing null for them is what tells brotli to fall back on `malloc` — which is what every example does. Never inferred, only written by hand: a library that *requires* a real pointer there gets a null dereference with no diagnostic, so the decision belongs to someone who has read the documentation. The refusal names the spelling.
- **A name pointed at inside your own data comes back as a string.** `fdt_getprop_by_offset(const void *fdt, int off, const char **namep, int *lenp)` points `namep` into the device tree it was handed, so nothing was allocated and nothing has to be released.
- **`out_alloc_str` and `frees_with` for a string the library allocates and you then own.** The C is identical to the borrowed case above and a header never says which it is, so that shape is refused and the spelling named. Guessing one way leaks on every call, and the other frees memory that was never allocated.

- **Fixed: a pointer named like a position is no longer read as a buffer.** `lzma_stream_buffer_decode(…, size_t *in_pos, size_t in_size, …)` has exactly the shape of a buffer and its count, and is a position beside an unrelated size. The binding allocated `in_size` of them and handed the library a pointer to scratch instead of to the position. The pointer's own name breaks the tie, the same way the count's name already did.
- **Fixed: a lone `void *` on a call that reports something is an in-place edit, not a deallocator.** `fdt_pack(void *fdt)` and three of its neighbours were caught by the rule meant for `ares_free_string`. Returning nothing is what marks a call that frees what it is given.
- **Fixed: a library symbol sharing a name with a shim helper is refused by name.** Every wrapper is `jade_shim_<symbol>`, and so is every helper the shim emits, so a library exporting `bytes` or `handle` defined one of them twice. The C compiler reports that against generated source, hundreds of lines from anything you wrote.
- **Fixed: expanding a typedef through a pointer dropped its `const`.** `const uint8_t *` became `unsigned char *`, so a read-only input buffer was read as a writable one — the shim allocated scratch and the caller's data never reached the library. The lookup key has `const` stripped by design, and the type was being rebuilt from the key.
- **Fixed: `void *fdt` was bound as scratch sized by a node offset.** Fourteen of libfdt's writers took the shape "writable pointer, then an int", which the generator read as a buffer and its element count. Calling one allocated `nodeoffset` bytes of uninitialised memory, handed it to the library as the device tree, and let it write there. They bind correctly now as in-place edits.
- **Fixed: two results in one call could refuse the whole dependency.** `fdt_overlay_apply(void *fdt, void *fdto)` hands back two blobs, and they reached the shim generator without the keys the header names them by. That is refused — correctly — but the shim generator refuses the *dependency*, not the symbol, so one such function made the library uninstallable.
- **Fixed: a writable byte pointer with no length beside it is no longer bound as a single value.** `lzma_stream_footer_encode(const lzma_stream_flags*, uint8_t *out)` writes exactly twelve bytes. The generator read `out` as one value written back, so the shim declared a one-byte local and handed the library its address — a stack overflow the C compiler cannot see, reported as a routine assumption. A byte pointer alone is a buffer whose size only the documentation gives, and it is refused by name now.

## v1.3.7

- **A scalar written through a pointer can be bound.** `int *count`, `uint64_t *progress` — C's way of returning a second value, and there was no spelling for it. `out_scalar:<ctype>` consumes no Jade argument and comes back as part of the result. libfdt goes from 41 bound symbols to 51.
- **A symbol may now have more than one out-parameter.** The rule was one, on the grounds that two would come back as a pair with no obvious names. They are not nameless: the header names them, and those names become the keys. `int divmod(int, int, int *quot, int *rem)` is called as `divmod(17, 5)` and hands back `.ret`, `.quot` and `.rem`. A header that does not name its parameters is skipped rather than given invented ones.
- **`inout_scalar:<ctype>` for the values a library reads before it writes.** A position the caller sets and the call advances is not an out-parameter — zeroing it is right once and wrong on the second call. Nothing in C tells the two apart, so the generator emits `out_scalar` and lists it as an assumption naming the fix, the same way it already handles the out-buffer guess.
- **The assumptions section is grouped by reason.** Every out-scalar carries the same caveat, so a library with thirty of them printed the sentence thirty times — which is how a section meant to be read teaches people to skip it.

- **Fixed: a struct the caller owns is no longer bound as an out-parameter.** `int f(S* s)` where the header defines `S` looks like one shape and is three. A struct the library *hands out* is a handle. A struct the caller allocates and the library keeps between calls — `lzma_stream`, `ZSTD_outBuffer`, `fd_set` — cannot be an out-parameter at all, because the shim declares a zeroed local every call: `lzma_easy_encoder` initialised a stream and threw it away, and `lzma_code` then ran against a different zeroed one. Twelve of liblzma's symbols compiled, installed, ran and did nothing; `ZSTD_compressStream` would have written through a NULL destination. Those are refused by name now, and a record one call fills is unaffected.
- **liblzma's reported coverage falls from 49 symbols to 36, and that is the fix.** Those thirteen were not working before, they were reporting success.

- **A header that includes its neighbours can be read.** libfdt and both halves of brotli could not be bound at all — `jade pkg bind` reported "clang could not parse" on headers that compile perfectly well. Two directories were missing, for two different kinds of include: `libfdt.h` does `#include <libfdt_env.h>` from its own directory, which an angled include does not search, and `brotli/encode.h` does `#include <brotli/port.h>`, which resolves against the directory above. libfdt goes from unreadable to 41 symbols bound.
- **The shim compile and the header read now get the same include directories.** They were computed separately, so `cc` was given the header's own directory and clang was not. That asymmetry was the bug above.
- **A symbol the header declares but the library does not export is skipped, not bound.** A header is written for the newest version while the artifact may have been built without part of it. libbrotlienc's header declares two such functions; binding them produced a shim that compiled and then failed to link, and the linker refuses the whole dependency rather than the two symbols. The export table decides when it can be read.

- **`break` and `continue`.** Jade had neither. Leaving a loop early meant `return`ing out of the enclosing function or writing the exit into the condition, which does not work when the exit only becomes known part-way through the body. `break` leaves the innermost `for` or `while`; `continue` starts its next iteration.
- **Both work across a `try`.** `while true { try { step() } catch e { break } }` is the shape a C library pushes you toward, because [`fails_when`](packages#the-binding-vocabulary) turns its end-of-input code into an exception. Leaving by a jump would otherwise skip the handler teardown and leave a frame pointing at code the loop has already left, so the next `raise` anywhere in the function landed in the wrong arm. The handlers a `break` or `continue` escapes are popped on the way out.
- **Using either without a loop is refused when the file is parsed.** A loop outside the enclosing function does not count: leaving it would mean crossing a call frame, which is what `return` is for.
- **A library split across several headers can be bound.** `jade pkg bind` merged its symbols but *replaced* its header list, so binding `archive_entry.h` after `archive.h` dropped the first header while keeping the symbols that came from it. The shim then declared none of them — and C lets an undeclared function be called, assuming it returns `int` — so a call that really returned a pointer came back truncated, and the crash landed several calls later with nothing pointing at the manifest. It compiled clean, with no diagnostic anywhere. The list merges now, and the shim is built with `-Werror=implicit-function-declaration` so the same gap arriving any other way is a named error.
- **Types are read from every header in the include tree.** Only the named one was read before, so a library that keeps its types in `git2/types.h` and declares functions against them elsewhere reported almost every function as taking an unsupported type. Functions still come only from the header you name, or binding `archive.h` would bind `stdio.h` with it.
- **Umbrella headers work.** `lzma.h`, `git2.h` and `alsa/asoundlib.h` declare nothing and exist to include the files that do. Pointing at one said "no declarations found"; pointing at a sub-header usually failed because a sub-header does not compile alone. When the named header declares no functions itself, the library's own export table picks the declarations instead — an exact test rather than a guess about which directories count as system ones. liblzma goes from refusing to bind at all to 49 of its 114 symbols.
- **A C `enum` is an `int`.** Status-code enums are how most C libraries report failure, and they were unbindable, which on liblzma alone accounted for 60 of 114 symbols — `lzma_code` among them.
- **One unbindable symbol no longer takes the whole dependency with it.** The generator emitted a symbol that fills a struct while dropping that struct's field table, and the shim refuses a reference to a table that is not there — refusing the *dependency*, not the symbol. So one struct of unrepresentable fields made an otherwise fine library uninstallable; `sqlite3_snapshot_free` and `zip_file_attributes_init` are both that shape. The symbol is skipped as a unit now, with the reason in the report.

## v1.3.6

- **A C library with no header now produces a manifest anyway.** `jade pkg add demo --path libdemo.dylib` used to stop and tell you to go find a header. It now reads the library's export table and writes every function it found into `jade.toml` with `"?"` where the prototype belongs, so the work left is filling in blanks in a file that already lists the whole API.
- **`"?"` is a real spelling in the symbol table.** It means the name is known and the types are not. A shared library carries names and nothing else — C keeps no argument or return types in a compiled artifact, and DWARF is stripped from release builds and left in the `.o` files by the macOS linker — so the missing half genuinely cannot be read back out. Jade will not guess at it either: a wrong prototype is a corrupted stack several calls later with nothing pointing back at the manifest, which is worse than a blank.
- **Fixed: a header whose structs are named by tag generated a shim that would not compile.** `typedef struct ZSTD_CCtx_s ZSTD_CCtx;` is how most C libraries declare an opaque type, and the generator wrote `ZSTD_CCtx_s*` where C requires `struct ZSTD_CCtx_s*`. The type name is stripped of `struct` so it can be looked up however it was written, and the stripped form was then used as source text too. It only ever worked for the narrower `typedef struct X X;` shape, where the bare name really is a type — which is what every test in the suite used, and why nothing caught it. Handles, out-handles and struct out-parameters were all affected; all three now write the type the way C writes it, and a test drives a tag-only header through to a real compile.
- **Everything that would use the binding refuses a `"?"` and names the symbols.** `jade check`, `jade run` and `jade build` all stop, list what is unfilled, and show both ways out — the prototype to write, or `jade pkg bind --header` if you do have the header after all. `jade pkg list` and `jade pkg remove` keep working, since a half-filled manifest is a state you need to be able to look at.

## v1.3.5

- **Fixed: `jade.lock` and `jade.toml` could disagree about what a dependency is.** The two were compared by name only, so a lock saying `abi = "jade"` outlived a manifest corrected to `abi = "c"`. The build reads the lock rather than re-resolving — that is what a lock is for — so it skipped the binding shim and loaded a plain C library as though it were a Jade package, which the dynamic loader refused in the finished program for a missing symbol. A disagreement is now reported by name with both values and the command that fixes it.
- **Fixed: a C dependency with no symbols installed successfully and failed at run time.** With nothing to bind, shim generation skipped the dependency and left the raw C library in `libs/`, so `jade pkg install` reported success and `jade build` produced a binary that could not load it. It is an error now, naming the header that would fix it.
- **A missing `jade_pkg_init` says what it means.** The old message named the symbol, which tells you nothing you can act on unless you already know what defines it. Every library reaching that point is a plain C library that was never bound, so it says that instead, and gives the command. Both engines.

## v1.3.4

- **A dependency that is not a shared library is refused when you add it.** Jade checked what a file *exported* but never that it was loadable at all, so a file with the right name went into `jade.toml`, into `libs/`, through resolution and through the linker, and was first refused by the dynamic loader when the finished program ran. The check is now at both ends: `jade pkg add` reads the file before writing anything, and `jade pkg install` checks the bytes it is about to write — which covers a hand-written manifest and a fresh clone, neither of which goes through `add`.
- **It names the likely cause.** The common way to produce one of these is compiling the header instead of the source: `clang -o libadd.dylib add.h` emits a precompiled header, which is a perfectly ordinary file with a perfectly ordinary name. The error says so, and gives the command that works.
- **Fixed: a compiled binary would not say why a native library failed to load.** `jade run` reported the loader's own reason — "slice is not valid mach-o file", a missing dependent library, an architecture mismatch — and a compiled binary printed only the path. Which engine you happened to run decided whether you were told anything. Both carry the reason now.

## v1.3.3

- **Fixed: `sh.output` ran a command the trust model was supposed to refuse.** `sh.exec` and `sh.run` refuse a string that came from outside the program — a model reply, a file, the network, stdin — because it must not reach a shell. `sh.output` did not, and all three run through the same `sh -c`. So the check did not narrow what an untrusted command could do; it only decided how it had to be spelled, and `sh.output(x).stdout` was the way around it. Both engines refuse it now.
- **Fixed: `d.key` on a dict raised in a compiled binary.** Reading a dict entry with a dot worked under `jade run` and failed under `jade build` with "value has no fields", because the compiled runtime's field read handled structs and nothing else. Anything handing back a dict was affected, which is how it was found: `sh.output(cmd).code`. Method calls never were — `d.keys()` is compiled as a direct call rather than a field read — so the gap only showed on data keys, which is what kept it hidden.
- **Fixed: a failed `jade pkg add` left a broken entry in `jade.toml`.** The entry has to be written before it can be validated, so a failure landed after the write. Every other `pkg` command re-validates the whole manifest, so one `add` that failed on a missing file made `install`, `list` and even a later successful `add` fail on an orphan the user never managed to add, with nothing naming the cause. A new entry is now removed when the command fails, and a missing `--path` file is caught before anything is written. An entry that already existed is left alone, since rolling that back would delete a working dependency.
- **A nested `async fn` is a compile error, matching `fn`.** It parsed and ran before, and then handed you two surprises: the inner function could not see the outer one's parameters, so it failed at *run* time with an undefined variable; and a decorator on it was dropped without a word. The rule now reads the same for both forms — "function definitions cannot be nested" — and both surprises are gone with it. Declare the function at the top level.
- **Fixed: a namespaced decorator did not work on an `async fn`.** `@tools::register` resolved correctly on a `fn` and looked for a global literally named `tools.register` on an `async fn`. The two forms had separate copies of the same emission and had drifted; they share one path now.

## v1.3.2

- **A `let` or `prompt` declaration can carry a decorator.** `@shout let greeting = "hello"` is exactly `let greeting = shout("hello")`. Decorators already worked on `fn`, `struct` and `extend`; the two forms that bind a plain value were the ones left out, and they are where the repetition actually accumulates.
- **This is aimed at prompts.** Using a model in practice means wrapping the instruction in tags it recognises, and writing that wrapper around every prompt buries the one part that differs between them. `@instructions prompt summarize = "Summarize the document."` puts the framing above the line and leaves the prompt reading as the text it is. The decorator wraps the text when the prompt is built, so `?p` still means one thing, and the framing travels with the value if the prompt is handed to another file.
- **A decorator may take arguments, and several may stack.** `@fence("note")` passes the decorated value first and its own arguments after. With more than one, the decorator written first is applied first — the same nesting order `fn` already uses, which is the reverse of Python's rule.
- **A decorator on anything else is refused rather than ignored.** `@shout print("hi")` names the forms that work instead of dropping the decorator silently.

## v1.3.1

- **`jade uninstall` removes the toolchain.** There was a way to install Jade and a way to upgrade it, and no way to take it off a machine. It removes the binary and the `lib/jade` tree the installer lays down, and prints every path before touching one — the paths are the only thing that tells a real installation apart from a build directory you did not mean to delete.
- **Your data is kept unless you ask for it to go.** `~/.jade` holds your API key, your installed providers and the cache, none of which are part of the toolchain. Losing a credential to an uninstall you meant to undo is a nasty surprise, so it takes `--purge`, and the message says so either way.
- **`jade reinstall` repairs an installation that is already current.** `jade upgrade` returns immediately when you are on the latest version, which is right for an upgrade and no help at all when the reason you are running it is that something is broken. `--clean` clears `~/.jade` first for a genuinely fresh start.
- **Neither command reads silence as consent.** Both refuse a non-interactive stdin without `--yes`, so a script that did not ask for a deletion does not get one.
- **All three commands resolve the install path through symlinks.** Removing or replacing a link would leave the file it pointed at, which on a Homebrew-style install is the entire toolchain.

## v1.3.0

- **Native packages can hand Jade an opaque handle.** A handle is a pointer the library owns: Jade holds it, passes it back, and never looks inside. That one addition is what makes a whole class of C library bindable at all — SQLite, libsndfile, PCRE2, FreeType, libcurl and libarchive are all built around a pointer you keep between calls, and until now there was nowhere in the value ABI to put one, so it arrived as `nil`.
- **A handle carries the C type it came from.** `handle<sqlite3>` and `handle<sqlite3_stmt>` are different values even at the same address, so passing a statement where a connection belongs is an error you can read rather than a crash inside the library. Printing one shows `handle<sqlite3>` — never the address, which would differ on every run.
- **Jade never closes a handle for you.** It reclaims its own wrapper and leaves the pointer alone, because it cannot know what the pointer is or which allocator made it. Closing is a call the binding exposes. The trade is explicit: a handle you drop without closing leaks whatever the library allocated.
- **A handle cannot be passed into a task.** Jade can see nothing of what a library does with one, and cannot tell a thread-safe library from an unsafe one, so sharing a handle across tasks is refused at compile time instead of racing silently. Open one inside the task and close it before returning.
- **A C library can call a Jade function back.** Pass one where the library wants a callback and it is invoked as the library runs: iterators, comparators, per-row handlers. There is no `libffi` involved — because the binding is generated from a declared signature, the shim can simply declare a C function of exactly that shape.
- **A raise inside a callback is contained.** The library finishes cleanly and the error reaches your `catch` afterwards, rather than unwinding through the library's own frames mid-operation and leaving it in whatever state it happened to be in.
- **Callbacks work the same on both engines, by very different means.** Compiled code calls straight through. The interpreter cannot be re-entered from a C frame at all, so it runs the call on a worker thread and answers each callback from its own loop. The limit that follows is worth knowing: a callback is live only while the call that passed it is running, so a library that stores one and invokes it later is not supported.
- **Adding a dependency is the same command whichever kind it is.** `jade pkg add sqlite --path libsqlite3.dylib` reads the artifact to see what it is: a Jade package exports `jade_pkg_init`, a plain C library does not, and that is the same symbol the loader requires at run time. Both are a `.dylib`, so nothing about the filename could have told you. `--c-abi` is now only needed for a URL dependency, where there is no local file to read yet.
- **A C library's header is found for you.** Continuing the example, it works out that `libsqlite3` means `sqlite3.h`, looks through pkg-config, the usual include directories and the macOS SDK, reads the declarations with clang, and builds the binding — then `use sqlite` works. Pass `--header` when the guess would miss.
- **The library checks the header.** A shared library carries only symbol *names* — C does not mangle them, so nothing in a `.so` describes a signature — but the names are enough to tell whether a header really belongs to it. A header declaring none of the symbols the library exports is refused before anything is written, instead of failing later as an undefined symbol from the linker. The report also says how much of the library you actually got: "covers 181 of the 194 symbols the library exports" is the number that matters, where a bare "181 bound" is not. Transcribing signatures by hand was the real limit on binding anything substantial: SQLite has around 200 entry points, and 181 of them bind from the header with no help.
- **`jade pkg install` fills in anything still missing.** A dependency that names a header but has no symbols gets bound on install, so a hand-written entry needs only the library and its header. A manifest that already has its symbols is left alone, which means a fresh clone installs without needing clang at all; `--locked` never binds, so a reproducible install cannot depend on the local clang.
- **`jade pkg bind` is still there for the cases with a decision in them** — re-running after a header changes, or `--only` to take a large header a piece at a time. `--dry-run` shows what would be written without touching the manifest.
- **It tells you what it could not bind, and why.** Varargs, `void *` — each is a real signature this ABI cannot carry yet, and each is named with its reason rather than quietly dropped. A binding resting on a judgement call, such as reading a writable pointer beside a count as a buffer the call fills, is reported separately as an assumption to check.
- **Opaque pointers are recognised as handles automatically.** `typedef struct sqlite3 sqlite3;` is the C idiom for "hold this, do not look inside", so it becomes `handle<sqlite3>`; `sqlite3_open(path, &db)` becomes a call returning a handle; and a returned `T*` is wrapped as one. The failure convention is inferred where it is unambiguous — a NULL-returning open, or a status beside an out-handle.
- **A C binding can take and return binary data.** A `bytes` argument becomes the `(pointer, length)` pair C expects, and a function that fills a caller-allocated buffer — `read(fd, buf, n)` and everything shaped like it — is called from Jade without the buffer: `x_read(handle, n)` hands back the bytes. The shim owns the scratch, because a Jade blob is immutable and letting a C library write into one would break that. The blob comes back sized by what the library actually wrote, not by what you asked for.
- **A C function can fill a struct through an out-parameter.** `sf_open(path, mode, &info)` is called as `sf_open(path, mode)` and returns both results as `.ret` and `.out`. When the C function returns nothing, the filled struct is simply the result.
- **A struct out-parameter requires the library's header**, declared as `headers = ["sndfile.h"]`. The shim includes it and lets the C compiler own the layout. The alternative — describing the layout in `jade.toml` — puts integer widths and padding in a hand-written file, where one disagreement writes at the wrong offset with nothing to catch it. A field the struct does not have is now a compile error naming the field.
- **A failing C binding can say why.** A symbol in `jade.toml` may declare `fails_when` — `null`, `negative`, or `nonzero` — and the generated shim then turns a failed call into a catchable Jade error carrying the `errno` reason. Before this the reason the library had already recorded was thrown away, and the program saw `-1` and nothing else.
- **Fixed: a compiled binary read freed memory when a native function returned a pointer into its own argument.** The compiled runtime released the marshalled arguments before it converted the result, so such a call gave an empty string built and the right one run. The interpreter always had the order right.
- **`RUNTIME_ABI_VERSION` is 4, and `CACHE_FORMAT_VERSION` is 7.** Native packages built against an older Jade must be rebuilt — the loader refuses them by name and version rather than misreading a tag. Run `jade cache clean` if you built from a 1.2.x branch.

## v1.2.5

- **Fixed: `http.get_bytes` and `post_bytes` could not be compiled.** They shipped in v1.2.2 as interpreter-only functions, with no lowering and no `jrt_*` symbol. So a program using a byte body passed `jade check`, ran under `jade run`, and failed at `jade build` with "unsupported module call" — the one place a missing builtin surfaces last, when you try to ship. Both now work on both engines.
- **`std::uhttp` gained the same pair.** `uhttp.get_bytes` and `uhttp.post_bytes`, matching `std::http` exactly. Without them a daemon on a Unix socket could not answer with audio, an image, or a compressed stream: `uhttp.get` runs every reply through a lossy UTF-8 decode. A test now compares the two packages' function tables, since what made this gap easy to miss was that nothing did.
- **Fixed: a text body containing a NUL disagreed between the engines.** A Jade string is NUL-terminated, so a compiled binary truncated `.body` at the first zero byte while the interpreter kept going. The same program reported 8 characters run and 4 built. Both now stop at the NUL, and the rule lives once in the shared runtime rather than falling out of whichever path you took. Use `get_bytes` for a body that is not text — that is what it is for.
- **A wrong-typed body is reported, not dereferenced.** `post_bytes(url, "text")` names what it got on both engines instead of reading a string as a heap object.
- **Fixed: a compiled binary printed floats in scientific notation.** `print(10.0)` gave `1e+01` and `str(48000.0)` gave `4.8e+04` under `jade build`, where `jade run` gave `10.0` and `48000.0`. The rule was "whenever the float needs trailing zeros before the decimal point", so sample rates, byte counts and durations were exactly the values that broke while `1024.0` and `10.5` looked fine — which is what kept it hidden. The compiled runtime formatted floats itself, in C, rather than using the renderer the interpreter uses; now both engines share one. A log line or a display string built with `str(rate)` is correct again. `json.stringify` and printing a float inside an array were never affected.
- **A large float prints in full rather than truncating.** The same path formatted every scalar into a 64-byte buffer, and a float has no length bound — `1e300` is 301 digits. It now renders on the heap, like a collection already did.
- **`bytes` reaches a native package from a compiled binary.** The type shipped in v1.2.2 with its ABI tag defined but never wired into the compiled runtime's marshaller, so a blob argument silently arrived as `nil` and a blob return value crashed the process. The interpreter implemented the same tag fully, which is why this only showed up after `jade build`. Blobs now cross in both directions, at the top level and nested in a container, and arrive tainted the way any data from outside the program does.

## v1.2.4

- **`?p` is a buffered stream, like every other stream.** Reading one twice gives the same text twice. Until now the receiver was taken on first drain and a second read raised `DoubleStreamDrain`, so printing a dereference and then using the same value was an error rather than the obvious thing. That error is gone.
- **`stream()` is gone.** It existed only because a grammar-constrained dereference used to collapse into a blocking call, leaving no stream to print. It does not any more: `?p |> g` sends the grammar with the request and keeps the reply a stream, so `print(?p |> g)` streams live with the grammar's muted region suppressed, and reading it as a value gives the full text including that region. One operator covers what took a builtin and a keyword argument.
- **The mute spec rides on the stream.** `print` no longer has to be told what to suppress — the anchors come from the Grammar the `|>` stage named. There is now one place that builds a `?p` request, where before there were three that had already drifted: one sent a Grammar's bare pattern where another sent the wrapped GBNF, so the same Grammar constrained the model differently depending on how it was reached.

## v1.2.3

- **`yield` makes streaming an ordinary language feature.** A function whose body contains a `yield` returns a *stream* instead of a value: the body runs to completion filling a buffer, and the caller reads the buffer. `len`, indexing, `for`, and `print` all work on one.
- **A stream is a buffer, not a one-shot channel.** Everything it produced is retained, so reading it twice gives the same values twice. That is the whole model, and it is what removes a category of rules rather than adding them: there is no "already consumed" state, no replay semantics to define, and no error to hit on a second read.
- **A bare `return` stops a generator early; `return x` is a compile error.** A function that yields produces a stream, so returning a value as well would ask it to be two things at once.
- **Yields of different types widen rather than failing**, the same rule a mixed array literal follows.
- **A stream is an ordinary array in a compiled binary**, so `len`, indexing, iteration, and rendering reuse everything arrays already do rather than growing a parallel implementation.
- **`CACHE_FORMAT_VERSION` is 6.** The 1.2.x releases added variants in the middle of serde-serialized enums (`JadeType::Char`, `Bytes`, `Stream`; `Stmt::Yield`), which renumbers every variant after them. A cache written by an earlier 1.2.x build deserializes into the *wrong* types rather than failing loudly — it showed up as an imported struct losing its field defaults. Clear your cache with `jade cache clean` if you built from a 1.2.x branch before this.

## v1.2.2

- **`bytes` is a real type.** A counted sequence of raw octets, deliberately not a string. A Jade string is UTF-8 and NUL-terminated, so a blob with a zero byte in it would be truncated there and one that is not valid UTF-8 would be corrupted by anything assuming text — `fs.read` goes through a UTF-8 decode and cannot read a PNG at all. Conversion is explicit in both directions with `str.encode()` and `bytes.decode()`, and decoding invalid UTF-8 raises and names the offset rather than substituting replacement characters.
- **Indexing a blob gives an `int` in 0..=255, not a `char`.** A byte is not a Unicode scalar, and making `b[0]` look like `s[0]` would hide that the two differ on any non-ASCII input.
- **Byte I/O across files, HTTP, and the standard streams.** `fs.read_bytes` / `write_bytes` / `append_bytes`, `fs.read_stdin_bytes` / `write_stdout_bytes` so a program can sit in a binary pipeline, and `http.get_bytes` / `post_bytes` for a body that is not text.
- **A blob carries a trust byte, and that is the point of the design.** `fs.read_bytes` returns tainted data and the taint survives `.decode()`, so `fs.read_bytes(p).decode()` is refused by `sh.exec` exactly as `fs.read(p)` is. Without it, encoding and decoding would have been a laundering route straight through the trust model — and an invisible one, since every fixture in `examples/trust/` used whole strings.
- **Fixed: a refused tainted value killed a compiled program instead of raising.** The interpreter raises a catchable exception; the compiled runtime printed to stderr and exited. So `try { sh.exec(x) } catch e { … }` ran the handler under `jade run` and terminated the process when built. The compiled path now raises, matching the interpreter.
- **The native ABI carries bytes, so `RUNTIME_ABI_VERSION` is 3.** Bytes could not ride on the existing string tag, which is a NUL-terminated `char*`. **Every installed provider package must be rebuilt** — if `?p` stops working after upgrading, reinstall your providers.

## v1.2.1

- **`char` is a real type.** Indexing a string, iterating one, `char("x")`, and `?p |> char` all produce a single Unicode scalar rather than a one-character string. It is an *immediate*, riding inside the tagged value word next to `int`, `bool`, and `nil`, so scanning a string now allocates nothing where it used to allocate once per character.
- **Strings iterate.** `for c in s` was a type error until now; it binds a char per step and counts characters, so a four-character string with a two-byte character in it gives four steps and not five.
- **Breaking: `s[0]` is a char, not a `str`.** A char compares equal to the one-character string spelling it, orders against strings, and concatenates with them in either direction, so `if s[0] == "a"` keeps meaning what it meant. That is a deliberate exception to Jade's "no cross-type comparison" rule and it is now written down in the types reference rather than being folklore.
- **A char taken from a tainted string is still tainted.** The trust byte lives in a string's header, and a char has no header — so it rides in bit 63 of the value word in a compiled binary and in a field on `JChar` in the interpreter. Without it, a loop rebuilding a string character by character would have laundered it silently past `sh.exec`, and nothing in the trust fixtures would have caught it.
- **The tagged-value layout gained its first new immediate.** `char` claims bit 4 of the nil branch, the only unused immediate space left in the word. `is_nil` therefore tests five bits rather than four: before this, *any* word ending `0b0111` was nil whatever sat above it, so a char would have read as `nil`. The Rust and C copies of that test are two spellings of one rule and moved together.

## v1.2.0

- **`|>` is one operator again.** It was two, sharing a spelling. An ordinary pipe (`5 |> double`) was desugared by the parser into a call; a pipe after a prompt dereference (`?p |> int`) was read by a *different* rule that stored the stage on the dereference. Which one applied depended on surrounding syntax, decided before anything knew what the names meant. Now every `|>` parses the same way and the type checker decides what the stage is, which is where that decision belongs — a stage is a type, a Grammar, or a function, and only the checker knows which.
- **A dereference chains.** `?p |> int |> double` constrains the model to an integer, coerces the reply, and hands `double` a real int. This was previously unwritable: the dereference rule read its stage with a parser that stopped at the first `|>`, specifically so a chain could not form.
- **A typed dereference works inside `print()`.** `print(?p |> int)` was a compile error — the parser tracked whether it was inside a `print(...)` call and rejected the program with "assign to a variable first". Streaming is decided by what `print` receives, not by what the parser can see, so `print(?p |> int)` prints the coerced int and `print(?p)` still streams tokens live.
- **Fixed: a function on the right of `|>` after a dereference was silently treated as a grammar.** `?p |> parse(x)` did not fail. Inference fell back to "anything of unknown type is a Grammar value", so a user function was handed to the sampler as a sampling constraint. It now applies, like the pipe it looks like.
- **A bad stage is a type error naming what it found.** `5 |> 3` reported "expected function or call on right side of `|>`, got expression", because a parser matching on shape can only talk about shapes. The new `InvalidPipeStage` says it got an int. `StreamingWithType` is gone.
- **Two rules decide a name that could be more than one thing.** A builtin type keyword is always a type — `int` is also a callable constructor, so without this `?p |> int` would generate unconstrained and then fail to coerce, and the grammar is the valuable half of a typed dereference. A declared struct is always a type, for the same reason. Everything else prefers a function.

## v1.1.36

- **A package can now describe itself in `jade.toml`.** A new `[package]` section names the entry module, the files the package is made of, and the functions it exports, so `jade build --lib` reads a package's shape from the manifest instead of from flags somebody had to remember to type. `jade build --lib` with no file argument builds it.
- **Multi-file packages already worked; what was missing was saying so.** Every module the entry `use`s has always been compiled into the same artifact, each in its own namespace. The entry module is the API — only its top-level functions become bindings — which is unchanged, and is what keeps adding a helper to an internal module from silently widening what consumers can call.
- **`sources` is checked against what the entry actually imports.** The build finds a package's files by following `use`, so the list is not what makes it work. What it catches is the pair of mistakes the import graph cannot report on its own: a file you meant to ship but forgot to import, which would vanish from the artifact, and a file pulled in without you deciding to ship it. Either fails the build naming the file. Omit `sources` and the import graph is taken at its word.
- **Nothing changes for consumers.** The artifact is an ordinary Jade package, added and locked exactly as before.
- **The docs caught up with v1.1.35.** Local `path` dependencies get their own section in the packages guide — the re-pinning, the `jade pkg list` drift status, and the `--locked` error — which until now existed only in this changelog. The imports guide gains the rule that a dependency binds one name to one artifact however many files built it, since that is the thing most likely to be assumed otherwise now that a package can span files.

## v1.1.35

- **Fixed: rebuilding a local dependency did nothing, and nothing said so.** A `path` dependency was hashed once, by `jade pkg add`, and `jade.lock` held that digest forever. Installing checked `libs/` against the lock, found a match, and stopped — so the copy taken on the day you added the library kept loading no matter how many times you rebuilt the real one. `jade pkg install` did not help; only re-running `jade pkg add` did. There was no warning and no checksum complaint, because from the lock's point of view nothing was wrong.
- **A local source is now re-hashed on every install and every run.** It is the one kind of dependency that legitimately changes underneath a lock that is otherwise still correct: a URL either serves the bytes the lock pins or it does not, but a path points at a file you build. `jade pkg install` and `jade run` re-pin it, copy the new artifact into `libs/`, and say which dependency moved. Remote dependencies are untouched by this — re-pinning those would defeat the point of a lock.
- **`jade pkg install --locked` rejects a moved local source instead of installing the old one.** That is the CI case: a rebuilt library means the committed lock is stale, and the error names both digests and how to fix it.
- **`jade pkg list` marks a local dependency whose source has changed.** The state was previously indistinguishable from up to date.
- **Fixed: `jade build` never installed dependencies, so the two engines disagreed.** `jade run` has always fetched what `jade.lock` pins before executing; `build` did not, and linked against whatever `libs/` was last left holding — or, in a fresh clone, nothing at all. Both now install first, so a compiled binary and the interpreter see the same library.
- **A C binding shim is relinked when the library under it is rebuilt.** The shim's own generated source depends only on the declared symbols, so an unchanged symbol table skipped the compile even when the artifact it links against had been replaced.
- **Fixed: error messages named commands that do not exist.** The package commands are nested — `jade pkg install`, not `jade install` — but every message that told you how to recover said the latter, including the one you hit first: a project with `[dependencies]` and no lock. Corrected across the CLI, the lockfile reader, and the generated shim header.
- **Fixed: `jade fmt` reindented the inside of multi-line strings, changing what a program printed.** Indentation inside a `"""…"""` block is part of the string's value, and the formatter treated those lines as code — so running `jade fmt` over a file with a multi-line prompt or a heredoc rewrote the text, in place, with no warning. Anything you formatted with 1.1.34 or earlier and that contains a triple-quoted string is worth checking against git.
- **Fixed: the formatter did not know Jade had comments.** Its own source said so — "Jade has no line-comment syntax, so every character is significant" — which was true of the token stream it does not use and false of the text it does. A `{` anywhere in a `//` comment indented every line after it to the end of the file, and a `}` dedented them. The examples in this repo trip it on the first file.
- **Fixed: a `}` in a single-quoted string dedented the rest of the file**, for the same reason: the brace scanner tracked `"` and not `'`. Escaped quotes now end a string only when they are meant to, and `""` no longer reads as the start of a `"""`.
- **Fixed: wrapped expressions were flattened to column 0.** Only braces were counted, so a call, array, or struct literal spanning lines had its continuation lines pulled back to the enclosing block's depth, destroying alignment written by hand. Those lines keep the alignment they were written with — how a long expression wraps is a layout decision this formatter does not make. What separates the two cases is where the `{` sits: one that ends its line opens a block, so `let cfg = {` indents what follows, while `Result { name: name,` does not.
- **`jade fmt` now refuses to write a file it would change the meaning of.** Formatting moves whitespace, so the result has to lex to the same tokens; if it does not, the file is left alone and the mismatch is reported. A half-typed file that does not lex at all is left alone rather than refused — the formatter runs on code people are still writing. This is a backstop, not the fix: it would have turned the multi-line-string bug into an error message instead of silent corruption.
- **CI now holds `examples/` formatted.** `jade fmt` was the one command nothing in CI exercised, which is how it rotted this far unnoticed. The gate runs it over 70-odd real files on every push, and a unit test additionally asserts every fixture formats to the same tokens and settles in one pass.
- **The CLI got test coverage where it makes a decision.** Every subcommand handler ends in `process::exit`, so what is tested is the choice each makes before it touches the world: how source is formatted, where a build lands without `-o`, which release archive a platform wants, how `jade run -v` renders a value, which files a directory walk collects. Roughly 40 new tests. The commands themselves were checked by hand — `run`, `check`, `build`, `new`, `init`, `test`, `repl`, `env`, `cache`, `pkg`, `register`, `use` all work, and all report a non-zero exit on failure.

## v1.1.34

- **Fixed: a compiled binary was not trustworthy once a program did real work.** Three separate defects in the AOT backend corrupted memory in ways that surfaced as a segfault, an infinite spin, or a spurious `key not found` — the symptom moving with whatever had run first, which is what made it read as one unpredictable fault rather than three specific ones. `jade run` executed the same source correctly and repeatably throughout, so a program could pass every test under the interpreter and fail as a shipped binary. Anything you compiled with 1.1.33 or earlier and that uses `dict.get`, primitive methods on values whose type is not statically known, or `return` inside a `try` is worth rebuilding.
- **Fixed: `dict.get` handed back a value it did not own.** The runtime returns the value word *borrowed* — the dict keeps owning it — and `GetIndex` retains it for exactly that reason, but the `get` arm stored it without retaining. Every call decremented the entry, so a module-level table read through `TABLE.get(k)["field"]` returned correct values twice, double-freed the inner collection on the third, and then raised `key not found` off freed memory. Nested dicts took the visible damage; a flat table was corrupted just as much but survived longer, which made "keep module-level tables flat" look like a fix.
- **Fixed: calling a primitive method on the wrong kind of value killed the process instead of raising.** The backend picked the method from the *name* and then untagged the receiver to a pointer of the kind that name implied. A name does not prove a kind: in `fn f(v) { v.keys() }` the receiver is only known when `f` is called. So `v.keys()` on a string dereferenced a `char*` as a dict, and `v.upper()` on an int dereferenced a small integer. The receiver — and a str method's arguments, untagged the same way — are now checked at runtime, raising the interpreter's message.
- **That raise is catchable, which is the part that matters.** The idiomatic way to ask a value's type is `try { v.keys(); return true } catch e { return false }`, and it worked under `jade run` and took the process down compiled. No `catch` can see a segfault. The check also removes a silent wrong answer: `v.keys()` on an *array* returned as though the array were a dict rather than failing.
- **Fixed: returning out of a `try` left its handler registered.** The emitter places `PopHandler` on the try body's normal fall-through exit, which `try { …; return x } catch e { … }` never takes. The VM was unaffected — its handler stack is a local of the call frame, so it dies with the function — but the compiled backend used one thread-wide stack that nothing unwound on return, leaving an entry pointing at a `jmp_buf` in a frame that had already returned. The next `raise` then longjmp'd into dead stack. Compiled functions now snapshot the handler depth on entry and restore it on every return, matching the interpreter's scoping. A function containing no `try` emits neither the snapshot nor the restore.
- **The failing shape needs the leak to happen inside an enclosing `try`**, so the dead handler sits on top of the live one and the throw reaches it first. A `raise` whose own handler was pushed last always found the right frame, which is why this survived the obvious tests.
- **Three regression fixtures, all on the backend parity gate**: `examples/dicts/nested_get`, `examples/collections/method_type_guard`, and `examples/exceptions/return_inside_try`. Each fails on the pre-fix backend — the last one by spinning forever — and each now runs identically on both engines.
- **Fixed: `uhttp.stream` and the `write` builtin now compile.** Both existed only in the interpreter: they passed `jade check`, ran fine under `jade run`, and failed at `jade build` with an unsupported-call error. That is drift between the two engines, which this language defines itself as not having — a program discovered it at packaging time, after the code was written and tested.
- **`write(x)` is `print` with no newline, and it flushes.** The flush is the point rather than a detail: `write` exists for output that has no trailing newline, and stdout to a terminal is line-buffered, so without it the text appears late or, at exit, out of order. Same renderer as `print`, so the two agree on how every value looks.
- **`uhttp.stream(url, handler[, headers])` works compiled, including the early stop.** A handler returning `false` closes the socket and ends the stream, and the call returns the HTTP status either way. Only an explicit `false` stops it, so a handler that just prints does not have to end in a boolean.
- **The streaming reader now lives in the shared runtime crate**, where the rest of `uhttp` already was. It had been VM-only on the reasoning that streaming "calls back into Jade, so it cannot be a pure AOT symbol" — but `array.map` had been calling a Jade function from compiled code all along. What actually differs between the engines is who drives the loop, so the shared piece is pull-shaped: it yields the next line, the VM pumps it into its async channel from a worker thread, and the compiled path drives it inline. One socket reader, one line-framing implementation, both engines.
- **Fixed: a caught error was a struct under `jade run` and a plain string compiled.** The VM funnels every non-user error through one wrapper, so `catch e` binds a `RuntimeError` struct with a `message` field. The compiled backend raised the bare message string, so the *same* `try` block saw different types on the two engines: `e.message` raised in a compiled binary, and `catch RuntimeError e` never matched at all, silently falling through to an untyped arm or out of the function. A program that only reported the error looked fine; one that inspected it broke when you shipped it.
- **This was every raise, not just transport failures.** Division by zero, integer overflow, a method on the wrong type, and the `fs`/`http`/`uhttp`/`sh` failures all took the same path, so the fix is at the throw layer rather than in each module. The compiled side now builds the same `RuntimeError` struct, and I/O failures carry the same `I/O error: ` prefix the interpreter's wording adds.
- **A user's `raise x` is untouched.** The value thrown is the value written, so a raised string stays a string and a raised struct keeps its own type — matching the VM, which also raises the value verbatim. Only errors the runtime itself produces get wrapped.
- **Uncaught errors still say what went wrong.** Wrapping the value would have degraded the top-level report from `jade: division by zero` to `jade: uncaught RuntimeError`, so the uncaught printer now reads a raised struct's `message` field. Any struct carrying a str `message` reports that way, including your own error types.
- **`uhttp.stream`'s failures are now named `uhttp stream:`** rather than the bare `uhttp:`, matching the request path's `uhttp GET:` / `uhttp POST:` on both engines.
- **What still differs:** the interpreter prefixes a message with `[line:col]` and a compiled binary does not, because compiled code has no span at runtime. Every AOT raise already omitted it, so nothing changed here — but if you match on error text, match by substring rather than equality.

## v1.1.33

- **`jade check` now verifies that a program's imports resolve.** It did not, at all: `use totally_made_up_module` reported `ok` and then failed at run time with `cannot find import`. The cause is that import resolution was never a compile stage — the VM resolved a `use` when the `Import` opcode executed, and the AOT backend when it flattened imports, so `check`, which stops at bytecode emission, never looked. Every version up to this one behaved that way; it is not a recent regression. A checker that passes files which cannot load is not a gate, so if you were using `jade check` in CI to screen generated or untrusted `.jde` files, it was not screening imports.
- **A fake `std::` module is caught too.** `use std::totally_fake` passed `check` on the same path. `std::` is not a blanket escape hatch: a package that is not compiled into the binary falls through to ordinary file resolution and now fails like any other unknown name.
- **The walk is transitive, and each module resolves against its own directory.** A module that itself imports something missing breaks the program that imports it, so `check` reports the inner failure with the inner span. And a module's imports resolve relative to *that module's* directory rather than the entry file's, which is what makes a chain of imports across directories work. A circular import stops the walk rather than recursing forever; whether a cycle should also be a check-time error is a separate question from whether the files exist, and the VM still raises `CircularImport` at run time.
- **Checking a file loads nothing.** A native module is confirmed to exist but is never opened, because opening a shared library runs its initializer, and `check` must not execute the program it is checking. An imported Jade module is lexed and parsed to find its own imports, not type-checked — its type errors are its own business when you check it directly. And unlike `jade run`, `check` does not call `ensure_ready` to fetch missing dependencies: checking a file should not reach the network. In a project whose `libs/` has not been populated, dependency imports are therefore reported as unresolved, which is accurate — they cannot be loaded until `jade pkg install` runs.
- **One resolver behind all of it.** `project::resolve_import` is now the single answer to what a `use` names — a built-in package, a native library, or a Jade file — and the VM's `resolve_user_import` is a thin adapter over it. A `use` that `check` accepts and the VM then cannot find is no longer a shape the code can express.
- **What this does not fix.** Nothing an import brings in is visible to type inference: a module binds as an untyped dict, so `use std::math` followed by `math.no_such_function(1)` still passes `check` and fails at run time. Verifying symbols reached *through* an import means typing modules, which is a much larger change than confirming a file is there.
- **The `*_error.jde` fixture convention now covers imports.** The harness used to check a fixture by feeding it source text, which cannot resolve a `use` — there is no file to resolve it relative to. It checks by path now, so `examples/imports/missing_error/` genuinely fails the way it claims to.

## v1.1.32

- **A provider package built for an older Jade is now refused by name.** v1.1.31 began sending the inference request as a struct, which a package built before it cannot read — so every provider published to date failed with `native function returned an unknown value tag`, raised from inside the call, naming neither the version nor the fix. Both engines now compare the package's value ABI against the runtime's at load and refuse a mismatch with a message that says what to do. `jade build --lib` stamps every package it emits with the ABI it was built against; packages predating that are read through the runtime version they re-export. A plain C library wrapped by `jade pkg add --c-abi` declares neither, has no value ABI to disagree about, and loads as before.
- **If `?p` stopped working after upgrading, reinstall your providers.** The bundled `anthropic`/`openai` packages that ship in the release tarball must be rebuilt against v1.1.31 or later. Until they are, the error above is what you will see — which is the point of it.
- **Fixed: a compiled binary printed a prompt's text where `jade run` printed `<prompt>`.** A prompt is a type you dereference, not text you read, and it displays opaquely the way a future displays as `<future>`. The AOT backend held a prompt as the bare string it wraps, on the reasoning that a prompt only ever reaches a dereference — but a prompt can also be printed, held in a collection, stored in a struct field, passed to a function, or returned from one, and each of those showed a string where the interpreter showed a prompt. A prompt is now its own heap kind on that side too.
- **`jade build` supports `prompt` struct fields.** They were rejected outright, since there was nothing to store that would read back as a prompt. `examples/structs/prompt_fields` was excluded from the backend parity gate for that reason and now passes it, along with a check that a prompt keeps its type through a struct field and a collection.
- **Fixed: a compiled binary leaked one object per prompt.** The new heap kind was not reference-counted at first, so a loop building prompts never reclaimed them. A prompt is counted like a future now: header-carrying, owning one child.
- **The repository root holds only the things that must live there.** The Cargo build script moved to `src/runtime_aot/build.rs`, beside the C runtime it compiles, declared through `build =` in `Cargo.toml` — it could not go to `src/build.rs`, since `src/build/` already claims that module name. The backend parity gate and its stand-in inference provider moved to `src/scripts/`. The `design/` tree is gone, its two notes rehomed beside the code they govern as `src/providers/design.md` and `src/compiler/design.md`. And the root `install.sh` is gone: it was a byte-for-byte duplicate of `docs/static/install.sh`, which is the copy actually served at `jadelang.org/install.sh`. Nothing synced the two, so they matched only as long as every edit remembered both. No change to how anything builds, installs, or is tested.
- **An array literal may hold values of different types.** `[1, "two"]` and `[Token { text: "hi" }, Done { count: 2 }]` are now legal. The element type is whatever every element agrees on, and unknown when they differ — the same rule a dict's value type has always used. Nothing changed at runtime, because arrays were never typed there: the check was a frontend gate over machinery both engines already had, which is why building the same array with `push` had always worked. Mixed numerics widen rather than promoting, so `[1, 2.0]` has an unknown element type instead of a float one; typing `a[0]` as float while it holds an int would send compiled code down a specialized path for a value that is not that type.
- **What that costs you is compile-time errors, not correctness.** With a mixed array the compiler knows nothing specific about `arr[i]`, so `mixed[0] + mixed[1]` is checked when it runs rather than when it compiles. A uniform array still carries its element type and is checked as before.
- **Fixed: `contains` on a mixed array answered in the VM and raised in a compiled binary.** Membership was using the `==` operator, which rejects a comparison across types by design. But `arr.contains(x)` cannot ask which elements match without walking past the ones that do not, so an element of another type answers `false` rather than raising. Both engines now share one rule for it. `1` and `1.0` remain different values, matching `1 == 1.0` being an error.
- **Fixed: a compiled binary described a cross-type comparison wrongly.** `1 == "x"` reported `'==' requires numeric operands` — misleading, since the trouble is that the types differ, not that they are non-numeric — where `jade run` named both types. Both now say `'==' cannot compare int and str`, for every comparison operator.
- **Fixed: `jade run` leaked a Rust name into arithmetic type errors.** `1 + "x"` reported `Add requires numeric operands`, naming an internal enum variant, where the same program compiled said `'+' requires numeric operands`. Both now use the operator symbol.
- **Removed `JadeError::HeterogeneousArray`**, which nothing can raise any more.

## v1.1.31

- **Structs now cross the native package boundary, carrying their type name.** The FFI carried `nil`, `int`, `float`, `bool`, `str`, arrays, and dicts; a struct became `nil`. It is now a tag of its own — a dict plus the struct's type name, fields in declaration order — mirrored byte for byte in the VM's marshaller and the C runtime's. The type name is the point: a dict with the wrong keys reads as a set of nils and fails silently, so two programs sharing a dict share a convention, while two sharing a struct share a type the receiver can check. Functions and futures still do not cross. `RUNTIME_ABI_VERSION` is 2.
- **A provider receives an `InferRequest` struct, defined once outside this repo.** `?p` used to hand a provider package an anonymous dict whose keys — `prompt`, `grammar`, `anchor`, `stop_anchor` — were string literals written out separately in Rust and in C, and read by string in the package. A renamed key reached the provider as a silent `nil` with no error at any layer. The shape now lives in `ovata-infer-protocol`'s `jade/infer.jde`, consumed here as the `src/protocol/` submodule (pinned to `v0.5.0`) and by provider packages as a `[lib]` they `use ovata::infer`. Every field is always present; an unset one is `nil`, where the dict omitted the key.
- **The prompt field is called `input`.** `prompt` is a Jade keyword and cannot name a struct field in any spelling, and `input` is the better word regardless — what a provider receives is text to complete, not a Jade prompt binding.
- **The response is declared too, and an unreadable frame now raises.** The shared definition covers both directions: `Token`, `Done`, `Error`, `Meta`, and `Json` sit alongside the request. Both engines used to match a frame by bare string literal and *skip* anything they did not recognise, so a provider that wrote `"token"` lowercase or renamed `text` produced an empty reply with no error at any layer — the model appearing to have said nothing. An unknown frame type, a `Token` without string text, or an element that is neither a struct nor a tagged dict now raises a catchable inference error naming what arrived.
- **A frame may be a struct or a dict.** `Token { text: "hi" }` and `{"type": "Token", "text": "hi"}` are the same frame: a struct's type name is what the dict spells under `"type"`. The dict form stays supported. One wrinkle with structs — a Jade array literal must be homogeneous, so `[Token {…}, Done {…}]` is a type error; build the array with `push`.
- **Fixed: a struct crossing the FFI carried its import-mangled name.** `aot/imports.rs` renames an imported module-global `Foo` to `Foo$2` while flattening imports, and that name was baked into the compiled library, so a provider built with `use ovata::infer` returned frames named `Token$0` and the caller rejected its own protocol. Both marshallers now strip a trailing `$<digits>`, which is never part of a name anyone wrote — `$` is not legal in a Jade identifier. Affects any native package that returns a struct, not just providers.
- **Tripwire tests bind the compiler to that definition.** The compiler cannot `use` a `.jde` in its own Rust and C sources, so it keeps a hand-written copy of the names. Tests parse `src/protocol/jade/infer.jde` with the compiler's own lexer and parser — not a regex, so a comment naming a field cannot fool them — and fail on any difference in the request's fields or the set of frame names, in either direction. Two more read the C engine's source text and assert it names every request field and every frame, since a Rust constant cannot reach C. A missing submodule is a hard failure rather than a skip: an absent definition is exactly when drift goes unnoticed.
- **Fixed: `jade run` and `jade build` disagreed about which project a file belongs to.** The VM found the project root by walking up from the *current directory*, the AOT backend from the *source file's* directory. So from a repo root, `jade build sub/app.jde` resolved a `[lib]` import that `jade run sub/app.jde` reported as a missing file — same program, same shell, different answer. Both now resolve from the source file: which project a file belongs to is a property of the file, not of where you happen to be standing. `jade test` picks up the same rule. `jade run` with no file argument, `jade env`, and `jade pkg` still read the current directory, which is their only input. A new fixture, `examples/imports/project_lib/`, carries its own `jade.toml` so the parity gate can see this class of bug at all; nothing in `examples/` previously had a project root of its own.
- **Breaking for provider packages, again.** A package written against the v1.1.30 dict receives a struct and reads `request["prompt"]` as nothing. Providers must read `request.input` and be rebuilt. A package whose frames were already `Token`/`Done`/`Error`/`Meta`/`Json` needs no response change; one that emitted anything else now fails loudly instead of silently.

## v1.1.30

- **The inference daemon and its Unix socket are gone; a provider package is the only way to reach a model.** `?p` used to pick between two backends: the provider package it loads in-process, or a local daemon on `$HOME/.jade/llm.sock`. They did the same job, and the daemon was the one with a serialization boundary in the middle — a wire format, a framing layer, and a second process to keep running for what a linked library does with a function call. Removed with it: `jade-runtime`'s `infer/` module (the socket client, frame decoder, and `jrt_ipc_*` C entry points), `src/llm/jaded.rs`, `runtime_aot/ipc/`, the hand-rolled JSON request builder in `infer.c`, the `ovata-infer-protocol` dependency in both crates, and the `JADE_LLM_SOCK` environment variable. Roughly 1,900 lines. If you were running the daemon, run `jade register` to install a provider instead; a `?p` with none installed raises `NoInferenceBackend` naming that command.
- **Constrained decoding is the provider's job now.** A typed dereference (`?p |> Type`) or an explicit `Grammar.new` puts `grammar`, `anchor`, and `stop_anchor` in the request the package receives. The three travel together — the anchors bound the span the grammar constrains, so sending the pattern alone would silently drop half of a `Grammar.new(pattern, anchor, stop)`. A package that cannot honour a grammar returns an `Error` frame, which surfaces as a catchable Jade error rather than an unconstrained reply. This needs a provider built to accept them; older packages reject a grammar outright.
- **Fixed: a compiled binary printed anchored output the VM suppressed.** On the AOT streaming path, a provider's reply went straight to stdout without passing through the anchor-muting scanner, so `stream(?p, mute_on=[g])` printed a region that `jade run` hid. Both engines now run the reply through the same scanner. This was invisible to the parity gate, which reached the daemon on that path rather than a provider.
- **`InferenceRequest` is four fields instead of eleven.** It stopped being a wire type, so what the language cannot express no longer exists on it: `prompt`, `grammar`, `anchor`, `stop_anchor`. `model`, `max_tokens`, `keep_anchors`, and `trust` were already pinned to fixed defaults; `count_only`/`stats_only`/`health_only` lost their callers when the `llm` package was removed in v1.1.21; and `rlm` was never set by the language at all.
- **The parity gate's stand-in daemon became a stand-in provider.** `scripts/fake-jaded.py` served canned responses over a socket and had to be restarted between the VM and AOT runs of each example. `scripts/fake-provider.jde` is a Jade `--lib` the gate builds once and installs into a throwaway slot, so `examples/llm` now runs through the exact path a released binary takes. All 59 parity examples pass on both engines.
- **The two global allocators moved into `src/alloc/`, with tests.** `src/pool_alloc.rs` and `src/alloc_profile.rs` were loose files at the crate root; they are now `alloc::pool` and `alloc::profile` under one module whose docs carry the rule they exist to enforce — a global allocator is declared in the binary, never in `jade-runtime`, because a package linking a second instance is what corrupted the heap when this was mimalloc. Ten unit tests were added where there were none: the pool wrapper is checked for alignment, for actually delegating to the pool free list (a freed block must come back at the same address), for `alloc_zeroed` clearing a recycled block's stale bytes, and for preserving contents when a realloc crosses a size class; the profiler is checked for its bucket arithmetic at both ends and for its alloc/free/live-byte accounting, including that a realloc is counted as a free plus an alloc. No behavior change — the allocators themselves are byte-for-byte what they were.
- **Contributor documentation: a README in every major directory.** No language-visible change. Each major subtree of the repo now carries a `README.md` explaining what it is, why it was built that way, what each file does, and which other parts of the tree depend on it — the compiler pipeline (`frontend`, `compiler`, `bytecode`, `vm`, `aot`, `build`), both runtimes (`runtime`, `runtime_aot`), the language surface (`builtins`, `native`), the LLM path (`llm`, `providers`), the project tooling (`cli`, `project`, `pkg`, `cache`), and the non-code trees (`examples`, `scripts`, `bench`, `docs`, `design`). The root `README.md` is unchanged and remains the entry point for people working on the compiler.

## v1.1.28

- **REPL stops echoing redundant/void output.** An expression that prints as it evaluates — a bare `?p` (already suppressed) and now `stream(...)` — no longer has its result echoed again after the live output. And a void result is no longer echoed: `print("hi")` used to print `hi` then a stray `nil`; bare `nil` and any nil-returning call now display nothing.

## v1.1.27

- **Fixed a REPL defect: a string result printed its internal representation.** In the REPL, a bare expression evaluating to a string echoed the Rust struct `JStr { text: "…", trust: 0 }` instead of the string — most visibly with `stream(?p)`, but any bare string result was affected. It now echoes the string quoted (e.g. `"hey there!"`), Debugging the string contents rather than the internal tagged-string struct.

## v1.1.26

- **Fixed `jade upgrade` — it never worked.** It pointed at a nonexistent repo (`joericks1998/jade-os`, which 404s, so it silently reported "no releases published yet"), looked for a wrongly-named asset (`jade-<pkg-platform-tag>` like `jade-darwin-aarch64`, not the published `jade-macos-arm64.tar.gz`), and would have written the downloaded tarball straight to the binary path without extracting it. It now targets the real repo, matches the published archive name (`macos-arm64`/`linux-x86_64`), and extracts + installs the binary **plus** the runtime archives (`libJadeRuntime.a`/`libjade_runtime.a`) and bundled providers into `<prefix>/lib/jade/`, mirroring the installer, with an atomic binary replace. Note: any jade older than this still carries the broken `upgrade`, so reinstall once via `jadelang.org/install.sh` to reach a version whose `jade upgrade` works.

## v1.1.25

- **Provider-package `?p` now works in AOT-compiled binaries, not just the VM.** A provider is a compiled Jade `--lib` package (dovata's `anthropic`/`openai`) that exposes `infer(request) -> [Frame]` / `configure(opts)` and does its own HTTP to the vendor API. `jade run` already drove these; now a `jade build` binary does too — the C runtime loads the active provider through the existing native-package machinery (`jrt_native_load`/`jrt_native_call`, reusing the v1.1.24 dict/array FFI marshalling), calls `configure` with the stored credential, calls `infer({prompt})`, and folds the returned frame dicts into the response text. Each prompt path routes to the provider when one is active, else the daemon. An `Error` frame (e.g. a cloud auth failure) raises a catchable Jade error in both engines. Verified end-to-end on VM and AOT against the live Anthropic API. (The earlier `ovata_provider_*` cdylib ABI the language briefly targeted is gone — that ABI is the daemon's; the language hosts providers as Jade packages.)

## v1.1.24

- **Cloud inference with no daemon — `?p` through a provider package.** The `1.1.21` split made the daemon the only way to run inference, which effectively gated the language on local-model hardware. Inference providers (Anthropic, OpenAI) are now installable `.so` packages the language loads **in-process**, so `?p` works on any machine with just an API key — no daemon, no GPU. A provider is a library implementing `ovata-infer-protocol`'s `Provider` ABI (the same one the daemon hosts); the runtime loads the single active provider from `$HOME/.jade/provider/active/`, hands it an opaque credential blob, and decodes the same wire frames as the daemon path. It is deliberately **provider-blind** — one library, one config, no vendor knowledge in the language or the compiler. Works identically under `jade run` and `jade build`: the driver is single-sourced in `jade-runtime` (a `jrt_provider_*` C surface mirrors the daemon's `jrt_ipc_*`), and each prompt path routes to the provider when one is active, else the daemon. Providers ship with the toolchain; if none is active and no daemon is running, `?p` raises `NoInferenceBackend` (renamed from `MissingApiKey`) pointing at `jade register`.
- **`jade register` / `jade use` — choose and configure a provider.** `jade register [provider]` picks an inference provider (interactively when unnamed) and stores its API key under `~/.jade` (`0600`); `jade use <provider>` switches the active one without re-entering the key. A key can also come from `<PROVIDER>_API_KEY` in the environment, which is never written to disk. Exactly one provider is active at a time. `jade env` now reports the active provider, whether a key is set, and what's installed. The installer (`jadelang.org/install.sh`) offers to run `jade register` at the end instead of the removed `jade configure`.

## v1.1.23

- **`std/uhttp` now works in AOT-compiled binaries, not just the VM.** HTTP-over-Unix-socket was a VM-only package — `jade build` on a program using `uhttp.*` failed to lower it. The request transport core moved down into `jade-runtime` (one copy, shared by both engines, mirroring how `std/http` is structured), and a `jrt_uhttp_{get,post,put,delete,head}` C-ABI surface was added so native binaries reach it directly. `uhttp.get`/`post`/`put`/`delete`/`head` return the same `{status, body}` dict under `jade run` and `jade build`, with identical output verified against a live socket; a transport failure raises in both engines. Streaming (`uhttp.stream`) stays VM-only — it invokes a Jade handler per line and so can't be a pure native symbol.
- **Native packages can now exchange dicts and arrays, and no longer corrupt memory.** The native FFI (`jade build --lib` packages) previously marshalled only scalars — a dict or array argument silently became `nil`, and a dict/array return was dropped. The `JadeVal` ABI now carries arrays and dicts as nested trees, deep-copied at the boundary through a process-shared allocator, so collections (including nested ones) round-trip in both directions under both `jade run` and `jade build`. Structs still cross as unsupported (`nil`). Two memory-safety bugs are fixed with it: loading a package under the VM used to hang on exit, and dict/array results used to come back as corrupted memory — both were the same root cause (see below).
- **Replaced the mimalloc global allocator with our own, host-only pool.** mimalloc was declared in `jade-runtime`, which is *also* statically linked into every native package, so a process that loaded a package held two allocator instances whose duplicate symbols interposed across the boundary — corrupting the heap and deadlocking tokio's shutdown. It's gone. In its place the `jade` VM binary now installs a segregated free-list pool (size classes 8–4096 B, system fallback) declared **in the binary, never in `jade-runtime`** — so it applies only to the interpreter process and can never reach a loaded package. It recovers the ~2× on allocation-heavy code (`bench/alloc_heavy.jde`: 0.26s → 0.13s) without the corruption. The pool is shared with the AOT object path (`gc::leak_obj`) too; a `--features alloc-profile` build adds a size-class allocation profiler.
- **AOT: region allocation for non-escaping arrays.** Allocation-bound compiled code was dominated by collection churn (a 3M-iteration array loop spent ~97% of its time allocating). A new type-aware escape analysis on the typed IR proves which array literals never leave their region, and the AOT backend bump-allocates those in a per-frame arena — reset in bulk at each loop iteration and function return, with no per-object `malloc`/`free` and no refcounting. Sound by construction: arena objects carry an `ObjHeader` flag that makes the refcount ops no-op on them, so an arena pointer can flow through refcounted registers and never be freed by the collector — only the region reset frees it. v1 targets arrays of immediate scalars (`[i, i+1, i+2]`); a 3M-iteration non-escaping array loop drops from **0.15s to 0.06s (~2.5×)**, verified leak- and double-free-free by the heap instrument, with identical VM/AOT output. Constant-index literals (`[1,2,3][0]`) still fold to their elements with zero allocation.
- **Performance.** Backend/runtime optimizations with no language-visible change:
  - **AOT scalar specialization.** The native backend treated every value as a tagged word, so integer-only code (recursive `fib`, whose parameters are untyped) paid a runtime call per operation. Added the LLVM `-O2` pipeline, inlined an `is_heap` guard around reference-count ops so ints/bools/nil skip the runtime call, and inlined an int fast-path into dynamic `add`/`sub`/`mul` and compare. `fib(40)` compiled: **17.0s → 2.3s (7.4×)**, and native now beats Python where it was 2× slower. Overflow still raises exactly as the VM does.
  - **VM: FxHash globals.** Hash the interpreter's `globals` map with `FxHash` instead of SipHash — variable names are short internal keys, not attacker-controlled inputs. `fib(34)` under `jade run`: ~25% faster.
  - **VM: borrow the callee.** `Call` now borrows the `Arc<CompiledFn>` out of its slot for plain-function calls instead of cloning the whole value, avoiding an atomic refcount bump+drop per call (~10% more).
  - Internal: `src/vm/mod.rs` was split from a 3119-line monolith into focused modules (`dispatch`, `call`, `coerce`, `llm_prompt`, `ops`, `value`, `state`, `chunk`, `async_tasks`, `exceptions`).

## v1.1.22

- **Unified module imports; removed quoted file imports and the `as` alias (breaking).** There is now one import form: a `use` statement names a **module** with `::` notation (or a bare name) and binds its last path segment. A bare name resolves to a **sibling `.jde` file** (`use utils` → `./utils.jde`), a `::` path descends into subdirectories (`use sub::helper` → `./sub/helper.jde`), and the first segment naming a registered `[lib]` or an installed dependency resolves that instead. The quoted-path form (`use "lib.jde" as lib`) and the `as` alias are rejected at compile time (`QuotedImport` / `ImportAlias`) with a message pointing at the new syntax. Parent/cross-directory imports (`../`) are no longer expressible as a module path — register those directories as a `[lib]`, which anchors resolution at the project root. Resolution is identical in the VM and the AOT build.

## v1.1.21

- **Moved all remaining inference config to the daemon; the language is a pure wire-protocol client.** It no longer counts tokens (`llm.count_tokens`, `llm.total_tokens`, and the `token_count` state are gone), no longer caps generation length (`llm.set_max_tokens` is gone; requests send `max_tokens: 0`, so the daemon owns the budget), no longer tracks or selects a model (`llm.model()` is gone; requests send an empty `model`, so the daemon uses its configured/loaded one), no longer toggles anchor visibility (`llm.keep_anchors` is gone; requests send `keep_anchors: false`), and no longer re-asks the model on a coercion miss. A typed dereference (`?p |> Type`) is now single-shot — grammar-constrained sampling already forces the reply into the target shape — and raises `PromptOverflow` immediately if it doesn't coerce, in both the VM and the AOT engine. **The `use llm` package is removed entirely** — its remaining function, `llm.health()`, and the earlier `llm.model`/`keep_anchors`/`set_max_tokens`/`count_tokens`/`total_tokens`/`profile`/`find_tool_call`/`find_tool_calls`/`tool_grammar` are all gone. Running inference is language syntax now (`?p`, `?p |> Type`); the model-specific pieces ship with each model as Jade packages on the daemon side. The `JADE_MAX_RETRIES` env var and `max_retries` `jade.toml` key are removed

## v1.1.20

- **Dropped Windows support.** Jade is now macOS and Linux only. The toolchain is built on Unix domain sockets — `jade build` talks to the build daemon and the `jade` inference provider talks to the LLM daemon that way — so a Windows build was only ever the language with its interesting half stubbed out. Building for a non-Unix target now fails immediately with an explanatory error rather than producing a degraded binary. The `jade-windows-x86_64.zip` release artifact is no longer published; on Windows, use WSL2
- **Removed the build daemon.** `jade build` now compiles in-process. The daemon existed to keep LLVM out of the `jade` binary while code generation lived in a separate repository; once `src/aot/` and the C runtime moved here, its only remaining job was forwarding a request to a function this crate already exported — and a daemon built from an older commit could resolve imports differently from the CLI calling it, silently. LLVM 18 is now a build-time requirement for the toolchain (`LLVM_SYS_180_PREFIX`); running a released binary needs nothing installed. The `codegen` Cargo feature is gone, and `jade env` no longer reports daemon reachability
- Linux releases are now **glibc** (`x86_64-unknown-linux-gnu`) rather than musl: LLVM's prebuilt distributions are glibc-based, so a static musl build would mean sourcing or building a musl LLVM
- **Added a package manager.** `[dependencies]` in `jade.toml`, pinned by `jade.lock`, installed into a project-local `libs/`. Dependencies are prebuilt native shared libraries sourced from a URL or a local path — there is no registry, and so no transitive resolution and no version ranges. `jade pkg add/remove/install/update/list`; `jade run` and `jade test` install anything missing. A `{platform}` URL records an artifact per platform in the lock so a lock committed from macOS installs and verifies on Linux CI, while only the matching artifact is ever downloaded. Every artifact is checksum-verified on every install
- Dependencies are imported by **bare name** — `use fastmath` — resolving through the same `[lib]` machinery, so behavior is identical in the VM and the AOT build. A name matching both a library and a sibling `.jde` file is a hard error naming both
- Plain **C libraries** can be dependencies: `abi = "c"` plus a symbol table generates and compiles a binding shim, so a library exporting no `jade_pkg_init` still works. Requires a C compiler at install time
- **`jade build --lib`** compiles a Jade file to a shared library exporting `jade_pkg_init` — a package other Jade projects can depend on. `--export` narrows the binding set; the default is every top-level function

## v1.1.19

- Added the **`std/uhttp`** package — an HTTP/1.1 client that speaks over a **Unix domain socket** rather than a TCP host, for talking to local daemons such as the Docker Engine API (`/var/run/docker.sock`) and other socket-backed OS services. Mirrors `std/http`: `uhttp.get`/`post`/`put`/`delete`/`head` return the same `{status, body}` dict and accept an optional trailing `headers` dict
- The target is a single pseudo-URL of the form `unix://<socket-path>:<request-path>` (e.g. `unix:///var/run/docker.sock:/v1.43/containers/json`); the socket path runs to the first `:` after the scheme, the rest is the request path (defaulting to `/`)
- The transport is hand-framed HTTP/1.1 written directly onto a `UnixStream` — no new dependencies. Response framing honors `Content-Length`, `Transfer-Encoding: chunked` (de-chunked), and read-to-EOF on `Connection: close`. Unix-only; a missing socket, malformed pseudo-URL, or connection failure raises an `IoError`
- Added **`uhttp.stream(url, handler, headers?)`** for long-lived streaming endpoints (Docker `/events`, `/logs?follow=1`, image-pull progress). A worker thread owns the socket and decodes the body incrementally; the VM invokes the Jade `handler` once per newline-delimited line (mirroring the LLM token-stream drain pattern). The handler returning `false` stops the stream and closes the socket; `stream` returns the HTTP status once the stream ends

## v1.1.12

- Expanded the built-in `llm` package to expose the inference daemon's model profiles, tool-call helpers, protocol controls, and health to Jade programs. The package stays decoupled from the daemon — the Unix socket (`~/.jade/llm.sock`) is the only contract; jadelang implements the wire format itself, drift-guarded by a golden-bytes test
- Added **model profile** introspection — `llm.model()` returns the active model name; `llm.profile()` returns the model's token/tool vocabulary (tool-call delimiters, name field, special-token spans) as a dict. Profiles are selected by the model name the daemon reports
- Added **tool-call helpers** — `llm.find_tool_call(text)` returns the first tool call in a response as `{name, args}` (or `nil`); `llm.find_tool_calls(text)` returns all of them; `llm.tool_grammar()` returns the canonical tool-call GBNF. All resolve tool-call delimiters from the active model's profile, so they work across models. The canonical grammar is checked in at `grammars/tool_call.gbnf`
- Added **protocol controls** — the wire request now carries `keep_anchors` (toggle via `llm.keep_anchors(b)`, making tool-span boundaries observable in-band) and `trust` (prompt provenance), matching the daemon's request schema
- Added **daemon lifecycle** — `llm.health()` returns a daemon health snapshot (`status`, `model`, `model_loaded`, `uptime_secs`, `protocol_version`) via a new `health` op and structured-JSON response frame

## v1.1.11

- Improved type inference for values read out of a dict. A `let`-bound homogeneous dict literal now records its value type, so indexing it (`d["k"]`) infers that concrete type instead of `Unknown`. This lets the native (AOT) backend pick the right print/format codegen for, e.g., `bool` values stored in a dict; the VM is unaffected (it dispatches on runtime tags)
- Fixed a regression in unary `!` type inference. The v1.1.10 logical-operator fix typed *every* `!expr` as `bool`, which incorrectly accepted `!` on a known non-`bool` operand such as `!1` (this should be a `TypeError`). `!x` now short-circuits to `bool` only when the operand type is `Unknown` (e.g. `!method_call(x)` on an untyped value, where native codegen emits an `i1`); a known non-`bool` operand once again reports a `TypeError`. `&&` and `||` are unaffected — they continue to yield `bool` whenever an operand is `Unknown`

## v1.1.10

- Fixed a native build failure (LLVM verification error) when a function returns a logical expression with an untyped operand — `!x`, `a && b`, and `a || b` are now always typed as `bool` (matching the `i1` codegen emits), even when an operand is `Unknown` such as a method call on an untyped parameter. Previously these inferred `int`, mismatching the generated function signature. Mirrors the earlier comparison-operator fix

## v1.1.9

- **Breaking:** module-path imports now use `::` as the separator instead of `.` — `use std::math`, `from std::math use floor`, `use utils::math` for `[lib]` libraries. The `.` form is no longer accepted in module-path position (`.` is reserved for field and method access on values); `use std.math` is now a parse error
- Namespaced decorators also use `::` — `@tools::register` instead of `@tools.register`
- Quoted file-path imports (`use "lib.jde" as lib`) are unchanged
- Added `null` as a third spelling of `nil` — `nil`, `None`, and `null` are interchangeable aliases for the same value; they compare equal and may be used as literals, default parameter values, and type annotations

## v1.1.8

- Native code generation moved out of the `jade` binary — `jade build` now runs the language frontend (lex → parse → type-infer → typed IR) and hands the typed program to the **build daemon** over `$HOME/.jade/build.sock`, which performs import resolution, code generation, and linking. The in-process LLVM backend and the `llvm` Cargo feature were removed; `jade env` now reports build-daemon reachability instead of LLVM status
- Stdlib package imports must now use dot notation — `use std.math`, `use std.fs`, etc.; string-literal forms (`use "std/math"`) are now a compile-time error. Applies to both `use` and `from … use` forms
- File-path imports now require an alias — `use "lib.jde" as lib`; bare string imports without `as name` are now a compile-time error
- Native packages declared in `jade.toml [native]` now require an `alias` field specifying the global binding name
- Fixed: functions exported from imported modules can now access stdlib packages the module imported (e.g. `use std.fs` in a module is visible when module functions are called in the parent scope)
- Improved error messages — type errors now include the actual type of the offending value; heterogeneous array literals, nested function definitions, and non-string prompt struct fields each emit a dedicated error
- Added empty struct test coverage (`struct Unit {}`)

## v1.1.7

- Added `std/sh` package — execute shell commands from Jade via `sh.exec`, `sh.run`, and `sh.output`
- Added `std/json` package — parse JSON strings into Jade values and serialize Jade values back to JSON with `json.parse`, `json.stringify`, and `json.stringify_pretty`
- Added `std/env` package — read and write environment variables (`env.get`, `env.set`), inspect command-line arguments (`env.args`), and get the working directory (`env.cwd`)
- Added `std/path` package — cross-platform path manipulation: `path.join`, `path.basename`, `path.dirname`, `path.ext`, `path.stem`, `path.abs`, `path.is_abs`
- Added `std/random` package — random number generation with `random.int`, `random.float`, `random.choice`, `random.shuffle`, and a seedable global RNG via `random.seed`

## v1.1.6

- Added `input(prompt?)` built-in — reads a line from stdin; the optional `prompt` argument prints to stdout without a trailing newline before reading. Returns an empty string on EOF.
- Added `write(str)` built-in — prints to stdout without a trailing newline and flushes immediately (complements `print`, which adds `\n`)
- Fixed array mutation semantics — mutations to an array are now visible through all aliases (reference semantics); previously mutations did not propagate to other variables pointing at the same array
- Added `llm.set_max_tokens(n)` via `use "llm"` — configure the maximum token limit for LLM inference at runtime
- Extended LLVM native codegen: typed `try`/`catch` arms and struct method calls (`obj.method(args)`) now compile and run correctly in native binaries

## v1.1.5

- Added single-quote string literals — `'hello'` and `'''triple'''` are now equivalent to their double-quote forms; `f'…{expr}…'` f-strings work too
- Fixed `jade.toml` config loading — a config-only file with only a `[model]` section (no `[project]`) is now correctly picked up
- Added `jade upgrade` command — downloads and atomically replaces the binary from the latest GitHub release

## v1.1.4

- Added `async fn` definitions and `await` expressions — concurrent LLM inference via `await` on prompt dereferences
- Added Jade OS as a supported LLM backend provider
- Added comprehensive error handling for async tasks — panics from spawned tasks produce `AsyncPanic` errors with source location
- Switched TLS backend to `rustls` (no OpenSSL dependency)

## v1.1.3

- Added official install script at `https://jadelang.org/install.sh` — detects OS and architecture, downloads the correct prebuilt binary, and installs to `/usr/local/bin/jade`
- Added Windows prebuilt binary: `jade-windows-x86_64.exe` available from the GitHub Releases page
- Updated documentation installation page to document the install script and Windows download path

## v1.0.9

- Added `try`/`catch`/`raise` exception handling — raise any value as an exception, catch by struct type name or with a catch-all arm, nested `try`/`catch` blocks, built-in runtime errors (division by zero, type errors, etc.) are automatically catchable
- Upgraded CLI to full subcommand structure: `jade run`, `jade check`, `jade build`, `jade repl`, `jade test`, `jade fmt`, `jade env`, `jade cache`, `jade model`, `jade new`, `jade init`
- Fixed implicit function return: the last bare expression in a function body is now returned automatically without needing an explicit `return` keyword

## v1.0.8

- Added anonymous closures: `|x| x * 2` (inline expression body) and `|x| { … }` (block body) with environment capture at creation time
- Added `for` loops: `for x in array { … }` iteration over arrays (via bytecode VM)
- Added `dict` type: dictionary literals (`{"key": value}`), key access (`d["key"]`), key assignment, and `len` support
- Added `use "path.jde"` for multi-file imports
- Added bytecode compiler and VM — programs now run through type inference, bytecode emission, and a register-based VM
- Added multi-level AST and TIR caching to skip redundant compilation passes

## v1.0.7

- Added `str` type: string literals, triple-quoted strings, concatenation with `+`, character indexing, equality and lexicographic ordering
- Added f-string interpolation: `f"…{expr}…"` and `f"""…{expr}…"""`
- Added array literals (`[1, 2, 3]`), index access (`arr[i]`), and index assignment (`arr[i] = expr`)
- Added `print` and `len` built-in functions
- Added pipe operator `|>` for chaining function calls
- Added `interface` definitions and `extend Type: Interface` conformance checking
- Added `elif` clause for chained conditionals
- Added `jade configure` command for LLM backend configuration
- Added `prompt` declarations and `?` dereference for LLM inference

## v1.0.6

- Added `struct` definitions with named fields, field access, and field mutation
- Added `extend` blocks for attaching methods, with `self` binding
- Added bare variable assignment (`x = expr`)
- Added `while` loops with boolean condition

## v1.0.5

- Added `struct` definitions with named fields
- Added struct instantiation with `TypeName { field: value, … }` literals
- Added field access (`obj.field`) and field mutation (`obj.field = expr`)
- Added `extend` blocks for attaching methods to struct types
- Added method calls (`obj.method(args)`) with automatic `self` binding
- Added bare variable assignment (`x = expr`) as an alternative to `let` rebinding

## v1.0.4

- Added `while` loops with boolean condition

## v1.0.3

- Added `fn` definitions with parameter lists and `return`
- Added function calls as first-class expressions
- Added first-class function values — functions can be assigned to variables and passed as arguments
- Added recursion — functions can call themselves
- Added `if`/`else` control flow

## v1.0.2

- Modulus operator: `%`
- Bitwise operators: `&`, `|`, `^`, `<<`, `>>`
- Unary bitwise NOT: `~`
- Float literals (`f64`) and unary negation for floats
- Boolean literals: `true`, `false`
- Logical operators: `&&`, `||`, `!` with short-circuit evaluation
- Comparison operators: `==`, `!=`, `<`, `>`, `<=`, `>=`
- Runtime errors: remainder by zero, invalid shift amount (negative or ≥ 64)

## v1.0.1

- Initial interpreter release written in Rust
- `let` variable declarations with arithmetic expressions
- Operators: `+`, `-`, `*`, `/`
- Automatic semicolon insertion — no semicolons required
- Runtime errors: undefined variable, division by zero
- CLI: `jade <file>`, `--verbose`, `--help`
