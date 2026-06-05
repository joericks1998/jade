---
id: types
title: Types
sidebar_label: Types
---

Jade has nine runtime value types.

| Type | Description | Status |
|------|-------------|--------|
| `int` | 64-bit signed integer (`i64`) | Implemented |
| `float` | 64-bit floating point (`f64`) | Implemented |
| `bool` | Boolean `true` or `false` | Implemented |
| `fn` | First-class function value | Implemented |
| `struct` | User-defined record type with named fields | Implemented |
| `str` | UTF-8 string with indexing and concatenation | Implemented |
| `array` | Heterogeneous mutable array with index access | Implemented |
| `dict` | String-keyed mutable hash map | Implemented |
| `nil` | Absence of value | Implemented |

## Type Coercion Rules

Jade does not implicitly coerce types except in specific arithmetic and comparison contexts:

| Rule | Example | Result |
|------|---------|--------|
| Arithmetic: `int op float` → float | `1 + 0.5` | `1.5` (float) |
| Ordering: `int < float` allowed | `1 < 2.5` | `true` |
| Strict equality: no cross-type coercion | `1 == 1.0` | `TypeError` |
| Bool ordering: `false` = 0, `true` = 1 | `false < true` | `true` |
| Bitwise: integer operands only | `1.0 & 2` | `TypeError` |
| Logical: bool operands only | `1 && true` | `TypeError` |

:::note
Arithmetic promotion converts an `int` to `float` when the other operand is a `float`. Equality never promotes — comparing `1` to `1.0` is always a `TypeError`.
:::

## Nil

`nil` is Jade's single "absence of value" type. A function that reaches the end of its body without returning, a bare `return`, and most mutating built-ins (`arr.push(x)`, `fs.write(...)`) all evaluate to `nil`.

### Three spellings

`nil`, `None`, and `null` are interchangeable spellings of the **same** value. They all evaluate to the one `nil`, compare equal to each other, and can be used anywhere a literal is expected (including default parameter values and type annotations).

```jade
let a = nil
let b = None
let c = null

print(a == b)   // true
print(b == c)   // true

fn greet(name = null) {   // default parameter
    return name
}
print(greet())  // nil
```

:::note
The three spellings are aliases, not distinct types — there is no separate `null` type. JSON `null` also decodes to this same `nil`.
:::

## Dict

A `dict` is a mutable, string-keyed hash map. Keys are strings; values can be any Jade type. Dicts are created with curly-brace literal syntax and accessed with square-bracket indexing.

### Creating a dict

```jade
let d = {"name": "jade", "version": 1}
let empty = {}
```

Bare identifiers are also accepted as keys — the identifier's string value becomes the key:

```jade
let key = "hello"
let greet = {key: "world"}
print(greet["hello"])  // world
```

### Reading and writing values

```jade
print(d["name"])       // jade

d["version"] = 2       // update existing key
d["stable"] = true     // add new key
```

### Length

```jade
print(len(d))   // number of key-value pairs
```

### Value semantics

Assigning a dict to a new variable copies it. Mutations to the copy do not affect the original:

```jade
let d2 = d
d2["name"] = "copy"
print(d["name"])   // jade   (unchanged)
print(d2["name"])  // copy
```

### Nested dicts

```jade
let outer = {"inner": {"x": 42}}
let inner = outer["inner"]
print(inner["x"])  // 42
```

:::note
Dict keys are always strings. Accessing a key that does not exist produces a runtime error.
:::
