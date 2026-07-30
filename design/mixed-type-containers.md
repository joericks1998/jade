# Mixed-type containers

**Status:** wanted, not built. Recorded because a real contract needs it and the
current workaround is a footnote in three files.

## The rule today

An array literal must be homogeneous. `[1, 2, 3]` is fine; `[1, "two"]` is a type
error, and so is `[Token { text: "hi" }, Done { tokens_used: 1 }]` — two struct
types in one literal.

This is by design, not an oversight. A homogeneous array is what lets element
access have a known type without a runtime check, and it is what makes
`for x in arr` mean something the type checker can follow.

## Why it is a problem

The inference response is a **sequence of different things**: `Token*` then `Done`,
or a lone `Error`, with an optional leading `Meta`. That is the shape, and it is
declared as such in `ovata-infer-protocol`. A provider package cannot write it as
one literal.

Two workarounds exist, and both are worse than the thing they replace:

- **Build with `push`.** `let frames = []` then `frames.push(...)` per frame. This
  works on both engines — the homogeneity check is on the literal — which makes the
  restriction feel arbitrary rather than principled. It is what
  `scripts/fake-provider.jde` does.
- **Use dicts instead of structs.** Every dict is one type, so a mixed *array of
  dicts* is legal. This is why the frame protocol still accepts the dict form, and
  it is the form providers actually use. But a dict has no name, so a renamed key
  is a silent `nil` — exactly the failure mode v1.1.31 set out to remove. The
  language accepts a shape it can check less well because it cannot express the
  shape it can check better.

The second point is the real cost. Typing the response bought a loud failure on an
unrecognised frame, but it could not make the typed form the *natural* one to
write.

## What would fix it

Something in the language that holds values of several declared types and can still
be walked. Sketches, none decided:

- **A sum type**, declared where the members are declared:
  `type Frame = Token | Done | Error | Meta | Json`, with `[Frame]` a homogeneous
  array of it. Matches how the Rust half of the protocol already spells this
  (`enum Frame`), so the two halves would finally have the same shape rather than
  the same field names. Needs a matching construct to consume it.
- **A heterogeneous list type**, e.g. `list[any]`, opting out of element typing
  where the program does its own dispatch. Cheaper to add, and honest about what a
  frame array is, but it pushes every check to runtime and gives the type checker
  nothing to say about a frame at all.

The first is the better fit for the protocol, and it is a real language feature:
declaration syntax, inference, exhaustiveness, lowering in both engines, and a
representation in the native FFI so a sum type can cross into a package. The
second is a day's work and buys less.

## Where the workaround is documented today

Anywhere it might surprise someone:

- `ovata-infer-protocol`'s `jade/infer.jde` — the `push` recipe, next to the frames
- `scripts/fake-provider.jde` — why the stub builds its array that way
- `design/provider-packages.md` — the frame-shape section
