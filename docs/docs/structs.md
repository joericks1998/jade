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
- `<field>` — one or more field names separated by commas. Fields have no type annotation — they hold any value at runtime.

### Struct Instantiation

```jade
<TypeName> { <field>: <expr>, <field>: <expr>, ... }
```

- Every field declared in the struct definition must be present in the literal.
- Extra fields not declared in the definition raise an `UndefinedField` error.
- Missing fields raise a `MissingField` error.

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

## Error Conditions

| Error | Trigger | Example |
|-------|---------|---------|
| `UndefinedType` | Struct literal uses a type name that has not been defined | `let p = Foo { x: 1 }` when no `struct Foo` exists |
| `MissingField` | Struct literal omits a required field | `struct Point { x, y }` then `let p = Point { x: 1 }` |
| `UndefinedField` | Struct literal includes an undeclared field, or dot access targets an undeclared field | `struct Point { x, y }` then `Point { x: 1, y: 2, z: 3 }` |
| `NotAStruct` | Dot access or field assignment on a non-struct value | `let x = 5` then `let y = x.foo` |
| `ArityMismatch` | Method called with the wrong number of arguments (not counting `self`) | `extend Counter { fn add(self, n) { … } }` then `c.add(1, 2)` |

:::note
The arity check for method calls excludes `self` from the expected count — `self` is supplied automatically by the evaluator. A method defined as `fn add(self, n)` expects exactly one argument from the caller.
:::

## Implementation Notes

:::note
Struct instances are shared by reference. Assigning a struct instance to a new variable does not copy it — both variables reference the same object. A field mutation through one variable is immediately visible through the other.
:::

:::note
Struct literals are disallowed in `if` and `while` conditions. The parser sets `struct_literal_allowed = false` while parsing a condition so that `while running { … }` does not try to interpret `running {…}` as a struct literal.
:::
