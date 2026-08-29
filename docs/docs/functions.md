---
id: functions
title: Functions
sidebar_label: Functions
---

You define a Jade function with `fn`. It takes zero or more parameters and returns a value in one of two ways: an explicit `return` statement, or an implicit return of the last bare expression in the body. Functions are first-class values, so you can pass one to another function or assign it to a variable.

## Overview

A function is a named, reusable block of statements. Write the `fn` keyword, then a name, then a parameter list in parentheses, then a body in braces. Calling a function runs its body in a new scope and produces a value.

There are two ways to return a value from a function:

- *Explicit return.* `return <expr>` exits at once and produces that value.
- *Implicit return.* If the last statement in the body is a bare expression, with no `return` keyword, Jade returns that expression's value for you.

If the body ends with neither a `return` statement nor a final bare expression, the function returns `nil`. A bare `return` with no expression also produces `nil`.

Functions are first-class values in Jade. A function definition binds the name to a `fn` value in the current scope, the same way `let` binds a name to an integer or a float. You can store that value in a variable, pass it as an argument, and call it through any expression that produces a function.

## Syntax

### Function Definition

```jade
fn <name>(<param>, <param>, ...) {
    <body statements>
    return <expr>
}
```

- `<name>` is an identifier naming the function. It binds the name in the enclosing scope.
- `<param>` is a parameter name, and you can list several separated by commas. Each becomes a local variable inside the body. A parameter may carry a default, written `<param> = <expr>`, which makes it optional at the call site.
- `<body statements>` is any sequence of statements. If the last one is a bare expression, its value is returned implicitly.
- `return <expr>` exits at once and produces the given value. A bare `return` produces `nil`.

### Function Call

```jade
<expr>(<arg>, <arg>, ...)
```

- `<expr>` is any expression that produces a function value.
- `<arg>` is an argument expression. Jade evaluates the arguments left to right, in the caller's scope.

## Basic Examples

### A function with two parameters

```jade
fn add(a, b) {
    return a + b
}

let sum = add(3, 4)
```

### Implicit return of the last expression

```jade
fn double(x) {
    x * 2
}

print(double(5))  // 10
```

The last statement is the bare expression `x * 2`. It is not a `let`, an `if`, or any other statement, so Jade returns its value. Writing `return x * 2` would mean the same thing.

### A function with no parameters

```jade
fn get_answer() {
    return 42
}

let answer = get_answer()
```

Write the empty parameter list `()` even when the function takes nothing.

### Default parameter values

A caller can leave out a parameter that has a default. Jade uses the default expression in its place.

```jade
fn greet(name, greeting = "Hello") {
    return greeting + ", " + name
}

print(greet("Joe"))         // Hello, Joe
print(greet("Joe", "Hi"))   // Hi, Joe
```

Parameters without a default stay required. Leaving one out is still an `ArityMismatch`.

### Chaining calls

```jade
fn square(x) {
    return x * x
}

let chained = add(square(2), square(3))
```

Calls nest freely. `square(2)` gives `4`, `square(3)` gives `9`, and `add(4, 9)` returns `13`.

## Advanced Examples

### Recursion, with factorial

```jade
fn factorial(n) {
    if n == 0 {
        return 1
    }
    return n * factorial(n - 1)
}

let f5 = factorial(5)
```

A function can call itself. `factorial(5)` computes `5 * 4 * 3 * 2 * 1`, which is `120`. Mutual recursion works too, because Jade binds both definitions before either one is called.

### First-class and higher-order functions

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

Assigning `double` to `f` lets you call `f(5)`. `apply` takes a function as its first argument and calls it with the second. `compose(double, double, 3)` returns `12`.

### Fibonacci, with two recursive calls

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

*When* Jade finds a failure matters, because only the runtime ones can be handled with `try` and `catch`. A compile-time error means `jade check` rejects the file, and nothing runs at all.

### Found at compile time

`jade check` reports these. They cannot be caught.

| Error | Trigger | Example |
|-------|---------|---------|
| `NotCallable` | Calling a non-function value | `let x = 5` then `let y = x(1)` |
| `UndefinedVariable` | Referencing a name not in scope | `fn f() { return z }` where `z` is not defined |
| `NestedFunction` | Defining a `fn` or `async fn` inside another function, or inside a closure body | `fn outer() { fn inner() { return 1 } return 2 }` |
| `ReturnOutsideFunction` | Using `return` at the top level | `return 1` |
| `YieldOutsideFunction` | Using `yield` at the top level | `yield 1` |
| `YieldAndReturn` | A function that yields also returns a value | `fn g() { yield 1  return 2 }` |

### Found at runtime

These happen while the program runs, so a `try` and `catch` can handle them.

| Error | Trigger | Example |
|-------|---------|---------|
| `ArityMismatch` | Wrong number of arguments | `fn add(a, b) { return a + b }` then `add(1)` |
| `UndefinedVariable` | A closure reads a name that was not a top-level variable | A closure written inside a function, reading that function's parameter. See the closure warning below |

:::note
Neither `fn` nor `async fn` may nest. Declare both at the top level. Until v1.3.3 the rule covered only `fn`, so a nested `async fn` parsed and ran. It then failed at run time when it tried to read the enclosing function's parameters, which it cannot see.
:::

## Built-in Functions

Jade has a small set of global built-in functions that are always in scope.

| Function | Signature | Description |
|----------|-----------|-------------|
| `print` | `print(value)` | Writes `value` to stdout followed by a newline. Accepts any type. |
| `write` | `write(str)` | Writes a string to stdout with *no* trailing newline, and flushes immediately. |
| `len` | `len(value)` | Returns the length of a `str` (in characters), `array`, `dict`, `bytes`, stream, or `char` (always 1). |
| `input` | `input()` or `input(prompt)` | Reads one line from stdin and returns it as a `str`. With a `prompt`, it first prints that to stdout with no newline. Returns `""` at end of input. |
| `join` | `join(future, future, ...)` | Waits for several async calls at once and returns their results as an array, in argument order. See [Async / Await](async). |

```jade
// print compared to write
print("hello")       // hello\n
write("hello")       // hello  (no newline, flushed immediately)

// len on different types
let s = "jade"
print(len(s))        // 4

let arr = [1, 2, 3]
print(len(arr))      // 3

// input reads from stdin
let name = input("Enter your name: ")
print("Hello, " + name)

// input with no prompt
let line = input()
```

## Closures

A closure is an anonymous function written inline as `|params| body`. It captures a snapshot of the variables visible where you wrote it, so it can still read them when it is called later from somewhere else.

### Syntax

```jade
|<param>, <param>, ...| <expr>      // single-expression body
|<param>, <param>, ...| { <stmts> } // block body
|| <expr>                            // zero-parameter closure
```

### Basic examples

```jade
// Single-expression body, which returns the expression
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

A closure captures a snapshot of the *top-level* variables that exist when you create it. It carries its own copy, so it can still read them after the surrounding code has moved on.

```jade
let multiplier = 3
let triple = |x| x * multiplier
print(triple(5))   // 15
```

:::warning
*A closure captures top-level variables only.* It cannot see the locals or the parameters of a function it was written inside. Reading one is an error at the moment the closure runs:

```jade
fn make_adder(n) {
    return |x| x + n     // 'n' is a parameter of make_adder
}
let add5 = make_adder(5)
print(add5(10))          // runtime error: undefined variable 'n'
```

So the closure-factory pattern does not work in Jade today. Pass the value in as an argument instead:

```jade
fn add(n, x) {
    return n + x
}
print(add(5, 10))   // 15
```
:::

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

Closures are first-class values, so you can pass one to a higher-order function:

```jade
fn apply(f, x) {
    return f(x)
}

let result = apply(|x| x * x, 6)   // 36
```

:::note
Jade reads `|` as the start of a closure only when it appears at the start of an expression. Everywhere else it stays the bitwise OR operator. The no-parameter form uses `||`, which never collides with logical OR, because logical OR only appears between two operands and never at the start of an expression.
:::

## Streams: functions that yield

A function whose body contains `yield` returns a *stream* instead of a single value. Every `yield` adds to the stream, the body runs all the way through, and the caller receives the finished result.

```jade
fn doubles(n) {
    let i = 0
    while i < n {
        yield i * 2
        i = i + 1
    }
}

let s = doubles(4)
print(s)   // [0, 2, 4, 6]
```

### A stream is a buffer

That one fact answers most questions about streams. A stream holds all its values at once, so:

- `len(s)` gives the count.
- `s[0]` indexes it.
- `for x in s { … }` walks it, and walking it twice gives the same values twice. There is no "already consumed" error to learn.
- `print(s)` shows the contents.

```jade
let s = doubles(4)
print(len(s))   // 4
print(s[0])     // 0
print(s[3])     // 6

for x in s {
    print(x)
}
for x in s {
    print(x)    // same four values again
}
```

### Stopping early

A bare `return` stops a generator early. It carries no value.

```jade
fn upto(limit) {
    let i = 0
    while true {
        if i >= limit {
            return
        }
        yield i
        i = i + 1
    }
}

print(upto(3))   // [0, 1, 2]
```

:::warning
*A function that yields cannot also return a value.* Writing `return x` inside a generator asks it to be a stream producer and a plain function at the same time, which is a compile error:

```
a function that yields cannot also return a value — it produces a stream,
not a single value (a bare 'return' to stop early is fine)
```
:::

### Mixed types

Yields of different types widen into a mixed stream rather than failing, the same rule a mixed array literal follows.

```jade
fn mixed() {
    yield 1
    yield "two"
}

print(mixed())   // [1, two]
```

:::note
`yield` needs a function to belong to. Jade rejects it at the top level for the same reason it rejects a top-level `return`: there is no stream for the value to join.
:::

## Decorators

A decorator is a function applied to a declaration. Write it on the line above, with a leading `@`. Decorators work on `fn`, `async fn`, `let`, and `prompt`.

On a `let`, `@f let x = v` means exactly `let x = f(v)`. The point is not to save characters. The wrapping sits above the declaration instead of around it, which keeps the value itself easy to read.

```jade
fn shout(s) {
    return s.upper()
}

@shout
let greeting = "hello"

print(greeting)   // HELLO
```

### Decorators with arguments

A decorator may take its own arguments. The decorated value goes in first, and the arguments you wrote follow it.

```jade
fn fence(s, tag) {
    return f"<{tag}>{s}</{tag}>"
}

@fence("note")
let body = "keep it short"

print(body)   // <note>keep it short</note>
```

### Stacking

Decorators stack. *The one written first is applied first*, so you write the outermost wrapper last. That is the reverse of Python's rule.

```jade
fn a(s) { return s + "-a" }
fn b(s) { return s + "-b" }

@a
@b
let x = "v"

print(x)   // v-a-b
```

### On a function

`@f fn g() { … }` defines `g`, then rebinds the name to `f(g)`. The decorator receives the function itself.

```jade
let registry = []

fn register(f) {
    registry.push(f)
    return f
}

@register
fn inc(n) {
    return n + 1
}

print(len(registry))   // 1
print(inc(41))         // 42
```

:::note
A `fn` decorator that *wraps* the function, meaning it returns a closure that calls the original, does not work today. A closure cannot capture the decorator's own parameter. See the closure warning above. Decorators that register, tag, or inspect a function and hand it back unchanged work fine.
:::

### One trap

:::warning
*A decorator on a `struct` or an `extend` block is refused.* Both were removed in v1.4.0, because they ran under `jade run` and were skipped under `jade build`, so the two engines disagreed about what a program did. Call the function where you build the value instead.
:::
