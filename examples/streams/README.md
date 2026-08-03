# `examples/streams/` — `yield` and stream values

## What this subtree is

Fixtures for the stream type introduced in v1.2.3. A function whose body
contains a `yield` returns a *stream* instead of a value: the body runs to
completion filling a buffer, and the caller reads the buffer.

## Why the model matters to the fixtures

A stream **is a buffer**, not a one-shot channel. That single decision is what
`yield_basic.jde` is mostly checking, because it removes a whole category of
rules that would otherwise need defining:

- reading a stream twice gives the same values twice, so the fixture iterates
  the same stream in two consecutive `for` loops and expects identical output;
- there is no "already consumed" error to hit, so none is tested for;
- `len` and indexing work, because a buffer has both.

The earlier design had `?p` produce a one-shot token stream that errored on a
second drain (`DoubleStreamDrain`). Making a stream a buffer is what let that
error disappear rather than be relocated.

## What each file does

- **`yield_basic/`** — the whole surface in one file: a generator, `len`,
  indexing, iterating twice, printing, an early bare `return`, mixed yield types
  widening, and a generator calling another generator.
- **`yield_toplevel_error.jde`** — `yield` outside any function. Rejected for
  the same reason a top-level `return` is: there is no stream for the value to
  join. The `_error` suffix means CI asserts `jade check` *fails* on it.

## Gotchas

**A generator cannot also return a value.** A bare `return` is fine and stops it
early, but `return x` asks the function to be both a stream producer and a
plain function, which is a compile error (`YieldAndReturn`). The fixture covers
the legal half; the illegal half is a Rust test in `compiler/tests.rs`, since a
second `_error` fixture for it would add a file to prove one line.

**A stream renders like an array**, which is deliberate rather than incidental:
in a compiled binary a stream *is* an ordinary array, so `len`, indexing, `for`,
and printing reuse everything arrays already do. The parity gate diffs stdout,
so that rendering is a contract.

## Running them

```sh
./target/debug/jade run examples/streams/yield_basic/yield_basic.jde
./target/debug/jade check examples/streams/yield_toplevel_error.jde   # must fail
./src/scripts/backend-parity.sh
```
