# `src/http/`: the `std::http` package

## What this subtree is

This is the `std::http` package for HTTP over TCP, as the checker sees it. It is thin on purpose. It names the package's seven functions and registers the `http` global's type, and nothing about how a request is actually made lives in this directory. Until v1.4.8 it also held the VM's surface, which validated arguments, called into v1's runtime, and turned the result into a `VmValue`.

## Why it is only a surface

A compiled program never calls into this directory. `jade build` emits the request code into the program instead. While there were two engines, request logic living here would have meant each engine held its own copy of it, which is exactly how they used to drift apart.

The transport, the status parsing, and the trust marking therefore live once in `src/backend/http.rs`, which runs `curl` as v1 did, so HTTPS works with no TLS library in the binary.

That split is why adding a function is a change in several places at once: its name in `HTTP_PKG`, its type in `typechecker/infer/builtins.rs`, its `HttpFn` arm in `mir/packages.rs`, and its code in `src/backend/http.rs`.

Missing a later step used to be a late failure. Between v1.2.2 and v1.2.5 that is what happened to `get_bytes` and `post_bytes`: `jade check` passed, the build reported "unsupported module call", and a program only discovered the gap at packaging time. `jade check` now runs MIR lowering too, so a function with no lowering fails at `check` instead.

## What each file does

- *`mod.rs`* holds `HTTP_PKG`, the `Package` descriptor the import system reads, which names the seven functions, and the type registration for the `http` global.

## Text bodies and byte bodies

`get`, `post`, `put`, `delete`, and `head` all hand back `body` as a `str`. That loses information in two ways a caller cannot undo. Invalid UTF-8 becomes a replacement character, and the text stops at the first NUL byte, because a Jade string is NUL-terminated.

`get_bytes` and `post_bytes` exist for the responses that break on both counts, such as an image, an audio frame, or a compressed stream. Their `body` is a `bytes` value. The distinction is deliberate rather than automatic. A program says which one it wants, and nothing guesses from a `Content-Type` header.

Every response body is *tainted*, whichever spelling produced it. It came from outside the program, so `sh.exec` and the other sinks refuse it until it is explicitly trusted.

## Who uses it

The import system, in `src/project/`, resolves `use std::http` to `HTTP_PKG`. The compiled backend never calls into this directory. See `mir/packages.rs` for its lowering, and `src/backend/http.rs` for the implementation.

`src/uhttp/` is this package again, over a Unix domain socket. The two are meant to stay mirrors of each other. Until v1.4.8 `http_and_uhttp_expose_the_same_functions` in this directory's `tests.rs` failed if they stopped being. That file went with the VM in v2, so nothing checks it now.
