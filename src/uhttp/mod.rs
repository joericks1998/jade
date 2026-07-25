//! `std/uhttp` — HTTP/1.1 over a Unix domain socket (VM surface).
//!
//! Mirrors the `std/http` API (`get`/`post`/`put`/`delete`/`head`, returning a
//! `{status, body}` dict) but connects over a Unix socket path instead of a TCP
//! host. The single string argument is a pseudo-URL:
//!
//! ```text
//! unix://<socket-path>:<request-path>
//! unix:///var/run/docker.sock:/v1.43/containers/json
//! ```
//!
//! The request/response transport core lives once in `jade_runtime::uhttpf`
//! (shared with the AOT backend's `jrt_uhttp_*` symbols); this module is the
//! VM's thin `VmValue` marshalling over it. Streaming (`uhttp.stream`) stays
//! here — it invokes a Jade handler per line and so must dispatch through the
//! VM (see `NativeFnId::UhttpStream`).

use jade_runtime::coll::DictObj;
use jade_runtime::uhttpf;
use std::io::{BufRead, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext}, vm::{NativeFnId, VmValue},
    frontend::error::{JadeError, Result, Span},
};

use crate::builtins::{BuiltinFn, Package};

const ZERO: Span = Span { line: 0, col: 0 };
const TIMEOUT: Duration = Duration::from_secs(30);

fn uhttp_err(detail: &str) -> JadeError {
    JadeError::IoError { message: format!("uhttp: {}", detail), span: ZERO }
}

pub fn uhttp_io_error(detail: &str) -> JadeError {
    uhttp_err(detail)
}

fn require_str_owned(args: &[VmValue], pos: usize, fn_name: &str) -> Result<String> {
    match args.get(pos) {
        Some(VmValue::Str(s)) => Ok(s.to_string()),
        Some(_) => Err(JadeError::TypeError { message: fn_name.to_string(), span: ZERO }),
        None    => Err(JadeError::ArityMismatch { expected: pos + 1, got: args.len(), span: ZERO }),
    }
}

pub fn extract_headers(val: Option<&VmValue>) -> Result<Vec<(String, String)>> {
    match val {
        None | Some(VmValue::Nil) => Ok(vec![]),
        Some(VmValue::Dict(map)) => {
            let mut headers = Vec::new();
            for (k, v) in map.iter() {
                match v {
                    VmValue::Str(s) => headers.push((k.clone(), s.to_string())),
                    _ => return Err(JadeError::TypeError {
                        message: "uhttp header value must be str".to_string(),
                        span: ZERO,
                    }),
                }
            }
            Ok(headers)
        }
        Some(_) => Err(JadeError::TypeError { message: "uhttp headers must be a dict".to_string(), span: ZERO }),
    }
}

fn make_response(status: i64, body: String) -> VmValue {
    let mut map = DictObj::new();
    map.insert("status".to_string(), VmValue::Int(status));
    map.insert("body".to_string(), VmValue::Str(body.into()));
    VmValue::Dict(map)
}

/// Run one request through the shared `uhttpf` core, mapping its `(status, body)`
/// into a `{status, body}` dict and its transport failure into an `IoError`.
fn execute(url: &str, method: &str, body: Option<&str>, headers: Vec<(String, String)>) -> Result<VmValue> {
    uhttpf::request(method, url, body, &headers)
        .map(|(status, body)| make_response(status, body))
        // Message shape matches the AOT path's `set_err` ("uhttp <METHOD>: <detail>").
        .map_err(|message| JadeError::IoError { message: format!("uhttp {method}: {message}"), span: ZERO })
}

fn uhttp_get(args: &[VmValue]) -> Result<VmValue> {
    if args.is_empty() || args.len() > 2 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let url = require_str_owned(args, 0, "uhttp.get")?;
    let headers = extract_headers(args.get(1))?;
    execute(&url, "GET", None, headers)
}

fn uhttp_post(args: &[VmValue]) -> Result<VmValue> {
    if args.len() < 2 || args.len() > 3 {
        return Err(JadeError::ArityMismatch { expected: 2, got: args.len(), span: ZERO });
    }
    let url  = require_str_owned(args, 0, "uhttp.post")?;
    let body = require_str_owned(args, 1, "uhttp.post")?;
    let headers = extract_headers(args.get(2))?;
    execute(&url, "POST", Some(&body), headers)
}

fn uhttp_put(args: &[VmValue]) -> Result<VmValue> {
    if args.len() < 2 || args.len() > 3 {
        return Err(JadeError::ArityMismatch { expected: 2, got: args.len(), span: ZERO });
    }
    let url  = require_str_owned(args, 0, "uhttp.put")?;
    let body = require_str_owned(args, 1, "uhttp.put")?;
    let headers = extract_headers(args.get(2))?;
    execute(&url, "PUT", Some(&body), headers)
}

fn uhttp_delete(args: &[VmValue]) -> Result<VmValue> {
    if args.is_empty() || args.len() > 2 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let url = require_str_owned(args, 0, "uhttp.delete")?;
    let headers = extract_headers(args.get(1))?;
    execute(&url, "DELETE", None, headers)
}

fn uhttp_head(args: &[VmValue]) -> Result<VmValue> {
    if args.is_empty() || args.len() > 2 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let url = require_str_owned(args, 0, "uhttp.head")?;
    let headers = extract_headers(args.get(1))?;
    execute(&url, "HEAD", None, headers)
}

// ── Streaming ──────────────────────────────────────────────────────────────
//
// Streaming endpoints (Docker `/events`, `/logs?follow=1`, image-pull progress)
// return bodies that never terminate on their own. `uhttp.stream(url, handler)`
// consumes them line-by-line: a worker thread owns the socket, decodes framing
// incrementally, and pushes each line onto an mpsc channel; the VM drains that
// channel and invokes the Jade handler per line (see `NativeFnId::UhttpStream`).
// This stays VM-only: it calls back into Jade, so it cannot be a pure AOT symbol.

/// An event emitted by the streaming worker thread.
pub enum StreamEvent {
    /// The parsed HTTP status code (sent once, before any lines).
    Status(u16),
    /// One decoded line of the response body (newline-stripped).
    Line(String),
    /// A transport/parse failure; terminates the stream.
    Error(String),
}

/// Open a streaming request over a Unix socket. Spawns a worker thread that
/// connects, decodes the body incrementally (chunked or raw), and forwards each
/// line as a `StreamEvent` until the server closes the connection or the
/// receiver is dropped. URL/parse errors surface synchronously; connect and
/// read errors surface as `StreamEvent::Error`.
pub fn open_stream(url: &str, headers: Vec<(String, String)>) -> Result<mpsc::Receiver<StreamEvent>> {
    let (sock_path, req_path) = uhttpf::parse_unix_url(url).map_err(|e| uhttp_err(&e))?;
    let (tx, rx) = mpsc::channel::<StreamEvent>(64);
    std::thread::spawn(move || {
        if let Err(e) = stream_worker(&sock_path, &req_path, &headers, &tx) {
            let _ = tx.blocking_send(StreamEvent::Error(e));
        }
    });
    Ok(rx)
}

fn stream_worker(
    sock_path: &str,
    req_path: &str,
    headers: &[(String, String)],
    tx: &mpsc::Sender<StreamEvent>,
) -> std::result::Result<(), String> {
    let stream = UnixStream::connect(sock_path).map_err(|e| e.to_string())?;
    // Deliberately no read timeout: a streaming endpoint may sit idle between
    // events for arbitrarily long. The write timeout still bounds the request.
    stream.set_write_timeout(Some(TIMEOUT)).map_err(|e| e.to_string())?;
    let mut writer = stream.try_clone().map_err(|e| e.to_string())?;
    let mut reader = std::io::BufReader::new(stream);

    // ── request (connection stays open for the stream) ───────────────────
    let mut req = format!("GET {} HTTP/1.1\r\n", req_path);
    req.push_str("Host: localhost\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{}: {}\r\n", k, v));
    }
    req.push_str("\r\n");
    writer.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    writer.flush().map_err(|e| e.to_string())?;

    // ── status line ──────────────────────────────────────────────────────
    let mut status_line = String::new();
    reader.read_line(&mut status_line).map_err(|e| e.to_string())?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| format!("malformed status line: {}", status_line.trim()))?;
    if tx.blocking_send(StreamEvent::Status(status)).is_err() {
        return Ok(()); // consumer already gone
    }

    // ── headers ──────────────────────────────────────────────────────────
    let mut chunked = false;
    loop {
        let mut header = String::new();
        let n = reader.read_line(&mut header).map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(()); // premature EOF
        }
        let header = header.trim_end();
        if header.is_empty() {
            break; // end of headers
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.trim().eq_ignore_ascii_case("transfer-encoding")
                && value.to_ascii_lowercase().contains("chunked")
            {
                chunked = true;
            }
        }
    }

    // ── body → lines ─────────────────────────────────────────────────────
    let mut line_buf: Vec<u8> = Vec::new();
    if chunked {
        loop {
            let mut size_line = String::new();
            let n = reader.read_line(&mut size_line).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            let size_str = size_line.trim().split(';').next().unwrap_or("").trim();
            if size_str.is_empty() {
                continue;
            }
            let size = usize::from_str_radix(size_str, 16)
                .map_err(|_| format!("malformed chunk size: {size_str}"))?;
            if size == 0 {
                break; // final chunk
            }
            let mut chunk = vec![0u8; size];
            reader.read_exact(&mut chunk).map_err(|e| e.to_string())?;
            let mut crlf = [0u8; 2]; // trailing CRLF after chunk data
            let _ = reader.read_exact(&mut crlf);
            if !feed_lines(&mut line_buf, &chunk, tx) {
                return Ok(());
            }
        }
    } else {
        let mut buf = [0u8; 8192];
        loop {
            let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            if !feed_lines(&mut line_buf, &buf[..n], tx) {
                return Ok(());
            }
        }
    }

    // Flush any trailing partial line (body not newline-terminated).
    if !line_buf.is_empty() {
        let line = String::from_utf8_lossy(&line_buf).into_owned();
        let _ = tx.blocking_send(StreamEvent::Line(line));
    }
    Ok(())
}

/// Split `data` on `\n`, emitting each complete line (CR-stripped) via `tx`.
/// Partial trailing bytes stay in `line_buf` for the next call. Returns `false`
/// once the receiver is dropped so the worker can stop and close the socket.
fn feed_lines(line_buf: &mut Vec<u8>, data: &[u8], tx: &mpsc::Sender<StreamEvent>) -> bool {
    for &b in data {
        if b == b'\n' {
            let mut line = std::mem::take(line_buf);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let s = String::from_utf8_lossy(&line).into_owned();
            if tx.blocking_send(StreamEvent::Line(s)).is_err() {
                return false;
            }
        } else {
            line_buf.push(b);
        }
    }
    true
}

static UHTTP_PKG_FNS: &[BuiltinFn] = &[
    BuiltinFn { name: "get",    vm_impl: uhttp_get },
    BuiltinFn { name: "post",   vm_impl: uhttp_post },
    BuiltinFn { name: "put",    vm_impl: uhttp_put },
    BuiltinFn { name: "delete", vm_impl: uhttp_delete },
    BuiltinFn { name: "head",   vm_impl: uhttp_head },
];

fn register_uhttp_pkg_types(ctx: &mut TypeContext) {
    ctx.define("uhttp".to_string(), JadeType::Unknown);
}

/// The five request functions are pure; `stream` invokes a Jade handler and so
/// must dispatch through the VM.
static UHTTP_PKG_NATIVES: &[(&str, NativeFnId)] = &[("stream", NativeFnId::UhttpStream)];

pub static UHTTP_PKG: Package = Package {
    import_name: "std/uhttp",
    global_name: "uhttp",
    fns: UHTTP_PKG_FNS,
    natives: UHTTP_PKG_NATIVES,
    register_types: register_uhttp_pkg_types,
};

#[cfg(test)]
mod tests;
