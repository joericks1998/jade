---
id: llm
title: LLM Integration
sidebar_label: LLM Integration
---

Jade is built around first-class LLM access. A `prompt` declaration names a prompt string; the `?` operator sends it to your inference provider and returns the response. The `|>` pipe suffix coerces the response to a typed Jade value.

## Declaring a Prompt

Use the `prompt` keyword to bind a prompt string to a name. The right-hand side is any expression that evaluates to a `str`.

```jade
prompt p = "What is the capital of France?"
```

A `prompt` binding holds the prompt text — it does not call the model. The model is only called when the variable is dereferenced with `?`.

```jade
let question = "What is 2 + 2?"
prompt p = question
```

## Untyped Dereference — `?p`

Prefixing a prompt variable with `?` sends the prompt to your provider and returns the raw response as a `str`.

```jade
prompt p = "Say exactly: Hello from Jade!"
let response = ?p
print(response)
```

Each `?` dereference is an **independent, stateless request** — the language sends only that prompt, with no conversation history. Carrying context between calls is the program's job: build the prior turns into the prompt string yourself.

```jade
prompt p1 = "My name is Alice. What is 2 + 2?"
let _ = ?p1

// p2 does NOT see p1 — include the context you need in the prompt itself.
prompt p2 = "My name is Alice. What is my name?"
let name = ?p2       // "Alice"
print(name)
```

## Typed Dereference — `?p |> type`

Append `|> type` after a prompt dereference to coerce the model's response to a Jade value type. The supported target types are:

| Type | Accepted LLM output | Result |
|------|---------------------|--------|
| `int` | `"42"`, `"-7"` | `int` value |
| `float` | `"3.14"`, `"1e10"` | `float` value |
| `bool` | `"true"`, `"True"`, `"false"` | `bool` value |
| `str` | anything | `str` value (always succeeds) |

```jade
prompt p = "What is 3 + 4? Respond with only the number."
let n = ?p |> int
print(n + 1)          // 8
```

```jade
prompt p = "Is 5 greater than 3? Respond with only: true or false"
let result = ?p |> bool
if result {
    print("correct!")
}
```

:::note
`?p |> type` must be assigned to a variable — it cannot appear directly inside `print()`. Use `let n = ?p |> int` then `print(n)`.
:::

## Dereferencing a Prompt in a Field

Prefix `?` is for bare prompts. A prompt held in a struct field uses one of two
postfix forms, which put the operator next to the field it actually applies to:

```jade
struct Agent {
    prompt system = "You are a helpful assistant"
}

let a = Agent {}

let r = a.(?system)                  // explicit
let r = a~>system                    // terse — same thing
let n = a.(?system) |> int           // constraints work as usual
let d = build(cfg).agent.(?system)   // reads left-to-right; no backtracking
```

`obj.(?field)` and `obj~>field` are the same operation — `~>` is the shorter
spelling, the way C offers `p->x` for `(*p).x`. The `|>` constraint goes outside
the parens.

Plain `.` is untouched: `a.system` reads the field and yields the
undereferenced prompt value.

:::note
`?obj.field` is a **syntax error**. It reads as though `?` applies to `obj` when
it applies to `system`, so it's rejected in favor of the two forms above. So is
`obj.?field` — the parentheses are required.
:::

## Coercion Failure

A typed dereference is single-shot: grammar-constrained sampling already forces
the model's reply into a shape the target type accepts. If the reply still can't
be coerced, Jade raises a `PromptOverflow` runtime error naming the prompt
variable. The language does not re-ask — any retry policy is the provider's to own.

```jade
prompt p = "Pick a lucky number."
let n = ?p |> int
print(n)
```

## Configuration

There is no LLM configuration in the language — no `jade.toml` model section, no
provider keys in your source. A **provider package** owns all of it: which
vendor, which model, the API key, the token budget, and any retry policy.

Install one and set your key with `jade register`:

```sh
jade register                    # pick from what is installed, then enter a key
jade register anthropic sk-...   # or name the provider and key outright
jade use openai                  # switch without re-entering a key
jade env                         # what is active, and what is installed
```

A provider is an ordinary compiled Jade package that does its own HTTP to the
vendor. Jade loads whichever one is in the active slot, hands it the prompt, and
reads back the reply — it never learns a vendor name. Until one is registered, a
`?` dereference fails with "no inference backend available".

| Variable | Purpose |
|----------|---------|
| `JADE_PROVIDER_ACTIVE` | Directory holding the active provider (default `$HOME/.jade/provider/active`) |

## No `llm` package

There is no `use llm` package in the language. Everything it used to expose —
model selection and introspection, the token budget and token accounting, anchor
handling, retry policy, backend health, model profiles, and tool-call parsing —
belongs to the provider package now.

The language keeps only the inference *syntax*: declaring a prompt and
dereferencing it (`?p`, `?p |> Type`), with grammar constraints via
`?p |> grammar`. It builds a request, calls the provider, and reads the reply —
nothing else.

## Async Inference

Jade supports concurrent LLM inference through `async fn` definitions and `await` expressions. Defining a function with `async fn` allows it to run prompt dereferences concurrently with other async functions.

Within an `async fn`, prefix any expression with `await` to wait for its result. When running under `jade run`, async functions execute concurrently via the Tokio runtime — multiple LLM calls can be in-flight at the same time.

```jade
async fn ask_question(q) {
    prompt p = q
    return await ?p
}

let a = ask_question("What is the capital of France?")
let b = ask_question("What is the capital of Germany?")
print(await a)
print(await b)
```

The two calls above run concurrently — both prompts are sent to the model at the same time.

:::note
The REPL executes `async fn` definitions synchronously (one at a time). Use `jade run` for true concurrent execution. A warning is printed to stderr when an `async fn` is evaluated in the tree-walk path.
:::

See [Async / Await](async) for the full reference.

## Error Reference

| Error | Cause |
|-------|-------|
| `NoInferenceBackend` | `?p` was evaluated with no provider installed — run `jade register` |
| `NotAPrompt` | `?x` where `x` is not a `prompt` binding |
| `PromptOverflow` | Typed dereference produced a reply that didn't coerce to the target type (single-shot — the provider owns any retry policy) |
| `InferenceError` | The provider reported a failure (a bad API key, a rate limit, a grammar it cannot enforce) |
| `StreamingWithType` | `?p |> Type` used directly inside `print()` — assign to a variable first |
| `NotAFuture` | `await` applied to a non-Future value |
| `DoubleAwait` | The same Future was awaited more than once |
| `AsyncPanic` | A spawned async task panicked; the message and span are captured from the task |
