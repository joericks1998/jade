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

/// A tagged string word holding `bytes` with the given trust.
unsafe fn str_word(bytes: &[u8], trust: u8) -> W {
    let out = string::new(bytes.len(), trust);
    if !bytes.is_empty() {
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len()) };
    }
    JadeValue::from_str_ptr(out as *const ()).bits() as i64
}

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
        Value::String(s) => unsafe { str_word(s.as_bytes(), trust) },
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
    let v = JadeValue::from_bits(word as u64);
    if v.is_int() {
        return Value::Number(v.as_int().into());
    }
    if v.is_bool() {
        return Value::Bool(v.as_bool());
    }
    if v.is_nil() {
        return Value::Null;
    }
    if v.is_float() {
        return serde_json::Number::from_f64(unbox_float(v)).map(Value::Number).unwrap_or(Value::Null);
    }
    if v.is_str() {
        let bytes = unsafe {
            let p = v.as_ptr() as *const u8;
            core::slice::from_raw_parts(p, strlen(p))
        };
        return Value::String(String::from_utf8_lossy(bytes).into_owned());
    }
    // Non-string heap pointer: dispatch on kind.
    let p = v.as_ptr();
    let kind = unsafe { (*(p as *const ObjHeader)).kind };
    if kind == ObjKind::Array as u8 {
        let a = unsafe { &*(p as *const ArrayObj<W>) };
        Value::Array(a.as_slice().iter().map(|w| word_to_value(*w)).collect())
    } else if kind == ObjKind::Dict as u8 {
        let d = unsafe { &*(p as *const DictObj<W>) };
        // Collect into serde's Map (sorted keys), matching the VM.
        let m: serde_json::Map<String, Value> =
            d.entries().iter().map(|(k, w)| (k.clone(), word_to_value(*w))).collect();
        Value::Object(m)
    } else {
        Value::Null
    }
}

/// `json.parse(s)`: parse the (tagged) JSON string `s` into an ObjHeader value
/// tree. Invalid JSON → nil (the difftest suite only parses valid JSON; a raising
/// variant would need a C forwarder). Parsed strings inherit `s`'s trust byte.
///
/// # Safety
/// `s` is a NUL-terminated tagged-string data pointer (trust at `s[-1]`).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_json_parse_chunk(s: *const c_char) -> W {
    unsafe {
        let trust = string::trust_of(s as *const u8);
        let bytes = core::slice::from_raw_parts(s as *const u8, strlen(s as *const u8));
        match serde_json::from_slice::<Value>(bytes) {
            Ok(v) => value_to_word(&v, trust),
            Err(_) => NIL_BITS as i64,
        }
    }
}

/// `json.stringify(v)` / `json.stringify_pretty(v)` (when `pretty != 0`): render
/// the ObjHeader value `v` to a fresh TRUSTED tagged string, using serde's
/// compact / 2-space-pretty formatting so it matches the VM exactly.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_json_stringify_chunk(word: W, pretty: i32) -> *mut c_char {
    let v = word_to_value(word);
    let s = if pretty != 0 {
        serde_json::to_string_pretty(&v)
    } else {
        serde_json::to_string(&v)
    }
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
        let n = strlen(p as *const u8);
        String::from_utf8_lossy(core::slice::from_raw_parts(p as *const u8, n)).into_owned()
    }

    #[test]
    fn roundtrip_object() {
        let _g = crate::gc::test_support::lock_counter();
        let w = jrt_json_parse_chunk(b"{\"b\": 2, \"a\": 1}\0".as_ptr() as *const c_char);
        let s = jrt_json_stringify_chunk(w, 0);
        // serde sorts keys.
        assert_eq!(unsafe { read(s) }, r#"{"a":1,"b":2}"#);
        unsafe { string::free_str(s as *mut u8) };
    }

    #[test]
    fn pretty_and_nested() {
        let _g = crate::gc::test_support::lock_counter();
        let w = jrt_json_parse_chunk(b"{\"a\": {\"b\": 7}}\0".as_ptr() as *const c_char);
        let s = jrt_json_stringify_chunk(w, 1);
        assert_eq!(unsafe { read(s) }, "{\n  \"a\": {\n    \"b\": 7\n  }\n}");
        unsafe { string::free_str(s as *mut u8) };
    }
}
