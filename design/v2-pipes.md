# Pipes call once per piece

Status: decided and being built, 2026-11-14, for jade 2.0.4. Joe made the decisions
marked as his; the others are defaults chosen while building, listed at the end so
they can be overruled.

## The problem

Jade has three ways to call a function: `foo(x)`, `@foo let y = x`, and `x |> foo`.
Until now the third was spelling: `x |> foo` was `foo(x)`, with one exception,
`?p |> toInt`, which asked the model for an int. Three spellings of one call is two
too many, and none of them could act on data while it was still arriving.

## The rule

Joe's rule: a pipe listens to a value as it arrives.

- `foo(x)` and `@foo` call `foo` once with the whole value. When `x` is still being
  made, by a generator or a model, they wait for all of it.
- `x |> foo` calls `foo` once per piece of `x`, as each piece arrives.

```
fn numbers() {
    print("making 1")
    yield 1
    print("making 2")
    yield 2
}

numbers() |> print     // making 1, 1, making 2, 2
print(numbers())       // making 1, making 2, [1, 2]
?p |> write            // each piece of the reply as the model writes it
print(?p)              // the whole reply, once
5 |> double            // one piece, so double(5)
```

## What a piece is

- *A call to a generator* gives each value it yields, at the moment it yields it.
  The generator does not run ahead: a piece goes through every stage before the
  generator makes the next one.
- *A stream already held,* such as `let s = numbers()` piped later, gives its values
  in order. They have all arrived, so they go through one after another.
- *A prompt dereference,* `?p`, gives the reply's text as the model writes it, one
  token's text at a time. A character split across two tokens arrives whole, in the
  piece that finishes it.
- *Anything else is one piece,* arrays included: `[1, 2, 3] |> f` is `f([1, 2, 3])`.
  `map` already calls a function per element. (Joe agreed.)

## Stages

`x |> f |> g` has the source `x` and the stages `f` and `g`. A chain is one pipe: each
piece goes through `f` and then `g` before the next piece is made, so
`numbers() |> double |> print` prints each doubled number as it is yielded.

A stage is a function, or a call that names the other arguments: `x |> add(3)` calls
`add(piece, 3)`. A stage's other arguments are worked out once, before the first
piece, not once per piece.

## What a pipe gives back

A pipe keeps its source's shape. (Joe agreed.)

- Over one piece, it gives the last stage's result: `let n = 5 |> double` is 10.
- Over a stream, it gives a stream of the last stage's results:
  `let words = ?p |> clean` is a stream of strings. Written as a statement, the pipe
  runs to the end and its stream is dropped.

## A raise in a stage

A raise in a stage leaves the pipe, and the generator stops where it was. The
generator's own `try` does not catch it: a stage is the caller's code, and the
generator's `try` guards the generator's work. The raise is caught where the pipe
is written, as if the stage had raised there.

## Prompts

`?p |> f` is the streaming form. A typed dereference wraps the whole reply instead:

| Before | Now |
|--------|-----|
| `?p \|> toInt` | `toInt(?p)` |
| `?p \|> toFloat`, `toBool`, `toChar`, `toString` | `toFloat(?p)` and the rest |
| `?p \|> City` | `City(?p)` |
| `?p \|> g`, a grammar | `g(?p)` |

As before, a typed dereference asks the model for that type. A grammar constrains
sampling. Joe agreed to the move: types matter more now that Jade only compiles, so
asking for an int happens around the whole reply, never per piece.

The old spellings are build errors that name the new ones, rather than calls on each
piece. Calling `toInt` on a single token's text would almost never be what the
program meant:

```
'?p |> toInt' would call toInt on each piece of the reply as the model writes it; to
convert the whole reply, write toInt(?p)
```

`print(?p)` waits for the whole reply and prints it once. In the compiled backend it
already did; v1 streamed it as a special case, and there is no special case now.

`write` prints without a newline, so a reply piped into it reads as one line of
text growing; `?p |> print` would put each piece on a line of its own.

## What stays the same

- `5 |> double`, `x |> add(3)` and `"Jade" |> greet`: one piece, one call.
- `for x in numbers()` still waits for the whole stream. Only a pipe listens.
- A generator called any other way runs to the end into a buffer, as
  `design/v2-static-values.md` describes.

## How it compiles

- *Per-file inference* turns a chain into one node: the source, the stage arguments
  worked out once, the name of the current piece, and the stages applied to that
  name, `g(f(piece), a)`.
- *The checker* types the piece as the stream's element, or as the source itself,
  and the pipe as a stream of the last stage's type or as that type.
- *MIR* lowers a pipe over one piece as the calls in order, and a pipe over a held
  stream as a loop. A pipe over a call to a generator lowers the stages into a
  function of their own, plus a copy of the generator whose `yield` calls that
  function. The copy takes the stages' arguments as extra parameters, so nothing
  needs closures, and the generator runs no further than the piece in hand.
- *A prompt* source becomes a call to `reply_pieces`, a generator in the inference
  prelude that yields each piece. It asks the backend for one token at a time, so
  the backend's generation loop becomes `__infer.next`, one token per call.
  Between calls another task may run a prompt; the model's memory then holds the
  other conversation, and `next` reads this one's tokens back in before going on.

## Defaults chosen while building

1. A stage's other arguments are worked out once, before the first piece.
2. A raise in a stage is not caught by the generator's own `try`.
3. The old typed spellings, `?p |> toInt` and friends, are build errors naming the
   fix, rather than legal calls on each piece.
4. A grammar applies as `g(?p)`, to match `toInt(?p)` and `City(?p)`. Grammars are
   not compiled in v2 yet, so this only decides the spelling.
5. A pipe over a stream whose last stage gives nothing, such as `print`, still gives
   a stream, of nils. Written as a statement, which is the usual case, nothing sees
   it.

## Out of scope

- *Native sources.* A Rust function that returns an iterator is where pipes meet
  native code. That lands with the Rust packages of 2.0.4,
  `design/v2-rust-packages.md`. `uhttp.stream` keeps its handler argument until then.
- *`for` over a live source.* A loop could also run its body per piece as it
  arrives; nobody has asked for it.
