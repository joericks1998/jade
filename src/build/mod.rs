//! Build client — hands a type-inferred program (TIR) to the Jade build daemon
//! over a Unix domain socket and receives a native binary.
//!
//! Native code generation + linking now live in the build daemon, alongside the
//! C runtime that compiled binaries link against. This repo owns the language
//! frontend (lex → parse → type-infer → TIR); the daemon owns everything
//! downstream (import resolution → LLVM → object → link → binary).
//!
//! The transport mirrors the inference backend (`llm/jade_os.rs`): a
//! length-prefixed JSON request followed by a stream of typed response frames.
//!
//! Wire contract (confirm against the daemon's `build` op):
//!   request:  `[u32 LE len][ {"op":"build","emit":..,"out":..,"source_path":..,"tir":{..}} ]`
//!   response: `[u8 type][u16 LE len][payload]` frames —
//!               0x01 TOKEN (utf8 LLVM IR, when emit=ir), 0x02 DONE,
//!               0x03 ERROR (utf8), 0x04 META (utf8, informational).

#![cfg(unix)]

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use crate::compiler::tir::TProgram;

#[cfg(test)]
mod tests;

fn build_sock_path() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_owned());
    format!("{home}/.jade/build.sock")
}

/// Send a build request to the daemon. On success the daemon has written the
/// native binary to `out`; for `emit_ir`, the LLVM IR is streamed to stdout.
///
/// The daemon receives the *main file's* TIR plus its `source_path` and resolves
/// imports itself (reading sibling `.jde` files relative to `source_path`).
pub fn build(
    program: &TProgram,
    source_path: &Path,
    out: &Path,
    emit_ir: bool,
) -> Result<(), String> {
    let sock_path = build_sock_path();
    let mut stream = UnixStream::connect(&sock_path).map_err(|e| {
        if Path::new(&sock_path).exists() {
            // Socket is there but nothing is listening — the daemon is down.
            format!(
                "the Jade build daemon at {} isn't responding — is it running? ({e})",
                sock_path
            )
        } else {
            // No socket at all — native compilation isn't available here.
            "native compilation (`jade build`) needs the Jade build daemon, which \
             isn't present on this platform yet — standalone Mac/Linux runtimes \
             aren't released.\n       \
             To run this program now, use the VM instead:  jade run <file.jde>"
                .to_owned()
        }
    })?;

    let payload = encode_request(program, source_path, out, emit_ir)?;
    stream
        .write_all(&payload)
        .map_err(|e| format!("write to {} failed: {e}", sock_path))?;

    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        match decode_frame(&buf) {
            FrameResult::Token(text, consumed) => {
                if emit_ir {
                    print!("{text}");
                }
                buf.drain(..consumed);
            }
            FrameResult::Meta(consumed) => {
                buf.drain(..consumed);
            }
            FrameResult::Done(consumed) => {
                buf.drain(..consumed);
                return Ok(());
            }
            FrameResult::Error(msg, consumed) => {
                buf.drain(..consumed);
                return Err(msg);
            }
            FrameResult::Incomplete => {
                let n = stream
                    .read(&mut tmp)
                    .map_err(|e| format!("read from {} failed: {e}", sock_path))?;
                if n == 0 {
                    return Err("build daemon closed the connection before DONE".to_owned());
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            FrameResult::UnknownType(t) => {
                return Err(format!("unknown frame type from build daemon: {t:#04x}"));
            }
        }
    }
}

fn encode_request(
    program: &TProgram,
    source_path: &Path,
    out: &Path,
    emit_ir: bool,
) -> Result<Vec<u8>, String> {
    let tir =
        serde_json::to_value(program).map_err(|e| format!("failed to serialize TIR: {e}"))?;
    let req = serde_json::json!({
        "op": "build",
        "emit": if emit_ir { "ir" } else { "binary" },
        "out": out.to_string_lossy(),
        "source_path": source_path.to_string_lossy(),
        "target": serde_json::Value::Null,
        "tir": tir,
    });
    let json =
        serde_json::to_vec(&req).map_err(|e| format!("failed to encode build request: {e}"))?;
    let len = json.len() as u32;
    let mut payload = Vec::with_capacity(4 + json.len());
    payload.extend_from_slice(&len.to_le_bytes());
    payload.extend_from_slice(&json);
    Ok(payload)
}

// ── Response frames ───────────────────────────────────────────────────────────
// [u8 type][u16 LE len][payload], matching the inference daemon framing.

enum FrameResult {
    Token(String, usize),
    Meta(usize),
    Done(usize),
    Error(String, usize),
    Incomplete,
    UnknownType(u8),
}

fn decode_frame(buf: &[u8]) -> FrameResult {
    if buf.len() < 3 {
        return FrameResult::Incomplete;
    }
    let frame_type = buf[0];
    let payload_len = u16::from_le_bytes([buf[1], buf[2]]) as usize;
    if buf.len() < 3 + payload_len {
        return FrameResult::Incomplete;
    }
    let payload = &buf[3..3 + payload_len];
    let consumed = 3 + payload_len;
    match frame_type {
        0x01 => match std::str::from_utf8(payload) {
            Ok(s) => FrameResult::Token(s.to_owned(), consumed),
            Err(_) => FrameResult::Error(
                "build daemon sent invalid UTF-8 in TOKEN frame".to_owned(),
                consumed,
            ),
        },
        0x02 => FrameResult::Done(consumed),
        0x03 => match std::str::from_utf8(payload) {
            Ok(s) => FrameResult::Error(s.to_owned(), consumed),
            Err(_) => FrameResult::Error(
                "build daemon sent invalid UTF-8 in ERROR frame".to_owned(),
                consumed,
            ),
        },
        0x04 => match std::str::from_utf8(payload) {
            Ok(_) => FrameResult::Meta(consumed),
            Err(_) => FrameResult::Error(
                "build daemon sent invalid UTF-8 in META frame".to_owned(),
                consumed,
            ),
        },
        other => FrameResult::UnknownType(other),
    }
}
