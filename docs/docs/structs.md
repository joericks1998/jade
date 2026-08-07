---
id: structs
title: Structs
sidebar_label: Structs
---

Structs are named record types that group related values under named fields. Methods can be attached to any struct type using `extend` blocks.

## Overview

A `struct` definition introduces a new named type with an ordered list of field names. Once defined, a `struct` type can be instantiated by providing values for every field in a struct literal. The resulting value is a struct instance that holds those field values and can be stored in a variable.

Individual fields are read with dot syntax (`obj.field`) and updated with field assignment syntax (`obj.field = expr`). Field assignment mutates the existing instance in place — all variables that hold a reference to the same instance see the updated value immediately.

Methods are defined separately from the struct using an `extend` block. Each method receives the instance it was called on as its first parameter, conventionally named `self`. Calling a method through dot syntax (`obj.method(args)`) automatically supplies the instance as `self`; the caller does not pass it explicitly.

## Syntax

### Struct Definition

```jade
struct <TypeName> {
    <field>,
    <field>,
    ...
}
```

- `<TypeName>` — an identifier naming the type; registered in the global struct registry.
- `<field>` — zero or more field names separated by commas. Fields have no type annotation — they hold any value at runtime. A struct with no fields (`struct Unit {}`) is legal and useful as a marker or an exception type.

A field may instead be written as `let <field> = <expr>`, which gives it a default and makes it optional at construction. Defaulted fields do not need a trailing comma.

### Struct Instantiation

```jade
<TypeName> { <field>: <expr>, <field>: <expr>, ... }
```

- Every field *without a default* must be present in the literal. Omitting one raises `MissingField`.
- Fields with a default may be omitted, or given a value to override the default.
- Extra fields not declared in the definition raise an `UndefinedField` error.

### Field Access

```jade
<expr>.<field>
```

Evaluates `<expr>` to a struct instance, then returns the value of the named field. If the named field does not exist, raises `UndefinedField`. If `<expr>` does not evaluate to a struct, raises `NotAStruct`.

### Field Assignment

```jade
<variable>.<field> = <expr>
```

Updates the named field on the struct instance held by `<variable>`. The field must already exist on the instance.

### Extend Block

```jade
extend <TypeName> {
    fn <method>(self, <param>, ...) {
        <body>
        return <expr>
    }
    ...
}
```

Each method is a `fn` definition where the first parameter receives the receiver instance. Conventionally named `self`.

### Method Call

```jade
<expr>.<method>(<arg>, ...)
```

Field access on a struct first checks instance fields, then checks the method table for the struct type. When a method is found, a bound method value is returned. Calling it automatically passes the receiver as the first argument (`self`). The caller supplies only the arguments after `self`.

## Basic Examples

### Defining a struct and accessing its fields

```jade
struct Point {
    x,
    y
}

let p = Point { x: 10, y: 20 }
let px = p.x
let py = p.y
```

`p.x` evaluates to `10` and `p.y` evaluates to `20`.

### Mutating a field with field assignment

```jade
struct Point {
    x,
    y
}

let p = Point { x: 10, y: 20 }
p.x = 99
let updated_x = p.x
```

`p.x = 99` overwrites the `x` field on the existing instance. After the assignment, `p.x` evaluates to `99`. The instance is mutated in place.

### Fields with defaults

```jade
struct Config {
    let host = "localhost"
    let port = 8080
}

let c = Config {}
print(c.host)    // localhost
print(c.port)    // 8080

let c2 = Config { host: "example.com" }
print(c2.host)   // example.com
print(c2.port)   // 8080 — default still used
```

Defaulted and required fields can be mixed in one struct:

```jade
struct Mixed {
    x,
    y,
    let label = "origin"
}

let m = Mixed { x: 1, y: 2 }
print(m.label)   // origin
```

### Empty structs

A struct with no fields works as a marker type or a bare exception type.

```jade
struct Done {}

let d = Done {}

extend Done {
    fn tag(self) {
        return "done"
    }
}

print(d.tag())   // done
```

### Attaching a method with extend and calling it

```jade
struct Counter {
    count
}

extend Counter {
    fn increment(self) {
        self.count = self.count + 1
    }
    fn value(self) {
        return self.count
    }
}

let c = Counter { count: 0 }
c.increment()
c.increment()
let v = c.value()
```

After two calls to `c.increment()`, `c.value()` returns `2`. Mutations through `self` inside a method are visible on the original instance because `self` and the caller's variable share the same underlying struct object.

## Advanced Examples

### Method that uses a parameter alongside self

```jade
struct Accumulator {
    total
}

extend Accumulator {
    fn add(self, n) {
        self.total = self.total + n
    }
    fn result(self) {
        return self.total
    }
}

let acc = Accumulator { total: 0 }
acc.add(10)
acc.add(5)
acc.add(3)
let sum = acc.result()
```

`add` takes `self` and an extra parameter `n`. When called as `acc.add(10)`, the evaluator binds `self` to the receiver and `n` to `10`. After three calls, `acc.result()` returns `18`.

### Methods as values

A method can be used as a value, not just called. Reading `obj.method` without parentheses binds the receiver, so calling it later still knows its `self`.

```jade
struct Counter {
    count
}

extend Counter {
    fn bump(self, by) {
        self.count = self.count + by
        return self.count
    }
}

let c = Counter { count: 10 }

let bump = c.bump
print(bump(5))    // 15
print(bump(1))    // 16
print(c.count)    // 16 — the binding kept the receiver
```

Data fields win over methods of the same name, so a field named `bump` would shadow the method.

## Interfaces

An `interface` names a set of methods a type must provide. An `extend` block can declare that it satisfies one, and the compiler checks it.

```jade
interface Displayable {
    fn to_str(self)
}

struct Point {
    x,
    y
}

extend Point: Displayable {
    fn to_str(self) {
        return f"({self.x}, {self.y})"
    }
}

let p = Point { x: 3, y: 4 }
print(p.to_str())   // (3, 4)
```

An interface body lists method signatures only — `fn <name>(self, <params>)` with no body. Naming an interface after the colon is what turns the check on; a plain `extend Point { … }` is never checked against anything.

If a required method is missing, the program does not compile:

```
type 'Bad' does not implement interface 'Displayable': missing method 'to_str'
```

## Decorators on a struct

A `struct` can carry a decorator. It runs on **every instance** the program builds, not once on the type, and its extra arguments must be literals.

```jade
fn seen(p) {
    print("built one")
    return p
}

@seen
struct Point { x, y }

let p = Point { x: 1, y: 2 }   // prints: built one
```

See [Functions](functions#decorators) for decorator syntax, stacking, and application order.

## Error Conditions

| Error | Trigger | Example |
|-------|---------|---------|
| `UndefinedType` | Struct literal uses a type name that has not been defined | `let p = Foo { x: 1 }` when no `struct Foo` exists |
| `MissingField` | Struct literal omits a field that has no default | `struct Point { x, y }` then `let p = Point { x: 1 }` |
| `UndefinedField` | Struct literal includes an undeclared field, or dot access targets a field the value does not have | `struct Point { x, y }` then `Point { x: 1, y: 2, z: 3 }` |
| `NotAStruct` | Field *assignment* on a non-struct value | `let x = 5` then `x.foo = 1` |
| `ArityMismatch` | Method called with the wrong number of arguments | `extend Counter { fn add(self, n) { … } }` then `c.add(1, 2)` |

:::note
Reading a field off a non-struct is `UndefinedField`, not `NotAStruct` — `let x = 5` then `x.foo` reports `struct 'int' has no field 'foo'`. `NotAStruct` is reserved for *writing* a field on something that is not a struct.
:::

:::note
The arity numbers in a method-call error **include** `self`, even though you do not pass it. Calling `c.add(1, 2)` on `fn add(self, n)` reports `expected 2, got 3`.
:::

## Implementation Notes

:::note
Struct instances are shared by reference. Assigning a struct instance to a new variable does not copy it — both variables reference the same object. A field mutation through one variable is immediately visible through the other.
:::

:::note
Struct literals are disallowed in `if` and `while` conditions. The parser sets `struct_literal_allowed = false` while parsing a condition so that `while running { … }` does not try to interpret `running {…}` as a struct literal.
:::
