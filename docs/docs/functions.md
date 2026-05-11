---
id: functions
title: Functions
sidebar_label: Functions
---

Jade functions are defined with `fn`, accept zero or more parameters, and return a value either with an explicit `return` statement or via implicit return (the last bare expression in the body). Functions are first-class values and can be passed to other functions or assigned to variables.

## Overview

A function is a named, reusable block of statements. It is introduced with the `fn` keyword, followed by a name, a parenthesized parameter list, and a brace-delimited body. Calling a function evaluates its body in a new scope and returns a value.

There are two ways to return a value from a function:

- **Explicit return:** `return <expr>` exits immediately and produces that value.
- **Implicit return:** if the last statement in the body is a bare expression (no `return` keyword), that expression's value is returned automatically.

If execution reaches the end of the body without hitting a `return` statement and without a final bare expression, the function returns `nil`. A bare `return` with no expression also produces `nil`.

Functions are first-class values in Jade. A function definition binds the name to a `fn` value in the current environment, just like `let` binds a name to an integer or float. That value can be stored in a variable, passed as an argument, and called through any expression that evaluates to a function.

## Syntax

### Function Definition

```jade
fn <name>(<param>, <param>, ...) {
    <body statements>
    return <expr>
}
```

- `<name>` — an identifier naming the function; binds it in the enclosing scope.
- `<param>` — zero or more parameter names separated by commas; each becomes a local variable inside the body.
- `<body statements>` — any sequence of statements. If the last statement is a bare expression, its value is returned implicitly.
- `return <expr>` — exits immediately and produces the given value. A bare `return` produces `nil`.

### Function Call

```jade
<expr>(<arg>, <arg>, ...)
```

- `<expr>` — any expression that evaluates to a function value.
- `<arg>` — zero or more argument expressions evaluated left-to-right in the caller's scope.

## Basic Examples

### A function with two parameters

```jade
fn add(a, b) {
    return a + b
}

let sum = add(3, 4)
```

### Implicit return (last expression)

```jade
fn double(x) {
    x * 2
}

print(double(5))  // 10
```

The last statement is the bare expression `x * 2`. Because it is not a `let`, `if`, or other statement, its value is automatically returned. This is equivalent to writing `return x * 2`.

### A function with no parameters

```jade
fn get_answer() {
    return 42
}

let answer = get_answer()
```

The empty parameter list `()` is required even when there are no parameters.

### Chaining calls

```jade
fn square(x) {
    return x * x
}

let chained = add(square(2), square(3))
```

Call expressions can be nested. `square(2)` evaluates to `4`, `square(3)` evaluates to `9`, and `add(4, 9)` returns `13`.

## Advanced Examples

### Recursion — factorial

```jade
fn factorial(n) {
    if n == 0 {
        return 1
    }
    return n * factorial(n - 1)
}

let f5 = factorial(5)
```

Functions can call themselves. `factorial(5)` computes `5 * 4 * 3 * 2 * 1 = 120`. Mutual recursion also works because function definitions are bound before either is called.

### First-class functions — higher-order functions

```jade
fn double(x) {
    return x * 2
}

fn apply(f, x) {
    return f(x)
}

fn compose(f, g, x) {
    return f(g(x))
}

let f = double
let a = f(5)
let b = apply(double, 6)
let d = compose(double, double, 3)
```

`double` is assigned to `f`, which can then be called as `f(5)`. `apply` receives a function as its first argument and calls it with the second. `compose(double, double, 3)` returns `12`.

### Fibonacci — two recursive calls

```jade
fn fib(n) {
    if n <= 1 {
        return n
    }
    return fib(n - 1) + fib(n - 2)
}

let fib10 = fib(10)
```

`fib(10)` returns `55`.

## Error Conditions

| Error | Trigger | Example |
|-------|---------|---------|
| `NotCallable` | Calling a non-function value | `let x = 5` then `let y = x(1)` |
| `ArityMismatch` | Wrong number of arguments | `fn add(a, b) { return a + b }` then `add(1)` |
| `NestedFunction` | Defining a function inside another function body | `fn outer() { fn inner() { return 1 } return 2 }` |
| `ReturnOutsideFunction` | Using `return` at the top level | `return 1` |
| `UndefinedVariable` | Referencing a name not in scope | `fn f() { return z }` where `z` is not defined |

## Closures

A closure is an anonymous function written inline using the `|params| body` syntax. It captures a snapshot of the variables visible at the point it is created, so it can reference those variables even when called later from a different scope.

### Syntax

```jade
|<param>, <param>, ...| <expr>      // single-expression body
|<param>, <param>, ...| { <stmts> } // block body
|| <expr>                            // zero-parameter closure
```

### Basic examples

```jade
// Single-expression body — implicitly returns the expression
let double = |x| x * 2
print(double(5))   // 10

// Multiple parameters
let add = |x, y| x + y
print(add(3, 4))   // 7

// Zero parameters
let greet = || "hello"
print(greet())   // hello
```

### Capturing outer variables

All variables visible at closure creation are captured by copy. The closure carries its own snapshot — mutations inside the closure do not affect the outer scope.

```jade
let multiplier = 3
let triple = |x| x * multiplier
print(triple(5))   // 15
```

### Block body

```jade
let abs_val = |x| {
    if x < 0 {
        return -x
    }
    return x
}
print(abs_val(-7))   // 7
print(abs_val(4))    // 4
```

### Closures as arguments

Closures are first-class values and can be passed to higher-order functions:

```jade
fn apply(f, x) {
    return f(x)
}

let result = apply(|x| x * x, 6)   // 36
```

:::note
The `|` symbol that opens a closure is only treated as a closure delimiter when it appears at the start of an expression (primary position). In all other positions it remains the bitwise OR operator. The empty-param form uses `||`, which is distinct from the logical OR operator because `||` only appears as logical OR in an infix position, never at the start of an expression.
:::
