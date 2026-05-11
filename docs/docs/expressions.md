---
id: expressions
title: Expressions
sidebar_label: Expressions
---

An expression produces a value. Jade's runtime types are `int` (64-bit signed integer), `float` (64-bit floating point), `bool`, `str` (UTF-8 string), arrays, user-defined `struct`s, and `fn` (function values). Expressions can be nested arbitrarily using parentheses or evaluated left-to-right following standard operator precedence.

## Integer Literals

```jade
let a = 0
let b = 1000000
```

Integer literals are 64-bit signed integers (`i64`).

## Float Literals

```jade
let pi = 3.14
let half = 0.5
```

Float literals require at least one digit on each side of the decimal point. `3.14` is valid; bare `.5` is not.

## Boolean Literals

```jade
let yes = true
let no  = false
```

The keywords `true` and `false` produce `bool` values.

## Identifiers

A variable name used in an expression evaluates to the variable's current value.

```jade
let base = 8
let doubled = base * 2
```

## Parenthesized Expressions

Any expression can be wrapped in parentheses to override default precedence:

```jade
let a = (2 + 3) * 4
let b = -(3 + 4)
```

## Function Calls

A function value followed by a parenthesized argument list calls that function:

```jade
fn add(a, b) {
    return a + b
}
let sum = add(3, 4)
let nested = add(add(1, 2), 3)
```

See [Functions](functions) for the full reference.

## Binary Expressions

Two values combined with an operator:

```jade
let sum  = 3 + 4
let diff = 10 - 3
let prod = 6 * 7
let quot = 20 / 4
let rem  = 10 % 3
let bits = 255 & 15
let mask = 1 << 4
let flag = 1 < 2 && 3 > 0
```

Expressions associate left-to-right within the same precedence level:

```jade
let x = 10 - 3 - 2
```

This evaluates as `(10 - 3) - 2 = 5`.

## Unary Expressions

Jade has three unary prefix operators:

- `~` — bitwise NOT (integers only)
- `!` — logical NOT (booleans only)
- `-` — arithmetic negation (integers and floats)

```jade
let inv   = ~0
let neg   = -5
let nflag = !true
```

## String Literals

String literals may be delimited by double quotes (`"…"`) or single quotes (`'…'`) — both forms are identical. Triple-quoted strings (`"""…"""` or `'''…'''`) span multiple lines. The `+` operator concatenates two strings.

```jade
let hello = "hello"
let world = 'world'
let hw    = hello + " " + world

let multi = """
line one
line two
"""

let also_multi = '''
line one
line two
'''
```

Strings support indexing with `[i]` to extract a single character as a one-character string. Indexes are zero-based. Out-of-range indexes raise `IndexOutOfBounds`.

```jade
let s = "hello"
let h = s[0]
```

## F-String Interpolation

An f-string is prefixed with `f` before the opening quote. Any expression inside `{ }` is evaluated and its value is converted to a string and embedded in place. Both quote styles are supported.

```jade
let name = "Jade"
let n    = 42
let msg  = f"hello, {name}! answer is {n}"
let msg2 = f'hello, {name}! answer is {n}'
```

Triple-quoted f-strings are written as `f"""…"""` or `f'''…'''` and behave the same way.

## Array Literals

An array is written as a comma-separated list inside square brackets. Arrays may be empty and may hold values of any type.

```jade
let a     = [1, 2, 3]
let empty = []
let mixed = [1, 2.0, true, "hello"]
```

Elements are accessed with `arr[i]` (zero-based). Elements can be assigned with `arr[i] = expr`. Arrays have reference semantics — assigning an array creates an alias that shares the same backing store.

## Pipe Operator

The `|>` operator passes the left-hand value as the first argument to the right-hand function. Pipes chain left-to-right.

```jade
fn double(x) { return x * 2 }
let n = 5 |> double            // double(5) → 10
let m = 3 |> double |> double  // double(double(3)) → 12
```

When the right-hand side is a call expression, the left value is inserted as the first argument before those already listed:

```jade
fn add(a, b) { return a + b }
let r = 5 |> add(3)           // add(5, 3) → 8
```
