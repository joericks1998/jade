# `examples/streams/`: `yield` and stream values

## What this subtree is

These are the fixtures for the stream type introduced in v1.2.3. A function whose body contains a `yield` returns a *stream* rather than a single value. The body runs all the way through, filling a buffer, and the caller reads that buffer.

## Why the model matters to the fixtures

A stream *is a buffer*, not a one-shot channel. That single decision is most of what `yield_basic.jde` checks, because it removes a whole category of rules that would otherwise need defining:

- Reading a stream twice gives the same values twice, so the fixture iterates the same stream in two consecutive `for` loops and expects identical output.
- There is no "already consumed" error to hit, so nothing tests for one.
- `len` and indexing both work, because a buffer has both.

The earlier design had `?p` produce a one-shot token stream that raised `DoubleStreamDrain` on a second read. Making a stream a buffer is what let that error disappear rather than move somewhere else.

## What each file does

- *`yield_basic/`* covers the whole surface in one file: a generator, `len`, indexing, iterating twice, printing, an early bare `return`, mixed yield types widening, and a generator calling another generator.
- *`yield_toplevel_error.jde`* puts `yield` outside any function. It is rejected for the same reason a top-level `return` is: there is no stream for the value to join. The `_error` suffix means CI asserts that `jade check` *fails* on it.

## Gotchas

*A generator cannot also return a value.* A bare `return` is fine and stops it early. But `return x` asks the function to be both a stream producer and a plain function, which is a compile error called `YieldAndReturn`. The fixture covers the legal half. The illegal half is a Rust test in `compiler/tests.rs`, because a second `_error` fixture would add a file to prove one line.

*A stream renders like an array*, and that is deliberate rather than incidental. In a compiled binary, a stream *is* an ordinary array, so `len`, indexing, `for`, and printing all reuse what arrays already do. The parity gate diffs stdout, which makes that rendering a contract.

## Running them

```sh
./target/debug/jade run examples/streams/yield_basic/yield_basic.jde
./target/debug/jade check examples/streams/yield_toplevel_error.jde   # must fail
./src/scripts/backend-parity.sh
```
