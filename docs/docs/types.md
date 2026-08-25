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

An `int` runs from `-4611686018427387904` to `4611686018427387903`. One bit of the machine word is spent tagging what kind of value it holds, which leaves 63 bits for the number. The language exposes that limit directly, so a program means the same thing whether it is interpreted or compiled.

A literal outside that range is refused when the file is read. The lowest value is a special case. Jade treats `-` as a separate negation, so it checks the digits before applying the sign. That means you cannot type `-4611686018427387904` directly, though arithmetic can still reach it.

Arithmetic that would leave the range raises an error instead of wrapping around:

```jade
let big = 3037000500
try {
    print(big * big)
} catch e {
    print("overflow caught")
}
```

## Type Coercion Rules

Jade converts between types on its own only in a few arithmetic and comparison cases:

| Rule | Example | Result |
|------|---------|--------|
| Arithmetic: `int op float` → float | `1 + 0.5` | `1.5` (float) |
| Ordering: `int < float` allowed | `1 < 2.5` | `true` |
| Strict equality: no cross-type coercion | `1 == 1.0` | type error |
| Bool ordering: `false` = 0, `true` = 1 | `false < true` | `true` |
| String ordering, by character | `"abc" < "abd"` | `true` |
| Bitwise: integer operands only | `1.0 & 2` | type error |
| Logical: bool operands only | `1 && true` | type error |
| `char` and `str` compare and concatenate | `"hi"[0] == "h"` | `true` |

:::note
Arithmetic promotion converts an `int` to a `float` when the other operand is a `float`. Equality never promotes, so comparing `1` to `1.0` is always an error.

Every row marked "type error" is caught by `jade check` before the program runs, not while it runs.
:::

### The one exception: `char` and `str`

A `char` compares equal to the one-character string that spells it. It also orders against strings and concatenates with them in either direction. That is a deliberate hole in the strict-equality rule above, and the reason is worth knowing so you do not read the rest of the rule as merely a suggestion.

Indexing a string used to return a one-character `str`. As of v1.2.1 it returns a `char`. Without this exception, every `if s[0] == "a"` already written would have become a `TypeError` overnight. The exception keeps that code working.

```jade
let s = "café"
print(s[0] == "c")     // true
print(s[0] + "at")     // cat
for c in s { print(c) }  // four lines: "é" is one character, not two
```

## Char

A `char` is a single Unicode scalar, not a byte. It is an *immediate* value, meaning it fits inside the tagged value word alongside `int`, `bool`, and `nil`. Scanning a string therefore allocates nothing. The old one-character-string behavior allocated once per character.

There is no char literal. A char comes from indexing a string, iterating one, `char("x")`, `char(<number>)`, or `?p |> char`.

`int(c)` gives a character's Unicode scalar. `char(n)` builds a character from a number, and refuses anything that is not a valid scalar rather than substituting a replacement character.

The pair exists for reading fixed-size C fields across the FFI. A `char[32]` arrives as thirty-two characters with its NUL padding intact. Nothing trims the padding, because trimming would mean guessing where the text stops. Instead, a program finds the end itself by testing `int(c) == 0`.

```jade
let c = char("x")
let first = "hello"[0]
```

`len` of a char is 1. That matches the answer it gave back when indexing produced a one-character string.

A char taken from a tainted string stays tainted. So a loop that rebuilds a string one character at a time cannot quietly strip the taint and slip the result past `sh.exec`.

## Bytes

A `bytes` value is a counted sequence of raw octets. It is deliberately not a string, and the reason matters. A Jade string is UTF-8 and NUL-terminated. So a blob containing a zero byte would be cut short at that byte, and a blob that is not valid UTF-8 would be corrupted by anything treating it as text. `fs.read` decodes UTF-8, which is why it cannot read a PNG at all.

Conversion is explicit in both directions, never implicit:

```jade
use std::fs

let raw = "hi".encode()      // str -> bytes, UTF-8
print(raw)                    // b"hi"
print(len(raw))               // 2
print(raw[0])                 // 104. An octet is an int, not a char.
print(raw.decode())           // bytes -> str, raises on invalid UTF-8

fs.write_bytes("out.bin", raw)
let back = fs.read_bytes("out.bin")
```

Indexing gives an `int` from 0 to 255 rather than a `char`. A byte is not a Unicode scalar. Making `b[0]` behave like `s[0]` would hide the fact that the two give different answers on any non-ASCII input.

A blob has three methods: `len()`, `decode()`, and `slice(start, end)`. `slice` clamps its bounds rather than raising, so you can take the tail of a buffer without checking its length first.

```jade
let raw = "hello".encode()
print(raw.len())            // 5
print(raw.slice(0, 2))      // b"he"
print(raw.slice(3, 99))     // b"lo". The end clamps to the length.
```

Writing an octet is spelled `b[i] = v`, the same way an array works, and the value is an int from 0 to 255. A blob is *reference-semantic*: two names for one buffer see the same write. Building a blob from nothing is what [`std::bytes`](stdlib#stdbytes) is for.

```jade
use std::bytes

let buf = bytes.zeros(3)
buf[0] = 255
print(buf)                  // b"\xff\x00\x00"
```

`bytes` is deliberately not a second string type, so there is no way to compare two blobs directly. Using `==` on them is an error. Decode both first, or compare a slice.

`print(b)` shows an escaped `b"…"` form rather than dumping raw octets to your terminal, because a blob can contain control characters or an escape sequence. Use `decode()` when the bytes really are text.

Bytes carry a trust marker the same way strings do. So `sh.exec` refuses `fs.read_bytes(p).decode()` for the same reason it refuses `fs.read(p)`.

## Stream

A function whose body contains a `yield` returns a *stream* rather than a single value. The body runs all the way through, filling a buffer, and the caller receives that buffer.

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

A stream is a buffer, not a one-shot channel. It keeps everything it produced, so reading it twice gives the same values twice. There is no "already consumed" state to keep track of. `len` and indexing work for the same reason.

A prompt dereference is a stream too. `?p` produces one lazily, so `print(?p)` shows tokens as they arrive. You can still read the same value again afterwards.

A bare `return` stops a generator early. Writing `return x` is a compile error, because a function that yields already produces a stream. Returning a value as well would ask it to be two things at once.

Yields of different types widen into a mixed stream rather than failing. A mixed array literal follows the same rule.

## Prompt

A `prompt` holds text meant for a model. It is a value like any other, so you can pass it to a function, store it in a struct field, or hand it to another file. Nothing is sent until you dereference it with `?`.

```jade
prompt p = "Name one prime number under 10."
print(?p)
```

`?p` on its own produces a stream of the reply. A `|>` stage after it limits what the model may generate and coerces the result, so `?p |> int` gives you an `int`. See [Prompts and Inference](llm) for the whole story, and [Operators](operators#what-a-stage-can-be) for what a stage may be.

## Handle

A `handle<T>` is an opaque pointer a native package gave you, such as a database connection, an open audio file, or a decompression context. Jade holds it, passes it back to the library, and never looks inside. There are no operations on a handle itself. Everything you do with one is a call into the package that made it.

The `T` is the C type the pointer came from, so `handle<sqlite3>` and `handle<sqlite3_stmt>` are different types. Passing one where the other belongs gives you a readable error instead of a crash inside the library. Printing a handle shows `handle<sqlite3>`, never an address.

Two rules are worth knowing before you use one:

*Jade never closes a handle for you.* It cannot know what the pointer is or which allocator made it. Closing is a call the package exposes, and a handle dropped without that call leaks whatever the library allocated.

*A handle cannot be passed into a task.* Jade cannot see what a library does with a pointer, so it cannot tell a thread-safe library from an unsafe one. Sharing is refused at compile time. Open the handle inside the task instead.

See [Packages](packages) for installing a native package.

## Nil

`nil` is Jade's single "absence of value" type. Three things evaluate to it: a function that reaches the end of its body without returning, a bare `return`, and most built-ins that mutate something, such as `arr.push(x)` and `fs.write(...)`.

### Three spellings

`nil`, `None`, and `null` are three spellings of the *same* value. They all evaluate to the one `nil` and compare equal to each other. You can use any of them anywhere a literal is expected, including default parameter values and type annotations.

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
The three spellings are aliases, not distinct types. There is no separate `null` type. JSON `null` also decodes to the same `nil`.
:::

## Dict

A `dict` is a mutable hash map with string keys. Values can be any Jade type. You write a dict with curly braces and read it with square brackets.

### Creating a dict

```jade
let d = {"name": "jade", "version": 1}
let empty = {}
```

A key can be any expression that produces a string. Jade evaluates it when it builds the literal, so a variable in key position contributes its *value*, not its name:

```jade
let key = "hello"
let greet = {key: "world"}
print(greet["hello"])  // world
```

Writing `{name: 1}` where `name` is not a variable gives an undefined-variable error, not a key spelled `"name"`. Quote a literal key.

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
print(d.get("nope"))   // nil. A missing key is not an error here.
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

Arrays behave the opposite way. Assigning an array shares the same storage, so writing through the second name is visible through the first. Structs behave like arrays. The three containers genuinely differ here, so do not carry a habit from one to another.

| | assigning it | passing it to a function that writes to it |
|---|---|---|
| `dict` | copies | the caller does *not* see the write |
| `array` | shares | the caller sees the write |
| `struct` | shares | the caller sees the write |

### Passing a dict to a function

This is the same rule again, and it is the one that costs people an afternoon. A parameter is an assignment, so a function receives its own copy of a dict. A write to that copy is invisible to the caller. The identical code on an array or a struct does reach the caller:

```jade
fn set_it(d) { d["x"] = 99 }
fn push_it(a) { a.push(99) }

let d = {"x": 1}
set_it(d)
print(d["x"])    // 1. Unchanged: the function wrote to its own copy.

let a = [1]
push_it(a)
print(a.len())   // 2. Changed.
```

Nothing warns you, and the call reports success. So the search for the bug usually starts wherever the stale value is read, rather than at the write that never landed.

*Use a struct when a function needs to change something the caller can see.* That is the shape the language is built for. Declare the fields, and the change travels back.

```jade
struct Cursor { x, y }

fn advance(c, dy) { c.y = c.y + dy }

let cur = Cursor { x: 0, y: 0 }
advance(cur, 10)
print(cur.y)     // 10
```

### Nested dicts

Reading a dict out of a dict is also an assignment, so it copies. Writing to what you read back does not reach the outer dict:

```jade
let outer = {"inner": {"x": 42}}
let inner = outer["inner"]
print(inner["x"])  // 42

inner["x"] = 1
print(outer["inner"]["x"])  // 42. The outer dict still holds its own copy.
```

:::note
Dict keys are always strings. Accessing a key that does not exist produces a runtime error.
:::
