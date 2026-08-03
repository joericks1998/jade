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
| `/` | Division | `10 / 4` | `2` (int÷int) / `2.5` (float) |
| `%` | Remainder (modulus) | `10 % 3` | `1` |
| `-` | Unary negation | `-5` | `-5` |

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
| `&&` | Logical AND (short-circuit) | `true && false` | `false` |
| `\|\|` | Logical OR (short-circuit) | `false \|\| true` | `true` |
| `!` | Logical NOT (unary) | `!true` | `false` |

`&&` and `||` are short-circuit: the right operand is not evaluated if the left operand determines the result. Both operands must be `bool`; mixing types produces a `TypeError`.

## Comparison

| Operator | Name | Example | Result |
|----------|------|---------|--------|
| `==` | Equal | `3 == 3` | `true` |
| `!=` | Not equal | `3 != 4` | `true` |
| `<` | Less than | `1 < 2` | `true` |
| `>` | Greater than | `2 > 1` | `true` |
| `<=` | Less than or equal | `2 <= 2` | `true` |
| `>=` | Greater than or equal | `3 >= 2` | `true` |

Equality (`==`, `!=`) requires both operands to be the same type — mixing `int` and `float` produces a `TypeError`. Ordering operators (`<`, `>`, `<=`, `>=`) allow `int` vs `float` comparisons by promoting the integer to float.

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

Only one rule needs remembering when names collide. A builtin type keyword is always a type, because `?p |> int` has to keep constraining the model. Everything else prefers a function.

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
8. Comparison: `==`, `!=`, `<`, `>`, `<=`, `>=`
9. Logical AND: `&&`
10. Logical OR: `||`
11. Pipe: `|>` (lowest — entire left expression is the piped value)

```jade
let x = 2 + 3 * 4
let y = 1 << 2 & 15
let z = 1 < 2 && 3 > 0
```

## Runtime Errors

The following operations produce runtime errors:

- Division by zero: `x / 0`
- Remainder by zero: `x % 0`
- Invalid shift amount (negative or ≥ 64): `1 << 64`
- Type mismatch: `1 + true`, `1.0 & 2`, `1 == 1.0`
