# `src/http/` — the `std::http` package

## What this subtree is

The VM's surface for HTTP over TCP. It is thin on purpose: every function here
validates its arguments, calls into `jade_runtime::httpf`, and turns the result
into a `VmValue`. Nothing about how a request is actually made lives in this
directory.

## Why it is only a surface

The compiled backend cannot call these functions. `jade build` emits a call to a
`jrt_http_*` symbol instead, so if the request logic lived here the two engines
would each have their own copy of it — which is exactly how they used to drift.
The transport, the status parsing, and the trust marking therefore live once in
`jade_runtime::httpf`, and this directory only decides what a `VmValue` looks
like on either side of it.

That split is why a change here is usually a change in three places at once: the
function below, the `jrt_*` wrapper in `httpf.rs`, and the lowering arm in
`src/codegen/builtins.rs`. Missing the third is not a silent failure but it is
a late one — `jade check` passes and `jade build` reports "unsupported module
call", so a program discovers it at packaging time. That is what happened to
`get_bytes` and `post_bytes` between v1.2.2 and v1.2.5.

## What each file does

- **`mod.rs`** — argument validation, the `{status, body}` dict, and the
  `Package` descriptor the import system reads.
- **`tests.rs`** — arity and type branches (all of which fire before any socket
  I/O, so the suite makes no network calls), plus a check that this package and
  `std::uhttp` expose the same function names.

## Text bodies and byte bodies

`get`/`post`/`put`/`delete`/`head` hand back `body` as a `str`. That is lossy in
two ways a caller cannot undo: invalid UTF-8 becomes a replacement character,
and the text stops at the first NUL, because a Jade string is NUL-terminated.

`get_bytes` and `post_bytes` exist for the responses that break on both counts —
an image, an audio frame, a compressed stream. Their `body` is a `bytes` value.
The distinction is deliberate rather than automatic: a program says which one it
wants, and nothing guesses from a `Content-Type`.

Every response body is **TAINTED** whichever spelling produced it. It came from
outside the program, so `sh.exec` and friends refuse it until it is explicitly
trusted.

## Who uses this

The import system (`src/project/`) resolves `use std::http` to `HTTP_PKG`. The
AOT backend never touches this directory; see `src/codegen/builtins.rs` for
its half, and `src/runtime/src/httpf.rs` for the part they share.

`src/uhttp/` is this package again over a Unix domain socket, and the two are
meant to stay a mirror of each other — `http_and_uhttp_expose_the_same_functions`
in `tests.rs` fails if they stop being one.
