//! `std::http` runtime module — the AOT C-ABI surface, ported from the C leaf
//! `runtime_aot/http/http.c`. get/post/put/delete/head over the `curl` binary
//! (exec'd directly, not a shell), each returning an ObjHeader dict
//! `{ status: int, body: str(TAINTED) }`. Mirrors the VM's `http_pkg.rs` shape;
//! response bodies are external → TAINTED.
//!
//! Only a *transport* failure raises (curl exit ≠ 0 — DNS/connect/TLS/timeout);
//! an HTTP 4xx/5xx is a normal `status`. Since a Jade exception is a `longjmp`
//! that must not cross a Rust frame, the impls record a thread-local pending
//! error and thin C forwarders in `common.c` throw it.

use core::ffi::{c_char, c_void};
use std::cell::Cell;
use std::io::Write;
use std::process::{Command, Stdio};

use crate::coll::DictObj;
use crate::string::{self, TAINTED, TRUSTED};
use crate::value::JadeValue;
use crate::cstr;

type W = i64;

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

/// Drain the pending http error (a tagged string the caller owns), or null.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_http_take_error() -> *mut c_char {
    PENDING.with(|p| p.replace(core::ptr::null_mut()))
}

/// Human-readable reason for a curl exit code.
fn curl_reason(code: i32) -> &'static str {
    match code {
        6 => "could not resolve host",
        7 => "could not connect to host",
        28 => "request timed out",
        35 => "TLS handshake failed",
        127 => "curl executable not found",
        _ => "request failed",
    }
}

// ── Neutral core (used by both engines) ───────────────────────────────────────

/// Exec `curl` for `method url` (optional `body`, `headers`), capture stdout, and
/// parse the `\nJADE_STATUS:<code>` trailer → `Ok((status, body))`. `Err` (a full
/// message) only on a transport failure (curl exit ≠ 0); a 4xx/5xx is a normal
/// status. The VM maps `Err` to `IoError`; the AOT wrappers record it as pending.
pub fn request(
    method: &str,
    url: &str,
    body: Option<&str>,
    headers: &[(String, String)],
) -> Result<(i64, String), String> {
    let is_head = method == "HEAD";
    let mut cmd = Command::new("curl");
    cmd.arg("-sS");
    if is_head {
        cmd.args(["-o", "/dev/null"]);
    }
    cmd.args(["-X", method]);
    for (k, v) in headers {
        cmd.arg("-H").arg(format!("{k}: {v}"));
    }
    if body.is_some() {
        cmd.args(["--data-binary", "@-"]);
    }
    cmd.args(["-w", "\nJADE_STATUS:%{http_code}", "--"]).arg(url);
    cmd.stdout(Stdio::piped()).stderr(Stdio::null());
    cmd.stdin(if body.is_some() { Stdio::piped() } else { Stdio::null() });

    let mut child = cmd
        .spawn()
        .map_err(|_| format!("http {method} '{url}': {} (curl exit 127)", curl_reason(127)))?;
    if let (Some(b), Some(mut stdin)) = (body, child.stdin.take()) {
        let _ = stdin.write_all(b.as_bytes()); // dropping stdin closes it
    }
    let out = child
        .wait_with_output()
        .map_err(|_| format!("http {method} '{url}': {} (curl exit -1)", curl_reason(-1)))?;
    let code = out.status.code().unwrap_or(-1);
    if code != 0 {
        return Err(format!("http {method} '{url}': {} (curl exit {code})", curl_reason(code)));
    }

    // Split the "\nJADE_STATUS:<code>" trailer off the body.
    let buf = &out.stdout;
    const MARK: &[u8] = b"\nJADE_STATUS:";
    let (body_bytes, status): (&[u8], i64) = match buf.windows(MARK.len()).rposition(|w| w == MARK) {
        Some(i) => {
            let digits = &buf[i + MARK.len()..];
            let n: String = digits.iter().take_while(|c| c.is_ascii_digit()).map(|&c| c as char).collect();
            (&buf[..i], n.parse().unwrap_or(0))
        }
        None => (&buf[..], 0),
    };
    Ok((status, String::from_utf8_lossy(body_bytes).into_owned()))
}

// ── AOT C-ABI wrappers ────────────────────────────────────────────────────────

/// A tagged-string word's bytes as `&str` (non-string → "").
fn header_val(word: W) -> &'static str {
    let v = JadeValue::from_bits(word as u64);
    if v.is_str() {
        unsafe { cstr::borrow(v.as_ptr() as *const c_char) }
    } else {
        ""
    }
}

/// Read the AOT ObjHeader header-dict into `(name, value)` pairs.
fn read_headers(headers: *const c_void) -> Vec<(String, String)> {
    if headers.is_null() {
        return Vec::new();
    }
    let d = unsafe { &*(headers as *const DictObj<W>) };
    d.entries().iter().map(|(k, v)| (k.clone(), header_val(*v).to_owned())).collect()
}

/// Build the result dict `{ status, body: TAINTED }` as a tagged pointer word.
fn make_dict(status: i64, body: &str) -> W {
    let mut d = DictObj::<W>::new();
    d.insert("status", JadeValue::from_int(status).bits() as i64);
    let body_w = JadeValue::from_str_ptr(cstr::emit(body.as_bytes(), TAINTED) as *const ()).bits() as i64;
    d.insert("body", body_w);
    JadeValue::from_ptr(crate::gc::leak_obj(d) as *const c_void as *const ()).bits() as i64
}

/// Run `request`, building the result dict; on transport failure, record the
/// pending error (the C forwarder throws it) and return `{ status: 0, body: "" }`.
fn request_aot(method: &str, url: *const c_char, body: Option<&str>, headers: *const c_void) -> W {
    match request(method, unsafe { cstr::borrow(url) }, body, &read_headers(headers)) {
        Ok((status, body)) => make_dict(status, &body),
        Err(m) => {
            set_err(&m);
            make_dict(0, "")
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_http_get_impl(url: *const c_char, headers: *const c_void) -> W {
    request_aot("GET", url, None, headers)
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_http_post_impl(url: *const c_char, body: *const c_char, headers: *const c_void) -> W {
    request_aot("POST", url, Some(unsafe { cstr::borrow(body) }), headers)
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_http_put_impl(url: *const c_char, body: *const c_char, headers: *const c_void) -> W {
    request_aot("PUT", url, Some(unsafe { cstr::borrow(body) }), headers)
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_http_delete_impl(url: *const c_char, headers: *const c_void) -> W {
    request_aot("DELETE", url, None, headers)
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_http_head_impl(url: *const c_char, headers: *const c_void) -> W {
    request_aot("HEAD", url, None, headers)
}
