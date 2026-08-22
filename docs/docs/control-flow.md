---
id: control-flow
title: Control Flow
sidebar_label: Control Flow
---

Jade has three control flow constructs. `if`, `elif`, and `else` choose between branches. `while` repeats while a condition holds. `for` walks a sequence. Every condition must be a `bool`.

Inside a loop, `break` leaves it and `continue` starts its next iteration.

## Overview

The `if` statement tests a condition and runs the *then* block when the condition is `true`. An optional `else` block runs when the condition is `false`. With no `else` and a false condition, the statement does nothing.

The `while` statement checks its condition and runs its body, over and over, for as long as the condition stays `true`. Once the condition turns `false`, the program continues after the closing `}`. If the condition is `false` on the very first check, the body never runs at all.

Both constructs need a condition that produces a `bool`. Any other type is a compile error, so `if 1 { … }` fails with `type mismatch: expected bool, got int`. Non-boolean types are never treated as true or false.

Control flow statements can appear at the top level or inside a function body. A `return` inside a branch or a loop exits the function that encloses it. Using `return` with no enclosing function raises a `ReturnOutsideFunction` error.

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

Any number of `elif` branches can follow the first `if`. Jade tests each branch in order, runs the first one whose condition is `true`, and skips the rest. The closing `else` is optional, and it runs only when every condition before it was `false`.

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
The `else` keyword can go on the same line as the closing `}` of the then block, or on the next line. The lexer does not insert a semicolon after `}`, so both layouts work. The same is true of `} elif {`.
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

### if without else, the early return pattern

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

Several `if` statements without an `else` form an early-exit chain. `clamp(1, 5, 10)` matches the first condition and returns `5`. `clamp(7, 5, 10)` matches neither, so it falls through to `return x` and gives back `7`. `clamp(15, 5, 10)` matches the second condition and returns `10`.

### Nested if and else, a sign function

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
| `if <cond>` where the condition is `true` | `bool` | The then block runs |
| `if <cond>` where the condition is `false` | `bool` | The else block runs, if there is one |
| `if <cond>` where the condition is not a `bool` | `int`, `float`, `str`, `fn`, … | Compile error: `type mismatch: expected bool, got <type>` |
| `return` inside a branch | Any | Propagates return value up to enclosing function |

:::note
The condition must be exactly a `bool`. Nothing else counts as true or false, so `if 1 { … }` is a compile error rather than a truthy integer check.
:::

## while Loops

`while` is Jade's basic loop. It checks a `bool` condition before each pass and runs the body for as long as the condition is `true`. The loop ends only when something changes a value the condition tests. Usually that means a bare assignment such as `i = i + 1`, updating a variable declared before the loop.

### Syntax

```jade
while <condition> {
    <body statements>
}
```

:::note
Keep the condition and the opening `{` on one line. The lexer inserts a semicolon after any line ending in an integer, a float, an identifier, `true`, `false`, or `)`. Splitting the condition from the brace would therefore insert a semicolon where you did not want one. Write `while i < 5 {` on a single line.
:::

### Counting up to a limit

```jade
let i = 0
while i < 5 {
    i = i + 1
}
```

The bare assignment updates `i` on each pass. When `i` reaches `5`, the condition turns `false` and the loop ends. After the loop, `i` holds `5`.

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

After the loop, `sum` holds `55`, which is the sum of the integers 1 through 10.

### Condition false from the start

```jade
let never = 99
while never < 0 {
    never = never + 1
}
```

The condition is `false` on the first check, so the body never runs. `never` stays `99`.

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

The inner loop runs all the way through on every pass of the outer loop. After both finish, `total` holds `9`.

:::warning
*A `let` inside a loop body does not shadow. It overwrites.* If you `let` a name that already exists outside the loop, you are reassigning the outer variable, and the last value written survives after the loop ends.

```jade
let x = "outer"
let i = 0
while i < 3 {
    let x = i     // this is the same x
    i = i + 1
}
print(x)          // 2, not "outer"
```

The same is true inside a `for` body. Pick a different name for a value that belongs to the loop.
:::

## for Loops

The `for` statement walks a sequence and binds each element to a loop variable in turn. It is the normal way to visit every element without tracking an index yourself.

```jade
for <var> in <sequence> {
    <body statements>
}
```

- `<var>` is a name bound to each element in turn. It is visible throughout the loop body.
- `<sequence>` is an expression producing one of four types. Jade evaluates it once, before the loop starts.

| Sequence type | The loop variable holds |
|---------------|-------------------------|
| `array` | each element |
| `str` | each character, as a `char` |
| `bytes` | each byte, as an `int` |
| stream (from a `yield` function) | each yielded value |

Anything else is a compile error, such as `cannot iterate over dict`.

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

A function containing `yield` returns a stream, and `for` reads it like any other sequence. See [Functions](functions) for how streams are made.

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
A `dict` cannot be iterated. Writing `for k in some_dict { … }` is a compile error. Loop over `some_dict.keys()` instead.
:::

## break and continue

`break` leaves the innermost loop around it. `continue` skips the rest of the body and starts that loop's next pass. Both work in `while` and in `for`.

```jade
for i in [1, 2, 3, 4, 5] {
    if i == 4 { break }
    if i == 2 { continue }
    print(i)
}
// 1
// 3
```

Both act on the *innermost* loop only. To leave two loops at once, `return` out of the enclosing function.

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

Reach for `while true` with a `break` when you cannot know the exit condition until part-way through the body, rather than at the top.

```jade
let n = 0
while true {
    n = n + 1
    if n * n > 50 { break }
}
print(n)   // 8
```

### Leaving through a catch

A loop can also end because something raised. That is the natural shape for a C library which signals end-of-input with an error code, since [`fails_when`](packages#the-binding-vocabulary) turns the code into an exception.

```jade
while true {
    try {
        print(archive.next_entry(a))
    } catch e {
        break
    }
}
```

`break` and `continue` remove any exception handler they jump out of. So a `try` later in the same function still catches its own exceptions. That holds whether you leave from a `catch` arm, as above, or from the `try` body itself.

### Where they are not allowed

`break` and `continue` need a loop in the *same* function. A loop outside the enclosing `fn` or closure does not count. Leaving it would mean crossing a call frame, and `return` is the statement for that.

```jade
for i in [1, 2] {
    fn f() {
        break     // error: 'break' outside a loop
    }
}
```

Using either one at the top level, or inside an `if` with no loop around it, gives the same error. Jade reports it when it parses the file, not when the line runs.
