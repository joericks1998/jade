---
id: structs
title: Structs
sidebar_label: Structs
---

A struct is a named record type. It groups related values under named fields. You attach methods to a struct type with an `extend` block.

## Overview

A `struct` definition creates a new named type with an ordered list of field names. Once defined, you build an instance by giving a value for every field in a struct literal. The result is a struct instance holding those values, and you can store it in a variable.

Read a field with dot syntax, written `obj.field`. Write one with field assignment, written `obj.field = expr`. Field assignment changes the existing instance in place, so every variable pointing at that instance sees the new value right away.

You define methods separately from the struct, inside an `extend` block. Each method receives the instance it was called on as its first parameter, named `self` by convention. Calling a method with dot syntax, written `obj.method(args)`, supplies the instance as `self` for you. The caller never passes it.

## Syntax

### Struct Definition

```jade
struct <TypeName> {
    <field>,
    <field>,
    ...
}
```

- `<TypeName>` is an identifier naming the type. Jade registers it in the global struct registry.
- `<field>` is a field name, and you can list several separated by commas. Fields carry no type annotation, so each one holds any value at run time. A struct with no fields at all, such as `struct Unit {}`, is legal and useful as a marker or an exception type.

You can instead write a field as `let <field> = <expr>`. That gives it a default and makes it optional when you build an instance. A field with a default needs no trailing comma.

### Struct Instantiation

```jade
<TypeName> { <field>: <expr>, <field>: <expr>, ... }
```

- Every field *without a default* must appear in the literal. Leaving one out raises `MissingField`.
- A field with a default can be left out, or given a value to override the default.
- A field that the definition never declared raises `UndefinedField`.

### Field Access

```jade
<expr>.<field>
```

Jade evaluates `<expr>` to a struct instance, then returns the value of the named field. A field that does not exist raises `UndefinedField`. An `<expr>` that is not a struct raises `NotAStruct`.

### Field Assignment

```jade
<variable>.<field> = <expr>
```

This updates the named field on the struct instance held by `<variable>`. The field must already exist on that instance.

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

Each method is a `fn` definition whose first parameter receives the instance it was called on. Name that parameter `self` by convention.

### Method Call

```jade
<expr>.<method>(<arg>, ...)
```

Field access on a struct checks the instance fields first, then the method table for that struct type. When it finds a method, it produces a bound method value. Calling that value passes the instance in as the first argument, `self`. You supply only the arguments that come after `self`.

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

`p.x = 99` overwrites the `x` field on the existing instance, so afterwards `p.x` gives `99`. The change happens in place.

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
print(c2.port)   // 8080, still the default
```

One struct can mix fields with defaults and fields without them:

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

A struct with no fields works as a marker type, or as a bare exception type.

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

After two calls to `c.increment()`, `c.value()` returns `2`. A change made through `self` inside a method shows up on the original instance, because `self` and the caller's variable point at the same struct object.

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

`add` takes `self` and one more parameter, `n`. In the call `acc.add(10)`, Jade binds `self` to the instance and `n` to `10`. After three calls, `acc.result()` returns `18`.

### Methods as values

A method is a value, not just something to call. Reading `obj.method` without parentheses binds the instance to it, so calling it later still knows its `self`.

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
print(c.count)    // 16. The binding kept the instance.
```

A data field beats a method of the same name, so a field called `bump` would hide the method.

## Inheritance

A struct can name parents in parentheses after its own name. It takes their fields, their defaults, and their `extend` methods as if it had written them itself.

```jade
struct Animal {
    name,
    let legs = 4
}

struct Dog(Animal) {
    breed
}

let d = Dog { name: "rex", breed: "corgi" }
print(d.name)    // rex
print(d.legs)    // 4
print(d.breed)   // corgi
```

You may name as many parents as you like, and inheritance is transitive: a grandparent's fields arrive too.

```jade
struct Trainable {
    let tricks = []
}

struct Puppy(Dog, Trainable) {
    let weeks = 0
}
```

### A field name may appear only once

A field name has to be unique across a struct and everything it inherits. Two parents declaring the same field is an error, and so is a child redeclaring a field it already has. Two fields with one name would mean two storage slots and nothing to say which one a literal meant.

```
field 'name' comes from both 'Animal' and 'Dog'; a field name may appear
once across a struct and everything it inherits
```

### A child's method wins

Methods work the other way, because there is something to decide between them. A child's own method overrides the one it would have inherited, which is the main reason to inherit at all. The nearest declaration wins, so a grandchild's method beats its parent's, which beats its grandparent's.

```jade
extend Animal {
    fn speak(self) {
        return "..."
    }
}

extend Dog {
    fn speak(self) {
        return "woof"
    }
}

print(Dog { name: "rex", breed: "corgi" }.speak())   // woof
```

Two *parents* supplying the same method name is an error, though. They are the same distance away, so neither is nearer and the program has to say which it meant.

An `extend` block is collected before anything runs, so where you write it in the file does not matter.

### A parent can live in another file

Write the parent the way you write any imported type, qualified with its module.

```jade
use creatures

struct Dog(creatures.Animal) {
    breed
}
```

Everything works the same: fields, defaults, and methods all come across, and a local `extend Dog` still overrides an imported parent's method.

### Catching by a parent

A typed `catch` arm matches the named type or anything that inherits it, so one arm can handle a family of errors. See [Exceptions](exceptions#matching-by-type).

### What inheritance is not

There is no `super`, so a child's method cannot call the version it replaced. There is no abstract or required member: a parent is an ordinary struct you can build on its own. And a struct cannot inherit from anything but a struct.

## Error Conditions

| Error | Trigger | Example |
|-------|---------|---------|
| `UndefinedType` | Struct literal uses a type name that has not been defined | `let p = Foo { x: 1 }` when no `struct Foo` exists |
| `MissingField` | Struct literal omits a field that has no default | `struct Point { x, y }` then `let p = Point { x: 1 }` |
| `UndefinedField` | A struct literal names a field that was never declared, or dot access names a field the value does not have | `struct Point { x, y }` then `Point { x: 1, y: 2, z: 3 }` |
| `NotAStruct` | Field *assignment* on a non-struct value | `let x = 5` then `x.foo = 1` |
| `ArityMismatch` | Method called with the wrong number of arguments | `extend Counter { fn add(self, n) { … } }` then `c.add(1, 2)` |

:::note
Reading a field off a non-struct gives `UndefinedField`, not `NotAStruct`. Writing `let x = 5` then `x.foo` reports `struct 'int' has no field 'foo'`. `NotAStruct` is reserved for *writing* a field on something that is not a struct.
:::

:::note
The argument counts in a method-call error *include* `self`, even though you never pass it. Calling `c.add(1, 2)` on `fn add(self, n)` reports `expected 2, got 3`.
:::

## Implementation Notes

:::note
Struct instances are shared by reference. Assigning one to a new variable does not copy it, so both variables point at the same object. A field change made through one variable is visible through the other right away.
:::

:::note
Struct literals are not allowed inside an `if` or `while` condition. The parser sets `struct_literal_allowed = false` while reading a condition, so `while running { … }` does not get read as a struct literal named `running`.
:::
