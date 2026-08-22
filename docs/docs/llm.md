---
id: llm
title: LLM Integration
sidebar_label: LLM Integration
---

Jade treats LLM access as part of the language. A `prompt` declaration names a prompt string. The `?` operator sends that string to your inference provider and returns the response. A `|>` stage after the dereference says what shape the response must take, either a Jade type or a grammar you write yourself, and Jade constrains the model to produce it.

## Declaring a Prompt

Use the `prompt` keyword to bind a prompt string to a name. The right side is any expression that produces a `str`.

```jade
prompt p = "What is the capital of France?"
```

A `prompt` binding holds the prompt text and does not call the model. The call happens only when you dereference the variable with `?`.

```jade
let question = "What is 2 + 2?"
prompt p = question
```

## Decorating a Prompt

Getting useful work out of a model usually means wrapping the instruction in tags the model recognises. Writing that wrapper around every prompt buries the one part that actually differs between them:

```jade
prompt summarize = "<instructions>\nSummarize the document in one sentence.\n</instructions>"
prompt extract   = "<instructions>\nList every date you find.\n</instructions>"
```

A decorator moves the wrapper above the line, so the prompt still reads as the text it really is:

```jade
fn instructions(body) {
    return f"<instructions>\n{body}\n</instructions>"
}

@instructions
prompt summarize = "Summarize the document in one sentence."

@instructions
prompt extract = "List every date you find."
```

`@instructions prompt p = "..."` means exactly `prompt p = instructions("...")`. The decorator wraps the *text*, at the moment the prompt is built, not at the dereference. Two things follow:

- `?p` still means exactly one thing: send this prompt. Nothing hidden happens at the call.
- The framing travels with the value. A prompt handed to another file arrives already framed, instead of being framed again by whoever dereferences it.

Decorators take arguments and stack the same way they do on a `let`, and the first one you write is applied first. See [Variables](variables.md#decorators) for the full rules.

One thing to know while debugging: a prompt prints as `<prompt>` and never shows its text, so `print(p)` will not reveal what a decorator produced. If you need to see exactly what was sent, have the wrapper print or log what it built.

## Untyped dereference, written `?p`

Putting `?` in front of a prompt variable sends the prompt to your provider and returns the raw response as a `str`.

```jade
prompt p = "Say exactly: Hello from Jade!"
let response = ?p
print(response)
```

Each `?` dereference is an *independent, stateless request*. The language sends that prompt alone, with no conversation history. Carrying context between calls is your program's job, so build the earlier turns into the prompt string yourself.

```jade
prompt p1 = "My name is Alice. What is 2 + 2?"
let _ = ?p1

// p2 does NOT see p1 — include the context you need in the prompt itself.
prompt p2 = "My name is Alice. What is my name?"
let name = ?p2       // "Alice"
print(name)
```

:::note
Under `jade run`, sending the *same* prompt text with the same target type reuses the first reply instead of calling the model again. That cache lives in one process, so it never carries between runs. A compiled binary has no such cache, so under `jade build` every dereference is a real call. To get a fresh answer from a prompt you have already sent, change the text.
:::

## Typed dereference, written `?p |> type`

Add `|> type` after a prompt dereference to coerce the model's response into a Jade value type. Naming a type does two things. It builds a grammar that constrains how the model *generates*, and it converts the reply afterwards. These are the supported targets:

| Type | Accepted reply | Result |
|------|----------------|--------|
| `int` | `42`, `-7` | `int` value |
| `float` | `3.14`, `-0.5` | `float` value |
| `bool` | `true`, `True`, `false` | `bool` value |
| `char` | exactly one character | `char` value |
| `str` | anything | `str` value (always succeeds) |
| `array` | a JSON array | `array` of int, float, bool, or str |
| `dict` | a JSON object | `dict` |
| a `struct` name | a JSON object | an instance of that struct |

`str` is the one target with no grammar, because any text is already valid. `int`, `float`, `bool`, and `char` get a full grammar, since the output is short and constraining every token costs little. `array`, `dict`, and structs get a *prefix* grammar instead. A prefix grammar pins the opening bracket and then lets the model generate freely, which keeps the cost of grammar checking to the first few tokens.

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

### Coercing to a struct

Naming a `struct` asks the model for JSON and builds an instance from the result. The rule matches a struct literal. A field the reply leaves out takes its declared default. A required field the reply leaves out is a coercion failure, not a `nil`.

```jade
struct City {
    name,
    let population = 7,
    let country = "unknown"
}

prompt p = "Describe a city as JSON."
let c = ?p |> City

print(c.name)         // from the reply
print(c.country)      // "unknown" if the reply left it out
```

### Chaining past the type

A type stage is just one stage of an ordinary pipe, so anything can follow it. The type constrains how the model *generates*, and the next stage receives the value after coercion:

```jade
fn double(x) { return x * 2 }

prompt p = "What is 21 + 21? Respond with only the number."
let n = ?p |> int |> double   // 84
```

Order matters here, in a useful way. `?p |> int |> double` constrains the reply to an integer and hands `double` a real int. `?p |> double` gives Jade no type to build a grammar from, so the model generates freely and `double` receives the raw reply text.

:::note
Before v1.2.0 you could write neither of these. A `|>` after `?p` went through a separate parse path that read only one constraint, so no chain could form, and a typed dereference inside `print(...)` was rejected outright. Both work now. `print(?p |> int)` prints the coerced int, and `print(?p)` still streams tokens live.
:::

## Constraining with a Grammar

When the shape you want is not a Jade type, build a `Grammar` and use it as a `|>` stage. Its GBNF goes out with the request, and the reply stays a stream. Printing that stream shows tokens as they arrive, and reading it as a value gives you the full text.

```jade
let g = Grammar.new("\"yes\" | \"no\"")
prompt p = "yes or no?"

let answer = ?p |> g   // "yes" or "no", and nothing else
print(answer)
```

A grammar also decides what a *live* print shows, and this is the part worth getting right. A grammar can carry an *anchor* and a *stop*. Together they mark a region of the reply to hide from live output while keeping it in the value. That is how you hide a tool call or a chain-of-thought span from the reader without losing it to the program:

```jade
let g = Grammar.new("\"a\"|\"b\"", "<t>", "</t>")
prompt p = "Reply with <t>a</t> and nothing else."

print(?p |> g)        // everything between <t> and </t> is suppressed
let full = ?p |> g    // ... but it is still here
```

A grammar with *no anchor* hides output from the very first token, because the whole reply is then structured output with no prose to show. So `print(?p |> g)` on a grammar without an anchor prints an empty line. That is not a bug. It is the same rule, with the hidden region starting at token one. Read the value and print that instead, the way the first example does.

:::note
This used to be a builtin called `stream(?p, mute_on = [g])`. It existed only because a grammar-constrained dereference collapsed into a blocking call, leaving no stream to print. Since v1.2.4 it no longer collapses, so the pipe covers both cases and `stream()` is gone.
:::

## Dereferencing a Prompt in a Field

The prefix `?` is for bare prompts. A prompt held in a struct field uses one of two postfix forms instead, which put the operator next to the field it actually applies to:

```jade
struct Agent {
    prompt system = "You are a helpful assistant"
}

let a = Agent {}

let r = a.(?system)                  // explicit
let r = a~>system                    // shorter, and means the same
let n = a.(?system) |> int           // constraints work as usual
let d = build(cfg).agent.(?system)   // reads left-to-right; no backtracking
```

`obj.(?field)` and `obj~>field` are the same operation. `~>` is simply the shorter spelling, much as C offers `p->x` for `(*p).x`. A `|>` constraint goes outside the parentheses.

Plain `.` still works as before. `a.system` reads the field and gives you the prompt value itself, undereferenced.

:::note
`?obj.field` is a *syntax error*. It reads as though `?` applies to `obj`, when it really applies to `system`, so Jade rejects it in favor of the two forms above. `obj.?field` is rejected too, because the parentheses are required.
:::

## Coercion Failure

A typed dereference gets one attempt, because grammar-constrained sampling already forces the model's reply into a shape the target type accepts. If the reply still will not coerce, Jade raises a `PromptOverflow` runtime error at the dereference. The language never re-asks. Any retry policy belongs to the provider.

```jade
prompt p = "Pick a lucky number."
let n = ?p |> int
print(n)
```

If the model answers "seven, I think", that program stops with:

```
runtime error: [2:9] prompt '<prompt>' failed to produce a valid typed value after 1 attempt(s)
```

The error carries the line and column of the dereference rather than the name of the prompt binding. A prompt is often an expression rather than a variable, so there is not always a name to report.

`PromptOverflow` is an ordinary catchable error, so a program that would rather fall back than stop can wrap the dereference:

```jade
prompt p = "Pick a lucky number."
let n = 0
try {
    n = ?p |> int
} catch e {
    print("model did not give a number, using 0")
}
```

## Configuration

The language holds no LLM configuration at all. There is no model section in `jade.toml` and no provider key in your source. A *provider package* owns all of it: which vendor, which model, the API key, the token budget, and any retry policy.

Install one and set your key with `jade register`:

```sh
jade register                    # pick from what is installed, then enter a key
jade register anthropic sk-...   # or name the provider and key outright
jade register --list             # what is installed, and which one is active
jade register --remove anthropic # forget a stored key
jade use openai                  # switch without re-entering a key
jade env                         # what is active, and what is installed
```

Providers ship with Jade, so there is usually nothing to download. The installer puts them under `lib/jade/providers/`, and `jade register` copies the one you pick into the active slot.

A provider is an ordinary compiled Jade package that makes its own HTTP calls to the vendor. Jade loads whichever provider sits in the active slot, hands it the prompt, and reads back the reply. Jade itself never learns a vendor name. Until you register a provider, a `?` dereference fails with "no inference backend available".

| Variable | Purpose |
|----------|---------|
| `JADE_PROVIDER_ACTIVE` | Directory holding the active provider (default `$HOME/.jade/provider/active`) |

## No `llm` package

There is no `use llm` package in the language. Everything it used to expose now belongs to the provider package: model selection and introspection, the token budget and token accounting, anchor handling, retry policy, backend health, model profiles, and tool-call parsing.

The language keeps only the inference *syntax*: declaring a prompt, dereferencing it with `?p` or `?p |> Type`, and constraining it with `?p |> grammar`. Jade builds a request, calls the provider, and reads the reply. That is all it does.

## Async Inference

Independent prompts do not have to wait for each other. Calling an `async fn` starts its body and hands back a *future* right away. `await` collects the result later, so the calls overlap.

```jade
async fn ask(q) {
    prompt p = q
    return ?p
}

let a = ask("What is the capital of France?")
let b = ask("What is the capital of Germany?")
print(await a)
print(await b)
```

Both prompts are in flight before the program reaches the first `await`.

:::warning
Do not write `await ?p`. A dereference already waits for the model on its own. In a plain `fn` it blocks the thread, and in an `async fn` it suspends only that task. Either way `?p` produces the reply, not a future. `await ?p` is a compile error reading `type mismatch: expected future, got str`. `await` is for the value an `async fn` call returns.
:::

`join` waits for several futures at once and gives back their results as an array, in the order you passed them:

```jade
let results = join(ask("capital of France?"), ask("capital of Germany?"))
print(results[0])
print(results[1])
```

See [Async / Await](async) for the full reference.

## Error Reference

| Error | Cause |
|-------|-------|
| `NoInferenceBackend` | `?p` ran with no provider installed. Run `jade register` |
| `PromptOverflow` | A typed dereference produced a reply that would not coerce to the target type. There is one attempt, and the provider owns any retry policy |
| `InferenceError` | The provider reported a failure, such as a bad API key, a rate limit, or a grammar it cannot enforce |
| `InvalidPipeStage` | The right side of `\|>` is not a function, a type name, or a Grammar. This replaced `StreamingWithType` in v1.2.0 |
| `NotAPrompt` | `?x` where `x` is not a prompt. Usually caught earlier as a type error, where `jade check` reports `expected prompt, got int` |
| `PrefixDerefOnField` | `?obj.field`, which the parser rejects in favor of `obj.(?field)` or `obj~>field` |
| `NotAFuture` | `await` applied to a non-Future value |
| `DoubleAwait` | The same Future was awaited more than once |
| `AsyncPanic` | A spawned async task panicked. The message and source position are captured from the task |
