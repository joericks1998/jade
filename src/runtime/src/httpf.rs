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
use crate::cstr;
use crate::string::{self, TAINTED, TRUSTED};
use crate::value::JadeValue;

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

/// Text-bodied request. The reply goes through [`body_text`], so a binary body
/// comes back mangled — use [`request_bytes`] for anything that is not text.
pub fn request(
    method: &str,
    url: &str,
    body: Option<&str>,
    headers: &[(String, String)],
) -> Result<(i64, String), String> {
    request_bytes(method, url, body.map(|b| b.as_bytes()), headers)
        .map(|(status, bytes)| (status, body_text(&bytes)))
}

/// A response body as the text a Jade `str` can actually hold.
///
/// Two lossy steps, and both are forced. Invalid UTF-8 becomes a replacement
/// character, because a `str` is UTF-8. And the text stops at the first NUL,
/// because a `str` is NUL-terminated — a compiled binary hands its body to
/// `cstr::emit` and so truncates there whatever the VM does.
///
/// The truncation lives here, in the shared core, rather than in either engine.
/// Before v1.2.5 only the compiled path truncated: `http.get` on a body holding
/// a NUL reported 8 characters under `jade run` and 4 from the same program
/// built, which is a silent disagreement about what the language means. Use
/// `get_bytes` for a body that is not text — that is the whole reason it exists.
pub(crate) fn body_text(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Byte-bodied request: the reply is handed back as raw octets.
///
/// Exec `curl` for `method url` (optional `body`, `headers`), capture stdout,
/// and parse the `\nJADE_STATUS:<code>` trailer → `Ok((status, body))`. `Err`
/// (a full message) only on a transport failure (curl exit ≠ 0); a 4xx/5xx is a
/// normal status. The VM maps `Err` to `IoError`; the AOT wrappers record it as
/// pending.
///
/// This is the real implementation; [`request`] is a lossy view of it. Splitting
/// them this way means there is one place that spawns curl and one place that
/// parses the status trailer, so the text and byte paths cannot disagree about
/// either.
pub fn request_bytes(
    method: &str,
    url: &str,
    body: Option<&[u8]>,
    headers: &[(String, String)],
) -> Result<(i64, Vec<u8>), String> {
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
        let _ = stdin.write_all(b); // dropping stdin closes it
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
    let (body_bytes, status): (&[u8], i64) = match buf.windows(MARK.len()).rposition(|w| w == MARK)
    {
        Some(i) => {
            let digits = &buf[i + MARK.len()..];
            let n: String =
                digits.iter().take_while(|c| c.is_ascii_digit()).map(|&c| c as char).collect();
            (&buf[..i], n.parse().unwrap_or(0))
        }
        None => (&buf[..], 0),
    };
    Ok((status, body_bytes.to_vec()))
}

// ── AOT C-ABI wrappers ────────────────────────────────────────────────────────

/// A tagged-string word's bytes as `&str` (non-string → "").
pub(crate) fn header_val(word: W) -> &'static str {
    let v = JadeValue::from_bits(word as u64);
    if v.is_str() { unsafe { cstr::borrow(v.as_ptr() as *const c_char) } } else { "" }
}

/// Read the AOT ObjHeader header-dict into `(name, value)` pairs.
pub(crate) fn read_headers(headers: *const c_void) -> Vec<(String, String)> {
    if headers.is_null() {
        return Vec::new();
    }
    let d = unsafe { &*(headers as *const DictObj<W>) };
    d.entries().iter().map(|(k, v)| (k.clone(), header_val(*v).to_owned())).collect()
}

/// Build the result dict `{ status, body: TAINTED }` as a tagged pointer word.
pub(crate) fn make_dict(status: i64, body: &str) -> W {
    let mut d = DictObj::<W>::new();
    d.insert("status", JadeValue::from_int(status).bits() as i64);
    let body_w =
        JadeValue::from_str_ptr(cstr::emit(body.as_bytes(), TAINTED) as *const ()).bits() as i64;
    d.insert("body", body_w);
    JadeValue::from_ptr(crate::gc::leak_obj(d) as *const c_void as *const ()).bits() as i64
}

/// The same dict, with an undecoded `bytes` body.
///
/// Same key names and same order as [`make_dict`] on purpose: a caller reads
/// `.status` identically either way, and only `.body` differs in type. The blob
/// is TAINTED for the reason the string body is — it came off the network.
pub(crate) fn make_bytes_dict(status: i64, body: &[u8]) -> W {
    let mut d = DictObj::<W>::new();
    d.insert("status", JadeValue::from_int(status).bits() as i64);
    let blob = crate::gc::leak_obj(crate::bytesf::BytesObj::new(body.to_vec(), TAINTED));
    d.insert("body", JadeValue::from_ptr(blob as *const ()).bits() as i64);
    JadeValue::from_ptr(crate::gc::leak_obj(d) as *const c_void as *const ()).bits() as i64
}

/// Borrow the octets out of a tagged word that should hold a `bytes` value.
///
/// Returns `None` for anything else. The `_bytes` senders take their body as a
/// whole tagged word rather than a bare data pointer precisely so this check is
/// possible: inference does not yet distinguish a `bytes` argument from any
/// other value, so without it a `post_bytes(url, "text")` would dereference a
/// string as a heap object.
pub(crate) fn bytes_arg(word: W) -> Option<&'static [u8]> {
    let v = JadeValue::from_bits(word as u64);
    if !v.is_ptr() {
        return None;
    }
    let p = v.as_ptr() as *const crate::bytesf::BytesObj;
    if p.is_null() {
        return None;
    }
    let obj = unsafe { &*p };
    if obj.header.kind != crate::heap::ObjKind::Bytes as u8 {
        return None;
    }
    Some(obj.as_slice())
}

/// The Jade type name of a tagged word.
///
/// The VM's `value_type_name` answers the same question about a `VmValue`; this
/// answers it about a raw word, so a compiled binary can name what it was handed
/// in the *same sentence* the VM uses. Kept as a `CStr` so the C side can print
/// it without allocating — [`word_type_name`] is the Rust-facing view of the
/// same table, and there is only the one table.
pub(crate) fn word_type_cstr(word: W) -> &'static core::ffi::CStr {
    let v = JadeValue::from_bits(word as u64);
    if v.is_int() {
        return c"int";
    }
    if v.is_str() {
        return c"str";
    }
    if v.is_float() {
        return c"float";
    }
    if v.is_bool() {
        return c"bool";
    }
    if v.is_nil() {
        return c"nil";
    }
    if v.is_char() {
        return c"char";
    }
    if !v.is_ptr() || v.as_ptr().is_null() {
        return c"value";
    }
    use crate::heap::ObjKind::*;
    match unsafe { &*(v.as_ptr() as *const crate::heap::ObjHeader) }.kind {
        k if k == Array as u8 => c"array",
        k if k == Dict as u8 => c"dict",
        k if k == Struct as u8 => c"struct",
        k if k == Fn as u8 => c"fn",
        k if k == Future as u8 => c"future",
        k if k == Prompt as u8 => c"prompt",
        k if k == Grammar as u8 => c"grammar",
        k if k == BoundMethod as u8 => c"method",
        k if k == Bytes as u8 => c"bytes",
        _ => c"value",
    }
}

/// [`word_type_cstr`] as a Rust string.
pub(crate) fn word_type_name(word: W) -> &'static str {
    word_type_cstr(word).to_str().unwrap_or("value")
}

/// C-ABI view of [`word_type_cstr`]. The returned pointer is a static literal —
/// the caller neither owns nor frees it.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_type_name_of(word: W) -> *const c_char {
    word_type_cstr(word).as_ptr()
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
pub extern "C" fn jrt_http_post_impl(
    url: *const c_char,
    body: *const c_char,
    headers: *const c_void,
) -> W {
    request_aot("POST", url, Some(unsafe { cstr::borrow(body) }), headers)
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_http_put_impl(
    url: *const c_char,
    body: *const c_char,
    headers: *const c_void,
) -> W {
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

// ── Byte-bodied requests ──────────────────────────────────────────────────────
//
// Separate entry points rather than a flag on the existing ones, because the
// *return* type differs: `.body` is a `bytes` value, not a string. Both engines
// reach the same `request_bytes` core, so the two spellings cannot disagree
// about what a reply contains — only about how it is handed back.

/// Run `request_bytes`, building the byte-bodied dict; on transport failure,
/// record the pending error and return `{ status: 0, body: <empty> }`.
fn request_bytes_aot(
    method: &str,
    url: *const c_char,
    body: Option<&[u8]>,
    headers: *const c_void,
) -> W {
    match request_bytes(method, unsafe { cstr::borrow(url) }, body, &read_headers(headers)) {
        Ok((status, body)) => make_bytes_dict(status, &body),
        Err(m) => {
            set_err(&m);
            make_bytes_dict(0, &[])
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_http_get_bytes_impl(url: *const c_char, headers: *const c_void) -> W {
    request_bytes_aot("GET", url, None, headers)
}

/// `body` is the argument's whole tagged word so a non-`bytes` value can be
/// reported rather than dereferenced. See [`bytes_arg`].
#[unsafe(no_mangle)]
pub extern "C" fn jrt_http_post_bytes_impl(
    url: *const c_char,
    body: W,
    headers: *const c_void,
) -> W {
    match bytes_arg(body) {
        Some(b) => request_bytes_aot("POST", url, Some(b), headers),
        None => {
            set_err(&format!("http.post_bytes expects bytes, got {}", word_type_name(body)));
            make_bytes_dict(0, &[])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two lossy steps a text body goes through, separately visible.
    /// Truncation at a NUL is the one that used to differ by engine.
    #[test]
    fn body_text_substitutes_invalid_utf8_then_stops_at_a_nul() {
        assert_eq!(body_text(b"plain"), "plain");
        assert_eq!(body_text(&[0xFF, b'a']), "\u{FFFD}a");
        assert_eq!(body_text(&[b'a', 0x00, b'b']), "a");
        assert_eq!(body_text(&[0x00]), "");
    }

    #[test]
    fn word_type_names_the_immediates() {
        assert_eq!(word_type_name(JadeValue::from_int(1).bits() as W), "int");
        assert_eq!(word_type_name(crate::value::NIL.bits() as W), "nil");
        assert_eq!(word_type_name(crate::value::TRUE.bits() as W), "bool");
    }

    /// A bytes word is accepted and read back whole; anything else is declined
    /// rather than dereferenced, which is what keeps a wrong argument a message
    /// instead of a crash.
    #[test]
    fn bytes_arg_accepts_only_a_blob() {
        let blob = crate::gc::leak_obj(crate::bytesf::BytesObj::trusted(vec![1, 2, 3]));
        let word = JadeValue::from_ptr(blob as *const ()).bits() as W;
        assert_eq!(bytes_arg(word), Some(&[1u8, 2, 3][..]));
        assert_eq!(word_type_name(word), "bytes");

        assert_eq!(bytes_arg(JadeValue::from_int(7).bits() as W), None);
        assert_eq!(bytes_arg(crate::value::NIL.bits() as W), None);
    }
}
