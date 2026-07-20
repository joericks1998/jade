//! Coercing an LLM response into a struct — the AOT half of `?p |> City`.
//!
//! A typed prompt deref asks the model for a value of a named type and turns
//! the reply into that type. Primitives (`int`, `float`, `bool`, `str`) are
//! validated in C by `infer_valid_type`. **Structs were not**: that validator
//! returns "valid" for any name it does not recognise, so a struct-typed deref
//! accepted the raw reply and handed back a *string*. Field access on it then
//! failed with "value has no fields" — a silent wrong answer, not a decline,
//! and the VM had built a real struct all along.
//!
//! This module builds the struct. It needs something a compiled binary did not
//! have: a table from a type name to its fields, in declaration order, with
//! their defaults. Codegen emits that table at startup, next to the method
//! registry, and it is read-only afterwards — so a plain `Mutex<Vec<…>>` is
//! enough, exactly as in [`crate::methods`].
//!
//! ## The rule, which is the VM's rule
//!
//!  * a **required** field the reply omits → coercion fails, so the caller
//!    re-prompts. A missing required field is the model getting it wrong, not a
//!    value to paper over.
//!  * an **optional** field the reply omits → its declared default, the same as
//!    a struct literal. (The VM used to leave it out of the struct entirely, so
//!    `c.population` raised on a type that declares `population`.)
//!  * fields are set in **declaration order**, never the reply's key order.
//!
//! ## Raising
//!
//! Nothing here raises — a Jade error is a `longjmp` and must not cross a Rust
//! frame. Failure is a nil return, which the C retry loop turns into another
//! attempt and finally into a catchable error.

use core::ffi::c_char;
use std::sync::Mutex;

use crate::coll::{DictObj, StructObj};
use crate::heap::ObjKind;
use crate::sys::strlen;
use crate::value::{JadeValue, NIL_BITS};

type W = i64;

struct Field {
    name: String,
    /// The declared default as a tagged word, or `None` for a required field.
    default: Option<W>,
}

struct TypeEntry {
    name: String,
    fields: Vec<Field>,
}

static REGISTRY: Mutex<Vec<TypeEntry>> = Mutex::new(Vec::new());

#[inline]
unsafe fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe {
        let n = strlen(p as *const u8);
        String::from_utf8_lossy(core::slice::from_raw_parts(p as *const u8, n)).into_owned()
    }
}

/// Declare one field of `type_name`, appended in declaration order.
///
/// `has_default` distinguishes a required field from an optional one whose
/// default happens to be nil — they behave differently when the reply omits the
/// field (fail vs. fill), so a nil word alone cannot carry the distinction.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_struct_field(
    type_name: *const c_char,
    field: *const c_char,
    default_word: W,
    has_default: i32,
) {
    let (t, f) = unsafe { (cstr(type_name), cstr(field)) };
    let entry = Field {
        name: f,
        default: if has_default != 0 { Some(default_word) } else { None },
    };
    if let Ok(mut reg) = REGISTRY.lock() {
        match reg.iter_mut().find(|e| e.name == t) {
            Some(e) => e.fields.push(entry),
            None => reg.push(TypeEntry { name: t, fields: vec![entry] }),
        }
    }
}

/// Coerce the JSON in the tagged string `json` into a struct of `type_name`.
///
/// Returns the tagged struct word, or nil when the reply is not a JSON object,
/// omits a required field, or names an unregistered type. The caller re-prompts
/// on nil.
///
/// # Safety
/// `json` is a NUL-terminated tagged-string data pointer, `type_name` a
/// NUL-terminated C string.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_coerce_struct(json: *const c_char, type_name: *const c_char) -> W {
    let nil = NIL_BITS as W;
    let tn = unsafe { cstr(type_name) };

    let fields = {
        let Ok(reg) = REGISTRY.lock() else { return nil };
        let Some(e) = reg.iter().find(|e| e.name == tn) else { return nil };
        e.fields
            .iter()
            .map(|f| (f.name.clone(), f.default))
            .collect::<Vec<_>>()
    };

    // Reuse the shared JSON parser so a coerced struct's field values are built
    // exactly like `json.parse`'s — same number handling, same trust
    // propagation from the response string.
    let parsed = crate::jsonf::jrt_json_parse_chunk(json);
    let v = JadeValue::from_bits(parsed as u64);
    if !v.is_ptr() {
        return nil;
    }
    let p = v.as_ptr();
    if p.is_null() || unsafe { (*(p as *const crate::heap::ObjHeader)).kind } != ObjKind::Dict as u8
    {
        return nil; // not a JSON object — the reply is the wrong shape
    }
    let dict = unsafe { &*(p as *const DictObj<W>) };

    let mut sobj = StructObj::<W>::new(&tn);
    for (name, default) in &fields {
        let word = match dict.get(name) {
            Some(w) => *w,
            None => match default {
                Some(d) => *d,
                // A required field the model did not supply: fail, so the
                // caller asks again rather than inventing a nil.
                None => return nil,
            },
        };
        crate::gc::jrt_incref(word);
        sobj.set_field(name, word);
    }
    JadeValue::from_ptr(crate::gc::leak_obj(sobj) as *const ()).bits() as W
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    // The registry is process-global, so tests must not share type names.
    fn declare(type_name: &str, fields: &[(&str, Option<W>)]) {
        let tn = CString::new(type_name).unwrap();
        for (f, d) in fields {
            let fc = CString::new(*f).unwrap();
            jrt_struct_field(tn.as_ptr(), fc.as_ptr(), d.unwrap_or(0), d.is_some() as i32);
        }
    }

    fn coerce(json: &str, type_name: &str) -> Option<&'static StructObj<W>> {
        // A tagged string, since jrt_json_parse_chunk reads a trust byte at [-1].
        let raw = crate::string::new(json.len(), 0);
        unsafe { core::ptr::copy_nonoverlapping(json.as_ptr(), raw, json.len()) };
        let tn = CString::new(type_name).unwrap();
        let w = jrt_coerce_struct(raw as *const c_char, tn.as_ptr());
        if w == NIL_BITS as W {
            return None;
        }
        Some(unsafe { &*(JadeValue::from_bits(w as u64).as_ptr() as *const StructObj<W>) })
    }

    fn int_word(i: i64) -> W {
        JadeValue::from_int(i).bits() as W
    }

    #[test]
    fn fields_are_set_in_declaration_order_not_reply_order() {
        declare("C_order", &[("a", None), ("b", None), ("c", None)]);
        let s = coerce(r#"{"c": 3, "a": 1, "b": 2}"#, "C_order").unwrap();
        let names: Vec<&str> = s.fields().iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    // The behaviour that was wrong: an omitted optional field used to be left
    // out of the struct entirely, so reading it raised "no field" on a type
    // that declares one. It takes its declared default, like a literal.
    #[test]
    fn an_omitted_optional_field_takes_its_declared_default() {
        declare("C_default", &[("name", None), ("population", Some(int_word(7)))]);
        let s = coerce(r#"{"name": "Kyoto"}"#, "C_default").unwrap();
        assert_eq!(s.get_field("population"), Some(&int_word(7)));
    }

    // A missing *required* field is the model getting it wrong. Failing here is
    // what drives the caller's retry, so it must not be filled with nil.
    #[test]
    fn an_omitted_required_field_fails_so_the_caller_can_retry() {
        declare("C_required", &[("name", None), ("country", None)]);
        assert!(coerce(r#"{"name": "Kyoto"}"#, "C_required").is_none());
    }

    // A field declared `let x = nil` is optional and must be filled, not
    // treated as required — which is why has_default is a separate flag rather
    // than being inferred from the word.
    #[test]
    fn an_optional_field_defaulting_to_nil_is_still_optional() {
        declare("C_nildefault", &[("a", Some(NIL_BITS as W))]);
        let s = coerce("{}", "C_nildefault").unwrap();
        assert_eq!(s.get_field("a"), Some(&(NIL_BITS as W)));
    }

    #[test]
    fn a_reply_that_is_not_an_object_fails() {
        declare("C_shape", &[("a", None)]);
        assert!(coerce("[1, 2, 3]", "C_shape").is_none());
        assert!(coerce("\"just a string\"", "C_shape").is_none());
        assert!(coerce("not json at all", "C_shape").is_none());
    }

    #[test]
    fn keys_the_type_does_not_declare_are_dropped() {
        declare("C_extra", &[("kept", None)]);
        let s = coerce(r#"{"kept": 1, "stowaway": 2}"#, "C_extra").unwrap();
        assert_eq!(s.fields().len(), 1);
        assert_eq!(s.get_field("stowaway"), None);
    }

    #[test]
    fn an_unregistered_type_fails() {
        assert!(coerce("{}", "C_never_declared").is_none());
    }
}
