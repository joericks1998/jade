# `design/` — design notes

## What this subtree is

Written-up design decisions for changes big enough that the reasoning would not survive as code comments. Each note states the problem, the alternatives that were weighed, and what was chosen — the context you want before changing something rather than after.

These are not proposals waiting on approval. Each one carries a status line saying whether it shipped and in which version, so a note can be read as history as well as design.

## Why it exists separately from the code

Most of Jade's rationale lives in module headers, and that is the right home for anything scoped to one file. A design note is for a decision that spans several modules and both engines, where no single file is the natural place to explain it. The provider-package design is the example: it touches the CLI registry, the runtime slot resolver, the VM backend, the AOT loader, and the shape of a Jade package, and none of those five is where you would go looking for the reasoning.

## What each file does

- **`provider-packages.md`** — cloud inference without a daemon. Shipped: VM path in v1.1.24, AOT path in v1.1.25. Explains what a provider package is (a compiled Jade `--lib` exporting `infer(request) -> [Frame]` and optionally `configure(opts)`), why the language contains no vendor names at all, and how the active slot works. Read it before touching `src/providers/`, `src/llm/provider_backend.rs`, or `jade_runtime::provider`.

## Who uses it

*Read by:* anyone working in `src/providers/`, `src/llm/`, or `src/runtime/src/provider/`. Those directories' own READMEs point back here.

*Depends on:* nothing. These are prose.

## Conventions

Start a note with a `Status:` line — shipped and in which version, or in progress. Say *why* before *what*: the alternatives that were rejected are usually the more useful half. Update the status when the work lands rather than deleting the note, so the reasoning outlives the change.
