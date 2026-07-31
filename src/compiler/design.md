# Mixed-type containers

**Status:** shipped in v1.1.32 — mixed *arrays*, which is the second of the two
options below. A named sum type is still not built.

## The rule that changed

An array literal had to be homogeneous. `[1, "two"]` was a type error, and so was
`[Token { text: "hi" }, Done { tokens_used: 1 }]` — two struct types in one
literal.

The stated reason was that a homogeneous array lets element access have a known
type without a runtime check, and gives `for x in arr` something the type checker
can follow. Both are true, and neither required rejecting the mixed case: the
element type simply widens to `Unknown`, exactly as a dict's value type already
did under the same conditions, and every consumer of that type already fell back
to the dynamic opcodes both engines have always carried.

Three things said the check was a frontend gate over a runtime that never needed
it. `push` built the same array with no complaint. The docs advertised
`let mixed = [1, 2.0, true, "hello"]`, which did not compile. And the rule was
stricter than the language's own arithmetic — `[1, 2.0]` was refused while
`1 + 2.0` is fine.

## What removing it cost

Not the type checker. Deleting the check was about ten lines. The cost was the
divergences it had been hiding, because a mixed array was the only way to reach
them:

- `arr.contains(x)` answered in the VM and raised in a compiled binary. Membership
  needs an equality that walks past elements of other kinds; the AOT was using the
  strict `==`. Fixed by `ops::eq_total`, shared by both engines.
- A cross-kind comparison raised on both, but a compiled binary said
  `'==' requires numeric operands` — misleading, since the problem is that the
  kinds differ. Both now say `'==' cannot compare int and str`.
- The VM interpolated a Rust enum name into arithmetic type errors
  (`Add requires numeric operands`). Both now use the operator symbol.

The lesson worth keeping: a restriction that makes a class of program
unwritable also makes that class of bug unreachable, and removing it surfaces
however much drift accumulated meanwhile.

## Why it mattered

The inference response is a **sequence of different things**: `Token*` then `Done`,
or a lone `Error`, with an optional leading `Meta`. That is the shape, and it is
declared as such in `ovata-infer-protocol`. Until v1.1.32 a provider package could
not write it as one literal, and the two workarounds were both worse than the
thing they replaced:

- **Build with `push`.** `let frames = []` then `frames.push(...)` per frame. It
  worked on both engines, since the check was on the literal — which made the
  restriction feel arbitrary rather than principled.
- **Use dicts instead of structs.** Every dict is one type, so a mixed *array of
  dicts* was legal. But a dict has no name, so a renamed key is a silent `nil` —
  exactly the failure mode v1.1.31 set out to remove. The language accepted a shape
  it could check less well because it could not express the shape it could check
  better.

The second was the real cost. Typing the response bought a loud failure on an
unrecognised frame, but it could not make the typed form the *natural* one to
write. It can now.

## What is still missing

Mixed arrays are the *heterogeneous list* answer. They make the frame array
writable, and nothing more: the element type is `Unknown`, so the compiler has
nothing to say about a frame and every check happens at run time, in the decoder.

The other sketch is still open, and is the better fit for a protocol:

- **A sum type**, declared where the members are declared:
  `type Frame = Token | Done | Error | Meta | Json`, with `[Frame]` a homogeneous
  array of it. Matches how the Rust half of the protocol already spells this
  (`enum Frame`), so the two halves would finally have the same shape rather than
  the same field names. It would let the compiler reject a frame the protocol does
  not declare, which today only fails once a reply arrives.

That is a real language feature — declaration syntax, inference, exhaustiveness
checking, lowering in both engines, and a representation in the native FFI so it
can cross into a package. Worth doing when frames stop being the only caller.
