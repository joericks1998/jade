# `examples/http/`: `std::http`

## What this subtree is

The fixtures for `std::http`, HTTP over TCP through `curl`. Since 2.0.8 the package is written in Jade, `compiler/src/lib/http.jde`, with starting curl and reading what it writes as hooks in `compiler/src/codegen/sys/process.rs`.

- *`basic/`* makes each kind of request against `httpbin.org` and reads each response's status, with headers on one. It needs the network.
- *`local/`* drives the same request path against a stand-in for curl that the fixture writes into `/tmp` and puts first on `PATH`. The stand-in writes each argument it was given on a line of its own, keeps what came on its standard input, and answers as a mode file says, so the fixture prints the command line each method builds, headers in the order the program wrote them, a body on standard input, a text body cut at its first NUL and a bytes body whole, bad UTF-8 replaced, a status read from the last mark so a body cannot fake it, no status read as 0, a 4xx as an ordinary answer, and every transport failure's message, a missing curl included. A body is tainted, whichever spelling produced it.

## Why it was built

A request's command line and its reply's parsing are where `std::http`'s logic is, and until 2.0.8 no fixture ran them, because the only one needed the network. `local/` runs them with nothing outside the machine, and its `.expected` was recorded from the backend version before the move, so it pins that the Jade version builds the same command line and reads the same answers.

## Who uses it

`compiler/src/scripts/golden.sh` skips `basic/`, which needs the network, and runs `local/`; `jade check` holds both to the language. `examples/uhttp/` is this package over a Unix socket.
