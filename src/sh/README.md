# `src/sh/`: `std::sh`, the shell package

## What this subtree is

This directory names the functions of `std::sh` and registers the package's type with the checker. Until v1.4.8 it also held the VM's half of the package. `std::sh` is three functions that run a command through `sh -c`.

| Jade | Returns | On a non-zero exit |
|------|---------|--------------------|
| `sh.exec(cmd)` | captured stdout, trimmed | raises, with stderr in the message |
| `sh.run(cmd)` | the exit code | nothing, because the code *is* the answer |
| `sh.output(cmd)` | `{stdout, stderr, code}` | nothing, so inspect `code` yourself |

There are three rather than one because a caller wants something different in each case. Folding them together would force every caller to handle a failure mode it did not care about.

## Why it looks like this

*The work is not here.* Spawning, capturing, and the trust check live in `src/backend/sh.rs`, which emits them into a program that calls `std::sh`. Until phase 6 they lived in `jade-runtime` and v1's C runtime, and until v1.4.8 this directory held the VM's marshalling, meaning arguments out of `VmValue` and results back into one, plus the VM's own copy of the trust check.

*Everything this package does is a code-execution sink.* All three functions reach the same `sh -c`, so all three refuse a string whose trust word says it came from outside the program, whether that is a model reply, a file, the network, or stdin. That is the whole point of tracking trust: a program cannot be talked into running a command it was handed.

`sh.output` did not check until v1.3.3, and the shape of that bug is worth remembering before adding a fourth function here. It was not a smaller hole than a missing check on `exec` would have been. It was the *same* hole, because an attacker picks which function to reach. A check on two of three sinks does not narrow what an untrusted command can do. It only decides how the command has to be written.

*The output of a command is tainted.* That is what stops the obvious loop: run a command, take its stdout, and feed the result back in. `exec` returns a tainted string, so the second call is refused.

## What each file does

- *`mod.rs`* holds `SH_PKG`, the `Package` value the builtin registry picks up, which names the three functions, and the type registration for the `sh` global.

## Who uses it

*Depends on:* `builtins::Package` to register itself, and `compiler::type_infer` for the type registration.

*Used by:* `builtins/`, which mounts `SH_PKG` under `std/sh`. The compiled backend does not go through here at all. `mir/packages.rs` lowers each call to `Op::Sh`, and `sh_fn` in `src/backend/sh.rs` builds one function per kind of call. Each one calls `refuse_tainted`, defined in `src/backend/fs.rs`, before it spawns anything.

## Gotchas

*A fourth function needs the refusal in `src/backend/sh.rs`, as well as its name here.* Until v1.4.8 the two engines had separate refusals, and nothing checked that they agreed except `examples/trust/`. The refusal message stays byte-identical to what the VM printed, because `src/scripts/golden.sh` compares output with what was recorded then.

*The refusal is a raise, not an exit.* It is catchable, so `try { sh.exec(x) } catch e { … }` runs the handler. v1's compiled runtime used to print and exit instead, which made the same program behave differently on the two engines.

## Building and testing

```sh
./target/debug/jade run examples/trust/sh_sinks/sh_sinks.jde
./src/scripts/golden.sh
```
