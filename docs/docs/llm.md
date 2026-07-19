---
id: llm
title: LLM Integration
sidebar_label: LLM Integration
---

Jade is built around first-class LLM access. A `prompt` declaration names a prompt string; the `?` operator sends it to the configured model and returns the response. The `|>` pipe suffix coerces the response to a typed Jade value, with automatic retry on failure.

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

Prefixing a prompt variable with `?` sends the prompt to the configured model and returns the raw response as a `str`.

```jade
prompt p = "Say exactly: Hello from Jade!"
let response = ?p
print(response)
```

Each `?` dereference appends a user turn and the model's reply to the shared **conversation history** for this program run. Subsequent dereferences see the full prior context, so prompts can naturally build on each other.

```jade
prompt p1 = "My name is Alice."
prompt p2 = "What is my name?"

let _ = ?p1          // establishes context
let name = ?p2       // model sees p1 exchange, should reply "Alice"
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

## Retry on Coercion Failure

When the model's response cannot be parsed as the requested type, Jade automatically sends a correction follow-up and tries again. This continues up to `max_retries` times (default: `3`, giving 4 total attempts including the initial one).

If all attempts fail, Jade raises a `PromptOverflow` runtime error naming the prompt variable and the number of attempts made.

On success, the retry conversation turns are stripped from the shared history — only the final successful exchange is retained.

```jade
// With max_retries = 3, Jade will try up to 4 times to get a valid int.
prompt p = "Pick a lucky number."
let n = ?p |> int
print(n)
```

## Configuration

Jade reads LLM settings from `jade.toml` in the working directory. Environment variables override file values.

### `jade.toml` format

```toml
[model]
provider    = "anthropic"          # or "openai"
model       = "claude-haiku-4-5-20251001"
max_retries = 3
api_key     = "sk-..."             # optional — prefer the env var
```

### Environment variables

| Variable | Purpose |
|----------|---------|
| `JADE_API_KEY` | API key (overrides `api_key` in jade.toml) |
| `JADE_PROVIDER` | `anthropic` or `openai` |
| `JADE_MODEL` | Model name string |
| `JADE_MAX_RETRIES` | Integer, overrides `max_retries` |

### Interactive setup

Run `jade configure` to launch the interactive wizard. It prompts for provider, model, API key, and max retries, then writes `jade.toml` in the current directory.

```bash
jade configure
```

:::warning
**Security:** storing `api_key` in `jade.toml` saves it in plaintext. Prefer setting `JADE_API_KEY` in your shell environment and omitting the key from the file.
:::

### Supported providers

| Provider string | API used | Default model |
|----------------|----------|---------------|
| `anthropic` (default) | Anthropic Messages API | `claude-haiku-4-5-20251001` |
| `openai` | OpenAI Chat Completions | `gpt-4o-mini` |
| `jade-os` | Jade OS on-device kernel backend | set by device configuration |

## Session Variables

Jade pre-populates several read-only-by-convention variables in the global scope before your program runs. These are updated after each `?` dereference.

| Variable | Type | Description |
|----------|------|-------------|
| `__tokens__` | `int` | Running total of tokens consumed by all inference calls this run |
| `__model__` | `str` | Name of the model being used |
| `__max_retries__` | `int` | Configured maximum retry count for typed dereferences |
| `__retry_log__` | `array` | Log of retry events (reserved for future structured output) |

```jade
prompt p = "Say: hi"
let _ = ?p

print(__model__)        // e.g. "claude-haiku-4-5-20251001"
print(__max_retries__)  // 3
print(__tokens__)       // tokens used so far
```

## Runtime Configuration — `use llm`

Import the built-in `llm` package (dot notation, like all packages) to adjust LLM settings at runtime:

```jade
use llm

llm.set_max_tokens(256)    // cap responses at 256 tokens
```

### `llm.set_max_tokens(n)`

Sets the maximum number of tokens the model may generate per inference call. `n` must be a positive integer. The setting takes effect for all subsequent `?` dereferences in the same run.

```jade
use llm

llm.set_max_tokens(64)

prompt p = "Write a haiku about rain."
let haiku = ?p
print(haiku)
```

:::note
`set_max_tokens` overrides any `max_tokens` value set in `jade.toml` or environment variables for the remainder of the program run. There is no way to reset it to the config file value once changed.
:::

### `llm.count_tokens(text)`

Returns the number of tokens in `text` under the active model's tokenizer, as an `int`. Useful for budgeting prompt size before a `?` dereference.

```jade
use llm

let n = llm.count_tokens("How many tokens is this?")
print(n)
```

### `llm.total_tokens()`

Returns the total number of tokens consumed by LLM inference so far in the current run, as an `int`.

```jade
use llm

prompt p = "Summarize Jade in one line."
let _ = ?p
print(llm.total_tokens())   // tokens used so far this run
```

## Model Profiles

When inference runs through the Jade OS daemon backend, the active model has a
**profile** describing its model-specific token vocabulary — the delimiters it
wraps tool calls in, the JSON field that names the tool, and any special-token
spans (e.g. reasoning). The profile lets Jade construct and parse a model's tool
calls without any program hardcoding the model's delimiters.

### `llm.model()`

Returns the active model's name as a `str` (the name reported by the backend, or
the configured default).

```jade
use llm
print(llm.model())          // e.g. "Qwen3-Coder-30B"
```

### `llm.profile()`

Returns the active model's profile as a dict, or `nil` when the model has no
registered profile. Shape:

```jade
use llm

let p = llm.profile()
print(p.tool_call.open)        // "<tool_call>"
print(p.tool_call.close)       // "</tool_call>"
print(p.tool_call.name_field)  // "name"
// p.model      → the model name
// p.spans      → array of { tag, open, close } special-token spans
```

## Tool Calls

A model emits a tool call as a JSON object wrapped in its profile delimiters,
e.g. `<tool_call>{ "name": "get_weather", "arguments": { … } }</tool_call>`. The
`llm` package finds and extracts these using the **active model's profile**, so
the same code works across models.

### `llm.find_tool_call(text)`

Returns the **first** tool call found in `text` as a dict `{ name, args }` — where
`name` is the tool name lifted from the profile's name field and `args` is the
verbatim JSON body — or `nil` if none is present.

```jade
use llm

let reply = ?prompt "What's the weather in SF? Use a tool."
let call = llm.find_tool_call(reply)
if call != nil {
    print(call.name)   // "get_weather"
    print(call.args)   // {"name": "get_weather", "arguments": {"city": "SF"}}
}
```

### `llm.find_tool_calls(text)`

Returns **every** tool call in `text`, in order, as an array of `{ name, args }`
dicts (empty when none are present).

```jade
use llm
for call in llm.find_tool_calls(reply) {
    print(call.name)
}
```

### `llm.tool_grammar()`

Returns Jade's canonical tool-call GBNF grammar (the JSON body shape) as a `str`.
Pair it with the profile delimiters to constrain generation to a valid tool call:

```jade
use llm

let p = llm.profile()
let g = Grammar.new(llm.tool_grammar(), p.tool_call.open, p.tool_call.close)
let reply = stream(?prompt "Call a tool.", mute_on=[g])
```

### `llm.keep_anchors(enabled)`

Sticky session control (`bool`). When enabled, prompts ask the daemon to make the
tool-span boundary observable **in-band** — the closing delimiter is not stripped
from the streamed output and is synthesized at span close if the model omits it —
so a client can delimit a tool span by string matching. Applies to all subsequent
`?` dereferences in the run.

```jade
use llm
llm.keep_anchors(true)
```

## Daemon Health

### `llm.health()`

Returns a health snapshot of the inference daemon as a dict — a cheap
liveness/readiness probe that never runs the model. For non-daemon backends a
minimal `ok` snapshot is synthesized.

```jade
use llm

let h = llm.health()
print(h.status)        // "ok" | "degraded" | "loading" | "error"
print(h.model)         // active model name
print(h.model_loaded)  // true
print(h.uptime_secs)   // seconds since the daemon started serving
// h.protocol_version → the daemon's wire-protocol revision
```

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
| `MissingApiKey` | `?p` was evaluated but no API key was configured |
| `NotAPrompt` | `?x` where `x` is not a `prompt` binding |
| `PromptOverflow` | Typed dereference exhausted all retries without producing a valid value |
| `InferenceError` | HTTP or API error from the provider (non-2xx response, network failure, etc.) |
| `StreamingWithType` | `?p |> Type` used directly inside `print()` — assign to a variable first |
| `NotAFuture` | `await` applied to a non-Future value |
| `DoubleAwait` | The same Future was awaited more than once |
| `AsyncPanic` | A spawned async task panicked; the message and span are captured from the task |
