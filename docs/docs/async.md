---
id: async
title: Async / Await
sidebar_label: Async / Await
---

Jade runs LLM calls concurrently through `async fn` definitions and `await` expressions. Several prompt dereferences can be in flight at once, so a program that sends several independent prompts finishes sooner.

## Overview

By default, a `?p` dereference waits until the model responds. Waiting on each of many independent prompts, one after another, wastes time. `async fn` lets you say that a function's body can be put off until later. The call returns a *future* right away, and the body runs alongside the rest of the program. You only ask for the result when you `await` it.

Under `jade run` and in a compiled binary, async functions go to a real task runtime. Several async calls run at once, so network round-trips to the LLM overlap instead of stacking up end to end.

:::note
`?p` already waits for the model on its own. In a plain `fn` it blocks the thread, and in an `async fn` it suspends only that task. Either way, `?p` produces the reply, not a future. Do not write `await ?p`, which is a compile error reading `type mismatch: expected future, got str`. `await` is for the value an `async fn` call returns. See [LLM Integration](llm) for the full rule.
:::

## Defining an `async fn`

Put `async` in front of `fn` to mark a function as asynchronous. The rest of the syntax matches a regular function.

```jade
async fn <name>(<params>) {
    <body>
    return <expr>
}
```

Calling an `async fn` does *not* run its body right away. It returns a *future*, which stands for a computation still in progress. Under `jade run`, the body starts running in the background at once.

```jade
async fn fetch(q) {
    prompt p = q
    return ?p
}

// Both calls start at once, and the bodies run concurrently.
let a = fetch("What is the capital of France?")
let b = fetch("What is the capital of Germany?")

// await waits here, but only until each result is ready.
print(await a)   // Paris
print(await b)   // Berlin
```

:::warning
Declare `async fn` at the top level. Nesting one is a compile error reading `function definitions cannot be nested`, exactly as it is for `fn`. Before v1.3.3 it parsed and then failed at run time, because the inner function cannot see the outer one's parameters.
:::

## The `await` Expression

`await` is a prefix expression. It waits until the future finishes, then produces the future's value.

```jade
let result = await <expr>
```

`<expr>` must produce a future, which means the return value of an `async fn` call. When the compiler can already see the type, `await 5` fails at compile time with `type mismatch: expected future, got int`. When it cannot see the type, such as with an untyped parameter, the same mistake shows up at run time as `NotAFuture`. Awaiting the same future twice raises `DoubleAwait`, because the first await consumes it.

### Prompts inside an async function

A `?p` dereference inside an `async fn` suspends only that task while it waits for the model. Other tasks keep running. No `await` is involved.

```jade
async fn summarize(text) {
    prompt p = "Summarize in one sentence: " + text
    return ?p
}
```

### Awaiting at the top level

`await` can also appear at the top level, to collect results from async calls you started earlier.

```jade
let t1 = summarize("The quick brown fox...")
let t2 = summarize("Four score and seven years...")

let s1 = await t1
let s2 = await t2
print(s1)
print(s2)
```

## Concurrency Model

Jade's async model follows one simple rule. Calling an `async fn` starts the work, and `await` collects the result. Keeping the two steps separate is what lets the work overlap.

- *Call phase.* The `async fn` body goes to the task runtime, and the call expression returns a future right away.
- *Concurrent phase.* The caller keeps running, starting more async calls or computing other values, while the async bodies work in the background.
- *Await phase.* `await future` pauses the caller until that one task finishes, then gives back its value.

## How many tasks run at once

A task is a real operating system thread, so something has to bound how many exist. That bound is one number, and a program can read it and change it.

```jade
print(max_tasks())        // 32, the default

set_max_tasks(8)
print(max_tasks())        // 8
```

The default is 32 on every machine. A task usually spends its time waiting on a model or a socket rather than using a core, so the number is not tied to how many cores you have, and a fan-out takes the same number of waves on a laptop as on a build server.

`set_max_tasks` answers with the value that actually took effect, because a request outside `1` to `512` is clamped rather than refused.

```jade
print(set_max_tasks(9999))   // 512
print(set_max_tasks(0))      // 1
```

Raising the limit is worth it when your tasks mostly wait. Sixteen requests against a slow server finish in one wave at the default and in two waves at `set_max_tasks(8)`. Lowering it is worth it when a service you call cannot take the load, which is what the number is really for.

A task that is waiting inside `await` does not count against the limit, so a task that awaits another cannot deadlock against it even at `set_max_tasks(1)`.

Both engines obey the same number. `jade run` and a binary from `jade build` run the same fan-out the same number of tasks at a time.

## No shared mutation

Tasks in Jade run on a *shared heap*, so two tasks can see the same array or the same struct. That makes passing data around cheap, and it also makes a data race possible. The compiler refuses the race outright.

*A task may not mutate anything it did not create.* Jade checks the rule at compile time, before your program runs. Here is what it covers:

| What a task does | Verdict |
|------------------|---------|
| Assigns to a top-level variable | Rejected |
| Assigns to a field of a struct passed in | Rejected |
| Assigns into an array or dict passed in | Rejected |
| Calls a mutating method (`push`, `pop`, `insert`, `remove`, `clear`, `sort`, `reverse`, `extend`, `set`, `update`) on a collection passed in | Rejected |
| Reads anything | Fine |
| Mutates something it allocated itself | Fine |

The check follows calls, so hiding the mutation inside an ordinary helper does not get around it.

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

Take the help line literally, because it describes the whole design. Values go in as parameters and results come back through the future. The answer then no longer depends on which task happened to run first.

```jade
async fn bump(n) {
    return n + 1
}

let results = join(bump(0), bump(10), bump(100))
print(results[0])   // 1
print(results[1])   // 11
print(results[2])   // 101
```

A task may freely mutate anything it allocated itself, because nothing else can see it:

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
*REPL limitation.* `async` and `await` do not carry across `jade repl` entries. An `async fn` defined at one prompt cannot be awaited at the next, and the await fails with `'await' applied to a non-Future value`. Use `jade run` or `jade build` instead.
:::

## Common Patterns

### Fan out: send several prompts, then collect every result

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

All three prompts are sent before the program reaches any `await`. It then waits for each one in the order you collect them, but the LLM calls themselves run in parallel.

### Collecting many results with `join`

`join` waits for several futures at once and returns their results as an array, in argument order. Every task is already running by the time you call `join`, so `join` collects results rather than starting work.

```jade
async fn square(n) {
    return n * n
}

let squares = join(square(2), square(3), square(4))
print(squares[0])   // 4
print(squares[1])   // 9
print(squares[2])   // 16
```

`join` takes any number of futures. Like `await`, it consumes each one, so a future passed to `join` cannot be awaited again.

If one task raises, `join` passes that exception on to the call site. The other tasks are *not* cancelled, and they run to completion.

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

A typed dereference such as `|> int` or `|> bool` works inside an async function.

### Passing futures to functions

A future is a first-class value. You can pass it to another function, store it in a variable, or return it from a function.

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

A future can also live in an array, and you do not have to `await` in the order the tasks started:

```jade
async fn work(n) {
    return n * 2
}

let fs = [work(1), work(2), work(3)]
print(await fs[2])   // 6
print(await fs[0])   // 2
```

Printing a future is allowed. It shows as `<future>`, because it has no meaningful text form until it finishes.

## Error Reference

| Error | When | Cause |
|-------|------|-------|
| `type mismatch: expected future, got …` | Compile time | `await` applied to a value the compiler already knows is not a future, including `await ?p` |
| `SharedMutation` | Compile time | An `async fn` mutates state it did not create. See [No shared mutation](#no-shared-mutation). |
| `HandleAcrossTask` | Compile time | A `handle<T>` from a native package was passed into an `async fn` |
| `NotAFuture` | Runtime | `await` applied to a non-future whose type the compiler could not see ahead of time |
| `DoubleAwait` | Runtime | The same future was awaited or joined more than once. The first await consumes it |
| `AsyncPanic` | Runtime | A spawned async task panicked internally; the panic message and source span are captured and reported |
| `ArityMismatch` | Runtime | `max_tasks()` was given an argument, or `set_max_tasks` was given none or more than one |
| `PromptOverflow` | Runtime | Inside an async task, a typed dereference produced a reply that would not coerce to the target type. The same rule applies in synchronous code |
| `InferenceError` | Runtime | The inference provider failed inside an async task. The error passes to the awaiting call site |

An exception raised inside an async function travels out through `await`, and ordinary `try` and `catch` handle it:

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

- [Functions](functions) covers regular `fn` syntax, closures, and first-class functions.
- [LLM Integration](llm) covers `prompt` declarations, the `?` dereference, typed coercion, and configuration.
- [Exceptions](exceptions) covers how runtime errors surface and how to handle them.
