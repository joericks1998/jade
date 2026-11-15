# Sized number types

Status: decided and built, 2026-11-13, for jade 2.0.4. Joe made every decision here; this file
records them before the code, so the code has one place to answer to.

## The problem

Jade has one integer type, `int`, a signed 64-bit integer, and one float, `float`,
64 bits. A compiled language talks to C libraries, GPU kernels and file formats that
use other widths, and since 2.0.4 a type name is a promise the compiler keeps:
`int(x)` checks that `x` is an int and never converts it. So the widths a program
needs should be types the checker knows, not conversions a program hopes for.

## The types

| Type | Width | Signed | Range |
|------|-------|--------|-------|
| `byte` | 8 | no | 0 to 255 |
| `short` | 16 | yes | -32768 to 32767 |
| `ushort` | 16 | no | 0 to 65535 |
| `int32` | 32 | yes | -2147483648 to 2147483647 |
| `uint32` | 32 | no | 0 to 4294967295 |
| `int`, also spelled `long` | 64 | yes | the int's range, unchanged |
| `ulong` | 64 | no | 0 to 18446744073709551615 |
| `float32` | 32 | | IEEE single precision |
| `float`, also spelled `double` | 64 | | IEEE double precision, unchanged |

The names are C's, with `int` and `float` keeping the 64 bits they have always had, so
no existing program changes meaning. `long` and `double` are the same types as `int`
and `float`, not new ones. C's name for 32 bits, `int`, is taken, so 32 bits is
`int32`, `uint32` and `float32`.

## Each type has a type function and a converter

As with the built-in types since 2.0.4, the type function checks and the converter
converts.

| Type | Type function | Converter |
|------|---------------|-----------|
| `byte` | `byte(x)` | `toByte(x)` |
| `short` | `short(x)` | `toShort(x)` |
| `ushort` | `ushort(x)` | `toUshort(x)` |
| `int32` | `int32(x)` | `toInt32(x)` |
| `uint32` | `uint32(x)` | `toUint32(x)` |
| `long` | `long(x)`, the same as `int(x)` | `toLong(x)`, the same as `toInt(x)` |
| `ulong` | `ulong(x)` | `toUlong(x)` |
| `float32` | `float32(x)` | `toFloat32(x)` |
| `double` | `double(x)`, the same as `float(x)` | `toDouble(x)`, the same as `toFloat(x)` |

A type decorator is the type function, as it is for `int`: `@byte let b = v` is
`let b = byte(v)`.

## Arithmetic follows assembly

Joe's rule: follow the conventions assembly already established. An `add` instruction
takes two operands of one width; a program that adds a 16-bit value to a 64-bit one
first extends it, with `movsx` for a signed value or `movzx` for an unsigned one.
Signedness is not part of a register, it is part of the instruction: `idiv` or `div`,
`jl` or `jb`, `sar` or `shr`.

1. *Both operands have the same type, and so does the result.* `short + int` is a
   build error that names the fix: `toInt(s) + n`, or `s + toShort(n)`. This holds
   for `+ - * / %`, the bitwise operators and the comparisons.
2. *Widening is explicit.* `toInt(s)` sign-extends a signed type, and `toUlong(u)`
   zero-extends an unsigned one. A widening converter never fails.
3. *Narrowing is explicit and checked.* `toByte(n)` raises when `n` does not fit.
   Assembly keeps the low bits instead; Jade raises, as it does on overflow
   everywhere else. The low bits are `toByte(n & 0xff)`.
4. *The type picks the instruction.* An unsigned type divides, compares and shifts
   right as unsigned; a signed one as signed.
5. *A literal takes the type it meets,* as an immediate takes its operand's width.
   `b + 1` with `b` a byte adds the byte 1. `@uint32 let x = 5` is fine, and
   `@byte let c = 300` is a build error, since 300 does not fit a byte. A literal
   with nothing to meet is an `int`, or a `float` if it has a decimal point. A
   variable never changes type: `uint32(n)` with `n` an int needs `toUint32(n)`.
6. *Overflow raises.* A signed type raises on signed overflow and an unsigned one on
   carry or borrow, so a byte holding 3 minus 5 raises rather than wrapping to 254.
   These are the flags assembly sets, checked after every operation.
7. *Division by zero raises,* where the CPU would trap. So does the one signed
   division that overflows, the smallest value divided by -1.
8. *A shift amount is an int or a literal,* and must be less than the type's width,
   as `1 << 64` already raises for an int.

*The one exception, kept on purpose:* `int` and `float` still mix. `1 + 0.5` is 1.5,
and `float(5)` is 5.0, because an int is also a float. Assembly would need a convert
instruction there, and Joe chose to keep the existing rule. It does not extend to the
sized types: `float32 + float` and `int32 + float` are build errors.

## What the converters accept

Every converter takes any number type, `bool`, `char` and `str`, and a JSON value
holding a number or a string. A float converts toward zero; a string is read as a
number. Each raises when the result does not fit its type, or the text is not a number.
`toString` takes every type, as before.

## Out of scope for now

- *A `ulong` literal past the int's range,* such as `0xffffffffffffffff`, cannot be
  written yet; the lexer bounds every literal to the int's range. Arithmetic and
  converters can reach those values.
- *`std::math` and other packages* take `int` and `float`. A sized value is passed
  through a converter first.
- *Native packages* keep declaring C scalars as `int` and `float` in `jade.toml`;
  mapping `scalar:uint32_t` and friends to the sized types is a later change.
- *Typed dereferences* stay the five converters of 2.0.4, written `toInt(?p)` since pipes run once per piece; see `v2-pipes.md`.

## What changes in the compiler

- `typechecker/infer/ty.rs`: `Ty::Sized(SizedNum)`, one variant for the seven new
  types, so every existing `Ty::Int` and `Ty::Float` path stays as it is.
- `typechecker/infer/`: the type functions, converters, the same-type rule for every
  operator, and literals taking the type they meet, at operators, assignments,
  struct fields, array elements and type functions.
- `mir/`: `NumTy` gains the sized kinds, so `Arith`, `Bit` and `Neg` carry the width;
  a constant can be sized; conversions are one operation, from any number type to any.
- `backend/`: storage at each width, the overflow intrinsics at each width with the
  unsigned forms, unsigned division, comparison and right shift, `float32`
  arithmetic, and printing, including a `ulong` past the int's range and a `float32`
  as its own shortest digits.
