---
id: expressions
title: Expressions
sidebar_label: Expressions
---

An expression produces a value. Expressions nest freely, and within one precedence level they evaluate left to right. See [Types](types) for every kind of value an expression can produce, and [Operators](operators) for the full precedence table.

## Integer Literals

```jade
let a = 0
let b = 1000000
```

An `int` is a signed 63-bit integer, so the digits of a literal cannot exceed `4611686018427387903`. Anything larger is refused when the file is read, with a "numeric literal overflows its type" error. There is no hex, octal, binary, or underscore-separated form.

## Float Literals

```jade
let pi = 3.14
let half = 0.5
```

A float literal needs at least one digit on each side of the decimal point. `3.14` is valid, while `.5` and `3.` are not. There is no exponent form, so write `1000.0` rather than `1e3`.

A float always prints with a decimal point, so `print(6.0)` shows `6.0` and never `6`.

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

Wrap an expression in parentheses to override the default precedence:

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

- `~` is bitwise NOT, for integers only.
- `!` is logical NOT, for booleans only.
- `-` is arithmetic negation, for integers and floats.

```jade
let inv   = ~0
let neg   = -5
let nflag = !true
```

You can also spell `!` as `not`, `&&` as `and`, and `||` as `or`. The word forms mean exactly the same thing and bind the same way.

## String Literals

A string literal can use double quotes or single quotes. The two forms are identical. Triple quotes, written `"""…"""` or `'''…'''`, span multiple lines. The `+` operator joins two strings.

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

A string recognises five escapes: `\\`, `\n`, `\t`, `\r`, and the quote character that opened it. Any other backslash is an error rather than a literal backslash, so there is no `\u` or `\0` form.

Indexing a string with `[i]` gives a [`char`](types#char), which is a single Unicode scalar rather than a one-character string. Indexes start at zero and count characters, so a two-byte character still counts once. An index outside the string is a runtime error, and there is no negative indexing.

```jade
let s = "café"
print(s[0])          // c
print(len(s))        // 4, not 5
print(s[0] == "c")   // true. A char compares equal to the string spelling it.
```

A string also iterates, one `char` per step:

```jade
for c in "café" {
    print(c)
}
```

## F-String Interpolation

An f-string starts with `f` before the opening quote. Jade evaluates any expression inside `{ }`, converts the result to a string, and drops it in place. Both quote styles work.

```jade
let name = "Jade"
let n    = 42
let msg  = f"hello, {name}! answer is {n}"
let msg2 = f'hello, {name}! answer is {n}'
```

Triple-quoted f-strings are written as `f"""…"""` or `f'''…'''` and behave the same way.

To put a literal brace in an f-string, escape it with a backslash. Doubling the brace does not work, because `{{` opens a nested expression rather than an escape.

```jade
print(f"a \{literal\} brace")   // a {literal} brace
```

## Array Literals

An array is a comma-separated list inside square brackets. An array may be empty, and it may hold values of any type.

```jade
let a     = [1, 2, 3]
let empty = []
let mixed = [1, 2.0, true, "hello"]
```

Read an element with `arr[i]`, counting from zero, and write one with `arr[i] = expr`. Arrays have reference semantics, so assigning an array creates a second name for the same storage.

When every element has the same type, the compiler knows that type and can check what you do with `arr[i]`. When the elements differ, the compiler knows nothing more specific, so it checks operations on elements while the program runs instead. A mixed array costs you compile-time errors, not correctness:

```jade
let mixed = [1, "two"]
print(mixed[0] + mixed[1])   // runs, then fails: '+' requires numeric operands
```

`arr.contains(x)` is the one place where a type mismatch is not an error. Membership asks whether any element *is* `x`, and an element of another type simply answers `false`:

```jade
let mixed = [1, "two", true]
print(mixed.contains("two"))   // true
print(mixed.contains(9))       // false, not an error
```

That is deliberately different from `==`, which rejects a comparison across types rather than quietly answering it. Note that both treat `1` and `1.0` as different values.

`in` and `not in` ask the same question as an infix operator. They also work on a string and on a dict's keys:

```jade
print(2 in [1, 2, 3])          // true
print(4 not in [1, 2, 3])      // true
print("ell" in "hello")        // true
print("k" in {"k": 1})         // true
```

## Dict Literals

A dict is written with curly braces and string keys. See [Types](types#dict) for the full reference.

```jade
let d = {"name": "jade", "version": 1}
print(d["name"])
```

## Closures

`|params| body` builds an anonymous function value. A body without braces is an implicit return.

```jade
let double = |x| x * 2
let add    = |a, b| { return a + b }
let seven  = || 7

print(double(4))   // 8
```

See [Functions](functions) for the full reference.

## Pipe Operator

The `|>` operator passes the value on its left as the first argument to the function on its right. Pipes chain from left to right.

```jade
fn double(x) { return x * 2 }
let n = 5 |> double            // double(5) → 10
let m = 3 |> double |> double  // double(double(3)) → 12
```

When the right side is already a call, the left value goes in as the first argument, before the ones you wrote:

```jade
fn add(a, b) { return a + b }
let r = 5 |> add(3)           // add(5, 3) → 8
```

A prompt dereference pipes like any other value. A type name used as a stage limits what the model generates and coerces the reply. A function stage placed after it receives the coerced value. See [Operators](operators#what-a-stage-can-be) for the full rule.

```jade
prompt p = "What is 21 + 21? Respond with only the number."
let n = ?p |> int |> double   // 84
```
