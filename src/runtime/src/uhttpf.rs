//! `std::uhttp` runtime module — HTTP/1.1 over a Unix domain socket, shared by
//! both engines. Mirrors `httpf.rs` (the TCP `std::http` module): the neutral
//! [`request`] core is called by the VM (`src/uhttp/mod.rs`) and by the AOT
//! C-ABI wrappers (`jrt_uhttp_*_impl`) here; both return a `{ status, body }`
//! dict. Response bodies are external → TAINTED.
//!
//! Unlike `httpf` (which execs `curl`), the transport is hand-framed HTTP/1.1
//! written directly onto a `std::os::unix::net::UnixStream` — there is no
//! Unix-socket HTTP client in the dependency tree. Response framing honors
//! `Content-Length`, `Transfer-Encoding: chunked` (de-chunked), and read-to-EOF
//! on `Connection: close`. True streaming endpoints (bodies that never
//! terminate) are the VM's `uhttp.stream`, not this request path.
//!
//! Only a *transport* failure raises (connect/timeout/malformed response); an
//! HTTP 4xx/5xx is a normal `status`. As in `httpf`, a Jade exception is a
//! `longjmp` that must not cross a Rust frame, so the AOT wrappers record a
//! thread-local pending error and the C forwarders in `common.c` throw it.

use core::ffi::{c_char, c_void};
use std::cell::Cell;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use crate::cstr;
use crate::httpf::{make_dict, read_headers};
use crate::string::{self, TRUSTED};

type W = i64;

const TIMEOUT: Duration = Duration::from_secs(30);

// ── URL + framing helpers (neutral; unit-tested) ──────────────────────────────

/// Split `unix://<socket-path>:<request-path>` into its two parts.
///
/// Requires the `unix://` scheme. The socket path is everything up to the first
/// `:` after the scheme; the request path is the remainder (defaulting to `/`
/// when absent). Splitting on the *first* `:` keeps colons inside a request
/// path (query strings, matrix params) intact, at the cost of not supporting a
/// `:` inside the socket path itself.
pub fn parse_unix_url(url: &str) -> Result<(String, String), String> {
    let rest = url
        .strip_prefix("unix://")
        .ok_or_else(|| "url must start with unix://".to_string())?;
    match rest.split_once(':') {
        Some((sock, path)) => {
            if sock.is_empty() {
                return Err("empty socket path".to_string());
            }
            let path = if path.is_empty() { "/".to_string() } else { path.to_string() };
            Ok((sock.to_string(), path))
        }
        None => {
            if rest.is_empty() {
                return Err("empty socket path".to_string());
            }
            Ok((rest.to_string(), "/".to_string()))
        }
    }
}

/// Parse a raw HTTP/1.1 response into `(status, body)`.
///
/// `is_head` suppresses the body (a HEAD response carries none even when it
/// advertises a `Content-Length`).
pub fn parse_response(raw: &[u8], is_head: bool) -> Result<(i64, String), String> {
    let sep = find_subsequence(raw, b"\r\n\r\n")
        .ok_or_else(|| "malformed response: no header terminator".to_string())?;
    let head = String::from_utf8_lossy(&raw[..sep]);
    let body_bytes = &raw[sep + 4..];

    let mut lines = head.split("\r\n");
    let status_line = lines.next().ok_or_else(|| "empty response".to_string())?;
    let status: i64 = status_line
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
pub fn dechunk(data: &[u8]) -> Result<Vec<u8>, String> {
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
        // The chunk payload MUST be followed by its own CRLF. Require both the
        // payload *and* that trailing CRLF to be present before advancing `pos`,
        // so a truncated frame (payload fits but the trailer is missing) can
        // never push `pos` past the buffer — which would panic on the next
        // iteration's `&data[pos..]`. `checked_add` also rejects an absurd hex
        // size that would wrap the arithmetic. (Slice indexing is bounds-checked
        // regardless, so this is a robustness fix, not a memory-safety one.)
        let end = size
            .checked_add(2)
            .and_then(|n| pos.checked_add(n))
            .ok_or_else(|| "malformed chunk: size overflow".to_string())?;
        if end > data.len() {
            return Err("malformed chunk: truncated body".to_string());
        }
        out.extend_from_slice(&data[pos..pos + size]);
        pos += size + 2; // past the chunk data's trailing CRLF
    }
    Ok(out)
}

pub fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

// ── Neutral request core (used by both engines) ───────────────────────────────

fn verb_has_body(method: &str) -> bool {
    matches!(method, "POST" | "PUT")
}

/// Perform one `method` request over the Unix socket named in `url`
/// (`unix://<sock>:<path>`), returning `(status, body)`. `Err` (a full message)
/// only on a transport/parse failure; a 4xx/5xx is a normal status. Runs on a
/// fresh OS thread so the blocking socket I/O stays off any surrounding async
/// runtime's worker threads (mirrors the VM's original `execute`).
///
/// SAFETY (FFI): all response parsing (`parse_response`/`dechunk`) runs inside
/// this spawned worker; `join()` turns any unwind into `Err(_)`, so a panic can
/// never escape across the `extern "C"` `jrt_uhttp_*_impl` boundary (which would
/// be UB). This containment is what makes those wrappers panic-safe — do NOT
/// move the parsing into the caller's frame, and note that switching the crate
/// to `panic = "abort"` would turn a malformed-response panic into a process
/// abort instead of a contained error.
pub fn request(
    method: &str,
    url: &str,
    body: Option<&str>,
    headers: &[(String, String)],
) -> Result<(i64, String), String> {
    let (sock_path, req_path) = parse_unix_url(url)?;
    let is_head = method == "HEAD";
    let method = method.to_string();
    let body = body.map(str::to_string);
    let headers = headers.to_vec();

    let joined = std::thread::spawn(move || -> Result<(i64, String), String> {
        let mut stream = UnixStream::connect(&sock_path).map_err(|e| e.to_string())?;
        stream.set_read_timeout(Some(TIMEOUT)).map_err(|e| e.to_string())?;
        stream.set_write_timeout(Some(TIMEOUT)).map_err(|e| e.to_string())?;

        // ── request ──────────────────────────────────────────────────────
        let mut req = format!("{} {} HTTP/1.1\r\n", method, req_path);
        req.push_str("Host: localhost\r\n");
        req.push_str("Connection: close\r\n");
        for (k, v) in &headers {
            req.push_str(&format!("{}: {}\r\n", k, v));
        }
        if verb_has_body(&method) {
            let len = body.as_deref().map_or(0, str::len);
            req.push_str(&format!("Content-Length: {}\r\n", len));
        }
        req.push_str("\r\n");
        if verb_has_body(&method) {
            if let Some(b) = body.as_deref() {
                req.push_str(b);
            }
        }
        stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
        stream.flush().map_err(|e| e.to_string())?;

        // ── response ─────────────────────────────────────────────────────
        // `Connection: close` makes the server close the socket after the
        // response, so read-to-EOF yields the full message.
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).map_err(|e| e.to_string())?;
        parse_response(&raw, is_head)
    })
    .join();

    match joined {
        Ok(inner) => inner,
        Err(_) => Err("request thread panicked".to_string()),
    }
}

// ── Streaming reads ───────────────────────────────────────────────────────────
//
// A streaming endpoint holds the connection open and emits lines until it (or
// the caller) decides to stop, so it cannot use `request` above — that reads to
// EOF and returns one body.
//
// This lives here, in the shared crate, rather than beside the VM. It used to be
// VM-only on the reasoning that streaming "calls back into Jade, so it cannot be
// a pure AOT symbol". That is not what stops it: `array.map` already calls a
// Jade function from a compiled binary (a function value's box holds the raw
// pointer at offset 0). What actually differs is *who drives the loop*, so the
// split here is pull-based — this type only ever yields the next line, and each
// engine drives it the way that suits it. The VM pumps it from a worker thread
// into its async channel; the AOT C forwarder drives it inline and calls the
// handler directly.

/// A blocking, incremental reader over a streaming uhttp response.
///
/// Deliberately no read timeout: a streaming endpoint may sit idle between
/// events for arbitrarily long, and a timeout would end the stream mid-life.
/// The write timeout still bounds the request itself.
pub struct Stream {
    reader: std::io::BufReader<UnixStream>,
    status: i64,
    chunked: bool,
    /// Bytes of a line seen so far, still waiting for its `\n`.
    line_buf: Vec<u8>,
    /// Complete lines decoded from the last read but not yet handed out.
    pending: std::collections::VecDeque<String>,
    done: bool,
}

impl Stream {
    /// Connect, send the request, and read the status line and headers. Returns
    /// once the body is ready to read, so `status` is known before any line.
    pub fn open(url: &str, headers: &[(String, String)]) -> Result<Self, String> {
        use std::io::BufRead;

        let (sock_path, req_path) = parse_unix_url(url)?;
        let stream = UnixStream::connect(&sock_path).map_err(|e| e.to_string())?;
        stream.set_write_timeout(Some(TIMEOUT)).map_err(|e| e.to_string())?;
        let mut writer = stream.try_clone().map_err(|e| e.to_string())?;
        let mut reader = std::io::BufReader::new(stream);

        let mut req = format!("GET {} HTTP/1.1\r\n", req_path);
        req.push_str("Host: localhost\r\n");
        for (k, v) in headers {
            req.push_str(&format!("{}: {}\r\n", k, v));
        }
        req.push_str("\r\n");
        writer.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())?;

        let mut status_line = String::new();
        reader.read_line(&mut status_line).map_err(|e| e.to_string())?;
        let status: i64 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .ok_or_else(|| format!("malformed status line: {}", status_line.trim()))?;

        let mut chunked = false;
        loop {
            let mut header = String::new();
            let n = reader.read_line(&mut header).map_err(|e| e.to_string())?;
            if n == 0 {
                break; // premature EOF; the body loop will see it too
            }
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':') {
                if name.trim().eq_ignore_ascii_case("transfer-encoding")
                    && value.to_ascii_lowercase().contains("chunked")
                {
                    chunked = true;
                }
            }
        }

        Ok(Stream {
            reader,
            status,
            chunked,
            line_buf: Vec::new(),
            pending: std::collections::VecDeque::new(),
            done: false,
        })
    }

    /// The HTTP status, known as soon as `open` returns.
    pub fn status(&self) -> i64 {
        self.status
    }

    /// The next body line (newline- and CR-stripped), or `None` at end of
    /// stream. Blocks until a line is available.
    pub fn next_line(&mut self) -> Result<Option<String>, String> {
        loop {
            if let Some(line) = self.pending.pop_front() {
                return Ok(Some(line));
            }
            if self.done {
                // Flush a trailing partial line (body not newline-terminated).
                if self.line_buf.is_empty() {
                    return Ok(None);
                }
                let line = core::mem::take(&mut self.line_buf);
                return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
            }
            self.fill()?;
        }
    }

    /// Read one more piece of the body and split it into `pending`.
    fn fill(&mut self) -> Result<(), String> {
        use std::io::BufRead;

        if self.chunked {
            let mut size_line = String::new();
            let n = self.reader.read_line(&mut size_line).map_err(|e| e.to_string())?;
            if n == 0 {
                self.done = true;
                return Ok(());
            }
            let size_str = size_line.trim().split(';').next().unwrap_or("").trim();
            if size_str.is_empty() {
                return Ok(()); // the CRLF between chunks
            }
            let size = usize::from_str_radix(size_str, 16)
                .map_err(|_| format!("malformed chunk size: {size_str}"))?;
            if size == 0 {
                self.done = true;
                return Ok(());
            }
            let mut chunk = vec![0u8; size];
            self.reader.read_exact(&mut chunk).map_err(|e| e.to_string())?;
            let mut crlf = [0u8; 2]; // trailing CRLF after chunk data
            let _ = self.reader.read_exact(&mut crlf);
            self.feed(&chunk);
        } else {
            let mut buf = [0u8; 8192];
            let n = self.reader.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                self.done = true;
                return Ok(());
            }
            self.feed(&buf[..n]);
        }
        Ok(())
    }

    /// Split `data` on `\n`, queueing each complete line. Partial trailing bytes
    /// stay in `line_buf` for the next read.
    fn feed(&mut self, data: &[u8]) {
        for &b in data {
            if b == b'\n' {
                let mut line = core::mem::take(&mut self.line_buf);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                self.pending.push_back(String::from_utf8_lossy(&line).into_owned());
            } else {
                self.line_buf.push(b);
            }
        }
    }
}

// ── AOT C-ABI wrappers ────────────────────────────────────────────────────────

thread_local! {
    static PENDING: Cell<*mut c_char> = const { Cell::new(core::ptr::null_mut()) };
}

fn set_err(msg: &str) {
    let s = cstr::emit(msg.as_bytes(), TRUSTED);
    PENDING.with(|p| {
        let old = p.replace(s);
        if !old.is_null() {
            string::free_str(old as *mut u8);
        }
    });
}

/// Drain the pending uhttp error (a tagged string the caller owns), or null.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_uhttp_take_error() -> *mut c_char {
    PENDING.with(|p| p.replace(core::ptr::null_mut()))
}

/// Run `request`, building the result dict; on transport failure, record the
/// pending error (the C forwarder throws it) and return `{ status: 0, body: "" }`.
fn request_aot(method: &str, url: *const c_char, body: Option<&str>, headers: *const c_void) -> W {
    match request(method, unsafe { cstr::borrow(url) }, body, &read_headers(headers)) {
        Ok((status, body)) => make_dict(status, &body),
        Err(m) => {
            set_err(&format!("uhttp {method}: {m}"));
            make_dict(0, "")
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_uhttp_get_impl(url: *const c_char, headers: *const c_void) -> W {
    request_aot("GET", url, None, headers)
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_uhttp_post_impl(url: *const c_char, body: *const c_char, headers: *const c_void) -> W {
    request_aot("POST", url, Some(unsafe { cstr::borrow(body) }), headers)
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_uhttp_put_impl(url: *const c_char, body: *const c_char, headers: *const c_void) -> W {
    request_aot("PUT", url, Some(unsafe { cstr::borrow(body) }), headers)
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_uhttp_delete_impl(url: *const c_char, headers: *const c_void) -> W {
    request_aot("DELETE", url, None, headers)
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_uhttp_head_impl(url: *const c_char, headers: *const c_void) -> W {
    request_aot("HEAD", url, None, headers)
}

// ── Streaming, for the compiled path ──────────────────────────────────────────
//
// A pull-shaped handle rather than one call taking a callback: the driver loop
// lives in C (`jrt_uhttp_stream` in common.c), which is what calls the Jade
// handler. Keeping the callback on that side means no function pointer has to
// cross back into Rust, and no `longjmp` from a raising handler can unwind
// through a Rust frame — the same constraint that makes the request path record
// a pending error instead of throwing.

/// Open a stream. Returns an opaque handle, or null with a pending error set.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_uhttp_stream_open(url: *const c_char, headers: *const c_void) -> *mut c_void {
    match Stream::open(unsafe { cstr::borrow(url) }, &read_headers(headers)) {
        Ok(s) => Box::into_raw(Box::new(s)) as *mut c_void,
        Err(m) => {
            set_err(&format!("uhttp stream: {m}"));
            core::ptr::null_mut()
        }
    }
}

/// The stream's HTTP status.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_uhttp_stream_status(h: *mut c_void) -> i64 {
    if h.is_null() {
        return 0;
    }
    unsafe { (*(h as *const Stream)).status() }
}

/// The next line as a tagged TAINTED string word in `*out`.
///
/// Returns 1 on a line, 0 at end of stream, -1 on a read failure (with a pending
/// error set). A body line is external input, so it is TAINTED exactly like a
/// response body from the request path.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_uhttp_stream_next(h: *mut c_void, out: *mut W) -> i32 {
    if h.is_null() || out.is_null() {
        return 0;
    }
    let s = unsafe { &mut *(h as *mut Stream) };
    match s.next_line() {
        Ok(Some(line)) => {
            let w = crate::JadeValue::from_str_ptr(
                cstr::emit(line.as_bytes(), crate::string::TAINTED) as *const ()
            )
            .bits() as i64;
            unsafe { *out = w };
            1
        }
        Ok(None) => 0,
        Err(m) => {
            set_err(&format!("uhttp stream: {m}"));
            -1
        }
    }
}

/// Close the stream and free the handle. Dropping the `UnixStream` closes the
/// socket, which is how an early stop tells the server to stop sending.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_uhttp_stream_close(h: *mut c_void) {
    if !h.is_null() {
        drop(unsafe { Box::from_raw(h as *mut Stream) });
    }
}

#[cfg(test)]
mod tests;
