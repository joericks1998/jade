---
id: exceptions
title: Exceptions
sidebar_label: Exceptions
---

Jade provides structured exception handling through three keywords: `raise` raises any value as an exception, `try` wraps a block that might raise, and `catch` handles the raised value by type or unconditionally.

## Overview

An exception is any Jade value raised with the `raise` statement. The raised value can be a string, a struct instance, an integer, or any other runtime value. The most common pattern is to define a dedicated `struct` type to carry the exception payload, then raise an instance of that type. This lets `catch` arms match by type name and access the fields of the caught value.

A `try`/`catch` block wraps the statements that might raise. If the `try` body completes without raising, execution continues normally after the last `catch` arm and no catch arm runs. If a `raise` occurs, execution of the `try` body stops immediately and the runtime searches the catch arms in order, executing the first one that matches the raised value.

Built-in runtime errors — division by zero, type errors, undefined variable references, index out of bounds, and all other errors — are automatically wrapped in a synthetic `RuntimeError` struct with a single `message` field. A catch-all arm (`catch e { … }`) will catch these just as it catches user-raised exceptions.

If no `catch` arm matches the raised value, the exception propagates outward to the nearest enclosing `try`/`catch`. If it reaches the top level without being caught, the program exits with an error message.

## Syntax

### raise

```jade
raise <expr>
```

`<expr>` — any expression. The evaluated value becomes the raised exception. Execution of the enclosing block stops immediately after `raise`.

### try / catch

```jade
try {
    <body statements>
} catch <TypeName> <binding> {
    <arm statements>
} catch <binding> {
    <arm statements>
}
```

- `<body statements>` — any sequence of statements that might raise.
- `<TypeName>` — an optional struct type name. When present, this arm only matches exceptions that are instances of that struct type. When absent, the arm is a catch-all and matches any raised value.
- `<binding>` — a name bound to the caught value inside the arm body.

Any number of typed `catch` arms may appear before the optional catch-all. Arms are tested in the order they are written; the first match wins. A catch-all arm should appear last because it matches everything.

## Basic Examples

### Raising and catching a string

```jade
fn risky() {
    raise "something went wrong"
}

try {
    risky()
} catch e {
    print(e)
}
```

`risky` raises a string. The catch-all arm binds the string to `e` and prints it. Output: `something went wrong`.

### Typed catch with a struct exception

```jade
struct ValueError { message }

fn parse_age(n) {
    if n < 0 {
        raise ValueError { message: "age cannot be negative" }
    }
    return n
}

try {
    parse_age(-1)
} catch ValueError e {
    print(e.message)
}
```

Defining a dedicated exception struct lets catch arms match by type. `catch ValueError e` only runs when the raised value is a `ValueError` instance. Output: `age cannot be negative`.

### Catching a built-in runtime error

```jade
try {
    let x = 1 / 0
} catch e {
    print("caught runtime error")
}
```

Division by zero normally terminates the program. Inside a `try` block, built-in runtime errors are automatically wrapped in a `RuntimeError` struct with a `message` field. The catch-all arm binds this struct to `e` and the program continues. Output: `caught runtime error`.

## Advanced Examples

### Multiple typed catch arms — first match wins

```jade
struct NetworkError { code, message }
struct ValueError { message }

try {
    raise NetworkError { code: 503, message: "service unavailable" }
} catch ValueError e {
    print("wrong: value error")
} catch NetworkError e {
    print(e.code)
    print(e.message)
} catch e {
    print("wrong: catch-all")
}
```

Three arms are checked in order. The raised value is a `NetworkError`, so the first arm does not match. The second arm matches and runs, printing `503` then `service unavailable`. The catch-all is never reached.

### Exception propagation through the call stack

```jade
struct ValueError { message }

fn inner() {
    raise ValueError { message: "deep error" }
}

fn outer() {
    inner()
}

try {
    outer()
} catch ValueError e {
    print(e.message)
}
```

When `inner` raises, execution unwinds through `outer` without any catch arm in either function. The exception propagates to the `try`/`catch` block at the call site, which catches it and prints `deep error`. Exceptions cross function boundaries automatically.

### Nested try/catch — inner handles if it matches, outer handles if it does not

```jade
struct ValueError { message }
struct NetworkError { code, message }

try {
    try {
        raise ValueError { message: "inner error" }
    } catch NetworkError e {
        print("wrong: inner caught network error")
    }
} catch ValueError e {
    print(e.message)
}
```

The inner `try`/`catch` only handles `NetworkError`. Because a `ValueError` was raised and the inner arm does not match, the exception propagates to the outer `try`/`catch`, which catches it and prints `inner error`.

### try body completes normally — catch never runs

```jade
try {
    let x = 1 + 1
    print(x)
} catch e {
    print("wrong: should not run")
}
```

When no exception is raised, the `try` body executes fully and no catch arm runs. Output: `2`.

## Type Rules

| Operation | Condition | Result |
|-----------|-----------|--------|
| `raise <expr>` | Any value | Exception propagation; execution of current block stops |
| `catch TypeName e` — typed arm | Raised value must be a `Struct` with `type_name == TypeName` | Arm body executes; `e` bound to the struct instance |
| `catch e` — catch-all arm | Any raised value, including built-in runtime errors | Arm body executes; `e` bound to the raised value |
| Built-in runtime error inside `try` | Any internal error | Wrapped as `RuntimeError { message }` struct; catchable by catch-all or `catch RuntimeError e` |
| No catch arm matches | Raised value does not match any typed arm | Exception re-raised; propagates to nearest enclosing `try` |

:::note
Typed `catch` arms only match struct values — they check the `type_name` field of the struct instance at runtime. Raising a plain string or integer and then catching with a typed arm will not match; use a catch-all arm to handle non-struct raises.
:::

## Interaction with Other Features

- **Structs**: The most common pattern is to raise struct instances as typed exceptions. Typed `catch` arms match by struct type name. See [Structs](structs).
- **Functions**: Exceptions propagate through function call boundaries automatically. A `raise` inside a function unwinds the call stack until it reaches the nearest enclosing `try`/`catch`. See [Functions](functions).
- **Control flow**: `try`/`catch` blocks can appear anywhere a statement is valid. A `return` inside a `try` body exits the enclosing function normally without triggering any `catch` arm. See [Control Flow](control-flow).
- **Variables**: The binding introduced by a `catch` arm is scoped to that arm's body. Use bare assignment to write back to a variable declared in an enclosing scope from inside a catch arm.
