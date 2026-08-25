---
id: variables
title: Variables
sidebar_label: Variables
---

Variables are declared with the `let` keyword. Every variable must be initialized at declaration.

```jade
let x = 42
let y = x + 1
let z = x * y - 10
```

Variable names must start with a letter or underscore and may contain letters, digits, and underscores.

## Rules

- A variable can hold any Jade value. See [Types](types) for the full list.
- A variable may be referenced in any expression written after it.
- Referencing an undeclared name is a compile-time error. `jade check` catches it before the program runs, so no part of the program executes.
- Variables declared inside a function body are local to that call and are not visible outside.

## Blocks

A name first introduced inside an `if`, `while`, or `for` block cannot be used after the block ends:

```jade
if true {
    let inner = 9
}
print(inner)     // error: undefined variable 'inner'
```

Reusing an outer name is a different case. A `let` inside the block overwrites the outer variable rather than shadowing it, so the new value survives after the block ends.

```jade
let x = 1
if true {
    let x = 99
}
print(x)         // 99, not 1
```

Pick a fresh name inside a block when you want the outer one left alone.

## Statements end at the line break

There is no statement separator to write. A statement ends where its line ends, and Jade fills in the break for you.

Jade rejects a semicolon you type yourself. Writing `let x = 1;` is a lexer error, not a harmless extra. Leave the line ending bare:

```jade
let x = 1
let y = 2
```

A line break inside `(` … `)` or `[` … `]` does not end a statement, so a long call or array can span several lines.

## Reassigning

Assignment without `let` changes a variable that already exists:

```jade
let count = 1
count = count + 1
print(count)        // 2
```

A second `let` on the same name also works, and simply rebinds it. In both cases the new value does not have to match the old type. Jade infers a variable's type from what it currently holds, rather than fixing the type at declaration:

```jade
let value = "text"
value = 7
print(value)        // 7
```

## Decorators

A `let` may carry a decorator, which wraps the value in a function call:

```jade
fn shout(s) {
    return s.upper()
}

@shout
let greeting = "hello"     // same as: let greeting = shout("hello")

print(greeting)            // HELLO
```

The point is not to save keystrokes. The wrapper sits above the declaration instead of around it, which keeps the value itself easy to read.

A decorator may take its own arguments. The decorated value goes first:

```jade
fn fence(s, tag) {
    return f"<{tag}>{s}</{tag}>"
}

@fence("note")
let body = "keep it short"    // same as: let body = fence("keep it short", "note")
```

Decorators stack, and the one written *first* is applied first:

```jade
@shout
@fence("p")
let loud = "hello"            // fence(shout("hello"), "p")  →  <p>HELLO</p>
```

That is the same order `fn` decorators use, and the reverse of Python's.

A decorator can also be namespaced, using `::` like an import:

```jade
@style::tagged
let body = "keep it short"
```

The same syntax works on a `prompt` declaration, which is where decorators are most useful. See [Prompts and Inference](llm.md#decorating-a-prompt).
