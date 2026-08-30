//! The single value-display implementation, shared by both engines.
//!
//! Rendering a Jade value to its user-visible text used to exist twice: the VM's
//! `value_to_display` (Rust, over `VmValue`) and the C runtime's
//! `jrt_snprintf_any`/`jrt_render_any` (over tagged words). Every divergence
//! between them — a float that printed `3` in one and `3.0` in the other, a dict
//! whose keys sorted differently — was a difftest failure reconciled after the
//! fact.
//!
//! This module is the fix. The divergence-prone primitives live here once:
//!
//!  * [`format_float`] — the shortest round-tripping decimal, with the trailing
//!    `.0` on integer-valued floats;
//!  * [`render_array`] / [`render_dict`] — the `[a, b]` and sorted, key-quoted
//!    `{"k": v}` framing.
//!
//! Both engines call these. The VM's `value_to_display` recurses over `VmValue`
//! and hands each rendered child to [`render_array`]/[`render_dict`]; the AOT
//! side uses [`render_word`], which interprets a tagged word and recurses over
//! the shared [`crate::coll`] payloads. Only the per-engine *iteration* differs;
//! the formatting rules are shared, so they cannot drift.

use crate::coll::{ArrayObj, DictObj};
use crate::heap::{ObjHeader, ObjKind};
use crate::value::JadeValue;

// ── Shared formatting primitives (called by BOTH engines) ─────────────────────

/// Format an `f64` exactly as the VM's `value_to_display` does: Rust's `{}`
/// (shortest decimal that round-trips), then a trailing `.0` for an
/// integer-valued float so `3.0` never prints as `3`. `NaN`/`inf`/`-inf` are
/// left as Rust prints them (they contain non-digit chars, so no `.0`).
pub fn format_float(f: f64) -> String {
    let s = format!("{}", f);
    if s.chars().all(|c| c.is_ascii_digit() || c == '-') { format!("{}.0", s) } else { s }
}

/// Frame already-rendered array elements as `[a, b, c]`.
pub fn render_array(parts: &[String]) -> String {
    format!("[{}]", parts.join(", "))
}

/// Render a binary blob as `b"…"` with non-printable octets escaped.
///
/// Printing raw bytes to a terminal is a bad default: a blob can contain
/// control characters, an escape sequence that reconfigures the terminal, or a
/// lone 0xFF that is not valid UTF-8 at all. So `print(b)` shows an escaped,
/// unambiguous form. Use `decode()` when the bytes really are text and you want
/// to see it.
pub fn render_bytes(data: &[u8]) -> String {
    let mut out = String::from("b\"");
    for &c in data {
        match c {
            b'\\' => out.push_str("\\\\"),
            b'"' => out.push_str("\\\""),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0x20..=0x7e => out.push(c as char),
            _ => out.push_str(&format!("\\x{c:02x}")),
        }
    }
    out.push('"');
    out
}

/// Frame already-rendered dict entries as `{"k": v, ...}` with keys sorted
/// ascending and quoted Rust-`{:?}`-style. `entries` is `(key, rendered_value)`;
/// it is sorted in place by key. Matches the VM (`sort_by_key(k)` +
/// `format!("{:?}: {}", k, v)`) and the former C `jk_render`.
pub fn render_dict(entries: &mut [(String, String)]) -> String {
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let parts: Vec<String> =
        entries.iter().map(|(k, v)| format!("{}: {}", quote_key(k), v)).collect();
    format!("{{{}}}", parts.join(", "))
}

/// Quote a dict key as a Rust `{:?}` string literal (`"…"` with `"`, `\`, `\n`,
/// `\t`, `\r`, and other control chars escaped). Identical to what the VM's
/// `format!("{:?}", k)` produces, so AOT and VM key rendering match exactly.
#[inline]
pub fn quote_key(k: &str) -> String {
    format!("{:?}", k)
}

// ── AOT word renderer (interprets a tagged word over `coll` payloads) ─────────

/// Render a tagged value word to its display string, recursing into shared
/// collection payloads. This is the AOT side's single renderer, replacing the C
/// `jrt_render_any`/`jk_render`. Scalars use the shared primitives above, so an
/// AOT-rendered value is byte-identical to the VM rendering of the same value.
///
/// # Safety
/// A `TAG_PTR` word must point at a live [`ObjHeader`]-prefixed object allocated
/// by this crate (array/dict/struct), and a `TAG_STR` word at a NUL-terminated
/// tagged string. That is the AOT ABI invariant; violating it is UB.
pub fn render_word(word: i64) -> String {
    let v = JadeValue::from_bits(word as u64);
    if v.is_int() {
        return v.as_int().to_string();
    }
    if v.is_bool() {
        return if v.as_bool() { "true" } else { "false" }.to_string();
    }
    if v.is_nil() {
        return "nil".to_string();
    }
    // Must come before the heap checks and after `is_nil`: a char is an
    // immediate sharing the nil branch of the tag space.
    if v.is_char() {
        return v.as_char().map(String::from).unwrap_or_default();
    }
    if v.is_float() {
        return format_float(crate::float::unbox_float(v));
    }
    if v.is_str() {
        return unsafe { crate::cstr::to_string(v.as_ptr() as *const core::ffi::c_char) };
    }
    // Non-string heap pointer: dispatch on the object's kind.
    debug_assert!(v.is_ptr());
    let p = v.as_ptr();
    let kind = unsafe { (*(p as *const ObjHeader)).kind };
    if kind == ObjKind::Bytes as u8 {
        let b = unsafe { &*(p as *const crate::bytesf::BytesObj) };
        return render_bytes(b.as_slice());
    }
    if kind == ObjKind::Handle as u8 {
        let h = unsafe { &*(p as *const crate::handle::HandleObj) };
        return crate::handle::render(h);
    }
    if kind == ObjKind::Array as u8 {
        let a = unsafe { &*(p as *const ArrayObj<i64>) };
        let parts: Vec<String> = a.as_slice().iter().map(|w| render_word(*w)).collect();
        render_array(&parts)
    } else if kind == ObjKind::Dict as u8 {
        let d = unsafe { &*(p as *const DictObj<i64>) };
        let mut entries: Vec<(String, String)> =
            d.entries().iter().map(|(k, w)| (k.clone(), render_word(*w))).collect();
        render_dict(&mut entries)
    } else if kind == ObjKind::Struct as u8 {
        // The VM renders any struct opaquely as `<struct>` (no field walk).
        "<struct>".to_string()
    } else if kind == ObjKind::Future as u8 {
        // Matches the VM (`VmValue::Future` renders as `<future>`); the parity
        // gate diffs stdout, so this string is a contract, not a cosmetic.
        "<future>".to_string()
    } else if kind == ObjKind::Prompt as u8 {
        // Matches the VM (`VmValue::Prompt` renders as `<prompt>`). A prompt is
        // opaque on purpose: it is a type you dereference, not text you read. The
        // AOT used to hold a prompt as its bare string, so `print(p)` printed the
        // prompt's text here and `<prompt>` under `jade run`.
        "<prompt>".to_string()
    } else if kind == ObjKind::Grammar as u8 {
        // Matches the VM (`VmValue::Grammar` renders as `<grammar>`). Opaque on
        // both engines: a grammar is a constraint you attach to a deref, not
        // text to read back.
        "<grammar>".to_string()
    } else if kind == ObjKind::BoundMethod as u8 {
        // Matches the VM's `VmValue::BoundMethod`.
        "<bound method>".to_string()
    } else if kind == ObjKind::Fn as u8 {
        // A callable box: {entry@0, kind@8, …}. The byte above the ObjKind says
        // which sort, because all three shapes share the same ObjKind — see the
        // JRT_FN_* notes in runtime_aot/runtime.h.
        let kw = unsafe { *(p.byte_add(8) as *const i64) };
        // The name a builtin/typeref box carries at offset 16.
        let boxed_name = || -> Option<String> {
            let n = unsafe { *(p.byte_add(16) as *const *const core::ffi::c_char) };
            if n.is_null() { None } else { Some(unsafe { crate::cstr::to_string(n) }) }
        };
        match (kw >> 8) & 0xff {
            // A core builtin, carrying its name at offset 16.
            1 => match boxed_name() {
                Some(n) => format!("<builtin {n}>"),
                None => "<fn>".to_string(),
            },
            // A primitive type constructor. The interpreter holds these as type
            // references rather than functions, and prints them that way.
            3 => match boxed_name() {
                Some(n) => format!("<type {n}>"),
                None => "<fn>".to_string(),
            },
            // `print`, which the interpreter models as a native fn and prints
            // without naming.
            4 => "<native fn>".to_string(),
            // A native package binding: offset 16 is its env, whose second word
            // is the bound name.
            2 => {
                let env = unsafe { *(p.byte_add(16) as *const *const *const core::ffi::c_char) };
                if env.is_null() {
                    "<native lib fn>".to_string()
                } else {
                    let name = unsafe { *env.add(1) };
                    format!("<native lib fn {}>", unsafe { crate::cstr::to_string(name) })
                }
            }
            _ => "<fn>".to_string(),
        }
    } else {
        // A heap kind with no renderer of its own.
        "<object>".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coll::{ArrayObj, DictObj, StructObj};

    fn int_word(i: i64) -> i64 {
        JadeValue::from_int(i).bits() as i64
    }

    // Box a collection to a TAG_PTR word for rendering, returning the raw box so
    // the test can reclaim it (avoids a leak under Miri/ASan).
    fn ptr_word<T>(b: Box<T>) -> (i64, *mut T) {
        let raw = Box::into_raw(b);
        (JadeValue::from_ptr(raw as *const ()).bits() as i64, raw)
    }

    #[test]
    fn float_formatting_matches_vm() {
        assert_eq!(format_float(3.0), "3.0"); // integer-valued gets .0
        assert_eq!(format_float(3.5), "3.5");
        assert_eq!(format_float(-2.0), "-2.0");
        assert_eq!(format_float(0.1), "0.1");
        assert_eq!(format_float(f64::NAN), "NaN");
        assert_eq!(format_float(f64::INFINITY), "inf");
        assert_eq!(format_float(f64::NEG_INFINITY), "-inf");
    }

    /// Never scientific notation, whatever the magnitude.
    ///
    /// The AOT runtime used to format floats in C with `"%.*g"` at the fewest
    /// digits that round-tripped, and `%g` switches to exponent form as soon as
    /// the exponent reaches the precision — precisely when a float needs
    /// trailing zeros before the decimal point. So a compiled binary printed
    /// `1e+01` for `10.0` and `4.8e+04` for `48000.0`, which is to say sample
    /// rates, byte counts and durations were the values that broke, while
    /// `1024.0` and `10.5` looked fine. Both engines now share this function.
    #[test]
    fn integer_valued_floats_never_go_scientific() {
        assert_eq!(format_float(10.0), "10.0");
        assert_eq!(format_float(100.0), "100.0");
        assert_eq!(format_float(110.0), "110.0");
        assert_eq!(format_float(48000.0), "48000.0");
        assert_eq!(format_float(44100.0), "44100.0");
        assert_eq!(format_float(-10.0), "-10.0");
        // The neighbours that always worked, so a regression is visible as a
        // change rather than as a whole category going quiet.
        assert_eq!(format_float(1024.0), "1024.0");
        assert_eq!(format_float(10.5), "10.5");
        assert_eq!(format_float(0.001), "0.001");
    }

    /// Float text has no length bound, which is why `print` and `str` render it
    /// on the heap rather than into a fixed scratch buffer: 1e300 is 301 digits
    /// and the old AOT path formatted scalars into 64 bytes.
    #[test]
    fn a_large_float_expands_in_full() {
        let s = format_float(1e300);
        assert!(s.len() > 64, "expected full expansion, got {} chars", s.len());
        assert!(s.starts_with('1'), "{s}");
        assert!(!s.contains('e'), "no exponent form: {s}");
    }

    #[test]
    fn scalar_words_render() {
        assert_eq!(render_word(int_word(42)), "42");
        assert_eq!(render_word(int_word(-7)), "-7");
        assert_eq!(render_word(crate::value::TRUE.bits() as i64), "true");
        assert_eq!(render_word(crate::value::FALSE.bits() as i64), "false");
        assert_eq!(render_word(crate::value::NIL.bits() as i64), "nil");
        assert_eq!(render_word(crate::float::box_float(1.5).bits() as i64), "1.5");
    }

    #[test]
    fn array_word_renders_recursively() {
        let mut inner = ArrayObj::<i64>::new();
        inner.push(int_word(1));
        inner.push(int_word(2));
        let (inner_w, inner_raw) = ptr_word(Box::new(inner));

        let mut outer = ArrayObj::<i64>::new();
        outer.push(inner_w);
        outer.push(int_word(3));
        let (outer_w, outer_raw) = ptr_word(Box::new(outer));

        assert_eq!(render_word(outer_w), "[[1, 2], 3]");

        unsafe {
            drop(Box::from_raw(outer_raw));
            drop(Box::from_raw(inner_raw));
        }
    }

    #[test]
    fn dict_word_renders_sorted_and_quoted() {
        let mut d = DictObj::<i64>::new();
        d.set("b", int_word(2));
        d.set("a", int_word(1)); // inserted second, must sort first
        let (w, raw) = ptr_word(Box::new(d));
        assert_eq!(render_word(w), r#"{"a": 1, "b": 2}"#);
        unsafe { drop(Box::from_raw(raw)) };
    }

    #[test]
    fn struct_word_is_opaque() {
        let s = StructObj::<i64>::new("Point");
        let (w, raw) = ptr_word(Box::new(s));
        assert_eq!(render_word(w), "<struct>");
        unsafe { drop(Box::from_raw(raw)) };
    }
}
