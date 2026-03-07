# Typed Output Feature Plan

## Overview

Add output type constraints to prompt dereferences using `-> Type` syntax.
When a prompt is dereferenced with a type annotation, Jade automatically:
1. Injects a schema instruction into the prompt
2. Parses the LLM response into the requested type
3. Retries with a correction message if parsing fails
4. Logs failures to `__retry_log__` for observability

## Syntax

```python
prompt p = "What is 2 + 2?"
result = ?p -> int

prompt q = "Describe today's weather in Seattle"
report = ?q -> WeatherReport
print(report.temperature)
```

## Locked Decisions

| Question | Decision |
|---|---|
| Syntax | `?p -> Type` |
| Forces dynamic (runtime) evaluation | Yes, always — even if `p` was a static prompt |
| Streaming with `-> Type` | Compile-time error — `print(?p -> int)` is invalid |
| Supported types | Primitives (`int`, `float`, `str`, `bool`) + any class with a single-arg constructor |
| Coercion mechanism | `output_type(raw_response.strip())` — plain Python constructor calling |
| Retry mechanism | Follow-up correction message sent in same conversation |
| Retry turns in history | Stripped from `active_conversation.messages` after resolution |
| Max retries builtin | `__max_retries__` — global scope only, consistent with `__model__` etc. |
| Default max retries | 3 |
| Exhausted retries | Raises `PromptOverflowError` (custom Jade error) |
| Logging builtin | `__retry_log__` |
| What gets logged | Only dereferences with at least one failure — clean first-try successes are not logged |
| Log entry structure | `prompt`, `type`, `attempts`, `success`, `failed[]` |

### `__retry_log__` structure

```python
# Only populated when at least one parsing attempt failed
__retry_log__ = [
    {
        "prompt":   "Describe today's weather in Seattle",
        "type":     "WeatherReport",
        "attempts": 3,
        "success":  True,       # recovered after retries
        "failed":   [
            "Today it is cloudy with mild temperatures...",
            '{"temp": 58, "sky": "overcast"}'
        ]
    },
    {
        "prompt":   "How many planets?",
        "type":     "int",
        "attempts": 3,
        "success":  False,      # exhausted max retries
        "failed":   ["Eight!", "There are 8 planets.", "8 planets"]
    }
]
```

The user can infer clean successes from the absence of a log entry.


## Implementation Plan

### Files to change

| File | Change |
|---|---|
| `tokenref.py` | Add `__retry_log__` and `__max_retries__` to `BUILTIN_MAP` |
| `heap.py` | Add `retry_log: list`, `ask_typed()` method with retry loop and cleanup |
| `processer.py` | Extend `process_2` to detect `ARROW` + type token after `?identifier` and emit `ask_typed()` |

### New helpers in `heap.py`

| Function | Purpose |
|---|---|
| `_build_schema_hint(type)` | Appends output format instruction to the prompt — for custom classes, inspects `__init__` signature |
| `_build_correction(type, error)` | Builds the retry correction message from the parse error |
| `_coerce(raw, type)` | `output_type(raw.strip())` — pure Python constructor call |
| `_cleanup_retry_turns(n)` | Strips last `n * 2` messages from `active_conversation.messages` |

### `ask_typed` sketch

```python
def ask_typed(self, prompt_text: str, output_type, max_retries: int) -> any:
    schema_hint = _build_schema_hint(output_type)
    augmented = prompt_text + "\n\n" + schema_hint
    failed_responses = []

    for attempt in range(max_retries):
        if attempt == 0:
            raw = self._call(augmented)
        else:
            correction = _build_correction(output_type, last_error)
            raw = self._call(correction)  # continues the conversation

        try:
            result = _coerce(raw, output_type)
            if failed_responses:
                # Had at least one failure — log it, strip retry turns
                self._cleanup_retry_turns(attempt)
                self.retry_log.append({
                    "prompt":   prompt_text,
                    "type":     output_type.__name__,
                    "attempts": attempt + 1,
                    "success":  True,
                    "failed":   failed_responses,
                })
            return result
        except Exception as e:
            failed_responses.append(raw)
            last_error = e

    # All attempts exhausted
    self._cleanup_retry_turns(max_retries)
    self.retry_log.append({
        "prompt":   prompt_text,
        "type":     output_type.__name__,
        "attempts": max_retries,
        "success":  False,
        "failed":   failed_responses,
    })
    raise PromptOverflowError(
        f"Could not coerce LLM output to {output_type.__name__} after {max_retries} attempts"
    )
```

### `process_2` extension

Currently handles: `PROMPTDREF IDENTIFIER`
Needs to also handle: `PROMPTDREF IDENTIFIER SPACE ARROW SPACE IDENTIFIER`

Emits:
- Without type: `__jade_heap.ask(__p__name)`
- With type: `__jade_heap.ask_typed(__p__name, TypeName)`
