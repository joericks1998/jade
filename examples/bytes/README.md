# `examples/bytes/`: building and writing into a binary blob

## What this subtree is

The fixtures for `std::bytes` and for `b[i] = v`. Two programs, each built and run by `src/scripts/golden.sh`, which compares what the binary prints with the `.expected` file beside it.

- *`construct/construct.jde`* covers `bytes.zeros`, `bytes.from_ints`, and `bytes.concat`, including every failure each can raise.
- *`buffer/buffer.jde`* covers writing an octet, and the reference semantics that come with it.

The other blob fixtures live elsewhere because they are about something else. `examples/fs/bytes_roundtrip/` is about surviving a trip through the filesystem, and `examples/trust/bytes_taint/` and `examples/trust/bytes_concat_taint/` are about the taint checker.

## Why `buffer.jde` writes to a blob twice

Because a module-level binding and a local inside a function reach the write by different paths. Until v1.4.8 they were two bytecode instructions, `SetIndexGlobal` and `SetIndex`, and both engines had separate arms for each. In MIR the write is one `Op::BytesSet`, but its target is a `Place::Global` for one and a `Place::Local` for the other, and the backend reads each its own way. A fixture that only wrote to a module-level buffer would leave the other path untested, and the failure it misses is the ordinary one: `fn fill(buf) { buf[0] = 1 }` is how buffer code is actually written.

## Why the `try` blocks print a literal

Until v1.4.8 the parity gate diffed stdout, stderr, and the exit code byte for byte. The VM prefixed a raised message with its source span and a compiled binary has no span at run time, so a fixture that printed `e.message` would have failed the gate on plumbing rather than on behavior. Printing `"refused a negative length"` still proves the raise happened, still proves it was catchable, and proved both engines agreed on *whether* it raises. The `.expected` files were recorded from those runs, so the literal stays.

The wording itself lives in `src/backend/bytes.rs`, which emits the support code that raises it.

## Who uses it

`src/scripts/golden.sh` runs both files. `src/cli/check.rs` asserts every example passes `jade check`, and `src/cli/tests.rs` asserts the formatter leaves them alone. See [`examples/README.md`](../README.md) for the rules a fixture has to follow.
