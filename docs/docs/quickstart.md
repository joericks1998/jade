---
id: quickstart
title: Quick Start
sidebar_label: Quick Start
---

Create a file named `hello.jde`:

```jade
let x = 10
let y = 32
let answer = x + y
print(answer)
```

Run it:

```bash
jade run hello.jde
```

Output:

```
42
```

`jade run` also accepts the name of a script defined in `jade.toml`. With no argument at all, it runs the current project's entry file. See the [CLI Reference](cli#jade-run).

## Seeing what a program left behind

Pass `--verbose` to print every global variable after the program finishes:

```bash
jade run hello.jde --verbose
```

Output:

```
42
Grammar = {"new": <builtin new>}
answer = 42
x = 10
y = 32
```

The program's own output comes first. The globals follow, in alphabetical order. Built-in globals such as `Grammar` appear alongside the ones you defined.

## A slightly bigger program

Here is `greet.jde`, which uses functions, f-strings, arrays, and a `for` loop:

```jade
fn greet(name) {
    return f"Hello, {name}!"
}

let people = ["Ada", "Grace", "Alan"]

for p in people {
    print(greet(p))
}
```

```bash
jade run greet.jde
```

```
Hello, Ada!
Hello, Grace!
Hello, Alan!
```

To type-check without running:

```bash
jade check greet.jde
```

```
greet.jde: ok
```

## Compiling to a binary

The same file compiles to a native executable. The result runs on its own, with no copy of `jade` installed:

```bash
jade build greet.jde -o greet
./greet
```

```
built: greet
Hello, Ada!
Hello, Grace!
Hello, Alan!
```

`jade run` and `jade build` are two engines for one language. A program means the same thing under either one.

## Starting a project

Once a program outgrows a single file, `jade new` sets up a project for you:

```bash
jade new myapp
```

```
created: myapp/
  jade.toml
  main.jde
  .gitignore

Get started:
  cd myapp
  jade run
```

```bash
cd myapp
jade run
```

```
Hello from myapp!
```

`jade run` with no argument runs the entry file named in `jade.toml`. From here on, it is the only run command you need.

## Calling a model

This step is the reason Jade exists. A `prompt` is a value. The `?` operator sends it to your provider. The `|> int` stage makes the result an `int`, or raises if it cannot:

```jade
prompt p = "How many moons does Mars have? Reply with just the number."

let n = ?p |> int
print(n + 1)
```

This program needs an inference provider registered first, so run `jade register` before you try it. See [LLM Integration](llm) for the whole picture.
