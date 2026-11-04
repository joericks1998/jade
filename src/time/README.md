# `src/time/`: `std::time`

## What this subtree is

`use std::time` binds one global, `time`, holding nine functions: two wall clocks, a monotonic clock, `sleep`, a deadline you can wait on, and four ways to move between a timestamp and a calendar.

This directory holds only their names. Every function's actual behavior lives once in [`jade-runtime::timef`](../runtime/src/timef.rs), and the AOT backend calls it through the `jrt_time_*` C-ABI wrappers at the bottom of that file. The exception is `after`, which is a future, so it lives with the task pool in `jade-runtime`'s `task.rs` and is reached through `jade_time_after` in `runtime_aot/posix.c`. Until v1.4.8 this directory was the *VM half*: a `VmValue` wrapper around the same cores, which is why the two engines could not drift on what a date means.

## What each file does

- *`mod.rs`* holds `TIME_PKG`, the `Package` that names the nine functions, and the type registration for the `time` global. The calendar arithmetic is tested in `jade-runtime`, against dates checked independently with `date -u`.

## The surface

| Function | Returns | |
|----------|---------|--|
| `time.now()` | `int` | Unix seconds |
| `time.now_ms()` | `int` | Unix milliseconds |
| `time.monotonic()` | `float` | seconds from a fixed point in this process |
| `time.sleep(secs)` | `nil` | int or float; non-positive is a no-op |
| `time.after(secs)` | `future` | finishes with nil at the deadline |
| `time.local(tz)` | `str` | formatted local time, `nil` tz for the system zone |
| `time.utc(ts)` | `str` | ISO 8601, `2026-08-16T14:03:22Z` |
| `time.parts(ts)` | `dict` | eight UTC calendar fields |
| `time.stamp(y, mo, d[, h[, mi[, s]]])` | `int` | the inverse of `parts` |

## Three things that are the way they are on purpose

*There are two clocks, and they answer different questions.* `now` and `now_ms` read the wall clock. That is what a timestamp is, and it is what a duration must never be measured with, because NTP and a person with the right permissions can both move it, backwards included. `monotonic` never moves backwards, and it means nothing on its own. Only the gap between two readings means anything.

*`utc` is trusted and `local` is tainted.* `local` shells out to `date` and reads text back, so its result came from outside the program. `utc` is computed in process from an integer, and an integer carries no taint to inherit. Marking it tainted would keep a formatted date out of `sh.exec` for no reason anyone could explain.

*`stamp` carries instead of failing.* Month 13 is next January, and day 0 is the last day of the previous month. That is what lets date arithmetic be a single call rather than a calendar table. Rejecting those inputs would be easier to defend on its own, and far less useful in practice.

## Who uses it

- [`src/builtins/`](../builtins/README.md) registers `TIME_PKG` in the package table.
- [`src/codegen/builtins.rs`](../codegen/README.md) lowers each `time.*` call to the matching `jrt_time_*` symbol, or `jade_time_after` for `after`. A function added here without an arm there is a program `jade check` accepts and `jade build` refuses. `every_package_fn_lowers_in_the_chunk_backend` in `src/codegen/tests.rs` exists to catch exactly that.
- [`src/runtime_aot/runtime.h`](../runtime_aot/README.md) declares the C-ABI prototypes the generated code calls.
