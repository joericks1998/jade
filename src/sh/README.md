# `src/sh/`: `std::sh`, the shell package

## What this subtree is

This is the VM's half of `std::sh`, which is three functions that run a command through `sh -c`.

| Jade | Returns | On a non-zero exit |
|------|---------|--------------------|
| `sh.exec(cmd)` | captured stdout, trimmed | raises, with stderr in the message |
| `sh.run(cmd)` | the exit code | nothing, because the code *is* the answer |
| `sh.output(cmd)` | `{stdout, stderr, code}` | nothing, so inspect `code` yourself |

There are three rather than one because a caller wants something different in each case. Folding them together would force every caller to handle a failure mode it did not care about.

## Why it looks like this

*The work is not here.* Spawning and capturing live in `jade_runtime::shf`, the shared crate, so a compiled binary and the interpreter run a command the same way. What stays in this directory is the marshalling, meaning arguments out of `VmValue` and results back into one, plus the part that must not be shared: the trust check.

*Everything this package does is a code-execution sink.* All three functions reach the same `sh -c`, so all three refuse a string whose trust byte says it came from outside the program, whether that is a model reply, a file, the network, or stdin. That is the whole point of tracking trust: a program cannot be talked into running a command it was handed.

`sh.output` did not check until v1.3.3, and the shape of that bug is worth remembering before adding a fourth function here. It was not a smaller hole than a missing check on `exec` would have been. It was the *same* hole, because an attacker picks which function to reach. A check on two of three sinks does not narrow what an untrusted command can do. It only decides how the command has to be written.

*The output of a command is tainted.* That is what stops the obvious loop: run a command, take its stdout, and feed the result back in. `exec` returns a `JStr::tainted`, so the second call is refused.

## What each file does

- *`mod.rs`* holds `require_str`, `refuse_if_tainted`, the three functions, and the `Package` value the builtin registry picks up.
- *`tests.rs`* covers the behavior of each function and the trust model: a tainted command refused at each sink, a trusted one accepted, and `exec` output coming back tainted.

## Who uses it

*Depends on:* `jade_runtime::shf` for the spawning, `jade_runtime::trust` for `JStr` and the refusal message, and `builtins::Package` to register itself.

*Used by:* `builtins/`, which mounts `SH_PKG` under `std/sh`. The compiled backend does not go through here at all. `src/codegen/builtins.rs` lowers to `jrt_sh_exec`, `jrt_sh_run`, and `jrt_sh_output` in `runtime_aot/common.c`. Those are thin forwarders that do the same refusal before calling the same shared implementation.

## Gotchas

*A change here needs the matching change in `runtime_aot/common.c`.* The two engines have separate marshalling and separate refusals, and nothing checks that they agree except `examples/trust/`. The refusal message is byte-identical on purpose, because the parity gate diffs output.

*The refusal is a raise, not an exit.* It is catchable, so `try { sh.exec(x) } catch e { … }` runs the handler. The compiled runtime used to print and exit instead, which made the same program behave differently on the two engines.

## Building and testing

```sh
cargo test sh::
./target/debug/jade run examples/trust/sh_sinks/sh_sinks.jde
```
