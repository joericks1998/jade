//! `json.parse` / `json.stringify` on tagged value words, for the AOT backend.
//!
//! The VM's `json` package (`src/json/mod.rs`) round-trips through `serde_json`,
//! so this uses the same crate to guarantee byte-identical output — number
//! formatting, key sorting (serde's default `Map` is a `BTreeMap`), string
//! escaping, and pretty-print layout all come from serde, not a hand-rolled
//! parser that could drift.
//!
//! Conversions bridge `serde_json::Value` and the Chunk backend's ObjHeader
//! collections: parse builds `ArrayObj`/`DictObj` trees; stringify walks them.

use core::ffi::c_char;

use serde_json::Value;

use crate::coll::{ArrayObj, DictObj};
use crate::float::{box_float, unbox_float};
use crate::heap::{ObjHeader, ObjKind};
use crate::string;
use crate::sys::strlen;
use crate::value::{JadeValue, NIL_BITS};

type W = i64;

/// `serde_json::Value` → an ObjHeader value word. Parsed strings inherit `trust`
/// (taint propagates from the JSON source). Mirrors the VM's `json_to_vm`.
pub(crate) fn value_to_word(v: &Value, trust: u8) -> W {
    match v {
        Value::Null => NIL_BITS as i64,
        Value::Bool(b) => JadeValue::from_bool(*b).bits() as i64,
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                JadeValue::from_int(i).bits() as i64
            } else {
                box_float(n.as_f64().unwrap_or(0.0)).bits() as i64
            }
        }
        Value::String(s) => {
            JadeValue::from_str_ptr(crate::cstr::emit_str(s, trust) as *const ()).bits() as W
        }
        Value::Array(a) => {
            let mut arr = ArrayObj::<W>::new();
            for e in a {
                arr.push(value_to_word(e, trust));
            }
            JadeValue::from_ptr(crate::gc::leak_obj(arr) as *const ()).bits() as i64
        }
        Value::Object(m) => {
            // serde's Map iterates in sorted key order (BTreeMap) by default.
            let mut d = DictObj::<W>::new();
            for (k, e) in m {
                d.insert(k.clone(), value_to_word(e, trust));
            }
            JadeValue::from_ptr(crate::gc::leak_obj(d) as *const ()).bits() as i64
        }
    }
}

/// An ObjHeader value word → `serde_json::Value`. Mirrors the VM's `vm_to_json`
/// (non-JSON kinds like structs → `Null`).
fn word_to_value(word: W) -> Value {
    word_to_value_at(word, 0).unwrap_or(Value::Null)
}

/// [`word_to_value`] at nesting depth `depth`, refusing past
/// [`crate::render::MAX_DEPTH`].
///
/// Arrays and dicts are reference-semantic and nothing collects cycles, so a
/// value can contain itself. This recursed without a bound, and a compiled
/// `json.stringify(a)` on such a value ran out of stack and died on SIGSEGV.
/// The bound is the one both renderers use, so the two engines refuse the same
/// values.
fn word_to_value_at(word: W, depth: usize) -> Option<Value> {
    if depth > crate::render::MAX_DEPTH {
        return None;
    }
    word_to_value_inner(word, depth)
}

fn word_to_value_inner(word: W, depth: usize) -> Option<Value> {
    let v = JadeValue::from_bits(word as u64);
    if v.is_int() {
        return Some(Value::Number(v.as_int().into()));
    }
    if v.is_bool() {
        return Some(Value::Bool(v.as_bool()));
    }
    if v.is_nil() {
        return Some(Value::Null);
    }
    if v.is_float() {
        return Some(
            serde_json::Number::from_f64(unbox_float(v)).map(Value::Number).unwrap_or(Value::Null),
        );
    }
    if v.is_str() {
        let bytes = unsafe {
            let p = v.as_ptr() as *const u8;
            core::slice::from_raw_parts(p, strlen(p))
        };
        return Some(Value::String(String::from_utf8_lossy(bytes).into_owned()));
    }
    // Non-string heap pointer: dispatch on kind.
    let p = v.as_ptr();
    let kind = unsafe { (*(p as *const ObjHeader)).kind };
    if kind == ObjKind::Array as u8 {
        let a = unsafe { &*(p as *const ArrayObj<W>) };
        let mut out = Vec::with_capacity(a.as_slice().len());
        for w in a.as_slice() {
            // `?`, not a `null` in the element's place: a refusal has to reach
            // the caller so it can raise, the way the VM does. Filling in
            // `null` would hand back a document that does not say what the
            // value held.
            out.push(word_to_value_at(*w, depth + 1)?);
        }
        Some(Value::Array(out))
    } else if kind == ObjKind::Dict as u8 {
        let d = unsafe { &*(p as *const DictObj<W>) };
        // Collect into serde's Map (sorted keys), matching the VM.
        let mut m = serde_json::Map::new();
        for (k, w) in d.entries() {
            m.insert(k.clone(), word_to_value_at(*w, depth + 1)?);
        }
        Some(Value::Object(m))
    } else {
        Some(Value::Null)
    }
}

/// Parse a tagged JSON string, giving back the value word or serde's complaint.
///
/// # Safety
/// `s` is a NUL-terminated tagged-string data pointer (trust at `s[-1]`).
unsafe fn parse(s: *const c_char) -> Result<W, serde_json::Error> {
    unsafe {
        let trust = string::trust_of(s as *const u8);
        let bytes = core::slice::from_raw_parts(s as *const u8, strlen(s as *const u8));
        serde_json::from_slice::<Value>(bytes).map(|v| value_to_word(&v, trust))
    }
}

/// `json.parse(s)` where nothing is watching: invalid JSON is nil rather than a
/// raise. The one caller is `infer.c` reading a provider's stored config, which
/// runs where no Jade frame exists to catch anything — a malformed config there
/// should leave the provider unconfigured, not abort the program.
///
/// The builtin does not come through here; see [`jrt_json_parse_impl`].
///
/// # Safety
/// `s` is a NUL-terminated tagged-string data pointer (trust at `s[-1]`).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_json_parse_chunk(s: *const c_char) -> W {
    unsafe { parse(s).unwrap_or(NIL_BITS as i64) }
}

// ── The builtin's pending-error channel ───────────────────────────────────────
//
// A longjmp must not cross a Rust frame, so failure is recorded here and the C
// forwarder (`jrt_json_parse` in common.c) raises it. Exactly the shape the fs,
// http and bytes wrappers use.

thread_local! {
    static PENDING: core::cell::Cell<*mut c_char> =
        const { core::cell::Cell::new(core::ptr::null_mut()) };
}

/// `json.parse(s)` for the builtin: the same parse, but a failure is recorded
/// for the forwarder to raise rather than turned into nil.
///
/// Silently answering nil was a real divergence: the VM raises, so a compiled
/// program took the success branch on input the interpreter rejected, and every
/// `try`/`catch` written around a parse stopped running.
///
/// # Safety
/// `s` is a NUL-terminated tagged-string data pointer (trust at `s[-1]`).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_json_parse_impl(s: *const c_char) -> W {
    let (w, err) = match unsafe { parse(s) } {
        Ok(w) => (w, core::ptr::null_mut()),
        // The VM's wording, from `json_parse` in src/json/mod.rs. The `I/O
        // error: ` ahead of it is the IoError display, added by jrt_throw_io.
        Err(e) => (
            NIL_BITS as i64,
            crate::cstr::emit(format!("json.parse: {e}").as_bytes(), crate::string::TRUSTED),
        ),
    };
    // Replaced, not just set on failure: a success has to clear whatever an
    // earlier one left, or the next valid parse raises the last one's error.
    PENDING.with(|p| {
        let old = p.replace(err);
        if !old.is_null() {
            string::free_str(old as *mut u8);
        }
    });
    w
}

/// Record a pending error, replacing any earlier one.
fn set_err(msg: String) {
    let s = crate::cstr::emit(msg.as_bytes(), crate::string::TRUSTED);
    PENDING.with(|p| {
        let old = p.replace(s);
        if !old.is_null() {
            string::free_str(old as *mut u8);
        }
    });
}

/// Drain the pending parse error (a tagged string the caller owns), or null.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_json_take_error() -> *mut c_char {
    PENDING.with(|p| p.replace(core::ptr::null_mut()))
}

/// `json.stringify(v)` / `json.stringify_pretty(v)` (when `pretty != 0`): render
/// the ObjHeader value `v` to a fresh TRUSTED tagged string, using serde's
/// compact / 2-space-pretty formatting so it matches the VM exactly.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_json_stringify_impl(word: W, pretty: i32) -> *mut c_char {
    // Null plus a pending error when the value nests too deep, matching how the
    // parse path reports and how the VM raises. Answering `null` instead would
    // hand back a document that does not say what the value held.
    let Some(v) = word_to_value_at(word, 0) else {
        set_err(format!(
            "json.stringify: value nests deeper than {} levels (a value that contains itself \
             cannot be represented as JSON)",
            crate::render::MAX_DEPTH
        ));
        return core::ptr::null_mut();
    };
    let s = if pretty != 0 { serde_json::to_string_pretty(&v) } else { serde_json::to_string(&v) }
        .unwrap_or_default();
    let bytes = s.as_bytes();
    unsafe {
        let out = string::new(bytes.len(), string::TRUSTED);
        if !bytes.is_empty() {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len());
        }
        out as *mut c_char
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe fn read(p: *const c_char) -> String {
        unsafe {
            let n = strlen(p as *const u8);
            String::from_utf8_lossy(core::slice::from_raw_parts(p as *const u8, n)).into_owned()
        }
    }

    #[test]
    fn roundtrip_object() {
        let _g = crate::gc::test_support::lock_counter();
        let w = jrt_json_parse_chunk(c"{\"b\": 2, \"a\": 1}".as_ptr());
        let s = jrt_json_stringify_impl(w, 0);
        // serde sorts keys.
        assert_eq!(unsafe { read(s) }, r#"{"a":1,"b":2}"#);
        string::free_str(s as *mut u8);
    }

    #[test]
    fn pretty_and_nested() {
        let _g = crate::gc::test_support::lock_counter();
        let w = jrt_json_parse_chunk(c"{\"a\": {\"b\": 7}}".as_ptr());
        let s = jrt_json_stringify_impl(w, 1);
        assert_eq!(unsafe { read(s) }, "{\n  \"a\": {\n    \"b\": 7\n  }\n}");
        string::free_str(s as *mut u8);
    }

    #[test]
    fn malformed_input_records_the_vms_wording() {
        let _g = crate::gc::test_support::lock_counter();
        let w = jrt_json_parse_impl(c"{\"typ".as_ptr());
        assert_eq!(w, NIL_BITS as i64);
        let e = jrt_json_take_error();
        assert!(!e.is_null(), "a parse failure has to leave something to raise");
        // The VM's `json_parse` formats the same prefix; the `I/O error: ` ahead
        // of it is added by jrt_throw_io on the C side.
        assert!(unsafe { read(e) }.starts_with("json.parse: "), "{}", unsafe { read(e) });
        string::free_str(e as *mut u8);
    }

    #[test]
    fn a_good_parse_clears_the_last_ones_error() {
        // Otherwise the failure is raised by whichever call comes next, which
        // is a valid parse reporting an error about input it never saw.
        let _g = crate::gc::test_support::lock_counter();
        jrt_json_parse_impl(c"{\"typ".as_ptr());
        jrt_json_parse_impl(c"{\"a\": 1}".as_ptr());
        assert!(jrt_json_take_error().is_null(), "the good parse should have cleared it");
    }

    #[test]
    fn the_non_raising_entry_still_answers_nil() {
        // infer.c reads a provider's stored config where no Jade frame exists to
        // catch anything, so that path keeps the old behaviour on purpose.
        let _g = crate::gc::test_support::lock_counter();
        assert_eq!(jrt_json_parse_chunk(c"{\"typ".as_ptr()), NIL_BITS as i64);
        assert!(jrt_json_take_error().is_null(), "it must not arm the raising channel");
    }
}
