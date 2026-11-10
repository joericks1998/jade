# `src/time/`: `std::time`

## What this subtree is

`use std::time` binds one global, `time`, holding nine functions: two wall clocks, a monotonic clock, `sleep`, a deadline you can wait on, and four ways to move between a timestamp and a calendar.

This directory holds only their names. Every function's actual behavior lives once in [`src/backend/time.rs`](../backend/README.md), which emits it into the program that calls it, with the same calendar arithmetic v1 used. The exception is `after`, which is a future, so it lives with the task queue in `src/backend/tasks.rs`. Until phase 6 the behavior lived in `jade-runtime::timef`, and until v1.4.8 this directory was the *VM half*: a `VmValue` wrapper around the same cores, which is why the two engines could not drift on what a date means.

## What each file does

- *`mod.rs`* holds `TIME_PKG`, the `Package` that names the nine functions, and the type registration for the `time` global. The calendar arithmetic is pinned by `examples/time/calendar/`, whose recorded output came from v1's arithmetic, which was tested against dates checked independently with `date -u`.

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
| `time.parts(ts)` | `TimeParts` | eight UTC calendar fields, read as `p.year` or `p["year"]` |
| `time.stamp(y, mo, d[, h[, mi[, s]]])` | `int` | the inverse of `parts` |

## Three things that are the way they are on purpose

*There are two clocks, and they answer different questions.* `now` and `now_ms` read the wall clock. That is what a timestamp is, and it is what a duration must never be measured with, because NTP and a person with the right permissions can both move it, backwards included. `monotonic` never moves backwards, and it means nothing on its own. Only the gap between two readings means anything.

*`utc` is trusted and `local` is tainted.* `local` shells out to `date` and reads text back, so its result came from outside the program. `utc` is computed in process from an integer, and an integer carries no taint to inherit. Marking it tainted would keep a formatted date out of `sh.exec` for no reason anyone could explain.

*`stamp` carries instead of failing.* Month 13 is next January, and day 0 is the last day of the previous month. That is what lets date arithmetic be a single call rather than a calendar table. Rejecting those inputs would be easier to defend on its own, and far less useful in practice.

## Who uses it

- [`src/builtins/`](../builtins/README.md) registers `TIME_PKG` in the package table.
- [`typechecker/infer/builtins.rs`](../typechecker/infer/README.md) gives each `time.*` call its static type, such as the `TimeParts` struct `time.parts` returns.
- [`mir/packages.rs`](../mir/README.md) lowers each call to its MIR operation. A function added here without an arm there is a program the checker types and MIR refuses, and `jade check` reports it, since it runs MIR lowering too.
- [`src/backend/time.rs`](../backend/README.md) and `src/backend/tasks.rs` emit the code.
