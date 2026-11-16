# `examples/fs/`: `std::fs`

## What this subtree is

The fixtures for `std::fs`: reading and writing files as text and as bytes, the questions `exists`, `is_file`, `is_dir` and `size`, listing, making, copying, renaming and removing, the two standard streams, and what is trusted. Since 2.0.8 the package is written in Jade, `compiler/src/lib/fs.jde`, with each C library call a hook in `compiler/src/codegen/sys/fs.rs`, and every fixture here prints what the backend version before it printed.

## Why it was built

A file is the commonest way outside data reaches a program, so the package's errors and its trust rules are what a program relies on most. A fixture that exercised only the happy path would pass with a broken failure path in it, which is what `metadata/` was written to catch, and what `edges/` covers for every function.

## What each file does

- *`basic/`* writes, reads, appends, lists, makes a directory and deletes.
- *`bytes_roundtrip/`* writes and reads bytes that a string cannot hold, a NUL and a byte that is not UTF-8, and appends to them.
- *`metadata/`* covers what v1.3.23 added, `is_file`, `is_dir`, `size`, `copy`, `rename` and `rmdir`, and that each failing call raises catchably.
- *`edges/`* prints every failure's message word for word, `I/O error: <op> '<path>': <reason> (os error <n>)`, choosing only errors whose text is the same on macOS and Linux; `mkdir` making parents, accepting a directory already there, and the error it reports for a file in the way or a path through one; text that is not UTF-8; files larger than one read; a copy that names its source when its destination fails; and trust: what a file holds is tainted unless the call says `trust = true`, a tainted path is refused for reading unless the call says the same, and entry names are the program's own.
- *`stdio/`* reads standard input and writes standard output byte for byte. A fixture cannot choose its own input, so it runs itself as a child through the shell, with the input it wants and an argument that says what to do; `env.args()` is trusted, so its own path may be part of a command. What standard input holds is tainted.

## Who uses it

`compiler/src/scripts/golden.sh` builds each and compares its output with its `.expected` file. `edges/` and `stdio/` were recorded from the backend version before 2.0.8 moved the package into Jade. `examples/trust/` uses `std::fs` too, to show that taint survives bytes.
