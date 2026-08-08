---
id: control-flow
title: Control Flow
sidebar_label: Control Flow
---

Jade has three control flow constructs: `if`/`elif`/`else` for conditional branching, `while` for condition-driven loops, and `for` for iterating over a sequence. All conditions must be `bool`.

Inside a loop, `break` leaves it and `continue` starts its next iteration.

## Overview

The `if` statement tests a condition expression and executes the *then* block when the condition is `true`. An optional `else` block executes when the condition is `false`. If no `else` is provided and the condition is `false`, the statement does nothing.

The `while` statement repeatedly evaluates its condition and executes its body as long as the condition remains `true`. When the condition becomes `false`, execution continues with the statement after the closing `}`. If the condition is `false` on the first check, the body never executes.

Both constructs require the condition to evaluate to a `bool` value. Providing any other type is a compile error: `if 1 { … }` fails with `type mismatch: expected bool, got int`. There is no implicit truthiness for non-boolean types.

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
| `if <cond>` — condition is not a `bool` | `int`, `float`, `str`, `fn`, … | Compile error: `type mismatch: expected bool, got <type>` |
| `return` inside a branch | Any | Propagates return value up to enclosing function |

:::note
The condition must be exactly a `bool`. There is no implicit truthiness: `if 1 { … }` is a compile error, not a truthy integer check.
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

:::warning
**A `let` inside a loop body does not shadow — it overwrites.** If you `let` a name that already exists outside the loop, you are reassigning the outer variable, and the last value written survives the loop.

```jade
let x = "outer"
let i = 0
while i < 3 {
    let x = i     // this is the same x
    i = i + 1
}
print(x)          // 2, not "outer"
```

The same applies to a `for` body. Pick a different name for a loop-local value.
:::

## for Loops

The `for` statement walks a sequence, binding each element to a loop variable in turn. It is the idiomatic way to process every element without managing an index manually.

```jade
for <var> in <sequence> {
    <body statements>
}
```

- `<var>` — a name bound to each element on each iteration. It is visible throughout the loop body.
- `<sequence>` — an expression that evaluates to one of four types. It is fully evaluated once before iteration begins.

| Sequence type | The loop variable holds |
|---------------|-------------------------|
| `array` | each element |
| `str` | each character, as a `char` |
| `bytes` | each byte, as an `int` |
| stream (from a `yield` function) | each yielded value |

Anything else is a compile error: `cannot iterate over dict`.

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

### Walking a string by character

```jade
for c in "jade" {
    print(c)
}
// j
// a
// d
// e
```

Each `c` is a `char`, not a one-character string.

### Walking a stream

A function that contains `yield` returns a stream, and `for` reads it like any other sequence. See [Functions](functions) for how streams are produced.

```jade
fn doubles(n) {
    let i = 0
    while i < n {
        yield i * 2
        i = i + 1
    }
}

for x in doubles(3) {
    print(x)
}
// 0
// 2
// 4
```

:::note
A `dict` is not iterable. `for k in some_dict { … }` is a compile error. Iterate over `some_dict.keys()` instead.
:::

## break and continue

`break` leaves the innermost enclosing loop. `continue` skips the rest of the body and starts that loop's next iteration. Both work in `while` and in `for`.

```jade
for i in [1, 2, 3, 4, 5] {
    if i == 4 { break }
    if i == 2 { continue }
    print(i)
}
// 1
// 3
```

They act on the *innermost* loop only. To leave two, `return` out of the enclosing function.

```jade
for a in [1, 2] {
    for b in [1, 2, 3] {
        if b == 2 { break }     // leaves the b loop, not the a loop
        print(f"{a}-{b}")
    }
}
// 1-1
// 2-1
```

### Loop until something happens

`while true` with a `break` is the shape to reach for when the exit condition only becomes known part-way through the body, rather than at the top.

```jade
let n = 0
while true {
    n = n + 1
    if n * n > 50 { break }
}
print(n)   // 8
```

### Leaving through a catch

A loop can end because something raised. This is the natural shape for a C library that signals end-of-input with an error code, since [`fails_when`](packages#the-binding-vocabulary) turns that code into an exception.

```jade
while true {
    try {
        print(archive.next_entry(a))
    } catch e {
        break
    }
}
```

`break` and `continue` remove any exception handler they jump out of, so a `try` later in the same function still catches its own exceptions. That holds whether you leave from a `catch` arm, as above, or from the `try` body itself.

### Where they are not allowed

`break` and `continue` need a loop in the *same* function. A loop outside the enclosing `fn` or closure does not count, because leaving it would mean crossing a call frame — which is what `return` is for.

```jade
for i in [1, 2] {
    fn f() {
        break     // error: 'break' outside a loop
    }
}
```

Using either at the top level, or in an `if` with no loop around it, is the same error. It is reported when the file is parsed, not when the line runs.
