# `src/scripts/` — development and CI gates

## What this subtree is

Two files, and they solve one problem between them: proving that Jade's two execution engines agree.

It lives under `src/` for tidiness rather than because it is source — nothing here is compiled into the crate. `jade.toml` makes the directory a Jade project root so the stub provider can reach the protocol submodule through a `[lib]` entry, which means the path in it is relative to *this* directory: `../protocol/jade` is `src/protocol/jade`.

## Why they exist

Jade has two independent execution paths — the bytecode VM (`jade run`) and the AOT LLVM backend (`jade build`) — and they have drifted three times: the build daemon resolving imports against stale code, imported `extend` methods reaching AOT but not the VM, and imported field defaults likewise. Every one of those was found by hand, because nothing ever ran the same program both ways and compared the output. The parity script does that.

The second file exists because the parity gate originally skipped everything under `examples/llm/`. A real model's output depends on the model rather than the backend, so it could not be diffed — which left the largest and most distinctive part of the language with no automated check that the two engines agree, and every backend divergence found so far had lived in exactly that kind of blind spot.

## What each file does

- **`backend-parity.sh`** — runs every example on both engines and diffs stdout. Takes an optional path to a `jade` binary. It builds the stand-in provider once, installs it in a throwaway slot, and points `JADE_PROVIDER_ACTIVE` there for the whole run. It maintains a skip list; read the header before assuming an example is covered.
- **`fake-provider.jde`** — a stand-in inference provider, answering every prompt with the reply in `JADE_FAKE_REPLY`. Built with `jade build --lib` and loaded exactly the way a released binary loads a real provider, so the gate exercises the real path. An example supplies its own reply as `responses.txt` beside the `.jde`; without one it gets a default.

This used to be `fake-jaded.py`, a stand-in *daemon* serving canned responses over a Unix socket, restarted between the VM and AOT runs so each engine read the same script from the top. The socket went away in v1.1.30, so the stub became a package — which needs no restart, since it holds no position in a script.

## Who uses it

*Used by:* `.github/workflows/ci.yml` runs `backend-parity.sh` as a required step on every pull request. Run it locally before opening one.

*Depends on:* a built `jade` binary (defaults to `./target/debug/jade`), the fixtures in `examples/`, and the `src/protocol` submodule the stub imports. Both paths are relative to the current directory, so run it from the repository root. Building the stand-in provider means the gate needs a working `jade build`, so an AOT regression fails here before it fails an example.

## Running them

```sh
cargo build
./src/scripts/backend-parity.sh                    # uses ./target/debug/jade
./src/scripts/backend-parity.sh /path/to/jade      # or a specific binary
```

To drive a Jade program against a canned reply by hand:

```sh
mkdir -p /tmp/slot && jade build src/scripts/fake-provider.jde --lib -o /tmp/slot/fake.so
JADE_PROVIDER_ACTIVE=/tmp/slot JADE_FAKE_REPLY="hello" jade run your.jde
```
