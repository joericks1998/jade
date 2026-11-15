# `examples/pipe/`: the pipe operator

## What this subtree is

These are the fixtures for `|>`. Since 2.0.3 a pipe listens to a value as it arrives: `x |> f` calls `f` once per piece of `x`, while a call, `f(x)`, waits for all of `x`. A call to a generator gives each value as it is yielded, a stream already held gives its values in order, a prompt's reply gives its text a piece at a time, and anything else is one piece. `design/v2-pipes.md` has the rules and why.

## Why it exists

The pipe used to be another spelling of a call, with one exception: a converter piped after `?p` asked the model for that type. Now a pipe always runs per piece and a typed dereference is a call around the reply, `toInt(?p)`. These fixtures pin both halves, so neither drifts back.

## What each file does

- *`pipe.jde`* pipes plain values, each one piece, so every pipe is one call: a function, a chain, a stage with arguments, and `print`.
- *`per_piece/`* pipes generators and streams. Its output interleaves the generator's prints with the stages', which is what shows each value going through the stages before the next is made. It also covers a held stream, an array as one piece, stage arguments worked out once, the stream a pipe gives back, and a raise in a stage passing the generator's own `try`.
- *`deref_chain/`* and *`deref_in_print/`* pipe and print typed dereferences, `toInt(?p) |> double` and `print(toInt(?p))`. They dereference a prompt, so `golden.sh` skips them; their `.expected` files are v1's records.
- *`bad_stage_error.jde`* pipes into an int, and the error names what it found.
- *`whole_reply_stage_error.jde`* writes the old typed spelling, `?p |> toInt`, and the error names `toInt(?p)`.
- *`deref_type_check_error.jde`* writes `int(?p)`, a type check on a reply, which is a str.

Each `*_error.jde` must fail `jade check`.

## Running them

```sh
./target/debug/jade run examples/pipe/per_piece/per_piece.jde
./target/debug/jade check examples/pipe/whole_reply_stage_error.jde   # must fail
./compiler/src/scripts/golden.sh
```
