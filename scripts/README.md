# `scripts/` — development and CI gates

## What this subtree is

Two scripts, and they solve one problem between them: proving that Jade's two execution engines agree.

## Why they exist

Jade has two independent execution paths — the bytecode VM (`jade run`) and the AOT LLVM backend (`jade build`) — and they have drifted three times: the build daemon resolving imports against stale code, imported `extend` methods reaching AOT but not the VM, and imported field defaults likewise. Every one of those was found by hand, because nothing ever ran the same program both ways and compared the output. The parity script does that.

The second script exists because the parity gate originally skipped everything under `examples/llm/`. A real daemon's output depends on the model rather than the backend, so it could not be diffed — which left the largest and most distinctive part of the language with no automated check that the two engines agree, and every backend divergence found so far had lived in exactly that kind of blind spot.

## What each file does

- **`backend-parity.sh`** — runs every example on both engines and diffs stdout. Takes an optional path to a `jade` binary. It restarts the fake daemon between the VM and AOT runs of each example, because responses are consumed in order and a shared daemon would hand the second engine a different script than the first, manufacturing failures. It maintains a skip list; read the header before assuming an example is covered.
- **`fake-jaded.py`** — a stand-in for the inference daemon, serving canned responses over the real wire protocol. Both engines honour `JADE_LLM_SOCK`, which is what makes this work without either engine knowing it is talking to a stub. An example supplies its own script as `responses.txt` beside the `.jde`; without one it gets a default reply. Python 3 only, no dependencies, so it runs on a bare CI runner.

## Who uses it

*Used by:* `.github/workflows/ci.yml` runs `backend-parity.sh` as a required step on every pull request. Run it locally before opening one.

*Depends on:* a built `jade` binary (defaults to `./target/debug/jade`), the fixtures in `examples/`, and `python3`.

## Running them

```sh
cargo build
./scripts/backend-parity.sh                    # uses ./target/debug/jade
./scripts/backend-parity.sh /path/to/jade      # or a specific binary
```

To drive a Jade program against canned responses by hand, start the fake daemon and point `JADE_LLM_SOCK` at its socket.
