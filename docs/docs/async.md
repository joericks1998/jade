---
id: async
title: Async / Await
sidebar_label: Async / Await
---

Jade supports concurrent LLM inference through `async fn` definitions and `await` expressions. Multiple prompt dereferences can be in-flight at the same time, reducing total wall-clock time when a program sends several independent prompts.

## Overview

By default, a `?p` dereference blocks until the model responds. When many independent prompts need answers, blocking on each one in sequence is wasteful. `async fn` lets you express that a function's body may be deferred — the call returns a *future* immediately, and execution of the body proceeds concurrently with the rest of the program. The result is only demanded when you `await` it.

Under `jade run` and in a compiled binary, async functions are dispatched onto a real task runtime. Multiple async calls run concurrently: network round-trips to the LLM overlap instead of stacking.

:::note
`?p` already waits for the model on its own — it blocks the thread in a plain `fn` and suspends the task in an `async fn` — so `?p` produces the reply, not a future. Do not write `await ?p`; it is a compile error (`type mismatch: expected future, got str`). `await` is for the value an `async fn` call returns. See [LLM Integration](llm) for the full rule.
:::

## Defining an `async fn`

Prefix `fn` with `async` to mark a function as asynchronous. The rest of the syntax is identical to a regular function.

```jade
async fn <name>(<params>) {
    <body>
    return <expr>
}
```

Calling an `async fn` does *not* run its body immediately. Instead it returns a **future** — a pending computation. The body begins running concurrently in the background (under `jade run`).

```jade
async fn fetch(q) {
    prompt p = q
    return ?p
}

// Both calls start immediately — bodies run concurrently.
let a = fetch("What is the capital of France?")
let b = fetch("What is the capital of Germany?")

// await blocks here, but only until each result is ready.
print(await a)   // Paris
print(await b)   // Berlin
```

:::warning
Declare `async fn` at the top level. Nesting one is a compile error — `function definitions cannot be nested` — exactly as it is for `fn`. It parsed before v1.3.3, and then failed at run time, because the inner function cannot see the outer one's parameters.
:::

## The `await` Expression

`await` is a prefix expression that blocks the current context until the future resolves, then produces the future's value.

```jade
let result = await <expr>
```

`<expr>` must evaluate to a future — the return value of an `async fn` call. When the compiler can already see the type, `await 5` fails at compile time with `type mismatch: expected future, got int`. When it cannot (an untyped parameter, for example), the same mistake surfaces at runtime as `NotAFuture`. Awaiting the same future twice raises `DoubleAwait`: a future is consumed on first await.

### Prompts inside an async function

A `?p` dereference inside an `async fn` suspends only that task while it waits for the model. Other tasks keep running. No `await` is involved.

```jade
async fn summarize(text) {
    prompt p = "Summarize in one sentence: " + text
    return ?p
}
```

### Awaiting at the top level

`await` can also appear at the top level of a program to collect results from previously launched async calls.

```jade
let t1 = summarize("The quick brown fox...")
let t2 = summarize("Four score and seven years...")

let s1 = await t1
let s2 = await t2
print(s1)
print(s2)
```

## Concurrency Model

Jade's async model follows a simple rule: calling an `async fn` starts the work; `await` collects the result. The two operations are separate, which is what enables overlap.

- **Call phase** — the `async fn` body is dispatched to the task runtime. The call expression returns a future handle immediately.
- **Concurrent phase** — the caller continues executing (starting more async calls, computing other values) while the async bodies run in the background.
- **Await phase** — `await future` suspends the caller until that specific task finishes and returns its value.

## No shared mutation

Tasks in Jade run on a **shared heap**. Two tasks can see the same array or the same struct. That makes passing data around cheap, and it makes a data race possible — so the compiler refuses the race outright.

*A task may not mutate anything it did not create.* The rule is checked at compile time, before your program ever runs, and it covers four things:

| What a task does | Verdict |
|------------------|---------|
| Assigns to a top-level variable | Rejected |
| Assigns to a field of a struct passed in | Rejected |
| Assigns into an array or dict passed in | Rejected |
| Calls a mutating method (`push`, `pop`, `insert`, `remove`, `clear`, `sort`, `reverse`, `extend`, `set`, `update`) on a collection passed in | Rejected |
| Reads anything | Fine |
| Mutates something it allocated itself | Fine |

The check follows calls, so hiding the mutation in an ordinary helper does not get around it.

```jade
let counter = 0

async fn bump() {
    counter = counter + 1
    return counter
}

let r = join(bump(), bump())
```

That program does not compile. The error names the task, the offending line, and the fix:

```
compile error: [3:5] async function 'bump' writes to the global `counter`
  tasks run concurrently on a shared heap, so this is a data race
  help: pass the value in as a parameter and return the result instead
```

Take the help literally — it is the whole design. Values go in as parameters, results come back through the future, and the answer no longer depends on which task ran first.

```jade
async fn bump(n) {
    return n + 1
}

let results = join(bump(0), bump(10), bump(100))
print(results[0])   // 1
print(results[1])   // 11
print(results[2])   // 101
```

A task is free to mutate whatever it allocated itself, because nothing else can see it:

```jade
async fn build(n) {
    let out = []
    out.push(n)
    out.push(n * 2)
    return out
}

print(await build(3))   // [3, 6]
```

:::note
A `handle<T>` from a native package cannot cross into a task either. A handle is a pointer into a C library, and Jade cannot tell whether that library is thread-safe. Passing one to an `async fn` is a compile error:

```
cannot pass handle<File> into a task
  a handle is a pointer into a C library, and Jade cannot see what the library
  does with it or know whether it is thread-safe
  help: open the handle inside the task and close it before returning
```
:::

:::warning
**REPL limitation:** `async` and `await` do not work across `jade repl` entries. An `async fn` defined at one prompt cannot be awaited at the next — the await fails with `'await' applied to a non-Future value`. Use `jade run` or `jade build`.
:::

## Common Patterns

### Fan-out: send N prompts, collect all results

```jade
async fn ask(q) {
    prompt p = q
    return ?p
}

let r1 = ask("Name a red fruit.")
let r2 = ask("Name a blue fruit.")
let r3 = ask("Name a green fruit.")

print(await r1)
print(await r2)
print(await r3)
```

All three prompts are dispatched before any `await` is reached. The program waits for each in order of collection, but the LLM calls run in parallel.

### Collecting many results with `join`

`join` waits for several futures at once and returns their results as an array, in argument order. Every task is already running by the time `join` is called, so it collects rather than starts.

```jade
async fn square(n) {
    return n * n
}

let squares = join(square(2), square(3), square(4))
print(squares[0])   // 4
print(squares[1])   // 9
print(squares[2])   // 16
```

`join` takes any number of futures. Like `await`, it consumes each one — a future passed to `join` cannot be awaited again.

If one task raises, `join` propagates that exception to the call site. The remaining tasks are *not* cancelled; they run to completion.

```jade
struct TaskError { message }

async fn safe(n) { return n }
async fn fail()  { raise TaskError { message: "task failed" } }

try {
    let results = join(safe(1), fail(), safe(3))
} catch TaskError e {
    print(e.message)   // task failed
}
```

### Async with typed dereference

```jade
async fn count_words(sentence) {
    prompt p = "How many words in: " + sentence + "? Reply with only the number."
    return ?p |> int
}

let n = count_words("The quick brown fox")
print(await n)   // integer word count
```

Typed dereference (`|> int`, `|> bool`, etc.) works inside async functions.

### Passing futures to functions

A future is a first-class value and can be passed to another function, stored in a variable, or returned from a function.

```jade
async fn get(q) {
    prompt p = q
    return ?p
}

fn print_result(future) {
    print(await future)
}

let f = get("What year did WWII end?")
print_result(f)
```

A future can also live in an array, and `await` need not follow the order the tasks were started in:

```jade
async fn work(n) {
    return n * 2
}

let fs = [work(1), work(2), work(3)]
print(await fs[2])   // 6
print(await fs[0])   // 2
```

Printing a future is allowed — it renders opaquely as `<future>`, because it has no meaningful text form until it resolves.

## Error Reference

| Error | When | Cause |
|-------|------|-------|
| `type mismatch: expected future, got …` | Compile time | `await` applied to a value the compiler already knows is not a future — including `await ?p` |
| `SharedMutation` | Compile time | An `async fn` mutates state it did not create. See [No shared mutation](#no-shared-mutation). |
| `HandleAcrossTask` | Compile time | A `handle<T>` from a native package was passed into an `async fn` |
| `NotAFuture` | Runtime | `await` applied to a non-future whose type the compiler could not see ahead of time |
| `DoubleAwait` | Runtime | The same future was awaited (or joined) more than once — a future is consumed on first await |
| `AsyncPanic` | Runtime | A spawned async task panicked internally; the panic message and source span are captured and reported |
| `PromptOverflow` | Runtime | Inside an async task, a typed dereference produced a reply that didn't coerce to the target type — same as in synchronous code |
| `InferenceError` | Runtime | The inference provider failed inside an async task — propagated to the awaiting call site |

Exceptions raised inside an async function propagate through `await` and are caught with ordinary `try`/`catch`:

```jade
struct TaskError { message }

async fn risky(n) {
    if n < 0 {
        raise TaskError { message: "negative input" }
    }
    return n * 10
}

try {
    let r = await risky(-1)
} catch TaskError e {
    print(e.message)   // negative input
}
```

## Related Pages

- [Functions](functions) — regular `fn` syntax, closures, first-class functions
- [LLM Integration](llm) — `prompt` declarations, `?` dereference, typed coercion, configuration
- [Exceptions](exceptions) — how runtime errors surface and how to handle them
