# `examples/sh/`: `std::sh`

## What this subtree is

The fixture for `std::sh`, which runs a command through `sh -c`. Since 2.0.8 the package is written in Jade, `compiler/src/lib/sh.jde`, with starting the shell, reading its output and the trust check as hooks in `compiler/src/codegen/sys/process.rs`.

- *`capture/`* shows what `exec` trims, trailing newlines and only those, output that is not UTF-8 with each bad sequence replaced, the message of a command that fails, with its standard error trimmed of Unicode white space at both ends, and of one a signal stopped, whose code reads as -1; what `run` and `output` give back; that a command filling both pipes at once does not stall; and the exact text of the refusal each of the three gives a tainted command, and that what `output` returns is tainted.

## Why it was built

Every function here is a code-execution sink, so the refusal and the taint on a command's output are what keep a program from running a command it was handed. `examples/trust/sh_sinks/` pins that all three refuse; this fixture pins their messages and the rest of the package, recorded from the backend version before 2.0.8 moved it into Jade.

## Who uses it

`compiler/src/scripts/golden.sh` builds it and compares its output with `capture.expected`. `examples/trust/` and `examples/dicts/dot_access/` use `std::sh` too.
