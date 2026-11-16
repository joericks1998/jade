# `examples/uhttp/`: HTTP over a Unix socket

## What this subtree is

Three fixtures for `std::uhttp`, which speaks HTTP/1.1 over a Unix domain socket. Two are written against the Docker Engine API at `/var/run/docker.sock`; the third runs against a stand-in for curl.

- *`basic/basic.jde`* makes each kind of request, reads a JSON body with `toDict`, and sends and receives `bytes` bodies.
- *`stream/stream.jde`* follows a stream that stays open, calling a handler once a line, and stops it early by returning `false`. Each line is a JSON object read with `toDict`.
- *`local/local.jde`* drives the same request path and `uhttp.stream` against a stand-in for curl that the fixture writes into `/tmp` and puts first on `PATH`. The stand-in writes each argument it was given on a line of its own, keeps what came on its standard input, and answers as a mode file says, so the fixture prints the command line each call builds: how the target `unix://<socket>:<path>` is read, headers in the order the program wrote them, a body on standard input, and every failure's message. It streams lines, an empty one and a last one without a newline included, stops curl when the handler returns `false` or raises, without waiting out the stand-in's `sleep`, and shows that a body and a streamed line are tainted. Its `.expected` was recorded from the backend version before 2.0.8 moved the package into Jade.

## Why it was built

They show the package's whole surface in the shape a program uses it. `basic` and `stream` need a running daemon, so `compiler/src/scripts/golden.sh` skips them and `jade check` is what holds them to the language; `local` needs neither a daemon nor the network, so it runs. Until 2.0.7 they read JSON with `std::json`. `toDict` reads an object, so `basic.jde` puts the container list, a JSON array, under a key, and indexing into it makes it an array of dicts.

## Who uses it

`compiler/src/cli/check.rs` asserts all three check, and `golden.sh` runs `local`. The package is Jade, `compiler/src/lib/uhttp.jde`, over `http.jde`, with starting curl, reading its output and stopping it as hooks in `compiler/src/codegen/sys/process.rs`. See [`examples/README.md`](../README.md) for the rules a fixture has to follow.
