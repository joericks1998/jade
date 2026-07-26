# `examples/` — the language fixture suite

## What this subtree is

Every `.jde` file here is a test fixture, not a demo. CI type-checks all of them on every pull request and runs most of them through *both* execution engines, diffing the output. Together they are the closest thing Jade has to a specification of its own surface.

They are also the intended starting point for a language change. The established workflow is: **write the `.jde` fixture first, then make the Rust match it.** Doing it in that order tends to surface design problems before you have built on top of them.

## How the naming works

Two rules, both enforced by CI:

- `<name>.jde` must **pass** `jade check`.
- `<name>_error.jde` must **fail** `jade check`. These document programs the language deliberately rejects, so one that quietly starts passing is a CI failure too.

## What is in here

One directory per language area, each with a subdirectory per case:

`arithmatic/` (arithmetic, bitwise, unary) · `arrays/` · `assignment/` · `async/` · `closures/` · `collections/` · `control_flow/` · `dicts/` · `exceptions/` · `for_loop/` · `fs/` · `functions/` · `http/` · `imports/` · `interfaces/` · `llm/` · `llvm/` · `numbers/` · `pipe/` · `strings/` · `structs/` · `trust/` · `uhttp/`

`llm/` is worth calling out. Those fixtures do real prompt dereferences, and they are still deterministic in CI because `scripts/backend-parity.sh` starts `scripts/fake-jaded.py` as a stand-in inference daemon serving canned responses over the real wire protocol. An example supplies its own script as a `responses.txt` beside the `.jde`; without one it gets the default reply. Pointing the parity gate at these turned up a VM muting bug and an AOT segfault immediately.

## Who uses it

*Used by:* `.github/workflows/ci.yml` runs `jade check` over every fixture. `scripts/backend-parity.sh` runs each one on the VM and the AOT backend and diffs stdout. The `docs/` site draws on several of them for its examples.

*Depends on:* only the `jade` binary. Fixtures never import from the Rust tree.

## Gotchas

**Fixtures are type-checked, never run, by the `jade check` gate.** That is what lets them depend on a network or an API key at run time. The flip side is that a fixture can type-check happily while printing something its own comments contradict. If the *behavior* matters, write a Rust test as well — `run_src` and `run_src_with_mock` in `src/vm/tests.rs` are the helpers for that.

The parity script maintains a skip list for examples that cannot run identically on both engines. Check its header before assuming an example is covered.

## Running them

```sh
./target/debug/jade check examples/structs/prompt_fields/prompt_fields.jde
./target/debug/jade run   examples/strings/fstrings/fstrings.jde
./scripts/backend-parity.sh
```
