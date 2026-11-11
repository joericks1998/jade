# `design/`: design documents for work in progress

## What this subtree is

Each file here is the design for one large change, written before the code so the decisions in it can be reviewed first. A design states the problem, what it recommends, what a program looks like under it, and what stops working. Once the work lands, the directory READMEs it touched become the reference, and the design stays as the record of why.

## Why it exists

Jade 2.0 changes the compiler's architecture end to end, and several of its choices change what programs the language accepts. Those choices belong to the person who owns the language, and a design document is where they can be made before anything is built on them. The directory READMEs describe code that exists; nothing in them could hold a decision about code that does not exist yet.

## What each file does

- *`v2-static-values.md`* is the design for compiling every value to a plain machine value with a type known at compile time, with no runtime library: how types are inferred per call, how `nil`, dicts, mixed values, JSON and exceptions are typed, how the compiler's pipeline changes, which examples change, and in what order the work lands.
- *`v2-classes.md`* is the design for 2.0.1's simpler language: classes in place of structs and `extend`, with methods attached by `fn A::m()`, private members, constructors, types as decorators, and no closures.

## Who uses it

People and sessions working on the `2.0.0` branch read it before working on the type checker, the code generator or the runtime. Nothing in the build reads it.
