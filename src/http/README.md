# `src/http/`: the `std::http` package

## What this subtree is

This is the VM's surface for HTTP over TCP. It is thin on purpose. Every function here validates its arguments, calls into `jade_runtime::httpf`, and turns the result into a `VmValue`. Nothing about how a request is actually made lives in this directory.

## Why it is only a surface

The compiled backend cannot call these functions. `jade build` emits a call to a `jrt_http_*` symbol instead. So if the request logic lived here, the two engines would each hold their own copy of it, which is exactly how they used to drift apart.

The transport, the status parsing, and the trust marking therefore live once in `jade_runtime::httpf`. This directory only decides what a `VmValue` looks like on either side of it.

That split is why a change here is usually a change in three places at once: the function itself, the `jrt_*` wrapper in `httpf.rs`, and the lowering arm in `src/codegen/builtins.rs`.

Missing the third is not a silent failure, but it is a late one. `jade check` passes, and `jade build` reports "unsupported module call", so a program only discovers the gap at packaging time. That is what happened to `get_bytes` and `post_bytes` between v1.2.2 and v1.2.5.

## What each file does

- *`mod.rs`* holds the argument validation, the `{status, body}` dict, and the `Package` descriptor the import system reads.
- *`tests.rs`* covers the argument-count and type branches, all of which fire before any socket I/O, so the suite makes no network calls. It also checks that this package and `std::uhttp` expose the same function names.

## Text bodies and byte bodies

`get`, `post`, `put`, `delete`, and `head` all hand back `body` as a `str`. That loses information in two ways a caller cannot undo. Invalid UTF-8 becomes a replacement character, and the text stops at the first NUL byte, because a Jade string is NUL-terminated.

`get_bytes` and `post_bytes` exist for the responses that break on both counts, such as an image, an audio frame, or a compressed stream. Their `body` is a `bytes` value. The distinction is deliberate rather than automatic. A program says which one it wants, and nothing guesses from a `Content-Type` header.

Every response body is *tainted*, whichever spelling produced it. It came from outside the program, so `sh.exec` and the other sinks refuse it until it is explicitly trusted.

## Who uses it

The import system, in `src/project/`, resolves `use std::http` to `HTTP_PKG`. The AOT backend never touches this directory. See `src/codegen/builtins.rs` for its half, and `src/runtime/src/httpf.rs` for the part the two share.

`src/uhttp/` is this package again, over a Unix domain socket. The two are meant to stay mirrors of each other, and `http_and_uhttp_expose_the_same_functions` in `tests.rs` fails if they stop being.
