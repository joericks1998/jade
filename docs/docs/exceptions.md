---
id: exceptions
title: Exceptions
sidebar_label: Exceptions
---

Jade handles exceptions with three keywords. `raise` throws any value as an exception. `try` wraps a block that might raise. `catch` handles the raised value, either by type or for anything at all.

## Overview

An exception is any Jade value thrown with the `raise` statement. That value can be a string, a struct instance, an integer, or anything else. The usual pattern is to define a dedicated `struct` type to carry the details, then raise an instance of it. Doing so lets `catch` arms match by type name and read the fields of the caught value.

A `try` and `catch` block wraps the statements that might raise. If the `try` body finishes without raising, the program continues after the last `catch` arm and no arm runs. If something raises, the `try` body stops at once, and the runtime checks the catch arms in order and runs the first one that matches.

Jade wraps its own runtime errors in a `RuntimeError` struct with a single `message` field. That covers division by zero, type errors, undefined variable references, index out of bounds, and every other built-in error. A catch-all arm, written `catch e { … }`, catches these the same way it catches the exceptions you raise yourself.

If no `catch` arm matches, the exception travels outward to the nearest enclosing `try` and `catch`. If it reaches the top level uncaught, the program prints `unhandled exception: <value>` and exits with status 1.

## Syntax

### raise

```jade
raise <expr>
```

`<expr>` is any expression. Its value becomes the raised exception, and the enclosing block stops running right there.

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

- `<body statements>` is any sequence of statements that might raise.
- `<TypeName>` is an optional struct type name. With a name, the arm matches only instances of that struct type. Without one, the arm is a catch-all and matches anything raised.
- `<binding>` is a name bound to the caught value inside the arm body.

You can write any number of typed `catch` arms before the optional catch-all. Jade tests them in the order you wrote them, and the first match wins. Put the catch-all last, because it matches everything.

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

`risky` raises a string. The catch-all arm binds it to `e` and prints it. The output is `something went wrong`.

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

A dedicated exception struct lets catch arms match by type. `catch ValueError e` runs only when the raised value is a `ValueError` instance. The output is `age cannot be negative`.

### Catching a built-in runtime error

```jade
try {
    let x = 1 / 0
} catch e {
    print("caught runtime error")
}
```

Division by zero normally ends the program. Inside a `try` block, Jade wraps its built-in runtime errors in a `RuntimeError` struct carrying a `message` field. The catch-all arm binds that struct to `e`, and the program continues. The output is `caught runtime error`.

### Separating built-in errors from your own

You can name `RuntimeError` in a typed arm. That lets a single `try` handle the language's errors and your own errors separately.

```jade
struct MyError { message }

try {
    let v = 42
    let bad = v.upper()
} catch MyError e {
    print("mine: " + e.message)
} catch RuntimeError e {
    print("built-in: " + e.message)
}
// built-in: struct 'int' has no field 'upper'
```

Two things follow from how the wrapping works:

- *Your `raise` is never wrapped.* The value you throw is the value that arrives. A raised string stays a string, and a raised struct keeps its own type. Only the language's own errors become a `RuntimeError`.
- *`RuntimeError` is the runtime's type, not one you declare.* You catch it. You never construct it or `raise` it.

:::note
The `message` on a caught `RuntimeError` carries a `[line:col]` prefix under `jade run`, but not in a binary built with `jade build`, because compiled code keeps no line information at run time. If you need to inspect the message, match on a substring rather than testing for equality.
:::

## Advanced Examples

### Several typed catch arms, where the first match wins

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

Jade checks the three arms in order. The raised value is a `NetworkError`, so the first arm does not match. The second arm matches and runs, printing `503` and then `service unavailable`. The catch-all never runs.

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

When `inner` raises, the program unwinds through `outer`, and neither function has a catch arm. The exception reaches the `try` and `catch` at the call site, which catches it and prints `deep error`. Exceptions cross function boundaries on their own.

### Nested try and catch, where the inner block handles only what it matches

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

The inner block handles `NetworkError` only. A `ValueError` was raised, so the inner arm does not match, and the exception travels out to the outer block. That one catches it and prints `inner error`.

### A try body that finishes normally, so no catch runs

```jade
try {
    let x = 1 + 1
    print(x)
} catch e {
    print("wrong: should not run")
}
```

With nothing raised, the `try` body runs all the way through and no catch arm runs. The output is `2`.

## Type Rules

| Operation | Condition | Result |
|-----------|-----------|--------|
| `raise <expr>` | Any value | The exception travels outward and the current block stops |
| `catch TypeName e`, a typed arm | The raised value is a struct whose `type_name` equals `TypeName` | The arm body runs, with `e` bound to the struct instance |
| `catch e`, a catch-all arm | Any raised value, including built-in runtime errors | The arm body runs, with `e` bound to the raised value |
| A built-in runtime error inside `try` | Any internal error | Wrapped as a `RuntimeError { message }` struct, caught by a catch-all or by `catch RuntimeError e` |
| No catch arm matches | The raised value matches no typed arm | The exception is re-raised and travels to the nearest enclosing `try` |

:::note
A typed `catch` arm matches struct values only, because it checks the struct instance's `type_name` at run time. Raising a plain string or integer and then catching it with a typed arm will never match. Use a catch-all arm for anything that is not a struct.
:::

## Interaction with Other Features

- *Structs.* The usual pattern is to raise struct instances as typed exceptions, and typed `catch` arms match by struct type name. See [Structs](structs).
- *Functions.* Exceptions cross function call boundaries on their own. A `raise` inside a function unwinds the call stack until it reaches the nearest enclosing `try` and `catch`. See [Functions](functions).
- *Control flow.* A `try` and `catch` block can go anywhere a statement is valid. A `return` inside a `try` body exits the enclosing function normally and triggers no `catch` arm. See [Control Flow](control-flow).
- *Variables.* The name a `catch` arm binds belongs to that arm's body only. To write back to a variable from an enclosing scope, use a bare assignment inside the arm.
