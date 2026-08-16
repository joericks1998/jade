# `src/time/` — the VM's `std::time`

`use std::time` binds one global, `time`, holding eight functions: two wall
clocks, a monotonic clock, `sleep`, and four ways to move between a timestamp
and a calendar.

This directory is only the **VM half**. Every function's actual behaviour lives
once in [`jade-runtime::timef`](../runtime/src/timef.rs), and what is here is the
`VmValue` wrapper around it — argument checking, the error to raise, and the
`VmValue` to hand back. The AOT backend calls the same cores through the
`jrt_time_*` C-ABI wrappers at the bottom of that file, which is why the two
engines cannot drift on what a date means.

## Files

- **`mod.rs`** — the eight `BuiltinFn`s and the `TIME_PKG` `Package` that
  registers them. Each is a thin shell: check arity, check the argument types,
  call `jade_runtime::timef`, wrap the answer.
- **`tests.rs`** — tests for *this* layer: the value types returned, the trust
  marking on `time.utc`, and the errors raised on wrong arity or wrong types.
  The calendar arithmetic itself is tested in `jade-runtime`, against dates
  independently checked with `date -u`.

## The surface

| Function | Returns | |
|----------|---------|--|
| `time.now()` | `int` | Unix seconds |
| `time.now_ms()` | `int` | Unix milliseconds |
| `time.monotonic()` | `float` | seconds from a fixed point in this process |
| `time.sleep(secs)` | `nil` | int or float; non-positive is a no-op |
| `time.local(tz)` | `str` | formatted local time, `nil` tz for the system zone |
| `time.utc(ts)` | `str` | ISO 8601, `2026-08-16T14:03:22Z` |
| `time.parts(ts)` | `dict` | eight UTC calendar fields |
| `time.stamp(y, mo, d[, h[, mi[, s]]])` | `int` | the inverse of `parts` |

## Three things that are the way they are on purpose

**There are two clocks, and they answer different questions.** `now`/`now_ms`
read the wall clock, which is what a timestamp is and what a duration must not
be measured with — NTP and a person with permissions both move it, backwards
included. `monotonic` never moves backwards and means nothing on its own; only
the gap between two readings does.

**`utc` is trusted and `local` is tainted.** `local` shells out to `date` and
reads text back, so its result came from outside the program. `utc` is computed
in process from an integer, and an integer carries no taint to inherit. Marking
it tainted would keep a formatted date out of `sh.exec` for no reason anyone
could explain.

**`stamp` carries instead of failing.** Month 13 is next January and day 0 is
the last day of the previous month, which is what lets date arithmetic be one
call rather than a calendar table. Rejecting them would be more defensible in
isolation and much less useful in practice.

## Used by

- [`src/builtins/`](../builtins/README.md) registers `TIME_PKG` in the package
  table.
- [`src/codegen/builtins.rs`](../codegen/README.md) lowers each `time.*` call to
  the matching `jrt_time_*` symbol. A function added here without an arm there
  is a program `jade run` accepts and `jade build` refuses — the parity test in
  `src/builtins/tests.rs` exists to catch exactly that.
- [`src/runtime_aot/runtime.h`](../runtime_aot/README.md) declares the C-ABI
  prototypes the generated code calls.
