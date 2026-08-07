---
id: variables
title: Variables
sidebar_label: Variables
---

Variables are declared with the `let` keyword. Every variable must be initialized at declaration.

```jade
let x = 42
let y = x + 1
let z = x * y - 10
```

Variable names must start with a letter or underscore and may contain letters, digits, and underscores.

## Rules

- Variables can hold any runtime value: `int`, `float`, `bool`, `str`, arrays, `struct` instances, and `fn` function values.
- A variable may be referenced in any expression declared after it.
- Referencing an undeclared variable is a runtime error (`UndefinedVariable`).
- Semicolons are optional — Jade inserts them automatically at line boundaries.
- Variables declared inside a function body are local to that call frame and are not visible outside.

## Decorators

A `let` may carry a decorator, which wraps the value in a function call:

```jade
fn shout(s) {
    return s.upper()
}

@shout
let greeting = "hello"     // same as: let greeting = shout("hello")

print(greeting)            // HELLO
```

The point is not brevity — it is that the wrapper sits above the declaration instead of around it, so what the value actually is stays readable.

A decorator may take its own arguments. The decorated value goes first:

```jade
fn fence(s, tag) {
    return f"<{tag}>{s}</{tag}>"
}

@fence("note")
let body = "keep it short"    // same as: let body = fence("keep it short", "note")
```

Decorators stack, and the one written **first** is applied first:

```jade
@shout
@fence("p")
let loud = "hello"            // fence(shout("hello"), "p")  →  <p>HELLO</p>
```

That is the same order `fn` decorators use, and the reverse of Python's.

A decorator can also be namespaced, using `::` like an import:

```jade
@style::tagged
let body = "keep it short"
```

The same syntax works on a `prompt` declaration, which is where it earns its keep — see [Prompts and Inference](llm.md#decorating-a-prompt).
