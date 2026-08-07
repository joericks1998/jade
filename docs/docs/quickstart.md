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

`jade run` also takes the name of a script from `jade.toml`, and with no argument at all it runs the current project's entry file. See the [CLI Reference](cli#jade-run).

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

The program's own output comes first, then the globals in alphabetical order. Built-in globals such as `Grammar` are listed alongside your own.

## A slightly bigger program

Functions, f-strings, arrays, and `for` loops, in `greet.jde`:

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

The same file compiles to a native executable that needs no `jade` installed to run:

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

`jade run` and `jade build` are two engines for one language, and a program means the same thing under either.

## Starting a project

Once a program outgrows a single file, `jade new` lays out a project:

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

`jade run` with no argument runs the entry file named in `jade.toml`, so from here on it is the only command you need.

## Calling a model

Jade's reason for existing is the next step. A `prompt` is a value, `?` sends it to your provider, and `|> int` makes the result an `int` or raises:

```jade
prompt p = "How many moons does Mars have? Reply with just the number."

let n = ?p |> int
print(n + 1)
```

This one needs an inference provider registered first — run `jade register`. See [LLM Integration](llm) for the whole picture.
