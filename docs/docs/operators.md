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

Two ints divide to an int, truncated toward zero, so `10 / 4` is `2` and `-7 / 2` is `-3`. If either side is a float, the result is a float, so `10 / 4.0` is `2.5`.

`%` takes its sign from the left operand, the same as C. So `-7 % 2` is `-1` and `7 % -2` is `1`.

Integer arithmetic is checked. A result outside the 63-bit `int` range raises `integer overflow` rather than wrapping around.

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

`&&` and `||` short-circuit. If the left operand already decides the answer, the right operand is never evaluated. Both operands must be `bool`, and mixing types is a type error.

The word forms are exact aliases of the symbols, not separate operators. `a and b` and `a && b` compile to the same thing and bind at the same level. Pick one style and stay with it.

## Comparison

| Operator | Name | Example | Result |
|----------|------|---------|--------|
| `==` | Equal | `3 == 3` | `true` |
| `!=` | Not equal | `3 != 4` | `true` |
| `<` | Less than | `1 < 2` | `true` |
| `>` | Greater than | `2 > 1` | `true` |
| `<=` | Less than or equal | `2 <= 2` | `true` |
| `>=` | Greater than or equal | `3 >= 2` | `true` |

Equality, meaning `==` and `!=`, requires both operands to be the same type. Mixing `int` and `float` is a type error that `jade check` catches. The one exception is `char` against `str`, explained in [Types](types#the-one-exception-char-and-str).

Ordering, meaning `<`, `>`, `<=`, and `>=`, works on five combinations: two ints, two floats, an int against a float, two bools where `false` sorts below `true`, and two strings compared character by character. Anything else is a type error.

Comparisons do not chain. `1 < 2 < 3` groups as `(1 < 2) < 3`, which compares a `bool` to an `int` and fails. Write `1 < 2 && 2 < 3` instead.

## Membership

| Operator | Name | Example | Result |
|----------|------|---------|--------|
| `in` | Contains | `2 in [1, 2]` | `true` |
| `not in` | Does not contain | `3 not in [1, 2]` | `true` |

`in` works on three kinds of value. On an array it asks whether any element equals the value. On a string it asks whether the text is a substring. On a dict it asks whether the text is a key, never a value. It always produces a `bool` and never raises on a type mismatch, because an element of another type simply answers `false`.

```jade
print(2 in [1, 2, 3])            // true
print("ell" in "hello")          // true
print("name" in {"name": 1})     // true
print("x" not in {"name": 1})    // true
```

## Pipe

| Operator | Name | Description |
|----------|------|-------------|
| `\|>` | Pipe | Call the right-hand function once per piece of the left-hand value, as each piece arrives |

A pipe listens to a value as it arrives. `x |> f` calls `f` once for each piece of `x`. A call such as `f(x)` waits for all of `x` and calls `f` once.

Most values are one piece, so for them a pipe is one call: `5 |> double` is `double(5)`. When the right side is already a call with arguments, the piece goes in as the *first* argument, so `5 |> add(3)` means `add(5, 3)`.

```jade
fn double(x) { return x * 2 }
fn add(a, b) { return a + b }

let n = 5 |> double            // 10
let m = 3 |> double |> double  // 12, left to right
let r = 5 |> add(3)            // add(5, 3) = 8
"hello" |> print
let x = (2 + 3) |> double      // 10
```

### What a piece is

| Source | Its pieces |
|--------|------------|
| A call to a function that `yield`s | Each value, at the moment it is yielded |
| A stream already held, such as a variable | Each value, one after another |
| A prompt dereference, `?p` | The reply's text, a piece at a time, as the model writes it |
| Anything else, arrays included | The whole value, once |

A chain is one pipe: each piece goes through every stage before the next piece is made.

```jade
fn numbers() {
    print("making 1")
    yield 1
    print("making 2")
    yield 2
}

numbers() |> double |> print   // making 1, 2, making 2, 4
print(numbers())               // making 1, making 2, [1, 2]
?p |> write                    // the reply, as the model writes it
```

A pipe keeps its source's shape. Over one piece it gives back what the last stage gave, and over a stream it gives back a stream of those results. Written as a statement, a pipe over a stream runs to the end.

A stage's other arguments are worked out once, before the first piece. A raise in a stage stops the pipe, and the generator with it. The generator's own `try` does not catch the raise, because a stage is the caller's code.

### What a stage can be

A stage is a function, or a call that names the other arguments. Anything else, such as `5 |> 3`, is an `InvalidPipeStage` error that names what it found.

To shape a whole reply, call the converter, class or grammar around the dereference instead of piping it: `toInt(?p)`, `City(?p)`, `g(?p)`. Piping one straight after `?p`, as in `?p |> toInt`, would run it on each piece of the reply, so the build refuses it and names the call to write. See [LLM Integration](llm.md#typed-dereference).

```jade
prompt p = "What is 21 + 21? Respond with only the number."

let n = toInt(?p) |> double     // ask for an int, then double it
```

Pipes are left-associative and bind more loosely than every other operator. So the whole expression to the left of `|>` is worked out first.

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
11. Pipe: `|>`, the loosest of all. The entire left expression becomes the piped value.

Note that comparison binds *more loosely* than the bitwise operators, which is the opposite of C. So `1 << 2 & 15` groups as `(1 << 2) & 15`, and `a & b == c` groups as `(a & b) == c`.

```jade
let x = 2 + 3 * 4
let y = 1 << 2 & 15
let z = 1 < 2 && 3 > 0
```

## When an operator fails

Operator failures divide into two groups by when Jade finds them. The split matters, because only one group can be caught with `try`.

*Caught by `jade check`, before anything runs.* These are type errors. The program never starts, so it produces no output at all.

- Mismatched operands: `1 + true`, `1.0 & 2`
- Cross-type equality: `1 == 1.0`
- Non-bool logic: `1 && true`
- A pipe stage that cannot be applied: `5 |> 3`

*Raised while the program runs.* These depend on values rather than types, so the checker cannot see them coming. You can catch each one with `try` and `catch`.

- Division by zero: `x / 0`
- Remainder by zero: `x % 0`
- Invalid shift amount, negative or ≥ 64: `1 << 64`
- Integer overflow: a result outside the 63-bit `int` range
- Index out of bounds: `[1, 2][5]`
- A missing dict key: `d["nope"]`

An operator on a value whose type the checker could not work out also lands in the second group. An element of a mixed array and a value from an imported package are both examples. The check is delayed until the program runs, not skipped:

```jade
let mixed = [1, "two"]
print(mixed[0] + mixed[1])   // passes `jade check`, then raises
```
