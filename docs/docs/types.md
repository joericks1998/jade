---
id: types
title: Types
sidebar_label: Types
---

These are Jade's value types.

| Type | Description |
|------|-------------|
| `int` | Signed integer, 63 bits wide |
| `float` | 64-bit floating point (`f64`) |
| `bool` | Boolean `true` or `false` |
| `char` | A single Unicode scalar; what indexing or iterating a string yields |
| `str` | UTF-8 string with indexing, iteration, and concatenation |
| `bytes` | A counted sequence of raw octets; binary data that is not text |
| `array` | Mutable array with index access; may hold values of different types |
| `dict` | String-keyed mutable hash map |
| `stream` | A buffered sequence produced by a `yield`ing function, or by `?p` |
| `struct` | User-defined record type with named fields |
| `fn` | First-class function value |
| `prompt` | Text to send to a model; `?p` dereferences one |
| `grammar` | A sampling constraint built by `Grammar.new(pattern)` |
| `handle<T>` | An opaque pointer a native package handed you |
| `nil` | Absence of value |

An `async fn` and the `future` its call produces are covered in [Async](async).

## Int is 63 bits, not 64

An `int` runs from `-4611686018427387904` to `4611686018427387903`. One bit of the machine word is spent tagging what the value is, and the language follows the representation so that a program means the same thing interpreted and compiled.

A literal outside that range is refused when the file is read. The lowest value is a special case: `-` is a separate negation, so the digits are bounded before the sign applies and `-4611686018427387904` cannot be typed directly. Arithmetic reaches it fine.

Arithmetic that would leave the range raises rather than wrapping:

```jade
let big = 3037000500
try {
    print(big * big)
} catch e {
    print("overflow caught")
}
```

## Type Coercion Rules

Jade does not implicitly coerce types except in specific arithmetic and comparison contexts:

| Rule | Example | Result |
|------|---------|--------|
| Arithmetic: `int op float` → float | `1 + 0.5` | `1.5` (float) |
| Ordering: `int < float` allowed | `1 < 2.5` | `true` |
| Strict equality: no cross-type coercion | `1 == 1.0` | type error |
| Bool ordering: `false` = 0, `true` = 1 | `false < true` | `true` |
| String ordering, by character | `"abc" < "abd"` | `true` |
| Bitwise: integer operands only | `1.0 & 2` | type error |
| Logical: bool operands only | `1 && true` | type error |
| **`char` and `str` compare and concatenate** | `"hi"[0] == "h"` | `true` |

:::note
Arithmetic promotion converts an `int` to `float` when the other operand is a `float`. Equality never promotes — comparing `1` to `1.0` is always an error.

Every row marked "type error" is caught by `jade check` before the program runs, not while it runs.
:::

### The one exception: `char` and `str`

A `char` compares equal to the one-character string spelling it, orders against strings, and concatenates with them in either direction. That is a deliberate hole in the strict-equality rule above, and it is worth knowing why it is there rather than assuming the rule is soft.

Indexing a string used to give back a one-character `str`. As of v1.2.1 it gives a `char`, so every `if s[0] == "a"` already written would have become a `TypeError` overnight. The exception keeps that code meaning what it meant.

```jade
let s = "café"
print(s[0] == "c")     // true
print(s[0] + "at")     // cat
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

A blob has three methods and no more: `len()`, `decode()`, and `slice(start, end)`. `slice` clamps rather than raising, so taking the tail of a buffer does not need a bounds check first.

```jade
let raw = "hello".encode()
print(raw.len())            // 5
print(raw.slice(0, 2))      // b"he"
print(raw.slice(3, 99))     // b"lo" — the end clamps
```

`bytes` is deliberately not a second string type, so there is nothing to compare two blobs with; `==` on them is an error. Decode both, or compare a slice.

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

## Prompt

A `prompt` holds text meant for a model. It is a value like any other: you can pass it to a function, store it in a struct field, or hand it to another file. Nothing is sent until you dereference it with `?`.

```jade
prompt p = "Name one prime number under 10."
print(?p)
```

`?p` on its own produces a stream of the reply. A `|>` stage after it constrains what the model may generate and coerces the result — `?p |> int` gives you an `int`. See [Prompts and Inference](llm) for the whole story, and [Operators](operators#what-a-stage-can-be) for what a stage may be.

## Handle

A `handle<T>` is an opaque pointer a native package gave you: a database connection, an open audio file, a decompression context. Jade holds it, passes it back to the library, and never looks inside. There are no operations on one — everything you do with a handle is a call into the package that made it.

The `T` is the C type it came from, so `handle<sqlite3>` and `handle<sqlite3_stmt>` are different values. Passing one where the other belongs is an error you can read instead of a crash inside the library. Printing a handle shows `handle<sqlite3>`, never an address.

Two rules are worth knowing before you use one:

- **Jade never closes a handle for you.** It cannot know what the pointer is or which allocator made it. Closing is a call the package exposes, and a handle dropped without it leaks whatever the library allocated.
- **A handle cannot be passed into a task.** Jade sees nothing of what a library does with one and cannot tell a thread-safe library from an unsafe one, so sharing is refused at compile time. Open one inside the task instead.

See [Packages](packages) for installing a native package.

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

A key can be any expression that produces a string, evaluated when the literal is built. So a variable in key position contributes its *value*, not its name:

```jade
let key = "hello"
let greet = {key: "world"}
print(greet["hello"])  // world
```

Writing `{name: 1}` where `name` is not a variable is an undefined-variable error, not a key spelled `"name"`. Quote a literal key.

### Reading and writing values

```jade
print(d["name"])       // jade

d["version"] = 2       // update existing key
d["stable"] = true     // add new key
```

### Length and lookup

```jade
print(len(d))          // number of key-value pairs
print(d.keys())        // [name, version]
print(d.values())      // [jade, 1]
print(d.has("name"))   // true
print(d.get("name"))   // jade
print(d.get("nope"))   // nil — a missing key is not an error here
print("name" in d)     // true
```

`in` and `not in` test keys, never values.

A dict cannot be iterated directly. Loop over `d.keys()` instead:

```jade
for k in d.keys() {
    print(k)
}
```

### Value semantics

Assigning a dict to a new variable copies it. Mutations to the copy do not affect the original:

```jade
let d2 = d
d2["name"] = "copy"
print(d["name"])   // jade   (unchanged)
print(d2["name"])  // copy
```

Arrays go the other way — assigning one shares the same storage, so writing through the second name is visible through the first. The two containers genuinely differ here; do not carry a habit from one to the other.

### Nested dicts

```jade
let outer = {"inner": {"x": 42}}
let inner = outer["inner"]
print(inner["x"])  // 42
```

:::note
Dict keys are always strings. Accessing a key that does not exist produces a runtime error.
:::
