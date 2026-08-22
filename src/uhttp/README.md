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

That framing lives in `jade_runtime::uhttpf` and is shared with the compiled backend. This directory is only the VM's marshalling over it, exactly as `src/http/` is.

## What each file does

- *`mod.rs`* holds the argument validation, the `{status, body}` dict, the streaming pump, and the `Package` descriptor.
- *`tests.rs`* covers the branches that fire before any socket I/O: header extraction, argument counts, argument types, and a malformed URL.

## Streaming

`uhttp.stream(url, handler)` is the one function `std::http` has no counterpart for. A Docker `/events` or `/logs?follow=1` response never ends on its own, so it cannot use the ordinary request path, which reads to end of file and returns one body.

The reader itself lives in the shared crate. What stays here is the async pump. A worker thread owns the reader and pushes each line onto an mpsc channel, and the VM drains that channel and calls the Jade handler once per line. The compiled path drives the same reader inline, from `jrt_uhttp_stream` in `runtime_aot/common.c`.

This was VM-only once, on the reasoning that calling back into Jade meant it could not be a plain AOT symbol. That reasoning was wrong, because `array.map` already calls a Jade function from compiled code. The cost was a builtin that passed `jade check` and failed at `jade build`, which a program only discovers when it tries to ship.

## Bodies that are not text

`get`, `post`, `put`, `delete`, and `head` all read `body` as a `str`, which is UTF-8 and NUL-terminated. A daemon answering with audio, an image, or a gzip stream breaks both rules. Invalid sequences become `�`, and everything from the first NUL byte is dropped.

`get_bytes` and `post_bytes` are the spellings for those cases, and their `body` is a `bytes` value. They arrived in v1.2.5, three releases after `std::http` got the same pair. The gap stayed invisible because nothing compared the two packages' function tables. Something does now, in `src/http/tests.rs`.

A response body is *tainted* either way, because it came off a socket the program does not control.

## Who uses it

The import system resolves `use std::uhttp` to `UHTTP_PKG`. The compiled backend never reads this directory. Its half is in `src/codegen/builtins.rs`, and the shared transport is `src/runtime/src/uhttpf.rs`.
