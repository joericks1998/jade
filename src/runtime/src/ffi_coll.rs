//! The C-ABI surface for the shared heap collections (arrays, dicts, structs).
//!
//! These replace the C runtime's kind-tagged `JK*` objects
//! (`runtime_lib/common.c`). The object *storage* now lives once in
//! [`crate::coll`] behind an [`crate::heap::ObjHeader`], and these
//! `#[no_mangle]` shims are what the AOT backend's emitted `jrt_*` calls
//! resolve against. `lower.rs` is unchanged — only the implementation behind the
//! symbols moved from C to Rust.
//!
//! ## Non-raising vs raising
//!
//! Functions here never raise: a Jade-catchable error cannot be a `longjmp`
//! across a Rust frame (the same rule the `jrt_core_*` ops follow). So:
//!
//!  * **builders / accessors / renderer** that cannot fail export their real
//!    `jrt_*` names directly (the C definitions are deleted);
//!  * operations that *can* raise (out-of-bounds index, missing key/field, a
//!    non-indexable value) keep a thin **C forwarder** under their original name
//!    that owns the bounds/type checks and the `throw_msg`, calling the
//!    `jrt_coll_*` storage helpers here for the raw reads/writes.
//!
//! ## Allocation
//!
//! Objects are `Box::into_raw`'d (Rust global allocator) and **leaked**, exactly
//! matching the C `JK*` objects, which were `malloc`'d and never freed. A `Box`
//! (not `sys::malloc`) is required because the payload owns a `Vec`; the future
//! cycle collector will reclaim via `Box::from_raw`. The pointer is >= 8-aligned
//! (the header is `align(8)`), so `TAG_PTR` tagging by codegen is valid.

use core::ffi::{c_char, c_void};

use crate::coll::{ArrayObj, DictObj, StructObj};
use crate::heap::{ObjHeader, ObjKind};
use crate::render::render_word;
use crate::string;
use crate::sys::{malloc, oom, strlen};
use crate::value::JadeValue;

/// The element word type the AOT backend stores: a tagged [`JadeValue`] as `i64`.
type W = i64;

// ── small unsafe helpers ──────────────────────────────────────────────────────

/// Borrow a NUL-terminated C string as `&str` (no trailing NUL). NULL or invalid
/// UTF-8 → `""` (Jade strings are always valid UTF-8, so this is lossless in
/// practice). Used to bridge the C-ABI `const char*` keys/field names to the
/// shared collections' `String` keys.
#[inline]
unsafe fn cstr_str<'a>(p: *const c_char) -> &'a str {
    if p.is_null() {
        return "";
    }
    unsafe {
        let n = strlen(p as *const u8);
        let bytes = core::slice::from_raw_parts(p as *const u8, n);
        core::str::from_utf8(bytes).unwrap_or("")
    }
}

/// The [`ObjKind`] byte at an object pointer's header.
#[inline]
unsafe fn kind_of(p: *const c_void) -> u8 {
    unsafe { (*(p as *const ObjHeader)).kind }
}

/// Allocate a fresh tagged string holding `bytes` with the given trust, and
/// return its data pointer (as the C runtime's `char*`).
unsafe fn tagged_string(bytes: &[u8], trust: u8) -> *mut c_char {
    let out = string::new(bytes.len(), trust);
    if !bytes.is_empty() {
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len()) };
    }
    out as *mut c_char
}

// ── Kind ──────────────────────────────────────────────────────────────────────

/// The runtime kind byte of a heap object (was `common.c` reading offset 0).
/// Returns an [`ObjKind`] discriminant: `Array=2`, `Dict=3`, `Struct=4`. The C
/// forwarders compare against the `JK_*` macros, which `runtime.h` now defines to
/// these same values.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_kind_of(p: *const c_void) -> i64 {
    unsafe { kind_of(p) as i64 }
}

/// The element/field count from the object header (O(1)). Used by the Chunk
/// backend's `len()` on a collection.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_coll_len(p: *const c_void) -> i64 {
    unsafe { (*(p as *const ObjHeader)).len as i64 }
}

/// `len(x)` for the Chunk backend's statically-`Unknown` arm: a string → byte
/// length (`strlen`), a heap collection → its header element/field count
/// (`ObjHeader.len`), anything else → 0. This is the Chunk-path twin of the C
/// `jrt_len_unknown`; unlike that legacy helper (which reads a length at the
/// `JrtArrayHdr` offset 8), it reads the shared `ObjHeader.len` at offset 4, so
/// it is correct for the kind-tagged collections this crate allocates. The two
/// never mix in one binary (a program is wholly one path), so keeping them
/// separate avoids a layout clash.
///
/// # Safety
/// A `TAG_STR` word must point at a NUL-terminated tagged string, and a `TAG_PTR`
/// word at an `ObjHeader`-prefixed collection (this crate's ABI invariant).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_len_chunk(word: i64) -> i64 {
    let v = JadeValue::from_bits(word as u64);
    if v.is_str() {
        let p = v.as_ptr() as *const u8;
        return if p.is_null() { 0 } else { unsafe { strlen(p) as i64 } };
    }
    if v.is_ptr() {
        return unsafe { (*(v.as_ptr() as *const ObjHeader)).len as i64 };
    }
    0
}

// ── Array ─────────────────────────────────────────────────────────────────────

/// Allocate an empty kind-tagged array (leaked; see module docs).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_karr_new() -> *mut c_void {
    Box::into_raw(Box::new(ArrayObj::<W>::new())) as *mut c_void
}

/// Append a tagged word (reference semantics — mutates in place).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_karr_push(arr: *mut c_void, val: W) {
    unsafe { (*(arr as *mut ArrayObj<W>)).push(val) }
}

/// Array length (helper for the `jrt_val_index` C forwarder's bounds check).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_coll_array_len(arr: *const c_void) -> i64 {
    unsafe { (*(arr as *const ArrayObj<W>)).len() as i64 }
}

/// Element at `i` (the caller has bounds-checked). Out-of-range → nil word.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_coll_array_get(arr: *const c_void, i: i64) -> W {
    unsafe {
        match (*(arr as *const ArrayObj<W>)).get(i as usize) {
            Some(w) => *w,
            None => JadeValue::from_bits(crate::value::NIL_BITS).bits() as i64,
        }
    }
}

/// Overwrite element `i` in place (the caller has bounds-checked).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_coll_array_set(arr: *mut c_void, i: i64, val: W) {
    unsafe {
        let _ = (*(arr as *mut ArrayObj<W>)).set(i as usize, val);
    }
}

/// `array.pop()`: remove and return the last element, or a nil word if empty
/// (matches the VM — no raise on empty).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_coll_array_pop(arr: *mut c_void) -> W {
    unsafe {
        (*(arr as *mut ArrayObj<W>))
            .pop()
            .unwrap_or(JadeValue::from_bits(crate::value::NIL_BITS).bits() as i64)
    }
}

/// `array.reverse()`: reverse in place (length unchanged).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_coll_array_reverse(arr: *mut c_void) {
    unsafe { (*(arr as *mut ArrayObj<W>)).reverse() }
}

/// `array.sort()`: sort in place by the VM's total order (`vm_cmp_for_sort`):
/// numeric compares numerically (int/float mixed), strings lexicographically,
/// bools false<true, and any other/mixed pairing compares equal (stable).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_coll_array_sort(arr: *mut c_void) {
    unsafe { (*(arr as *mut ArrayObj<W>)).sort_by(|a, b| cmp_for_sort(*a, *b)) }
}

/// Total order over tagged words matching the VM's `vm_cmp_for_sort`.
fn cmp_for_sort(a: W, b: W) -> core::cmp::Ordering {
    use core::cmp::Ordering::Equal;
    let (va, vb) = (JadeValue::from_bits(a as u64), JadeValue::from_bits(b as u64));
    if va.is_int() && vb.is_int() {
        return va.as_int().cmp(&vb.as_int());
    }
    let num = |x: JadeValue| -> Option<f64> {
        if x.is_int() {
            Some(x.as_int() as f64)
        } else if x.is_float() {
            Some(crate::float::unbox_float(x))
        } else {
            None
        }
    };
    if let (Some(x), Some(y)) = (num(va), num(vb)) {
        return x.partial_cmp(&y).unwrap_or(Equal);
    }
    if va.is_str() && vb.is_str() {
        let sa = unsafe { cstr_slice(va.as_ptr() as *const u8) };
        let sb = unsafe { cstr_slice(vb.as_ptr() as *const u8) };
        return sa.cmp(sb);
    }
    if va.is_bool() && vb.is_bool() {
        return va.as_bool().cmp(&vb.as_bool());
    }
    Equal
}

/// Borrow a NUL-terminated string's bytes (no trailing NUL). NULL → `&[]`.
#[inline]
unsafe fn cstr_slice<'a>(p: *const u8) -> &'a [u8] {
    if p.is_null() {
        return &[];
    }
    unsafe { core::slice::from_raw_parts(p, strlen(p)) }
}

/// `str.split(s, sep)`: split `s` on `sep` into a new array of substrings
/// (Rust `str::split` semantics, matching the VM). Each part inherits the
/// source string's trust byte (taint propagates).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_coll_str_split(s: *const c_char, sep: *const c_char) -> *mut c_void {
    unsafe {
        let trust = string::trust_of(s as *const u8);
        let (sv, sepv) = (cstr_str(s), cstr_str(sep));
        let mut arr = ArrayObj::<W>::new();
        for part in sv.split(sepv) {
            let ts = tagged_string(part.as_bytes(), trust);
            arr.push(JadeValue::from_str_ptr(ts as *const ()).bits() as i64);
        }
        Box::into_raw(Box::new(arr)) as *mut c_void
    }
}

// ── Dict ──────────────────────────────────────────────────────────────────────

/// Allocate an empty kind-tagged dict (leaked; see module docs).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_kdict_new() -> *mut c_void {
    Box::into_raw(Box::new(DictObj::<W>::new())) as *mut c_void
}

/// Set `key → val`, where `key_word` is a tagged-string value word (its bytes are
/// copied). Update-in-place if present, else append.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_kdict_set(dict: *mut c_void, key_word: W, val: W) {
    unsafe {
        let key_ptr = JadeValue::from_bits(key_word as u64).as_ptr() as *const c_char;
        (*(dict as *mut DictObj<W>)).set(cstr_str(key_ptr), val);
    }
}

/// Look up `key` (a NUL-terminated C string). On hit, writes the value word to
/// `*out` and returns `1`; on miss returns `0`.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_coll_dict_get(dict: *const c_void, key: *const c_char, out: *mut W) -> i32 {
    unsafe {
        match (*(dict as *const DictObj<W>)).get(cstr_str(key)) {
            Some(w) => {
                *out = *w;
                1
            }
            None => 0,
        }
    }
}

/// A value-copy of a dict with a fresh header (VM clone-on-mutation): keys
/// re-owned, values shared. Returns the new (leaked) object pointer.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_coll_dict_copy(dict: *const c_void) -> *mut c_void {
    unsafe {
        let copy = (*(dict as *const DictObj<W>)).value_copy();
        Box::into_raw(Box::new(copy)) as *mut c_void
    }
}

/// `dict.merge(a, b)`: a new dict = a's entries overlaid with b's (b wins on
/// conflict); the inputs are unchanged. Matches the VM's `dict.merge`.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_coll_dict_merge(a: *const c_void, b: *const c_void) -> *mut c_void {
    unsafe {
        let mut out = (*(a as *const DictObj<W>)).value_copy();
        for (k, v) in (*(b as *const DictObj<W>)).entries() {
            out.set(k, *v);
        }
        Box::into_raw(Box::new(out)) as *mut c_void
    }
}

/// `dict.keys()`: a new array of the keys as TRUSTED tagged strings, sorted
/// ascending (matching the VM's `dict.keys`).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_coll_dict_keys(dict: *const c_void) -> *mut c_void {
    unsafe {
        let d = &*(dict as *const DictObj<W>);
        let mut keys: Vec<&str> = d.entries().iter().map(|(k, _)| k.as_str()).collect();
        keys.sort_unstable();
        let mut arr = ArrayObj::<W>::new();
        for k in keys {
            let s = tagged_string(k.as_bytes(), string::TRUSTED);
            arr.push(JadeValue::from_str_ptr(s as *const ()).bits() as i64);
        }
        Box::into_raw(Box::new(arr)) as *mut c_void
    }
}

/// `dict.values()`: a new array of the values in key-sorted order (matching the
/// VM's `dict.values`). Values are shared words (cloned by word copy).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_coll_dict_values(dict: *const c_void) -> *mut c_void {
    unsafe {
        let d = &*(dict as *const DictObj<W>);
        let mut entries: Vec<&(String, W)> = d.entries().iter().collect();
        entries.sort_by(|x, y| x.0.cmp(&y.0));
        let mut arr = ArrayObj::<W>::new();
        for (_, v) in entries {
            arr.push(*v);
        }
        Box::into_raw(Box::new(arr)) as *mut c_void
    }
}

// ── Struct ────────────────────────────────────────────────────────────────────

/// Allocate a kind-tagged struct of type `type_name` (a C string) with no fields
/// yet (leaked; see module docs).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_kstruct_new(type_name: *const c_char) -> *mut c_void {
    unsafe {
        let obj = StructObj::<W>::new(cstr_str(type_name));
        Box::into_raw(Box::new(obj)) as *mut c_void
    }
}

/// Set `field → val` (field is a C string). Reference semantics — mutate in
/// place; update if present else append in definition order.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_kstruct_set(s: *mut c_void, field: *const c_char, val: W) {
    unsafe { (*(s as *mut StructObj<W>)).set_field(cstr_str(field), val) }
}

/// Read field `field` (a C string). On hit writes to `*out`, returns `1`; miss
/// returns `0`.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_coll_struct_get(s: *const c_void, field: *const c_char, out: *mut W) -> i32 {
    unsafe {
        match (*(s as *const StructObj<W>)).get_field(cstr_str(field)) {
            Some(w) => {
                *out = *w;
                1
            }
            None => 0,
        }
    }
}

/// The struct type name of `obj` as a fresh TRUSTED tagged string; the empty
/// string if `obj` is not a struct (VM `GetTypeName`, for typed `catch`). Never
/// raises.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_get_type_name(obj: W) -> *mut c_char {
    unsafe {
        let v = JadeValue::from_bits(obj as u64);
        if v.is_ptr() {
            let p = v.as_ptr() as *const c_void;
            if kind_of(p) == ObjKind::Struct as u8 {
                let name = (*(p as *const StructObj<W>)).type_name();
                return tagged_string(name.as_bytes(), string::TRUSTED);
            }
        }
        tagged_string(b"", string::TRUSTED)
    }
}

// ── Collection-producing stdlib ops (build ObjHeader collections) ─────────────

/// `sh.output(cmd)`: run `sh -c cmd`, capturing output → a new dict
/// `{stdout, stderr, code}` (stdout/stderr TAINTED — shell output; code Int).
/// Never raises (mirrors the VM). Returns the raw dict pointer (codegen tags it).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_coll_sh_output(cmd: *const c_char) -> *mut c_void {
    let cmd = unsafe { cstr_str(cmd) };
    let out = std::process::Command::new("sh").args(["-c", cmd]).output();
    let mut d = DictObj::<W>::new();
    let (so, se, code) = match out {
        Ok(o) => (
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
            o.status.code().unwrap_or(-1) as i64,
        ),
        Err(_) => (String::new(), String::new(), -1),
    };
    unsafe {
        let so_w = JadeValue::from_str_ptr(tagged_string(so.as_bytes(), 1 /*TAINTED*/) as *const ()).bits() as i64;
        let se_w = JadeValue::from_str_ptr(tagged_string(se.as_bytes(), 1) as *const ()).bits() as i64;
        d.insert("stdout", so_w);
        d.insert("stderr", se_w);
        d.insert("code", JadeValue::from_int(code).bits() as i64);
    }
    Box::into_raw(Box::new(d)) as *mut c_void
}

/// `fs.list_dir(path)`: a new array of the directory's entry names (TAINTED —
/// file-derived; no `.`/`..`; order is the OS enumeration, matching the VM's
/// `std::fs::read_dir`). On any I/O error sets `*err = 1` and returns null; the
/// C forwarder turns that into a catchable Jade error (a Rust frame can't
/// `longjmp`).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_coll_fs_list_dir(path: *const c_char, err: *mut i32) -> *mut c_void {
    let path = unsafe { cstr_str(path) };
    match std::fs::read_dir(path) {
        Ok(rd) => {
            let mut arr = ArrayObj::<W>::new();
            for entry in rd {
                match entry {
                    Ok(e) => {
                        let name = e.file_name();
                        let name = name.to_string_lossy();
                        let w = unsafe {
                            JadeValue::from_str_ptr(tagged_string(name.as_bytes(), 1) as *const ()).bits() as i64
                        };
                        arr.push(w);
                    }
                    Err(_) => {
                        unsafe { *err = 1 };
                        return core::ptr::null_mut();
                    }
                }
            }
            unsafe { *err = 0 };
            Box::into_raw(Box::new(arr)) as *mut c_void
        }
        Err(_) => {
            unsafe { *err = 1 };
            core::ptr::null_mut()
        }
    }
}

// ── Renderer ──────────────────────────────────────────────────────────────────

/// Render a type-erased value word to a fresh plain (system-`malloc`'d,
/// NUL-terminated) buffer the C caller frees with `free`. Replaces the C
/// `jrt_render_any`/`jk_render`; arrays render `[a, b]`, dicts `{"k": v}` (keys
/// sorted, quoted), structs `<struct>`. Scalars/strings format identically to the
/// VM (shared [`crate::render`] primitives).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_render_any(val: W) -> *mut c_char {
    let s = render_word(val);
    let bytes = s.as_bytes();
    let n = bytes.len();
    unsafe {
        let buf = malloc(n + 1);
        if buf.is_null() {
            oom();
        }
        if n > 0 {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, n);
        }
        *buf.add(n) = 0;
        buf as *mut c_char
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int_word(i: i64) -> W {
        JadeValue::from_int(i).bits() as i64
    }

    #[test]
    fn array_roundtrip_through_ffi() {
        let a = jrt_karr_new();
        jrt_karr_push(a, int_word(10));
        jrt_karr_push(a, int_word(20));
        assert_eq!(jrt_kind_of(a), ObjKind::Array as i64);
        assert_eq!(jrt_coll_len(a), 2);
        assert_eq!(jrt_coll_array_len(a), 2);
        assert_eq!(jrt_coll_array_get(a, 1), int_word(20));
        jrt_coll_array_set(a, 0, int_word(99));
        assert_eq!(jrt_coll_array_get(a, 0), int_word(99));
        unsafe { drop(Box::from_raw(a as *mut ArrayObj<W>)) };
    }

    #[test]
    fn dict_get_and_value_copy_through_ffi() {
        let d = jrt_kdict_new();
        // key as a tagged string word.
        let key = unsafe { tagged_string(b"k", string::TRUSTED) };
        let key_word = JadeValue::from_str_ptr(key as *const ()).bits() as i64;
        jrt_kdict_set(d, key_word, int_word(1));
        assert_eq!(jrt_kind_of(d), ObjKind::Dict as i64);

        let mut out: W = 0;
        assert_eq!(jrt_coll_dict_get(d, b"k\0".as_ptr() as *const c_char, &mut out), 1);
        assert_eq!(out, int_word(1));
        assert_eq!(jrt_coll_dict_get(d, b"x\0".as_ptr() as *const c_char, &mut out), 0);

        // value_copy is independent.
        let d2 = jrt_coll_dict_copy(d);
        jrt_kdict_set(d2, key_word, int_word(999));
        let mut o1: W = 0;
        let mut o2: W = 0;
        jrt_coll_dict_get(d, b"k\0".as_ptr() as *const c_char, &mut o1);
        jrt_coll_dict_get(d2, b"k\0".as_ptr() as *const c_char, &mut o2);
        assert_eq!(o1, int_word(1)); // original unchanged
        assert_eq!(o2, int_word(999));

        unsafe {
            drop(Box::from_raw(d as *mut DictObj<W>));
            drop(Box::from_raw(d2 as *mut DictObj<W>));
            string::free_str(key as *mut u8);
        }
    }

    #[test]
    fn struct_type_name_and_fields_through_ffi() {
        let s = jrt_kstruct_new(b"Point\0".as_ptr() as *const c_char);
        jrt_kstruct_set(s, b"x\0".as_ptr() as *const c_char, int_word(7));
        let obj_word = JadeValue::from_ptr(s as *const ()).bits() as i64;

        let name = jrt_get_type_name(obj_word);
        assert_eq!(unsafe { cstr_str(name as *const c_char) }, "Point");

        let mut out: W = 0;
        assert_eq!(jrt_coll_struct_get(s, b"x\0".as_ptr() as *const c_char, &mut out), 1);
        assert_eq!(out, int_word(7));
        assert_eq!(jrt_coll_struct_get(s, b"z\0".as_ptr() as *const c_char, &mut out), 0);

        // type name of a non-struct value is empty.
        let empty = jrt_get_type_name(int_word(5));
        assert_eq!(unsafe { cstr_str(empty as *const c_char) }, "");

        unsafe {
            drop(Box::from_raw(s as *mut StructObj<W>));
            string::free_str(name as *mut u8);
            string::free_str(empty as *mut u8);
        }
    }

    #[test]
    fn render_any_returns_freeable_buffer() {
        let a = jrt_karr_new();
        jrt_karr_push(a, int_word(1));
        jrt_karr_push(a, int_word(2));
        let obj_word = JadeValue::from_ptr(a as *const ()).bits() as i64;
        let buf = jrt_render_any(obj_word);
        assert_eq!(unsafe { cstr_str(buf as *const c_char) }, "[1, 2]");
        unsafe {
            crate::sys::free(buf as *mut u8); // plain free, as the C caller does
            drop(Box::from_raw(a as *mut ArrayObj<W>));
        }
    }
}
