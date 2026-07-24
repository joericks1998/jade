---
id: async
title: Async / Await
sidebar_label: Async / Await
---

Jade supports concurrent LLM inference through `async fn` definitions and `await` expressions. Multiple prompt dereferences can be in-flight at the same time, reducing total wall-clock time when a program sends several independent prompts.

## Overview

By default, a `?p` dereference blocks until the model responds. When many independent prompts need answers, blocking on each one in sequence is wasteful. `async fn` lets you express that a function's body may be deferred — the call returns a *future* immediately, and execution of the body proceeds concurrently with the rest of the program. The result is only demanded when you `await` it.

Under `jade run`, async functions are dispatched onto the Tokio runtime. Multiple async calls run concurrently: network round-trips to the LLM overlap instead of stacking. Under the tree-walk evaluator (REPL), async functions execute synchronously one at a time, and a warning is printed to stderr.

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
    return await ?p
}

// Both calls start immediately — bodies run concurrently.
let a = fetch("What is the capital of France?")
let b = fetch("What is the capital of Germany?")

// await blocks here, but only until each result is ready.
print(await a)   // Paris
print(await b)   // Berlin
```

:::note
Nested `async fn` definitions (an `async fn` inside another function body) are rejected at parse time with a `NestedFunction` error.
:::

## The `await` Expression

`await` is a prefix expression that blocks the current context until the future resolves, then produces the future's value.

```jade
let result = await <expr>
```

`<expr>` must evaluate to a future (the return value of an `async fn` call). Applying `await` to any other value raises `NotAFuture`. Awaiting the same future twice raises `DoubleAwait`.

### Awaiting inside an async function

You can `await` a prompt dereference directly inside an `async fn` body to suspend that task until the model responds, while other tasks continue running.

```jade
async fn summarize(text) {
    prompt p = "Summarize in one sentence: " + text
    return await ?p
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

- **Call phase** — the `async fn` body is dispatched to the Tokio runtime. The call expression returns a future handle immediately.
- **Concurrent phase** — the caller continues executing (starting more async calls, computing other values) while the async bodies run in the background.
- **Await phase** — `await future` suspends the caller until that specific task finishes and returns its value.

:::warning
**REPL limitation:** The tree-walk evaluator used by the interactive REPL does not run a Tokio runtime. Async functions execute synchronously in that context, and a warning is printed to stderr. Use `jade run` for true concurrent execution.
:::

## Common Patterns

### Fan-out: send N prompts, collect all results

```jade
async fn ask(q) {
    prompt p = q
    return await ?p
}

let r1 = ask("Name a red fruit.")
let r2 = ask("Name a blue fruit.")
let r3 = ask("Name a green fruit.")

print(await r1)
print(await r2)
print(await r3)
```

All three prompts are dispatched before any `await` is reached. The program waits for each in order of collection, but the LLM calls run in parallel.

### Async with typed dereference

```jade
async fn count_words(sentence) {
    prompt p = "How many words in: " + sentence + "? Reply with only the number."
    return await ?p |> int
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
    return await ?p
}

fn print_result(future) {
    print(await future)
}

let f = get("What year did WWII end?")
print_result(f)
```

## Error Reference

| Error | Cause |
|-------|-------|
| `NotAFuture` | `await` applied to a value that is not a pending async result (e.g., an `int` or `str`) |
| `DoubleAwait` | The same future was awaited more than once — a future is consumed on first await |
| `AsyncPanic` | A spawned async task panicked internally; the panic message and source span are captured and reported |
| `PromptOverflow` | Inside an async task, a typed dereference exhausted all retries — same as in synchronous code |
| `InferenceError` | HTTP or API error from the provider inside an async task — propagated to the awaiting call site |

## Related Pages

- [Functions](functions) — regular `fn` syntax, closures, first-class functions
- [LLM Integration](llm) — `prompt` declarations, `?` dereference, typed coercion, configuration
- [Exceptions](exceptions) — how runtime errors surface and how to handle them
