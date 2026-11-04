# `src/uhttp/`: the `std::uhttp` package

## What this subtree is

This is `std::http` again, over a Unix domain socket rather than a TCP host. It has the same functions and the same `{status, body}` dict. One argument differs: the target is a pseudo-URL naming a socket path and a request path together.

```
unix://<socket-path>:<request-path>
unix:///var/run/docker.sock:/v1.43/containers/json
```

The socket path runs up to the *first* `:` after the scheme, so colons inside a query string survive. A missing request path defaults to `/`.

## Why it exists separately

Local daemons speak HTTP but do not listen on a port. Docker, systemd, and a sound server are all examples. Nothing in the dependency tree does HTTP over a Unix socket, so the transport here is framed by hand. The request head is built as text, the response is read to end of file, and the framing honors `Content-Length`, `Transfer-Encoding: chunked`, and `Connection: close`.

That framing lives in `jade_runtime::uhttpf`, which the compiled backend calls. This directory holds only the package's function names and type registration, exactly as `src/http/` does. Until v1.4.8 it held the VM's marshalling over the same framing.

## What each file does

- *`mod.rs`* holds `UHTTP_PKG`, the `Package` descriptor that names the eight functions, and the type registration for the `uhttp` global.

## Streaming

`uhttp.stream(url, handler)` is the one function `std::http` has no counterpart for. A Docker `/events` or `/logs?follow=1` response never ends on its own, so it cannot use the ordinary request path, which reads to end of file and returns one body.

The reader itself lives in the shared crate. The compiled path drives it inline, from `jrt_uhttp_stream` in `runtime_aot/common.c`, and calls the Jade handler once per line. Until v1.4.8 this directory also held the VM's async pump, where a worker thread owned the reader and pushed each line onto an mpsc channel for the VM to drain.

This was VM-only once, on the reasoning that calling back into Jade meant it could not be a plain AOT symbol. That reasoning was wrong, because `array.map` already calls a Jade function from compiled code. The cost was a builtin that passed `jade check` and failed at `jade build`, which a program only discovers when it tries to ship.

## Bodies that are not text

`get`, `post`, `put`, `delete`, and `head` all read `body` as a `str`, which is UTF-8 and NUL-terminated. A daemon answering with audio, an image, or a gzip stream breaks both rules. Invalid sequences become `�`, and everything from the first NUL byte is dropped.

`get_bytes` and `post_bytes` are the spellings for those cases, and their `body` is a `bytes` value. They arrived in v1.2.5, three releases after `std::http` got the same pair. The gap stayed invisible because nothing compared the two packages' function tables. From then until v2 a test in `src/http/tests.rs` did. That file went with the VM, so nothing compares them now.

A response body is *tainted* either way, because it came off a socket the program does not control.

## Who uses it

The import system resolves `use std::uhttp` to `UHTTP_PKG`. The compiled backend never calls into this directory. Its lowering is in `src/codegen/builtins.rs`, and the transport is `src/runtime/src/uhttpf.rs`.
