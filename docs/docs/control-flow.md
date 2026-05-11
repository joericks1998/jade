---
id: control-flow
title: Control Flow
sidebar_label: Control Flow
---

Jade provides four control flow constructs: `if`/`elif`/`else` for conditional branching, `while` for condition-driven loops, and `for` for iterating over arrays. All conditions must be `bool`.

## Overview

The `if` statement tests a condition expression and executes the *then* block when the condition is `true`. An optional `else` block executes when the condition is `false`. If no `else` is provided and the condition is `false`, the statement does nothing.

The `while` statement repeatedly evaluates its condition and executes its body as long as the condition remains `true`. When the condition becomes `false`, execution continues with the statement after the closing `}`. If the condition is `false` on the first check, the body never executes.

Both constructs require the condition to evaluate to a `bool` value. Providing any other type — an `int`, `float`, or `fn` value — raises a `TypeError`. There is no implicit truthiness for non-boolean types.

Control flow statements can appear at the top level or inside function bodies. A `return` inside a branch or loop body exits the enclosing function. Using `return` outside of a function body raises a `ReturnOutsideFunction` error.

## if / elif / else

### if without else

```jade
if <condition> {
    <then statements>
}
```

### if with else

```jade
if <condition> {
    <then statements>
} else {
    <else statements>
}
```

### if / elif / else

```jade
if <condition> {
    <then statements>
} elif <condition> {
    <elif statements>
} else {
    <else statements>
}
```

Any number of `elif` branches may follow the initial `if`. Each branch is tested in order; the first one whose condition is `true` executes, and the rest are skipped. The final `else` is optional and runs only when every preceding condition is `false`.

```jade
fn classify(x) {
    if x > 0 {
        return 1
    } elif x < 0 {
        return -1
    } else {
        return 0
    }
}

fn grade(score) {
    if score >= 90 {
        return 4
    } elif score >= 80 {
        return 3
    } elif score >= 70 {
        return 2
    } elif score >= 60 {
        return 1
    } else {
        return 0
    }
}
```

:::note
The `else` keyword must appear on the same line as the closing `}` of the `then` block, or on the next line. Because the lexer does not insert a semicolon after `}`, writing `} else {` on one line or splitting across lines both work correctly. The same applies to `} elif {`.
:::

## Basic Examples

### Returning different values based on a condition

```jade
fn max(a, b) {
    if a > b {
        return a
    } else {
        return b
    }
}

let m1 = max(3, 7)
let m2 = max(10, 2)
```

### if without else — early return pattern

```jade
fn clamp(x, lo, hi) {
    if x < lo {
        return lo
    }
    if x > hi {
        return hi
    }
    return x
}

let clamped_lo  = clamp(1, 5, 10)
let clamped_mid = clamp(7, 5, 10)
let clamped_hi  = clamp(15, 5, 10)
```

Multiple `if` statements without `else` create an early-exit chain. `clamp(1, 5, 10)` hits the first condition and returns `5`. `clamp(7, 5, 10)` skips both and falls through to `return x`, returning `7`. `clamp(15, 5, 10)` hits the second condition and returns `10`.

### Nested if/else — sign function

```jade
fn sign(x) {
    if x > 0 {
        return 1
    } else {
        if x < 0 {
            return -1
        } else {
            return 0
        }
    }
}
```

## Type Rules

| Operation | Condition Type | Result |
|-----------|---------------|--------|
| `if <cond>` — condition is `true` | `bool` | Then block executes |
| `if <cond>` — condition is `false` | `bool` | Else block executes (if present) |
| `if <cond>` — condition is not a `bool` | `int`, `float`, or `fn` | `TypeError` |
| `return` inside a branch | Any | Propagates return value up to enclosing function |

:::note
The condition must be exactly a `bool`. There is no implicit truthiness: `if 1 { … }` is a `TypeError`, not a truthy integer check.
:::

## while Loops

The `while` statement is Jade's iteration construct. It evaluates a `bool` condition before each iteration and executes the loop body as long as the condition is `true`. Loop termination is controlled entirely by changing the values that the condition expression tests — typically by using bare assignment (`i = i + 1`) to update a variable that was declared before the loop.

### Syntax

```jade
while <condition> {
    <body statements>
}
```

:::note
The condition and the opening `{` must be on the same line. Because the lexer inserts a semicolon after any line ending in an integer, float, identifier, `true`, `false`, or `)` token, writing the condition and brace on separate lines would insert an unexpected semicolon. Write `while i < 5 {` on one line.
:::

### Counting up to a limit

```jade
let i = 0
while i < 5 {
    i = i + 1
}
```

Bare assignment updates `i` on each iteration. When `i` reaches `5`, the condition becomes `false` and the loop exits. After the loop, `i` holds `5`.

### Accumulating a sum

```jade
let n = 10
let sum = 0
let i = 1
while i <= n {
    sum = sum + i
    i = i + 1
}
```

After the loop, `sum` holds `55` (the sum of integers 1 through 10).

### Condition false from the start

```jade
let never = 99
while never < 0 {
    never = never + 1
}
```

The condition is `false` on the first check, so the body never executes. `never` remains `99`.

### Iterative factorial

```jade
fn factorial(n) {
    let result = 1
    let i = 1
    while i <= n {
        result = result * i
        i = i + 1
    }
    return result
}

let f5 = factorial(5)
```

`factorial(5)` returns `120`.

### Nested while loops

```jade
let total = 0
let i = 0
while i < 3 {
    let j = 0
    while j < 3 {
        total = total + 1
        j = j + 1
    }
    i = i + 1
}
```

The inner loop runs to completion on each iteration of the outer loop. After both loops finish, `total` holds `9`.

## for Loops

The `for` statement iterates over an array, binding each element to a loop variable in turn. It is the idiomatic way to process every element of an array without managing an index manually.

```jade
for <var> in <array> {
    <body statements>
}
```

- `<var>` — a name bound to each element on each iteration. It is visible throughout the loop body.
- `<array>` — any expression that evaluates to an `array`. The array is fully evaluated once before iteration begins.

### Basic iteration

```jade
let nums = [1, 2, 3, 4, 5]
for n in nums {
    print(n)
}
// 1
// 2
// 3
// 4
// 5
```

### Inline array literal

```jade
for x in [10, 20, 30] {
    print(x)
}
```

### Accumulating a result

```jade
let total = 0
let nums = [1, 2, 3, 4, 5]
for n in nums {
    total = total + n
}
// total is 15
```

### for inside a function

```jade
fn sum_array(arr) {
    let total = 0
    for x in arr {
        total = total + x
    }
    return total
}

let s = sum_array([10, 20, 30])  // 60
```

:::note
The iterable expression must evaluate to an `array`. Attempting to iterate over any other type (such as a string or dict) produces a `TypeError`.
:::

:::note
**REPL limitation:** `for` loops are not supported in `jade repl`. The REPL uses the tree-walk evaluator, which does not implement `for`. Use `jade run` or `jade build` to run programs that contain `for` loops.
:::
