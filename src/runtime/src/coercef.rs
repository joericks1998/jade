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
//! This module holds the coercion **rule**, used by both engines, plus the
//! table the compiled path needs to apply it: a type name to its fields, in
//! declaration order, with their defaults. Codegen emits that table at startup
//! next to the method registry and it is read-only afterwards, so a plain
//! `Mutex<Vec<…>>` is enough, exactly as in [`crate::methods`].
//!
//! [`coerce_fields`] is the rule. The engines differ only in how a JSON value
//! becomes an element — a `VmValue` in the interpreter, a tagged word in
//! compiled code — which is a closure parameter. Extraction, field order,
//! defaults and which failures re-prompt are not duplicated.
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

use crate::coll::StructObj;
use crate::cstr;
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
    let (t, f) = unsafe { (cstr::to_string(type_name), cstr::to_string(field)) };
    let entry =
        Field { name: f, default: if has_default != 0 { Some(default_word) } else { None } };
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
    let tn = unsafe { cstr::to_string(type_name) };

    let fields = {
        let Ok(reg) = REGISTRY.lock() else { return nil };
        let Some(e) = reg.iter().find(|e| e.name == tn) else { return nil };
        e.fields
            .iter()
            .map(|f| FieldSpec { name: f.name.clone(), default: f.default })
            .collect::<Vec<_>>()
    };

    // The reply's trust follows its field values: a struct coerced from an
    // untrusted response is made of untrusted parts.
    let trust = crate::string::trust_of(json as *const u8);
    let text = unsafe { cstr::to_string(json) };

    // Extraction, field order, defaults and which failures re-prompt are the
    // shared rule; the only thing supplied here is how a JSON value becomes a
    // tagged word.
    let Ok(pairs) =
        coerce_fields(&text, &fields, |_name, v| Ok(crate::jsonf::value_to_word(v, trust)))
    else {
        return nil;
    };

    let mut sobj = StructObj::<W>::new(&tn);
    for (name, word) in pairs {
        crate::gc::jrt_incref(word);
        sobj.set_field(&name, word);
    }
    JadeValue::from_ptr(crate::gc::leak_obj(sobj) as *const ()).bits() as W
}

// ── The shared rule ──────────────────────────────────────────────────────────
//
// Everything below is written once and used by both engines. What differs
// between them is only how a JSON value becomes an *element*: the interpreter
// builds a `VmValue`, compiled code builds a tagged word. That difference is a
// closure parameter; the rule — extraction, field order, defaults, which
// failures re-prompt — is not.

/// Strip markdown code fences that LLMs often wrap JSON in (``` or ```json).
pub fn extract_json(text: &str) -> String {
    let t = text.trim();
    let inner = t
        .strip_prefix("```json")
        .or_else(|| t.strip_prefix("```"))
        .and_then(|s| s.strip_suffix("```"))
        .map(str::trim);
    let t = inner.unwrap_or(t);
    // Scan forward through every `{` or `[` start position and return the first
    // candidate that is parseable JSON (after optional normalization).
    let bytes = t.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if (bytes[i] == b'{' || bytes[i] == b'[')
            && let Some(end) = find_end(&t[i..])
        {
            let candidate = &t[i..i + end];
            if serde_json::from_str::<serde_json::Value>(candidate).is_ok() {
                return candidate.to_owned();
            }
            // Try normalizing: quote unquoted keys, remove commas inside numbers.
            let normalized = normalize(candidate);
            if serde_json::from_str::<serde_json::Value>(&normalized).is_ok() {
                return normalized;
            }
        }
        i += 1;
    }
    t.to_owned()
}

/// Quote unquoted object keys and strip thousands-separator commas from numbers.
/// Handles the two most common model formatting mistakes: `{key: val}` and `1,000`.
fn normalize(s: &str) -> String {
    let s = quote_keys(s);
    strip_number_commas(&s)
}

fn quote_keys(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 32);
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut in_string = false;
    let mut escape_next = false;
    while i < bytes.len() {
        if escape_next {
            escape_next = false;
            out.push(bytes[i] as char);
            i += 1;
            continue;
        }
        if bytes[i] == b'\\' && in_string {
            escape_next = true;
            out.push('\\');
            i += 1;
            continue;
        }
        if bytes[i] == b'"' {
            in_string = !in_string;
            out.push('"');
            i += 1;
            continue;
        }
        // Outside strings: detect unquoted key (word chars followed by ':').
        if !in_string && (bytes[i].is_ascii_alphabetic() || bytes[i] == b'_') {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &s[start..i];
            // Peek past whitespace to see if ':' follows (and it's not '::').
            let mut j = i;
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            let is_key = j < bytes.len()
                && bytes[j] == b':'
                && (j + 1 >= bytes.len() || bytes[j + 1] != b':');
            if is_key {
                out.push('"');
                out.push_str(word);
                out.push('"');
            } else {
                out.push_str(word);
            }
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn strip_number_commas(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut in_string = false;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' && (i == 0 || bytes[i - 1] != b'\\') {
            in_string = !in_string;
        }
        // Skip comma that sits between two digits outside a string.
        if !in_string
            && bytes[i] == b','
            && i > 0
            && bytes[i - 1].is_ascii_digit()
            && i + 1 < bytes.len()
            && bytes[i + 1].is_ascii_digit()
        {
            i += 1;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Scan `s` for the end of the first top-level JSON object or array, respecting
/// string escapes and nesting. Returns the exclusive byte index after the closing
/// bracket/brace, or `None` if `s` contains no top-level `{` or `[`.
fn find_end(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let start = bytes.iter().position(|&b| b == b'{' || b == b'[')?;
    let (open, close) = if bytes[start] == b'{' { (b'{', b'}') } else { (b'[', b']') };
    let mut depth = 0usize;
    let mut in_string = false;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if in_string => {
                i += 2;
            }
            b'"' => {
                in_string = !in_string;
                i += 1;
            }
            b if !in_string && b == open => {
                depth += 1;
                i += 1;
            }
            b if !in_string && b == close => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {
                i += 1;
            }
        }
    }
    None
}

/// One declared field of a struct, as the coercion needs to see it.
///
/// `default` is what makes a field optional: `None` means required, and a
/// required field the reply omits is the model getting it wrong.
pub struct FieldSpec<T> {
    pub name: String,
    pub default: Option<T>,
}

/// Why a reply could not become a struct. Each maps to a different correction
/// the caller can put to the model.
#[derive(Debug, PartialEq, Eq)]
pub enum CoerceError {
    /// The reply had no parseable JSON in it at all.
    NotJson(String),
    /// It parsed, but to an array/number/string rather than an object.
    NotObject,
    /// A field with no default was absent.
    MissingRequired(String),
    /// A field was present but its value could not be converted.
    BadField { name: String, detail: String },
}

/// Coerce a model reply into a struct's fields.
///
/// The reply is *extracted* first ([`extract_json`]), because models wrap JSON
/// in prose and code fences — "Sure! Here you go: {...}" is the normal case,
/// not an edge one. Compiled code used to parse the reply verbatim and so
/// failed on exactly the replies the interpreter handled.
///
/// Returns the fields in **declaration order**, which is the order they will be
/// set on the struct and therefore the order they render in.
///
/// `convert` builds one element from one JSON value, and is the only part
/// either engine supplies: it receives the field name so a caller can treat
/// particular fields specially (the interpreter's `prompt` fields become
/// prompts rather than strings).
pub fn coerce_fields<T, F>(
    text: &str,
    fields: &[FieldSpec<T>],
    convert: F,
) -> Result<Vec<(String, T)>, CoerceError>
where
    T: Clone,
    F: Fn(&str, &serde_json::Value) -> Result<T, String>,
{
    let raw = extract_json(text);
    let json: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| CoerceError::NotJson(e.to_string()))?;
    let obj = json.as_object().ok_or(CoerceError::NotObject)?;

    let mut out = Vec::with_capacity(fields.len());
    for f in fields {
        match obj.get(f.name.as_str()) {
            Some(v) => {
                let val = convert(&f.name, v)
                    .map_err(|detail| CoerceError::BadField { name: f.name.clone(), detail })?;
                out.push((f.name.clone(), val));
            }
            // Absent and optional: take the declared default, exactly as a
            // struct literal does. Absent and required: fail, so the caller
            // re-prompts rather than inventing a value.
            None => match &f.default {
                Some(d) => out.push((f.name.clone(), d.clone())),
                None => return Err(CoerceError::MissingRequired(f.name.clone())),
            },
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    // Every test below allocates through `gc::leak_obj`, which bumps the
    // process-global live count. Cargo runs the crate's tests on many threads in
    // one process, so an unlocked allocation here races every count assertion
    // elsewhere in the binary — see `gc::test_support::lock_counter`.
    fn counted() -> std::sync::MutexGuard<'static, ()> {
        crate::gc::test_support::lock_counter()
    }

    // ── the shared rule ──────────────────────────────────────────────────────
    //
    // These exercise `coerce_fields` directly, with a trivial element type, so
    // the structural rule is tested independently of either engine's values.

    fn spec(name: &str, default: Option<i32>) -> FieldSpec<i32> {
        FieldSpec { name: name.to_string(), default }
    }

    fn run(text: &str, fields: &[FieldSpec<i32>]) -> Result<Vec<(String, i32)>, CoerceError> {
        coerce_fields(text, fields, |name, v| {
            v.as_i64().map(|n| n as i32).ok_or_else(|| format!("{name} is not a number"))
        })
    }

    // Models wrap JSON in prose constantly; this is the normal case, and the
    // compiled path used to fail on all of it.
    #[test]
    fn json_is_extracted_from_surrounding_prose() {
        let _c = counted();
        let got = run(r#"Sure! Here you go: {"a": 1} — hope that helps"#, &[spec("a", None)]);
        assert_eq!(got.unwrap(), vec![("a".to_string(), 1)]);
    }

    #[test]
    fn json_is_extracted_from_a_markdown_fence() {
        let _c = counted();
        let got = run("```json\n{\"a\": 1}\n```", &[spec("a", None)]);
        assert_eq!(got.unwrap(), vec![("a".to_string(), 1)]);
    }

    // Two formatting mistakes common enough to be worth repairing rather than
    // re-prompting over: unquoted keys and thousands separators.
    #[test]
    fn common_malformations_are_repaired() {
        let _c = counted();
        assert_eq!(run(r#"{a: 1}"#, &[spec("a", None)]).unwrap(), vec![("a".to_string(), 1)]);
        assert_eq!(
            run(r#"{"a": 1,000}"#, &[spec("a", None)]).unwrap(),
            vec![("a".to_string(), 1000)]
        );
    }

    #[test]
    fn fields_come_back_in_declaration_order() {
        let _c = counted();
        let got = run(
            r#"{"c": 3, "a": 1, "b": 2}"#,
            &[spec("a", None), spec("b", None), spec("c", None)],
        )
        .unwrap();
        let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    #[test]
    fn an_omitted_optional_field_takes_its_default() {
        let _c = counted();
        let got = run(r#"{"a": 1}"#, &[spec("a", None), spec("b", Some(7))]).unwrap();
        assert_eq!(got, vec![("a".to_string(), 1), ("b".to_string(), 7)]);
    }

    // Each failure is a different correction to put to the model, so they stay
    // distinguishable rather than collapsing into one "bad reply".
    #[test]
    fn each_failure_is_distinguishable() {
        let _c = counted();
        assert!(matches!(run("no json here", &[spec("a", None)]), Err(CoerceError::NotJson(_))));
        assert_eq!(run("[1, 2]", &[spec("a", None)]).unwrap_err(), CoerceError::NotObject);
        assert_eq!(
            run(r#"{"b": 1}"#, &[spec("a", None)]).unwrap_err(),
            CoerceError::MissingRequired("a".to_string())
        );
        assert!(matches!(
            run(r#"{"a": "not a number"}"#, &[spec("a", None)]),
            Err(CoerceError::BadField { .. })
        ));
    }

    // Keys the type does not declare are dropped: the type decides the shape,
    // not the reply.
    #[test]
    fn undeclared_keys_are_dropped() {
        let _c = counted();
        let got = run(r#"{"a": 1, "stowaway": 2}"#, &[spec("a", None)]).unwrap();
        assert_eq!(got.len(), 1);
    }

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
        let _c = counted();
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
        let _c = counted();
        declare("C_default", &[("name", None), ("population", Some(int_word(7)))]);
        let s = coerce(r#"{"name": "Kyoto"}"#, "C_default").unwrap();
        assert_eq!(s.get_field("population"), Some(&int_word(7)));
    }

    // A missing *required* field is the model getting it wrong. Failing here is
    // what drives the caller's retry, so it must not be filled with nil.
    #[test]
    fn an_omitted_required_field_fails_so_the_caller_can_retry() {
        let _c = counted();
        declare("C_required", &[("name", None), ("country", None)]);
        assert!(coerce(r#"{"name": "Kyoto"}"#, "C_required").is_none());
    }

    // A field declared `let x = nil` is optional and must be filled, not
    // treated as required — which is why has_default is a separate flag rather
    // than being inferred from the word.
    #[test]
    fn an_optional_field_defaulting_to_nil_is_still_optional() {
        let _c = counted();
        declare("C_nildefault", &[("a", Some(NIL_BITS as W))]);
        let s = coerce("{}", "C_nildefault").unwrap();
        assert_eq!(s.get_field("a"), Some(&(NIL_BITS as W)));
    }

    #[test]
    fn a_reply_that_is_not_an_object_fails() {
        let _c = counted();
        declare("C_shape", &[("a", None)]);
        assert!(coerce("[1, 2, 3]", "C_shape").is_none());
        assert!(coerce("\"just a string\"", "C_shape").is_none());
        assert!(coerce("not json at all", "C_shape").is_none());
    }

    #[test]
    fn keys_the_type_does_not_declare_are_dropped() {
        let _c = counted();
        declare("C_extra", &[("kept", None)]);
        let s = coerce(r#"{"kept": 1, "stowaway": 2}"#, "C_extra").unwrap();
        assert_eq!(s.fields().len(), 1);
        assert_eq!(s.get_field("stowaway"), None);
    }

    #[test]
    fn an_unregistered_type_fails() {
        let _c = counted();
        assert!(coerce("{}", "C_never_declared").is_none());
    }
}
