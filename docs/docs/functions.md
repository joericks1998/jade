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
- `<param>` — zero or more parameter names separated by commas; each becomes a local variable inside the body. A parameter may carry a default (`<param> = <expr>`), which makes it optional at the call site.
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

### Default parameter values

A parameter with a default may be left out by the caller. The default expression is used in its place.

```jade
fn greet(name, greeting = "Hello") {
    return greeting + ", " + name
}

print(greet("Joe"))         // Hello, Joe
print(greet("Joe", "Hi"))   // Hi, Joe
```

Parameters without a default stay required — omitting one is still an `ArityMismatch`.

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

*When* a failure is found matters, because only the runtime ones can be handled with `try`/`catch`. A compile-time error means `jade check` rejects the file and nothing runs at all.

### Found at compile time

`jade check` reports these. They cannot be caught.

| Error | Trigger | Example |
|-------|---------|---------|
| `NotCallable` | Calling a non-function value | `let x = 5` then `let y = x(1)` |
| `UndefinedVariable` | Referencing a name not in scope | `fn f() { return z }` where `z` is not defined |
| `NestedFunction` | Defining a `fn` inside another `fn`, or inside a closure body | `fn outer() { fn inner() { return 1 } return 2 }` |
| `ReturnOutsideFunction` | Using `return` at the top level | `return 1` |
| `YieldOutsideFunction` | Using `yield` at the top level | `yield 1` |
| `YieldAndReturn` | A function that yields also returns a value | `fn g() { yield 1  return 2 }` |

### Found at runtime

These happen while the program runs, and a `try`/`catch` can handle them.

| Error | Trigger | Example |
|-------|---------|---------|
| `ArityMismatch` | Wrong number of arguments | `fn add(a, b) { return a + b }` then `add(1)` |
| `UndefinedVariable` | A closure reads a name that was not a top-level variable | A closure written inside a function that reads that function's parameter — see the closure warning below |

:::note
`fn` definitions cannot nest, but `async fn` definitions currently can. A nested `async fn` parses without complaint — see the decorator warning below for one way that bites.
:::

## Built-in Functions

Jade provides a small set of global built-in functions that are always in scope.

| Function | Signature | Description |
|----------|-----------|-------------|
| `print` | `print(value)` | Writes `value` to stdout followed by a newline. Accepts any type. |
| `write` | `write(str)` | Writes a string to stdout **without** a trailing newline and flushes immediately. |
| `len` | `len(value)` | Returns the length of a `str` (in characters), `array`, `dict`, `bytes`, stream, or `char` (always 1). |
| `input` | `input()` / `input(prompt)` | Reads one line from stdin and returns it as a `str`. If `prompt` is given, prints it to stdout (no newline) before reading. Returns `""` on EOF. |
| `join` | `join(future, future, ...)` | Waits for several async calls at once and returns their results as an array, in argument order. See [Async / Await](async). |

```jade
// print vs write
print("hello")       // hello\n
write("hello")       // hello  (no newline, flushed immediately)

// len on different types
let s = "jade"
print(len(s))        // 4

let arr = [1, 2, 3]
print(len(arr))      // 3

// input — read from stdin
let name = input("Enter your name: ")
print("Hello, " + name)

// input with no prompt
let line = input()
```

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

A closure captures a snapshot of the *top-level* variables that exist when it is created. It carries its own copy, so it can still read them after the surrounding code has moved on.

```jade
let multiplier = 3
let triple = |x| x * multiplier
print(triple(5))   // 15
```

:::warning
**A closure only captures top-level variables.** It cannot see the locals or the parameters of a function it was written inside. Reading one is an error at the moment the closure runs:

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

## Streams: functions that yield

A function whose body contains `yield` returns a **stream** instead of a single value. Every `yield` appends to the stream, the body runs to completion, and the caller receives the finished result.

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

That single fact settles most of the questions you might have. A stream holds all its values at once, so:

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
**A function that yields cannot also return a value.** `return x` inside a generator asks it to be a stream producer and a plain function at once, which is a compile error:

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
`yield` needs a function to belong to. At the top level it is rejected for the same reason a top-level `return` is — there is no stream for the value to join.
:::

## Decorators

A decorator is a function applied to a declaration, written on the line above it with a leading `@`. It works on `fn`, `async fn`, `struct`, `extend`, `let`, and `prompt`.

For a `let`, `@f let x = v` is exactly `let x = f(v)`. The point is not that it saves characters — the wrapping sits above the declaration instead of around the thing being declared, so what the value actually *is* stays readable.

```jade
fn shout(s) {
    return s.upper()
}

@shout
let greeting = "hello"

print(greeting)   // HELLO
```

### Decorators with arguments

A decorator may take its own arguments. The decorated value is passed first, and the written arguments follow.

```jade
fn fence(s, tag) {
    return f"<{tag}>{s}</{tag}>"
}

@fence("note")
let body = "keep it short"

print(body)   // <note>keep it short</note>
```

### Stacking

Decorators stack. *The one written first is applied first*, so the outermost wrapper is written last. That is the reverse of Python's rule.

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
A `fn` decorator that *wraps* the function — returning a closure that calls it — does not work today, because a closure cannot capture the decorator's own parameter. See the closure warning above. Decorators that register, tag, or inspect a function and hand it back unchanged work fine.
:::

### On a struct

A `struct` decorator runs on **every instance** the program builds, not once on the type. Its extra arguments must be literals — numbers, strings, booleans, or `nil`.

```jade
fn seen(p) {
    print("built one")
    return p
}

@seen
struct Point { x, y }

let p = Point { x: 1, y: 2 }   // prints: built one
```

### Two traps

:::warning
**A decorator on a nested `async fn` is silently dropped.** Because `async fn` definitions are allowed to nest, you can write a decorated one inside another function body — and it compiles cleanly with the decorator thrown away. No error, no warning:

```jade
fn host() {
    @log
    async fn g() { return 1 }
    return 2
}
// `log` is never called.
```

Declare decorated functions at the top level.
:::

:::warning
**On an `extend` block, only `@route` means anything.** Any other decorator on `extend` compiles and does nothing at all.
:::
