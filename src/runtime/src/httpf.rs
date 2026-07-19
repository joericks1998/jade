//! `std::http` runtime module — the AOT C-ABI surface, ported from the C leaf
//! `runtime_lib/http/http.c`. get/post/put/delete/head over the `curl` binary
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
use crate::sys::strlen;
use crate::value::JadeValue;

type W = i64;

thread_local! {
    static PENDING: Cell<*mut c_char> = const { Cell::new(core::ptr::null_mut()) };
}

unsafe fn mk_str(bytes: &[u8], trust: u8) -> *mut c_char {
    let out = string::new(bytes.len(), trust);
    if !bytes.is_empty() {
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len()) };
    }
    out as *mut c_char
}

unsafe fn cstr(p: *const c_char) -> &'static str {
    if p.is_null() {
        return "";
    }
    unsafe {
        let n = strlen(p as *const u8);
        core::str::from_utf8(core::slice::from_raw_parts(p as *const u8, n)).unwrap_or("")
    }
}

fn set_err(msg: &str) {
    let s = unsafe { mk_str(msg.as_bytes(), TRUSTED) };
    PENDING.with(|p| {
        let old = p.replace(s);
        if !old.is_null() {
            unsafe { string::free_str(old as *mut u8) };
        }
    });
}

/// Drain the pending http error (a tagged string the caller owns), or null.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_http_take_error() -> *mut c_char {
    PENDING.with(|p| p.replace(core::ptr::null_mut()))
}

/// A tagged-string word's bytes as `&str` (non-string → "").
fn header_val(word: W) -> &'static str {
    let v = JadeValue::from_bits(word as u64);
    if v.is_str() {
        unsafe { cstr(v.as_ptr() as *const c_char) }
    } else {
        ""
    }
}

/// Build the result dict `{ status, body: TAINTED }` as a tagged pointer word.
fn make_dict(status: i64, body: &[u8]) -> W {
    let mut d = DictObj::<W>::new();
    d.insert("status", JadeValue::from_int(status).bits() as i64);
    let body_w = unsafe { JadeValue::from_str_ptr(mk_str(body, TAINTED) as *const ()).bits() as i64 };
    d.insert("body", body_w);
    JadeValue::from_ptr(crate::gc::leak_obj(d) as *const c_void as *const ()).bits() as i64
}

/// Human-readable reason for a curl exit code (matches the C leaf's mapping).
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

/// Core request: exec curl, capture stdout, parse the `\nJADE_STATUS:<code>`
/// trailer → `{ status, body }`. On transport failure (curl exit ≠ 0) records a
/// pending error and returns the default `{ status: 0, body: "" }` (the forwarder
/// throws before the caller sees it).
fn request(method: &str, url: &str, body: Option<&[u8]>, headers: *const c_void) -> W {
    let is_head = method == "HEAD";
    let mut cmd = Command::new("curl");
    cmd.arg("-sS");
    if is_head {
        cmd.args(["-o", "/dev/null"]);
    }
    cmd.args(["-X", method]);
    if !headers.is_null() {
        // Deterministic order (matches the C, which sorts keys) — insertion order
        // here; header order does not affect difftest (http isn't network-tested).
        let d = unsafe { &*(headers as *const DictObj<W>) };
        for (k, v) in d.entries() {
            cmd.arg("-H").arg(format!("{k}: {}", header_val(*v)));
        }
    }
    if body.is_some() {
        cmd.args(["--data-binary", "@-"]);
    }
    cmd.args(["-w", "\nJADE_STATUS:%{http_code}", "--"]).arg(url);
    cmd.stdout(Stdio::piped()).stderr(Stdio::null());
    cmd.stdin(if body.is_some() { Stdio::piped() } else { Stdio::null() });

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => {
            set_err(&format!("http {method} '{url}': {} (curl exit 127)", curl_reason(127)));
            return make_dict(0, b"");
        }
    };
    if let (Some(b), Some(mut stdin)) = (body, child.stdin.take()) {
        let _ = stdin.write_all(b); // dropping stdin closes it
    }
    let out = match child.wait_with_output() {
        Ok(o) => o,
        Err(_) => {
            set_err(&format!("http {method} '{url}': {} (curl exit -1)", curl_reason(-1)));
            return make_dict(0, b"");
        }
    };
    let code = out.status.code().unwrap_or(-1);
    if code != 0 {
        set_err(&format!("http {method} '{url}': {} (curl exit {code})", curl_reason(code)));
        return make_dict(0, b"");
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
    make_dict(status, body_bytes)
}

// ── Per-verb impls (the C forwarders throw the pending error) ─────────────────

#[unsafe(no_mangle)]
pub extern "C" fn jrt_http_get_impl(url: *const c_char, headers: *const c_void) -> W {
    request("GET", unsafe { cstr(url) }, None, headers)
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_http_post_impl(url: *const c_char, body: *const c_char, headers: *const c_void) -> W {
    request("POST", unsafe { cstr(url) }, Some(unsafe { cstr(body) }.as_bytes()), headers)
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_http_put_impl(url: *const c_char, body: *const c_char, headers: *const c_void) -> W {
    request("PUT", unsafe { cstr(url) }, Some(unsafe { cstr(body) }.as_bytes()), headers)
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_http_delete_impl(url: *const c_char, headers: *const c_void) -> W {
    request("DELETE", unsafe { cstr(url) }, None, headers)
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_http_head_impl(url: *const c_char, headers: *const c_void) -> W {
    request("HEAD", unsafe { cstr(url) }, None, headers)
}
