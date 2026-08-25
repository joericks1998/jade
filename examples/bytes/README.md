# `examples/bytes/`: building and writing into a binary blob

## What this subtree is

The fixtures for `std::bytes` and for `b[i] = v`. Two programs, both run by `src/scripts/backend-parity.sh` on the bytecode VM and on a compiled binary, with the output diffed.

- *`construct/construct.jde`* covers `bytes.zeros`, `bytes.from_ints`, and `bytes.concat`, including every failure each can raise.
- *`buffer/buffer.jde`* covers writing an octet, and the reference semantics that come with it.

The other blob fixtures live elsewhere because they are about something else. `examples/fs/bytes_roundtrip/` is about surviving a trip through the filesystem, and `examples/trust/bytes_taint/` and `examples/trust/bytes_concat_taint/` are about the taint checker.

## Why `buffer.jde` writes to a blob twice

Because the write is two instructions and not one. A module-level binding compiles to `SetIndexGlobal` and a local inside a function compiles to `SetIndex`, and the emitter picks between them by whether the name is a local. Both engines have separate arms for each. A fixture that only wrote to a module-level buffer would leave the other path untested, and the failure it misses is the ordinary one: `fn fill(buf) { buf[0] = 1 }` is how buffer code is actually written.

## Why the `try` blocks print a literal

The parity gate diffs stdout, stderr, and the exit code byte for byte. The VM prefixes a raised message with its source span and a compiled binary has no span at run time, so a fixture that printed `e.message` would fail the gate on plumbing rather than on behavior. Printing `"refused a negative length"` still proves the raise happened, still proves it was catchable, and still proves both engines agree on *whether* it raises.

The wording itself is pinned in Rust instead, in `src/runtime/src/bytesf.rs`, where both engines read it from one place.

## Who uses it

`src/scripts/backend-parity.sh` runs both files. `src/cli/check.rs` asserts every example passes `jade check`, and `src/cli/tests.rs` asserts the formatter leaves them alone. See [`examples/README.md`](../README.md) for the rules a fixture has to follow.
