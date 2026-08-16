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
//! The transport core lives once in `jade_runtime::uhttpf` (shared with the AOT
//! backend's `jrt_uhttp_*` symbols); this module is the VM's thin `VmValue`
//! marshalling over it. That includes streaming: `uhttpf::Stream` does the
//! socket reading and line framing for both engines, and each drives it its own
//! way — the VM pumps it from a worker thread into the channel below, while the
//! compiled path drives it inline from `jrt_uhttp_stream`.

use jade_runtime::coll::DictObj;
use jade_runtime::uhttpf;

use tokio::sync::mpsc;

use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext},
    frontend::error::{JadeError, Result, Span},
    vm::{NativeFnId, VmValue},
};

use crate::builtins::{BuiltinFn, Package};

const ZERO: Span = Span { line: 0, col: 0 };

/// A streaming failure. Named `uhttp stream:` rather than the bare `uhttp:`,
/// matching the request path's `uhttp GET:` / `uhttp POST:` and the compiled
/// backend's wording for the same failure (`jrt_uhttp_stream` in
/// `runtime_aot/common.c`), so a caught message reads the same on both engines.
pub fn uhttp_io_error(detail: &str) -> JadeError {
    JadeError::IoError { message: format!("uhttp stream: {}", detail), span: ZERO }
}

fn require_str_owned(args: &[VmValue], pos: usize, fn_name: &str) -> Result<String> {
    match args.get(pos) {
        Some(VmValue::Str(s)) => Ok(s.to_string()),
        Some(_) => Err(JadeError::TypeError { message: fn_name.to_string(), span: ZERO }),
        None => Err(JadeError::ArityMismatch { expected: pos + 1, got: args.len(), span: ZERO }),
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
                    _ => {
                        return Err(JadeError::TypeError {
                            message: "uhttp header value must be str".to_string(),
                            span: ZERO,
                        });
                    }
                }
            }
            Ok(headers)
        }
        Some(_) => Err(JadeError::TypeError {
            message: "uhttp headers must be a dict".to_string(),
            span: ZERO,
        }),
    }
}

fn make_response(status: i64, body: String) -> VmValue {
    let mut map = DictObj::new();
    map.insert("status".to_string(), VmValue::Int(status));
    map.insert("body".to_string(), VmValue::Str(body.into()));
    VmValue::dict(map)
}

/// The same dict with an undecoded `bytes` body.
///
/// TAINTED for the reason the string body is: it came off a socket. Same shape
/// as [`make_response`], so `.status` reads identically either way.
fn make_bytes_response(status: i64, body: Vec<u8>) -> VmValue {
    let mut map = DictObj::new();
    map.insert("status".to_string(), VmValue::Int(status));
    map.insert(
        "body".to_string(),
        VmValue::Bytes(std::sync::Arc::new(jade_runtime::bytesf::BytesObj::new(
            body,
            jade_runtime::trust::TAINTED,
        ))),
    );
    VmValue::dict(map)
}

/// Run one request through the shared `uhttpf` core, mapping its `(status, body)`
/// into a `{status, body}` dict and its transport failure into an `IoError`.
fn execute(
    url: &str,
    method: &str,
    body: Option<&str>,
    headers: Vec<(String, String)>,
) -> Result<VmValue> {
    uhttpf::request(method, url, body, &headers)
        .map(|(status, body)| make_response(status, body))
        // Message shape matches the AOT path's `set_err` ("uhttp <METHOD>: <detail>").
        .map_err(|message| JadeError::IoError {
            message: format!("uhttp {method}: {message}"),
            span: ZERO,
        })
}

/// [`execute`] for the byte-bodied pair. Same core, same error wording — only
/// the body's type differs.
fn execute_bytes(
    url: &str,
    method: &str,
    body: Option<&[u8]>,
    headers: Vec<(String, String)>,
) -> Result<VmValue> {
    uhttpf::request_bytes(method, url, body, &headers)
        .map(|(status, body)| make_bytes_response(status, body))
        .map_err(|message| JadeError::IoError {
            message: format!("uhttp {method}: {message}"),
            span: ZERO,
        })
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
    let url = require_str_owned(args, 0, "uhttp.post")?;
    let body = require_str_owned(args, 1, "uhttp.post")?;
    let headers = extract_headers(args.get(2))?;
    execute(&url, "POST", Some(&body), headers)
}

/// `uhttp.get_bytes(url[, headers])` — a response whose body is not decoded.
///
/// `uhttp.get` runs the reply through a lossy UTF-8 decode, which mangles a WAV
/// frame or a PNG as surely over a Unix socket as over TCP. This is the one to
/// reach for when a daemon answers with something that is not text.
fn uhttp_get_bytes(args: &[VmValue]) -> Result<VmValue> {
    if args.is_empty() || args.len() > 2 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let url = require_str_owned(args, 0, "uhttp.get_bytes")?;
    let headers = extract_headers(args.get(1))?;
    execute_bytes(&url, "GET", None, headers)
}

/// `uhttp.post_bytes(url, body[, headers])` — send raw octets.
fn uhttp_post_bytes(args: &[VmValue]) -> Result<VmValue> {
    if args.len() < 2 || args.len() > 3 {
        return Err(JadeError::ArityMismatch { expected: 2, got: args.len(), span: ZERO });
    }
    let url = require_str_owned(args, 0, "uhttp.post_bytes")?;
    let body = match args.get(1) {
        Some(VmValue::Bytes(b)) => b.as_slice().to_vec(),
        Some(other) => {
            return Err(JadeError::TypeError {
                message: format!(
                    "uhttp.post_bytes expects bytes, got {}",
                    crate::vm::value_type_name(other)
                ),
                span: ZERO,
            });
        }
        None => return Err(JadeError::ArityMismatch { expected: 2, got: args.len(), span: ZERO }),
    };
    let headers = extract_headers(args.get(2))?;
    execute_bytes(&url, "POST", Some(&body), headers)
}

fn uhttp_put(args: &[VmValue]) -> Result<VmValue> {
    if args.len() < 2 || args.len() > 3 {
        return Err(JadeError::ArityMismatch { expected: 2, got: args.len(), span: ZERO });
    }
    let url = require_str_owned(args, 0, "uhttp.put")?;
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
// consumes them line-by-line: a worker thread owns a `uhttpf::Stream` and pushes
// each line onto an mpsc channel; the VM drains that channel and invokes the
// Jade handler per line (see `NativeFnId::UhttpStream`).
//
// This used to be VM-only, on the reasoning that calling back into Jade meant it
// "cannot be a pure AOT symbol". That was wrong — `array.map` already calls a
// Jade function from compiled code — and the cost of the mistake was a builtin
// that passed `jade check` and failed at `jade build`, which a program only
// discovers at packaging time. The reader now lives in the shared crate; what
// stays here is the async pump, which is a VM concern and genuinely not shared.

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
pub fn open_stream(
    url: &str,
    headers: Vec<(String, String)>,
) -> Result<mpsc::Receiver<StreamEvent>> {
    // Parse before spawning so a malformed URL is a synchronous error, not an
    // event the caller has to drain the channel to discover.
    uhttpf::parse_unix_url(url).map_err(|e| uhttp_io_error(&e))?;
    let (tx, rx) = mpsc::channel::<StreamEvent>(64);
    let url = url.to_string();
    std::thread::spawn(move || {
        let mut stream = match uhttpf::Stream::open(&url, &headers) {
            Ok(s) => s,
            Err(e) => {
                let _ = tx.blocking_send(StreamEvent::Error(e));
                return;
            }
        };
        if tx.blocking_send(StreamEvent::Status(stream.status() as u16)).is_err() {
            return; // consumer already gone
        }
        loop {
            match stream.next_line() {
                // A send failure means the receiver was dropped: stop and let
                // `stream` drop too, which closes the socket.
                Ok(Some(line)) => {
                    if tx.blocking_send(StreamEvent::Line(line)).is_err() {
                        return;
                    }
                }
                Ok(None) => return,
                Err(e) => {
                    let _ = tx.blocking_send(StreamEvent::Error(e));
                    return;
                }
            }
        }
    });
    Ok(rx)
}

static UHTTP_PKG_FNS: &[BuiltinFn] = &[
    BuiltinFn { name: "get", vm_impl: uhttp_get },
    BuiltinFn { name: "post", vm_impl: uhttp_post },
    BuiltinFn { name: "put", vm_impl: uhttp_put },
    BuiltinFn { name: "delete", vm_impl: uhttp_delete },
    BuiltinFn { name: "head", vm_impl: uhttp_head },
    BuiltinFn { name: "get_bytes", vm_impl: uhttp_get_bytes },
    BuiltinFn { name: "post_bytes", vm_impl: uhttp_post_bytes },
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
