#!/usr/bin/env python3
"""A stand-in for the jade-tree inference daemon, serving canned responses.

Why this exists
---------------
Every LLM path in Jade was untestable. `scripts/backend-parity.sh` runs each
example on both engines and diffs stdout, but it skips all of examples/llm
because a real daemon's output depends on the model, not on the backend. That
left the largest and most distinctive part of the language with no automated
check that the two engines agree — and every backend divergence found so far
lived in exactly that kind of blind spot.

Both engines honour `JADE_LLM_SOCK` (src/llm/jaded.rs, runtime_lib/ipc/ipc.c),
so pointing them at this script makes inference deterministic and the LLM
examples parity-testable.

Protocol
--------
Request (client -> daemon):   [4-byte LE length][JSON body]
Response (daemon -> client):  a stream of [1-byte type][2-byte LE length][payload]

Frame types are from jade-protocol/src/response.rs:
    0x01 TOKEN   one chunk of generated text
    0x02 DONE    payload is tokens_used as 8 LE bytes
    0x03 ERROR   payload is a message
    0x04 META    payload is the model name
    0x05 JSON    structured payload (health reports)

A normal response is META -> TOKEN* -> DONE.

Canned responses
----------------
Responses come from a file, one per line, consumed in order and then cycled.
`\n` escapes become real newlines. Blank lines are skipped; `#` at the start of
a line is a comment. With no file, every request gets `--reply`'s value.

Responses are keyed by *example*, not by request content, on purpose. The two
engines do not send identical requests for a typed deref — the VM constrains
generation with a GBNF grammar built from the output type, while the AOT path
sends no grammar and instead validates the reply locally, retrying with a
correction prompt. A daemon that tried to infer the wanted type from the
request would therefore answer the two engines differently and manufacture
parity failures that are not backend drift. Replaying a fixed script sidesteps
that: both engines see the same bytes, so any difference in output is real.

Streaming is exercised by splitting each response into several TOKEN frames,
since `stream()` and `?p` differ in how they consume them.
"""

import argparse
import json
import os
import socket
import struct
import sys
import threading

TOKEN, DONE, ERROR, META, JSON_FRAME = 0x01, 0x02, 0x03, 0x04, 0x05

# A frame's length field is 2 bytes, so a payload cannot exceed this. Responses
# are split into token-sized chunks well under it; this is the hard ceiling.
MAX_PAYLOAD = 0xFFFF

# Default bytes per TOKEN frame. Small enough that every example sends several
# frames, so the streaming path is genuinely exercised rather than trivially
# satisfied by a single frame.
#
# Tunable because chunk size is not cosmetic: an anchor split across two frames
# is the hard case for any mute implementation, and varying this is how you tell
# "muting is broken" from "muting is broken at frame boundaries".
DEFAULT_CHUNK = 8


def frame(kind: int, payload: bytes) -> bytes:
    if len(payload) > MAX_PAYLOAD:
        raise ValueError(f"payload of {len(payload)} exceeds the 2-byte length field")
    return bytes([kind]) + struct.pack("<H", len(payload)) + payload


def read_exactly(conn: socket.socket, n: int) -> bytes | None:
    """Read exactly n bytes, or None if the peer closed first."""
    buf = b""
    while len(buf) < n:
        chunk = conn.recv(n - len(buf))
        if not chunk:
            return None
        buf += chunk
    return buf


def load_responses(path: str | None, default: str) -> list[str]:
    if not path:
        return [default]
    out = []
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            line = line.rstrip("\n")
            if not line or line.startswith("#"):
                continue
            out.append(line.replace("\\n", "\n"))
    return out or [default]


class Daemon:
    def __init__(self, responses: list[str], model: str, verbose: bool, chunk: int):
        self.responses = responses
        self.model = model
        self.verbose = verbose
        self.chunk = chunk
        self.n = 0
        self.lock = threading.Lock()

    def next_response(self) -> str:
        with self.lock:
            r = self.responses[self.n % len(self.responses)]
            self.n += 1
            return r

    def serve(self, conn: socket.socket) -> None:
        """Serve requests on one connection until the peer closes.

        Clients open a single socket and hold it for the process lifetime,
        serializing requests behind their own mutex, so a connection carries
        many requests.
        """
        while True:
            hdr = read_exactly(conn, 4)
            if hdr is None:
                return
            (req_len,) = struct.unpack("<I", hdr)
            body = read_exactly(conn, req_len)
            if body is None:
                return

            try:
                req = json.loads(body)
            except json.JSONDecodeError as e:
                conn.sendall(frame(ERROR, f"fake-jaded: malformed request: {e}".encode()))
                continue

            if self.verbose:
                summary = {k: req.get(k) for k in ("prompt", "grammar", "anchor", "stop_anchor")}
                print(f"fake-jaded <- {summary}", file=sys.stderr, flush=True)

            # Health and token-count probes want a JSON frame, not tokens.
            if req.get("stats_only") or req.get("count_only"):
                conn.sendall(frame(JSON_FRAME, json.dumps({
                    "model": self.model,
                    "tokens_used": 0,
                    "healthy": True,
                }).encode()))
                conn.sendall(frame(DONE, struct.pack("<Q", 0)))
                continue

            text = self.next_response().encode("utf-8")
            conn.sendall(frame(META, self.model.encode()))
            for i in range(0, len(text), self.chunk):
                conn.sendall(frame(TOKEN, text[i:i + self.chunk]))
            # An empty response still needs at least the DONE frame; clients
            # treat a bare META -> DONE as a valid empty generation.
            conn.sendall(frame(DONE, struct.pack("<Q", max(1, len(text) // 4))))


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("socket", help="Unix socket path to listen on (set JADE_LLM_SOCK to this)")
    ap.add_argument("--responses", help="file of canned responses, one per line, cycled")
    ap.add_argument("--reply", default="ok", help="response used when --responses is absent")
    ap.add_argument("--model", default="fake-model", help="model name reported in the META frame")
    ap.add_argument("--chunk", type=int, default=DEFAULT_CHUNK,
                    help="bytes per TOKEN frame; small values split anchors across frames")
    ap.add_argument("-v", "--verbose", action="store_true", help="log each request to stderr")
    args = ap.parse_args()

    # A stale socket file makes bind() fail with EADDRINUSE even when nothing
    # is listening, which is the usual way this script appears broken.
    try:
        os.unlink(args.socket)
    except FileNotFoundError:
        pass

    daemon = Daemon(load_responses(args.responses, args.reply), args.model,
                    args.verbose, max(1, args.chunk))

    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    srv.bind(args.socket)
    srv.listen(16)
    # Tell the parent it is safe to launch a client: the socket is bound and
    # listening, so there is no connect/bind race to sleep around.
    print("ready", flush=True)

    try:
        while True:
            conn, _ = srv.accept()
            threading.Thread(target=daemon.serve, args=(conn,), daemon=True).start()
    except KeyboardInterrupt:
        return 0
    finally:
        srv.close()
        try:
            os.unlink(args.socket)
        except FileNotFoundError:
            pass


if __name__ == "__main__":
    sys.exit(main())
