# `src/http/`: the `std::http` package

## What this subtree is

This is the `std::http` package for HTTP over TCP, as the checker sees it. It is thin on purpose. It names the package's seven functions and registers the `http` global's type, and nothing about how a request is actually made lives in this directory. Until v1.4.8 it also held the VM's surface, which validated arguments, called into `jade_runtime::httpf`, and turned the result into a `VmValue`.

## Why it is only a surface

A compiled program never calls into this directory. `jade build` emits a call to a `jrt_http_*` symbol instead. While there were two engines, request logic living here would have meant each engine held its own copy of it, which is exactly how they used to drift apart.

The transport, the status parsing, and the trust marking therefore live once in `jade_runtime::httpf`.

That split is why adding a function is a change in three places at once: its name in `HTTP_PKG`, the `jrt_*` wrapper in `httpf.rs`, and the lowering arm in `src/codegen/builtins.rs`.

Missing the third is not a silent failure, but it is a late one. `jade check` passes, and the build reports "unsupported module call". Between v1.2.2 and v1.2.5 that is what happened to `get_bytes` and `post_bytes`, and a program only discovered the gap at packaging time. `every_package_fn_lowers_in_the_chunk_backend` in `src/codegen/tests.rs` now fails on it at `cargo test`.

## What each file does

- *`mod.rs`* holds `HTTP_PKG`, the `Package` descriptor the import system reads, which names the seven functions, and the type registration for the `http` global.

## Text bodies and byte bodies

`get`, `post`, `put`, `delete`, and `head` all hand back `body` as a `str`. That loses information in two ways a caller cannot undo. Invalid UTF-8 becomes a replacement character, and the text stops at the first NUL byte, because a Jade string is NUL-terminated.

`get_bytes` and `post_bytes` exist for the responses that break on both counts, such as an image, an audio frame, or a compressed stream. Their `body` is a `bytes` value. The distinction is deliberate rather than automatic. A program says which one it wants, and nothing guesses from a `Content-Type` header.

Every response body is *tainted*, whichever spelling produced it. It came from outside the program, so `sh.exec` and the other sinks refuse it until it is explicitly trusted.

## Who uses it

The import system, in `src/project/`, resolves `use std::http` to `HTTP_PKG`. The AOT backend never calls into this directory. See `src/codegen/builtins.rs` for its lowering, and `src/runtime/src/httpf.rs` for the implementation.

`src/uhttp/` is this package again, over a Unix domain socket. The two are meant to stay mirrors of each other. Until v1.4.8 `http_and_uhttp_expose_the_same_functions` in this directory's `tests.rs` failed if they stopped being. That file went with the VM in v2, so nothing checks it now.
