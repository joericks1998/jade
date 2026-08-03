---
id: types
title: Types
sidebar_label: Types
---

Jade has twelve runtime value types.

| Type | Description | Status |
|------|-------------|--------|
| `int` | 64-bit signed integer (`i64`) | Implemented |
| `float` | 64-bit floating point (`f64`) | Implemented |
| `bool` | Boolean `true` or `false` | Implemented |
| `char` | A single Unicode scalar; what indexing or iterating a string yields | Implemented |
| `bytes` | A counted sequence of raw octets; binary data that is not text | Implemented |
| `stream` | A buffered sequence produced by a `yield`ing function | Implemented |
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
| **`char` and `str` compare and concatenate** | `"hi"[0] == "h"` | `true` |

:::note
Arithmetic promotion converts an `int` to `float` when the other operand is a `float`. Equality never promotes — comparing `1` to `1.0` is always a `TypeError`.
:::

### The one exception: `char` and `str`

A `char` compares equal to the one-character string spelling it, orders against strings, and concatenates with them in either direction. That is a deliberate hole in the strict-equality rule above, and it is worth knowing why it is there rather than assuming the rule is soft.

Indexing a string used to give back a one-character `str`. As of v1.2.1 it gives a `char`, so every `if s[0] == "a"` already written would have become a `TypeError` overnight. The exception keeps that code meaning what it meant.

```jade
let s = "café"
print(s[0] == "c")     // true
print(s[0] + "at")     // "cat"
for c in s { print(c) }  // four lines: "é" is one character, not two
```

## Char

A `char` is a single Unicode scalar, not a byte. It is an *immediate*: it rides inside the tagged value word alongside `int`, `bool`, and `nil`, so scanning a string allocates nothing at all, where the old one-character-string behavior allocated once per character.

There is no char literal. A char comes from indexing a string, iterating one, `char("x")`, or `?p |> char`.

```jade
let c = char("x")
let first = "hello"[0]
```

`len` of a char is 1, matching what it answered when indexing produced a one-character string.

A char taken from a tainted string stays tainted, so a loop that rebuilds a string character by character cannot quietly launder it past `sh.exec`.

## Bytes

A `bytes` value is a counted sequence of raw octets. It is deliberately not a string, and the reason is worth stating: a Jade string is UTF-8 and NUL-terminated, so a blob containing a zero byte would be truncated at it and one that is not valid UTF-8 would be corrupted by anything that assumed text. `fs.read` goes through a UTF-8 decode and cannot read a PNG at all.

Conversion is explicit in both directions, never implicit:

```jade
use std::fs

let raw = "hi".encode()      // str -> bytes, UTF-8
print(raw)                    // b"hi"
print(len(raw))               // 2
print(raw[0])                 // 104 — an octet is an int, not a char
print(raw.decode())           // bytes -> str, raises on invalid UTF-8

fs.write_bytes("out.bin", raw)
let back = fs.read_bytes("out.bin")
```

Indexing gives an `int` in 0..=255 rather than a `char`. A byte is not a Unicode scalar, and making `b[0]` look like `s[0]` would hide that the two differ on any non-ASCII input.

`print(b)` renders an escaped `b"…"` form rather than dumping raw octets to your terminal, since a blob can contain control characters or an escape sequence. Use `decode()` when the bytes really are text.

Bytes carry a trust byte like strings do, so `fs.read_bytes(p).decode()` is refused by `sh.exec` exactly as `fs.read(p)` is.

## Stream

A function whose body contains a `yield` returns a **stream** rather than a value. The body runs to completion filling a buffer, and the caller gets the buffer.

```jade
fn doubles(n) {
    let i = 0
    while i < n {
        yield i * 2
        i = i + 1
    }
}

let s = doubles(4)
print(len(s))     // 4
print(s[0])       // 0
for x in s { print(x) }
for x in s { print(x) }   // the same values again
```

A stream *is* a buffer, not a one-shot channel. Everything it produced is retained, so reading it twice gives the same values twice and there is no "already consumed" state to reason about. `len` and indexing work for the same reason.

A prompt dereference is a stream too. `?p` produces one lazily, so `print(?p)` shows tokens as they arrive, and the same value can be read again afterwards.

A bare `return` stops a generator early. `return x` is a compile error: a function that yields produces a stream, so returning a value too would ask it to be two things at once.

Yields of different types widen to a mixed stream rather than failing, the same rule a mixed array literal follows.

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
