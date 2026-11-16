# `examples/random/`: `std::random`

## What this subtree is

The fixture for `std::random`. Since 2.0.8 the package is written in Jade, `compiler/src/lib/random.jde`: ranges, `float`, `choice` and `shuffle` are Jade, and the generator, v1's xoshiro256** seeded through splitmix64, stays behind two hooks in `compiler/src/codegen/sys/random.rs`, under a lock, since tasks share it.

- *`seeded/`* seeds the generator and prints what it draws: ints in small ranges, the whole int range and half of it, ranges whose size is not a power of two, which the draw rejects and retries rather than bias, floats, `choice` over arrays of strings, ints, floats and classes, and `shuffle` of each; that seeding again starts the sequence again; and the two refusals, `min > max` and an empty array.

## Why it was built

A seeded program draws the same values every time, and programs rely on that. Until 2.0.8 nothing pinned the sequence; this fixture was recorded from the backend version before the move, so it holds the Jade version to every value the backend drew.

## Who uses it

`compiler/src/scripts/golden.sh` builds it and compares its output with `seeded.expected`.
