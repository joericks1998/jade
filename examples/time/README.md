# `examples/time/`: `std::time`

## What this subtree is

The fixtures for `std::time`: the wall and monotonic clocks, sleeping, and conversion between a timestamp and UTC calendar fields. Since 2.0.8 the package is written in Jade, `compiler/src/lib/time.jde`, with the clocks, sleeping and the time zone behind hooks; both fixtures print what the backend version before it printed.

## What each file does

- *`calendar/`* is the v1.3.24 fixture: the clocks, asked questions with fixed answers so nothing printed depends on when it runs, then `utc`, `parts` and `stamp` on fixed timestamps, with month and day carries and the leap days of 2000, 2024 and 2100.
- *`calendar_edges/`* covers the arithmetic's edges: leap years across four centuries and before year 0, timestamps before 1970, which floor rather than truncate, the largest and smallest timestamps, fields far out of range in both directions, and sums past the int range, which saturate.

## Who uses it

`compiler/src/scripts/golden.sh` builds both and compares their output with each `.expected` file. `examples/async/` uses `time.sleep`, `time.monotonic` and `time.after` too.
