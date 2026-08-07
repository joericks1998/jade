---
id: operators
title: Operators
sidebar_label: Operators
---

## Arithmetic

| Operator | Name | Example | Result |
|----------|------|---------|--------|
| `+` | Addition | `3 + 4` | `7` |
| `-` | Subtraction | `10 - 3` | `7` |
| `*` | Multiplication | `3 * 4` | `12` |
| `/` | Division | `10 / 4` | `2` |
| `%` | Remainder | `10 % 3` | `1` |
| `-` | Unary negation | `-5` | `-5` |

Two ints divide to an int, truncated toward zero: `10 / 4` is `2` and `-7 / 2` is `-3`. If either side is a float the result is a float, so `10 / 4.0` is `2.5`.

`%` takes its sign from the left operand, matching C: `-7 % 2` is `-1` and `7 % -2` is `1`.

Integer arithmetic is checked. A result outside the 63-bit `int` range raises `integer overflow` rather than wrapping.

## Bitwise

| Operator | Name | Example | Result |
|----------|------|---------|--------|
| `&` | Bitwise AND | `6 & 3` | `2` |
| `\|` | Bitwise OR | `5 \| 2` | `7` |
| `^` | Bitwise XOR | `7 ^ 3` | `4` |
| `~` | Bitwise NOT (unary) | `~0` | `-1` |
| `<<` | Left shift | `1 << 4` | `16` |
| `>>` | Right shift | `64 >> 2` | `16` |

## Logical

| Operator | Name | Example | Result |
|----------|------|---------|--------|
| `&&` or `and` | Logical AND (short-circuit) | `true && false` | `false` |
| `\|\|` or `or` | Logical OR (short-circuit) | `false \|\| true` | `true` |
| `!` or `not` | Logical NOT (unary) | `!true` | `false` |

`&&` and `||` are short-circuit: the right operand is not evaluated if the left operand decides the result. Both operands must be `bool`; mixing types is a type error.

The word forms are exact aliases of the symbols, not separate operators, so `a and b` and `a && b` compile to the same thing and bind at the same level. Pick one style and stay with it.

## Comparison

| Operator | Name | Example | Result |
|----------|------|---------|--------|
| `==` | Equal | `3 == 3` | `true` |
| `!=` | Not equal | `3 != 4` | `true` |
| `<` | Less than | `1 < 2` | `true` |
| `>` | Greater than | `2 > 1` | `true` |
| `<=` | Less than or equal | `2 <= 2` | `true` |
| `>=` | Greater than or equal | `3 >= 2` | `true` |

Equality (`==`, `!=`) requires both operands to be the same type — mixing `int` and `float` is a type error, caught by `jade check`. The one exception is `char` against `str`, which is explained in [Types](types#the-one-exception-char-and-str).

Ordering (`<`, `>`, `<=`, `>=`) works on two ints, two floats, an int against a float, two bools (`false` is below `true`), and two strings (character by character). Anything else is a type error.

Comparisons do not chain. `1 < 2 < 3` groups as `(1 < 2) < 3`, which then compares a `bool` to an `int` and fails. Write `1 < 2 && 2 < 3`.

## Membership

| Operator | Name | Example | Result |
|----------|------|---------|--------|
| `in` | Contains | `2 in [1, 2]` | `true` |
| `not in` | Does not contain | `3 not in [1, 2]` | `true` |

`in` works on an array (is any element this value), a string (is this a substring), and a dict (is this a key — never a value). It always produces a `bool` and never raises on a type mismatch: an element of another type simply answers `false`.

```jade
print(2 in [1, 2, 3])            // true
print("ell" in "hello")          // true
print("name" in {"name": 1})     // true
print("x" not in {"name": 1})    // true
```

## Pipe

| Operator | Name | Description |
|----------|------|-------------|
| `\|>` | Pipe | Pass left-hand value as the first argument to the right-hand function |

The pipe operator threads a value through a chain of function calls left-to-right. `x |> f` is equivalent to `f(x)`. When the right-hand side is a call expression with arguments, the piped value is inserted as the *first* argument: `5 |> add(3)` is equivalent to `add(5, 3)`.

```jade
fn double(x) { return x * 2 }
fn add(a, b) { return a + b }

// Simple pipe
let n = 5 |> double          // 10

// Chained pipes — left-associative
let m = 3 |> double |> double  // 12

// Pipe with extra arguments (value inserted as first arg)
let r = 5 |> add(3)          // add(5, 3) = 8

// Pipe to print
"hello" |> print

// Pipe with arithmetic on the left
let x = (2 + 3) |> double   // 10
```

### What a stage can be

A stage is the thing to the right of a `|>`. There are three kinds, and which one applies depends on what the name refers to, not on where the `|>` appears:

| Stage | Meaning |
|-------|---------|
| A function | Applied to the value, which becomes its first argument |
| A type name | On a prompt dereference, constrains generation with a grammar and coerces the reply; elsewhere it is the ordinary type constructor, so `x \|> int` is `int(x)` |
| A `Grammar` value | Constrains sampling on a prompt dereference |

Two rules settle a name that could be more than one thing, and they only matter on a prompt dereference:

- A builtin type keyword is always a type, never a function. `int`, `float`, `bool`, `char` and `str` are also callable constructors, so without this rule `?p |> int` would stop constraining the model and merely try to convert whatever came back.
- A `struct` you declared is always a type. A struct registers a constructor under its own name, so by callability alone every struct looks like a function; `?p |> City` would have become `City(?p)`.

Everything else prefers a function.

```jade
prompt p = "What is 21 + 21? Respond with only the number."

let n = ?p |> int |> double   // constrain, coerce, then apply
let g = Grammar.new('"yes" | "no"')
let a = ?p |> g               // constrain sampling with a grammar
```

:::note
Until v1.2.0 this was really two operators sharing a spelling. `|>` after a prompt dereference was read by a different parse rule that accepted exactly one constraint and could not chain, and a typed dereference was banned inside `print(...)`. Both restrictions are gone, and a stage that is none of the three above is now an `InvalidPipeStage` type error naming what it found rather than a parse error talking about tokens.
:::

Pipes are left-associative and have lower precedence than all other operators, so the entire expression to the left of `|>` is fully evaluated before being passed to the function on the right.

## Precedence

Operators bind from tightest to loosest in this order:

1. Unary: `~`, `!`, `-`
2. Multiplicative: `*`, `/`, `%`
3. Additive: `+`, `-`
4. Shifts: `<<`, `>>`
5. Bitwise AND: `&`
6. Bitwise XOR: `^`
7. Bitwise OR: `|`
8. Comparison and membership: `==`, `!=`, `<`, `>`, `<=`, `>=`, `in`, `not in`
9. Logical AND: `&&` / `and`
10. Logical OR: `||` / `or`
11. Pipe: `|>` (lowest — entire left expression is the piped value)

Note that comparison is *looser* than the bitwise operators, unlike C. `1 << 2 & 15` is `(1 << 2) & 15`, and `a & b == c` is `(a & b) == c`.

```jade
let x = 2 + 3 * 4
let y = 1 << 2 & 15
let z = 1 < 2 && 3 > 0
```

## When an operator fails

Operator failures split cleanly by when they are found, and the split is worth knowing because only one half can be caught with `try`.

**Caught by `jade check`, before anything runs.** These are type errors. The program does not start, so no output appears at all.

- Mismatched operands: `1 + true`, `1.0 & 2`
- Cross-type equality: `1 == 1.0`
- Non-bool logic: `1 && true`
- A pipe stage that cannot be applied: `5 |> 3`

**Raised while the program runs.** These depend on values, not types, so the checker cannot see them coming. Each is catchable with `try` / `catch`.

- Division by zero: `x / 0`
- Remainder by zero: `x % 0`
- Invalid shift amount, negative or ≥ 64: `1 << 64`
- Integer overflow: a result outside the 63-bit `int` range
- Index out of bounds: `[1, 2][5]`
- A missing dict key: `d["nope"]`

An operator on a value whose type the checker could not work out — an element of a mixed array, a value from an imported package — also lands in the second group. The check is deferred to run time rather than skipped:

```jade
let mixed = [1, "two"]
print(mixed[0] + mixed[1])   // passes `jade check`, then raises
```
