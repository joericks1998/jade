//! `std/uhttp` — HTTP/1.1 over a Unix domain socket.
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
//! Unlike `std/http` (reqwest), the transport is hand-framed HTTP/1.1 written
//! directly onto a `std::os::unix::net::UnixStream` — there is no Unix-socket
//! HTTP client in the dependency tree. Response framing honors `Content-Length`,
//! `Transfer-Encoding: chunked` (de-chunked), and read-to-EOF on
//! `Connection: close`. True streaming endpoints (bodies that never terminate)
//! are out of scope. Unix-only (`#![cfg(unix)]`).

#![cfg(unix)]

use std::collections::HashMap;
use std::io::{BufRead, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext, vm::{NativeFnId, VmValue}},
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
        Some(VmValue::Str(s)) => Ok(s.clone()),
        Some(_) => Err(JadeError::TypeError { message: fn_name.to_string(), span: ZERO }),
        None    => Err(JadeError::ArityMismatch { expected: pos + 1, got: args.len(), span: ZERO }),
    }
}

pub fn extract_headers(val: Option<&VmValue>) -> Result<Vec<(String, String)>> {
    match val {
        None | Some(VmValue::Nil) => Ok(vec![]),
        Some(VmValue::Dict(map)) => {
            let mut headers = Vec::new();
            for (k, v) in map {
                match v {
                    VmValue::Str(s) => headers.push((k.clone(), s.clone())),
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

fn make_response(status: u16, body: String) -> VmValue {
    let mut map = HashMap::new();
    map.insert("status".to_string(), VmValue::Int(status as i64));
    map.insert("body".to_string(), VmValue::Str(body));
    VmValue::Dict(map)
}

/// Split `unix://<socket-path>:<request-path>` into its two parts.
///
/// Requires the `unix://` scheme. The socket path is everything up to the first
/// `:` after the scheme; the request path is the remainder (defaulting to `/`
/// when absent). Splitting on the *first* `:` keeps colons inside a request
/// path (query strings, matrix params) intact, at the cost of not supporting a
/// `:` inside the socket path itself.
fn parse_unix_url(url: &str) -> Result<(String, String)> {
    let rest = url
        .strip_prefix("unix://")
        .ok_or_else(|| uhttp_err("url must start with unix://"))?;
    match rest.split_once(':') {
        Some((sock, path)) => {
            if sock.is_empty() {
                return Err(uhttp_err("empty socket path"));
            }
            let path = if path.is_empty() { "/".to_string() } else { path.to_string() };
            Ok((sock.to_string(), path))
        }
        None => {
            if rest.is_empty() {
                return Err(uhttp_err("empty socket path"));
            }
            Ok((rest.to_string(), "/".to_string()))
        }
    }
}

enum HttpMethod {
    Get,
    Post(String),
    Put(String),
    Delete,
    Head,
}

impl HttpMethod {
    fn verb(&self) -> &'static str {
        match self {
            HttpMethod::Get     => "GET",
            HttpMethod::Post(_) => "POST",
            HttpMethod::Put(_)  => "PUT",
            HttpMethod::Delete  => "DELETE",
            HttpMethod::Head    => "HEAD",
        }
    }

    fn body(&self) -> Option<&str> {
        match self {
            HttpMethod::Post(b) | HttpMethod::Put(b) => Some(b),
            _ => None,
        }
    }
}

// Runs the request on a fresh OS thread to mirror `http::execute` (keeps the
// blocking socket I/O off any surrounding async runtime's worker threads).
fn execute(url: String, method: HttpMethod, headers: Vec<(String, String)>) -> Result<VmValue> {
    let (sock_path, req_path) = parse_unix_url(&url)?;

    match std::thread::spawn(move || -> std::result::Result<(u16, String), String> {
        let mut stream = UnixStream::connect(&sock_path).map_err(|e| e.to_string())?;
        stream.set_read_timeout(Some(TIMEOUT)).map_err(|e| e.to_string())?;
        stream.set_write_timeout(Some(TIMEOUT)).map_err(|e| e.to_string())?;

        // ── request ──────────────────────────────────────────────────────
        let mut req = format!("{} {} HTTP/1.1\r\n", method.verb(), req_path);
        req.push_str("Host: localhost\r\n");
        req.push_str("Connection: close\r\n");
        for (k, v) in &headers {
            req.push_str(&format!("{}: {}\r\n", k, v));
        }
        if let Some(body) = method.body() {
            req.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        req.push_str("\r\n");
        if let Some(body) = method.body() {
            req.push_str(body);
        }
        stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
        stream.flush().map_err(|e| e.to_string())?;

        // ── response ─────────────────────────────────────────────────────
        // `Connection: close` makes the server close the socket after the
        // response, so read-to-EOF yields the full message.
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).map_err(|e| e.to_string())?;
        parse_response(&raw, matches!(method, HttpMethod::Head))
    })
    .join() {
        Ok(Ok((status, body))) => Ok(make_response(status, body)),
        Ok(Err(e))             => Err(uhttp_err(&e)),
        Err(_)                 => Err(uhttp_err("request thread panicked")),
    }
}

/// Parse a raw HTTP/1.1 response into `(status, body)`.
///
/// `is_head` suppresses the body (a HEAD response carries none even when it
/// advertises a `Content-Length`).
fn parse_response(raw: &[u8], is_head: bool) -> std::result::Result<(u16, String), String> {
    let sep = find_subsequence(raw, b"\r\n\r\n")
        .ok_or_else(|| "malformed response: no header terminator".to_string())?;
    let head = String::from_utf8_lossy(&raw[..sep]);
    let body_bytes = &raw[sep + 4..];

    let mut lines = head.split("\r\n");
    let status_line = lines.next().ok_or_else(|| "empty response".to_string())?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| format!("malformed status line: {status_line}"))?;

    if is_head {
        return Ok((status, String::new()));
    }

    // Collect the headers we care about for framing (case-insensitive names).
    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim();
            match name.as_str() {
                "content-length" => content_length = value.parse().ok(),
                "transfer-encoding" if value.to_ascii_lowercase().contains("chunked") => {
                    chunked = true;
                }
                _ => {}
            }
        }
    }

    let body = if chunked {
        dechunk(body_bytes)?
    } else if let Some(len) = content_length {
        body_bytes[..len.min(body_bytes.len())].to_vec()
    } else {
        body_bytes.to_vec()
    };

    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

/// Decode a `Transfer-Encoding: chunked` body.
fn dechunk(data: &[u8]) -> std::result::Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut pos = 0;
    loop {
        let line_end = find_subsequence(&data[pos..], b"\r\n")
            .ok_or_else(|| "malformed chunk: missing size line".to_string())?;
        let size_line = String::from_utf8_lossy(&data[pos..pos + line_end]);
        // A chunk size may carry extensions after a `;` — ignore them.
        let size_str = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_str, 16)
            .map_err(|_| format!("malformed chunk size: {size_str}"))?;
        pos += line_end + 2; // past the size line's CRLF
        if size == 0 {
            break; // final chunk
        }
        if pos + size > data.len() {
            return Err("malformed chunk: truncated body".to_string());
        }
        out.extend_from_slice(&data[pos..pos + size]);
        pos += size + 2; // past the chunk data's trailing CRLF
    }
    Ok(out)
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn uhttp_get(args: &[VmValue]) -> Result<VmValue> {
    if args.is_empty() || args.len() > 2 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let url = require_str_owned(args, 0, "uhttp.get")?;
    let headers = extract_headers(args.get(1))?;
    execute(url, HttpMethod::Get, headers)
}

fn uhttp_post(args: &[VmValue]) -> Result<VmValue> {
    if args.len() < 2 || args.len() > 3 {
        return Err(JadeError::ArityMismatch { expected: 2, got: args.len(), span: ZERO });
    }
    let url  = require_str_owned(args, 0, "uhttp.post")?;
    let body = require_str_owned(args, 1, "uhttp.post")?;
    let headers = extract_headers(args.get(2))?;
    execute(url, HttpMethod::Post(body), headers)
}

fn uhttp_put(args: &[VmValue]) -> Result<VmValue> {
    if args.len() < 2 || args.len() > 3 {
        return Err(JadeError::ArityMismatch { expected: 2, got: args.len(), span: ZERO });
    }
    let url  = require_str_owned(args, 0, "uhttp.put")?;
    let body = require_str_owned(args, 1, "uhttp.put")?;
    let headers = extract_headers(args.get(2))?;
    execute(url, HttpMethod::Put(body), headers)
}

fn uhttp_delete(args: &[VmValue]) -> Result<VmValue> {
    if args.is_empty() || args.len() > 2 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let url = require_str_owned(args, 0, "uhttp.delete")?;
    let headers = extract_headers(args.get(1))?;
    execute(url, HttpMethod::Delete, headers)
}

fn uhttp_head(args: &[VmValue]) -> Result<VmValue> {
    if args.is_empty() || args.len() > 2 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let url = require_str_owned(args, 0, "uhttp.head")?;
    let headers = extract_headers(args.get(1))?;
    execute(url, HttpMethod::Head, headers)
}

// ── Streaming ──────────────────────────────────────────────────────────────
//
// Streaming endpoints (Docker `/events`, `/logs?follow=1`, image-pull progress)
// return bodies that never terminate on their own. `uhttp.stream(url, handler)`
// consumes them line-by-line: a worker thread owns the socket, decodes framing
// incrementally, and pushes each line onto an mpsc channel; the VM drains that
// channel and invokes the Jade handler per line (see `NativeFnId::UhttpStream`).

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
    let (sock_path, req_path) = parse_unix_url(url)?;
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

pub static UHTTP_PKG: Package = Package {
    import_name: "std/uhttp",
    global_name: "uhttp",
    fns: UHTTP_PKG_FNS,
    register_types: register_uhttp_pkg_types,
};

/// Override for `Package::vm_dict_value`: the five request functions are pure
/// `BuiltinFn`s, but `stream` is state-mutating (it invokes a Jade handler), so
/// it is injected as `NativeFn(NativeFnId::UhttpStream)` and dispatched in the VM.
pub fn uhttp_vm_dict_value() -> VmValue {
    let mut map = HashMap::new();
    for f in UHTTP_PKG_FNS {
        map.insert(f.name.to_string(), VmValue::BuiltinFn(*f));
    }
    map.insert("stream".to_string(), VmValue::NativeFn(NativeFnId::UhttpStream));
    VmValue::Dict(map)
}

#[cfg(test)]
mod tests;
