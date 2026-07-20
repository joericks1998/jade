//! First-class types (`TypeRef`) for the AOT backend.
//!
//! A Jade type name is a *value* that can be called with one argument to
//! coerce or construct: `int("3")` → `3`, `City(dict)` → a `City` struct. The
//! VM has always had this (`VmValue::TypeRef` → `vm_type_call`). The primitive
//! coercions already lower in AOT as devirtualized builtin calls
//! (`jrt_int_any`, `jrt_bool_any`, …); what was missing is the **user-struct**
//! arm, which needs something the compiled binary did not have: a runtime table
//! mapping a type name to its field list in definition order.
//!
//! This module is that table. It mirrors [`crate::methods`] exactly — populated
//! once before `main`'s body by codegen-emitted [`jrt_type_register`] calls,
//! read-only thereafter, so a plain `Mutex<Vec<…>>` suffices.
//!
//! ## Why a runtime table rather than inline IR
//!
//! Field lists *are* known at compile time, so `City(d)` could be emitted as a
//! straight-line sequence of `jrt_coll_dict_get` + `jrt_kstruct_set` pairs. A
//! table was chosen instead because it puts the construction *semantics* — field
//! order, missing-field-is-nil, the same-type passthrough, the error text — in
//! one Rust function that reads like `vm_type_call`'s struct arm, rather than
//! spread across an IR emitter where drift from the VM is invisible. That is the
//! whole premise of the VmValue sunset.
//!
//! ## Raising
//!
//! Nothing here raises: a Jade error is a `longjmp`, which must not cross a Rust
//! frame. [`jrt_type_construct`] reports failure as a `0` return and the thin C
//! forwarder `jrt_type_call` (in `runtime_lib/common.c`) owns the `throw_msg` —
//! the same split `ffi_coll` uses.

use core::ffi::c_char;
use std::sync::Mutex;

use crate::coll::{DictObj, StructObj};
use crate::heap::ObjKind;
use crate::sys::strlen;
use crate::value::{JadeValue, NIL};

struct TypeEntry {
    name: String,
    /// Field names in **definition order**. Order is observable: it is the
    /// order a constructed struct renders its fields in.
    fields: Vec<String>,
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

/// Register that struct type `type_name` has a field `field`, appending it to
/// that type's field list. Codegen emits one call per field, in definition
/// order, at startup.
///
/// A type with no fields still needs one call to exist in the table — codegen
/// emits [`jrt_type_declare`] for that case.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_type_register(type_name: *const c_char, field: *const c_char) {
    let (t, f) = unsafe { (cstr(type_name), cstr(field)) };
    if let Ok(mut reg) = REGISTRY.lock() {
        match reg.iter_mut().find(|e| e.name == t) {
            Some(e) => e.fields.push(f),
            None => reg.push(TypeEntry { name: t, fields: vec![f] }),
        }
    }
}

/// Register a struct type that has no fields. See [`jrt_type_register`].
#[unsafe(no_mangle)]
pub extern "C" fn jrt_type_declare(type_name: *const c_char) {
    let t = unsafe { cstr(type_name) };
    if let Ok(mut reg) = REGISTRY.lock() {
        if !reg.iter().any(|e| e.name == t) {
            reg.push(TypeEntry { name: t, fields: Vec::new() });
        }
    }
}

/// Look up a registered type's field list.
fn fields_of(type_name: &str) -> Option<Vec<String>> {
    let reg = REGISTRY.lock().ok()?;
    reg.iter().find(|e| e.name == type_name).map(|e| e.fields.clone())
}

/// The struct arm of a type call: `City(x)`.
///
/// Mirrors `vm_type_call`'s fallthrough (`src/compiler/vm.rs`):
///
///  * `x` is a **dict** → a fresh struct with every declared field set to
///    `x[field]`, or `nil` where the dict has no such key. Fields are taken in
///    definition order and dict keys the type does not declare are dropped.
///  * `x` is already a struct of **this same type** → returned unchanged. Note
///    this is an identity, not a copy: the VM returns the same `Arc`, so
///    mutations through either binding are shared, and AOT returns the same
///    pointer.
///  * anything else → failure.
///
/// Returns `1` and writes the result word to `out` on success. On failure it
/// writes nothing and returns `0` (the argument is not constructible) or `-1`
/// (no such type is registered) so the forwarder can pick the same message the
/// VM would. Never raises.
///
/// # Safety
/// `type_name` must be a valid NUL-terminated C string and `out` a valid
/// writable `i64`.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_type_construct(type_name: *const c_char, arg: i64, out: *mut i64) -> i32 {
    let tn = unsafe { cstr(type_name) };
    let Some(fields) = fields_of(&tn) else {
        return -1; // unknown type — a distinct message in the forwarder
    };

    let v = JadeValue::from_bits(arg as u64);
    if !v.is_ptr() {
        return 0;
    }
    let p = v.as_ptr();
    if p.is_null() {
        return 0;
    }
    let kind = unsafe { (*(p as *const crate::heap::ObjHeader)).kind };

    if kind == ObjKind::Struct as u8 {
        // Same-type passthrough; a *different* struct type is an error, exactly
        // as in the VM (`s.lock().type_name() == name`).
        let s = unsafe { &*(p as *const StructObj<i64>) };
        if s.type_name() == tn {
            unsafe { *out = arg };
            return 1;
        }
        return 0;
    }

    if kind != ObjKind::Dict as u8 {
        return 0;
    }

    let d = unsafe { &*(p as *const DictObj<i64>) };
    let mut sobj = StructObj::<i64>::new(&tn);
    for f in &fields {
        // A field the dict does not carry becomes nil — the VM's
        // `map.get(fname).cloned().unwrap_or(VmValue::Nil)`.
        let w = d.get(f).copied().unwrap_or(NIL.bits() as i64);
        // The struct takes a reference to every collection it adopts; the dict
        // still holds its own. Without this, freeing the dict would leave the
        // struct pointing at reclaimed memory. A no-op for non-collection words
        // (ints, strings, floats) — see the `is_collection` gate in `gc`.
        crate::gc::jrt_incref(w);
        sobj.set_field(f, w);
    }
    let p = crate::gc::leak_obj(sobj) as *const ();
    unsafe { *out = JadeValue::from_ptr(p).bits() as i64 };
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    // The registry is process-global, so tests must not share type names.
    fn reg(name: &str, fields: &[&str]) {
        let tn = CString::new(name).unwrap();
        if fields.is_empty() {
            jrt_type_declare(tn.as_ptr());
            return;
        }
        for f in fields {
            let fc = CString::new(*f).unwrap();
            jrt_type_register(tn.as_ptr(), fc.as_ptr());
        }
    }

    fn construct(name: &str, arg: i64) -> Option<i64> {
        let tn = CString::new(name).unwrap();
        let mut out = 0i64;
        if jrt_type_construct(tn.as_ptr(), arg, &mut out) == 1 { Some(out) } else { None }
    }

    fn status(name: &str, arg: i64) -> i32 {
        let tn = CString::new(name).unwrap();
        let mut out = 0i64;
        jrt_type_construct(tn.as_ptr(), arg, &mut out)
    }

    /// Build a dict of tagged int values. Values must be *tagged* words, not
    /// raw integers — a raw `1` carries TAG_PTR and would be dereferenced.
    fn dict_word(pairs: &[(&str, i64)]) -> i64 {
        let mut d = DictObj::<i64>::new();
        for (k, v) in pairs {
            d.insert(*k, int_word(*v));
        }
        JadeValue::from_ptr(crate::gc::leak_obj(d) as *const ()).bits() as i64
    }

    /// Like `dict_word` but the values are already tagged words.
    fn dict_word_raw(pairs: &[(&str, i64)]) -> i64 {
        let mut d = DictObj::<i64>::new();
        for (k, v) in pairs {
            d.insert(*k, *v);
        }
        JadeValue::from_ptr(crate::gc::leak_obj(d) as *const ()).bits() as i64
    }

    fn int_word(i: i64) -> i64 {
        JadeValue::from_int(i).bits() as i64
    }

    fn as_struct(w: i64) -> &'static StructObj<i64> {
        unsafe { &*(JadeValue::from_bits(w as u64).as_ptr() as *const StructObj<i64>) }
    }

    #[test]
    fn a_dict_becomes_a_struct_with_fields_in_definition_order() {
        reg("T_order", &["a", "b", "c"]);
        // Deliberately supplied out of order: the *type* decides field order.
        let d = dict_word(&[("c", 30), ("a", 10), ("b", 20)]);
        let s = as_struct(construct("T_order", d).unwrap());
        let names: Vec<&str> = s.fields().iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    #[test]
    fn a_field_the_dict_lacks_becomes_nil() {
        reg("T_missing", &["present", "absent"]);
        let d = dict_word(&[("present", 42)]);
        let s = as_struct(construct("T_missing", d).unwrap());
        assert_eq!(s.get_field("present"), Some(&int_word(42)));
        assert_eq!(s.get_field("absent"), Some(&(NIL.bits() as i64)));
    }

    // The dict is the *source*, not the shape: keys the type never declared are
    // dropped rather than becoming extra fields.
    #[test]
    fn dict_keys_the_type_does_not_declare_are_dropped() {
        reg("T_extra", &["kept"]);
        let d = dict_word(&[("kept", 1), ("stowaway", 2)]);
        let s = as_struct(construct("T_extra", d).unwrap());
        assert_eq!(s.fields().len(), 1);
        assert_eq!(s.get_field("stowaway"), None);
    }

    #[test]
    fn a_struct_of_the_same_type_passes_through_by_identity() {
        reg("T_same", &["x"]);
        let first = construct("T_same", dict_word(&[("x", 7)])).unwrap();
        assert_eq!(construct("T_same", first), Some(first));
    }

    // Passthrough is same-type only — this is what stops `Dog(a_cat)` from
    // silently relabelling a value.
    #[test]
    fn a_struct_of_a_different_type_is_rejected() {
        reg("T_dog", &["legs"]);
        reg("T_cat", &["legs"]);
        let dog = construct("T_dog", dict_word(&[("legs", 4)])).unwrap();
        assert_eq!(construct("T_cat", dog), None);
    }

    #[test]
    fn a_type_with_no_fields_constructs_empty() {
        reg("T_unit", &[]);
        let s = as_struct(construct("T_unit", dict_word(&[("ignored", 1)])).unwrap());
        assert_eq!(s.fields().len(), 0);
    }

    // An unknown type and an unconstructible argument are different mistakes
    // and the VM words them differently, so they get distinct return codes.
    #[test]
    fn an_unregistered_type_is_distinguishable_from_a_bad_argument() {
        assert_eq!(status("T_never_registered", dict_word(&[])), -1);
        reg("T_known", &["x"]);
        assert_eq!(status("T_known", int_word(5)), 0);
    }

    #[test]
    fn a_non_pointer_argument_fails() {
        reg("T_scalar", &["x"]);
        assert_eq!(construct("T_scalar", int_word(5)), None);
        assert_eq!(construct("T_scalar", NIL.bits() as i64), None);
    }

    // Field values are shared with the source dict, so the struct must take its
    // own reference — otherwise freeing the dict would leave the struct pointing
    // at reclaimed memory. Refcounting applies to collection-kind values (the
    // `is_collection` gate in `gc`), so a nested dict is the thing to check;
    // strings and floats carry their own tags and are not refcounted here.
    #[test]
    fn adopted_collection_values_are_retained() {
        reg("T_rc", &["inner"]);
        let inner = dict_word(&[("k", 1)]);
        let hdr = JadeValue::from_bits(inner as u64).as_ptr() as *const crate::heap::ObjHeader;
        let before = unsafe { (*hdr).rc() };
        let _ = construct("T_rc", dict_word_raw(&[("inner", inner)])).unwrap();
        assert_eq!(
            unsafe { (*hdr).rc() },
            before + 1,
            "struct did not retain the collection it adopted"
        );
    }

}
