# `examples/uhttp/`: HTTP over a Unix socket

## What this subtree is

Two fixtures for `std::uhttp`, which speaks HTTP/1.1 over a Unix domain socket, written against the Docker Engine API at `/var/run/docker.sock`.

- *`basic/basic.jde`* makes each kind of request, reads a JSON body with `toDict`, and sends and receives `bytes` bodies.
- *`stream/stream.jde`* follows a stream that stays open, calling a handler once a line, and stops it early by returning `false`. Each line is a JSON object read with `toDict`.

## Why it was built

They show the package's whole surface in the shape a program uses it. Both need a running daemon, so `compiler/src/scripts/golden.sh` skips them and `jade check` is what holds them to the language. Until 2.0.7 they read JSON with `std::json`. `toDict` reads an object, so `basic.jde` puts the container list, a JSON array, under a key, and indexing into it makes it an array of dicts.

## Who uses it

`compiler/src/cli/check.rs` asserts both check. The package is `compiler/src/uhttp/`, and its code is in `compiler/src/backend/http.rs`. See [`examples/README.md`](../README.md) for the rules a fixture has to follow.
