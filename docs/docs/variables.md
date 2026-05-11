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
