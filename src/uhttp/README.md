# `src/uhttp/` — the `std::uhttp` package

## What this subtree is

`std::http` again, over a Unix domain socket instead of a TCP host. Same
functions, same `{status, body}` dict, one different argument: the target is a
pseudo-URL naming a socket path and a request path together.

```
unix://<socket-path>:<request-path>
unix:///var/run/docker.sock:/v1.43/containers/json
```

The socket path runs up to the **first** `:` after the scheme, so colons inside a
query string survive. A missing request path defaults to `/`.

## Why it exists separately

Local daemons — Docker, systemd, a sound server — speak HTTP but do not listen on
a port. Nothing in the dependency tree does HTTP over a Unix socket, so the
transport here is hand-framed: the request head is built as text, the response is
read to EOF, and framing honors `Content-Length`, `Transfer-Encoding: chunked`,
and `Connection: close`. That framing lives in `jade_runtime::uhttpf` and is
shared with the compiled backend; this directory is only the VM's marshalling
over it, exactly as `src/http/` is.

## What each file does

- **`mod.rs`** — argument validation, the `{status, body}` dict, the streaming
  pump, and the `Package` descriptor.
- **`tests.rs`** — the branches that fire before any socket I/O: header
  extraction, arity, argument types, and a malformed URL.

## Streaming

`uhttp.stream(url, handler)` is the one function `std::http` has no counterpart
for. A Docker `/events` or `/logs?follow=1` response never ends on its own, so it
cannot use the request path, which reads to EOF and returns one body.

The reader itself is in the shared crate. What stays here is the async pump: a
worker thread owns the reader and pushes each line onto an mpsc channel, and the
VM drains it and calls the Jade handler per line. The compiled path drives the
same reader inline from `jrt_uhttp_stream` in `runtime_aot/common.c`.

It was VM-only once, on the reasoning that calling back into Jade meant it could
not be a pure AOT symbol. That was wrong — `array.map` already calls a Jade
function from compiled code — and the cost was a builtin that passed `jade check`
and failed at `jade build`, which a program only discovers when it tries to ship.

## Bodies that are not text

`get`/`post`/`put`/`delete`/`head` read `body` as a `str`, which is UTF-8 and
NUL-terminated. A daemon answering with audio, an image, or a gzip stream breaks
both rules: invalid sequences become `�`, and everything from the first NUL is
dropped.

`get_bytes` and `post_bytes` are the spellings for those; their `body` is a
`bytes` value. They arrived in v1.2.5, three releases after `std::http` got the
same pair, and the gap was invisible because nothing compared the two packages'
function tables. Something does now, in `src/http/tests.rs`.

A response body is **TAINTED** either way — it came off a socket the program does
not control.

## Who uses this

The import system resolves `use std::uhttp` to `UHTTP_PKG`. The compiled backend
never reads this directory; its half is in `src/aot/lower/builtins.rs`, and the
shared transport is `src/runtime/src/uhttpf.rs`.
