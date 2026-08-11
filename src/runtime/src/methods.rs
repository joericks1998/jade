//! A runtime method registry for the AOT backend's dynamic method dispatch.
//!
//! Most extend-method calls devirtualize statically (the codegen resolves the
//! one method a call can target). But when two types define a method with the
//! same name *and* arity (e.g. an interface both `Dog` and `Cat` implement),
//! the target depends on the receiver's runtime type. For those, codegen emits a
//! `(type-name, method-name)` lookup here, then an indirect call.
//!
//! The table is populated once at startup (before `main`'s body, single-threaded)
//! by `jrt_method_register` calls the codegen emits, then only read — so a plain
//! `Mutex<Vec<…>>` is sufficient (lookups take the lock but never contend during
//! registration).

use core::ffi::{c_char, c_void};
use std::sync::Mutex;

use crate::cstr;
use crate::value::JadeValue;

struct Entry {
    type_name: String,
    method: String,
    fnptr: usize,
}

static REGISTRY: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

/// Register that struct type `type_name`'s method `method` is implemented by
/// `fnptr` (a `jf_<uid>` address). Called once per extend method at startup.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_method_register(
    type_name: *const c_char,
    method: *const c_char,
    fnptr: *const c_void,
) {
    let (t, m) = unsafe { (cstr::to_string(type_name), cstr::to_string(method)) };
    if let Ok(mut reg) = REGISTRY.lock() {
        reg.push(Entry { type_name: t, method: m, fnptr: fnptr as usize });
    }
}

/// Look up the implementation of `method` for the struct whose type name is in
/// `type_word` (a tagged string, from `jrt_get_type_name`). Returns the `jf_<uid>`
/// pointer, or null if none is registered (the caller raises).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_method_lookup(type_word: i64, method: *const c_char) -> *const c_void {
    let tn = {
        let v = JadeValue::from_bits(type_word as u64);
        if v.is_str() {
            unsafe { cstr::to_string(v.as_ptr() as *const c_char) }
        } else {
            String::new()
        }
    };
    let m = unsafe { cstr::to_string(method) };
    if let Ok(reg) = REGISTRY.lock() {
        for e in reg.iter() {
            if e.type_name == tn && e.method == m {
                return e.fnptr as *const c_void;
            }
        }
    }
    core::ptr::null()
}

/// A method bound to a receiver: `let greet = person.greet`.
///
/// Deliberately laid out to match the function-value shape codegen already
/// builds and `indirect_call` already knows how to read:
///
/// ```text
///   offset  0   fn_ptr     the jf_<uid> implementation
///   offset  8   kind       ObjKind::BoundMethod (9)
///   offset 16   self_word  the tagged receiver, passed as param 0
/// ```
///
/// The kind byte at offset 8 is the discriminator. Static fn boxes and native
/// fn values put `ObjKind::Fn` there; a caller reads the byte and, seeing `9`,
/// prepends `self_word` to the argument list. Note the older discriminator for
/// native fn values is a *sentinel address* at offset 0 — this uses the kind
/// byte instead, which is the invariant every `TAG_PTR` value already has to
/// satisfy, and does not burn a symbol address.
///
/// `kind_word` is a full `u64` whose low byte holds the kind, matching how
/// codegen stores that slot (an i64 store). That makes the layout
/// little-endian-dependent, exactly like the existing fn boxes.
#[repr(C)]
pub struct BoundMethodObj {
    fn_ptr: usize,
    kind_word: u64,
    self_word: i64,
}

/// Bind `method` on the receiver `recv_word` (a tagged struct pointer).
///
/// Returns the bound-method object, or null when the receiver's type has no
/// such method — the C forwarder raises, since a Jade error is a longjmp and
/// must not cross a Rust frame.
///
/// The bound object holds the receiver but takes **no** reference to it, and is
/// itself never freed: `ObjKind::BoundMethod` is not a collection, so the
/// refcount ops no-op on it, exactly as they do for the static fn boxes and
/// native fn values it shares a shape with. Consistent with the existing
/// function-value behaviour rather than better than it; making function values
/// reclaimable is its own change.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_bind_method_new(recv_word: i64, method: *const c_char) -> *mut c_void {
    let v = JadeValue::from_bits(recv_word as u64);
    if !v.is_ptr() {
        return core::ptr::null_mut();
    }
    let p = v.as_ptr();
    if p.is_null() {
        return core::ptr::null_mut();
    }
    let kind = unsafe { (*(p as *const crate::heap::ObjHeader)).kind };
    if kind != crate::heap::ObjKind::Struct as u8 {
        return core::ptr::null_mut();
    }
    let tn = unsafe { &*(p as *const crate::coll::StructObj<i64>) }.type_name().to_string();

    let m = unsafe { cstr::to_string(method) };
    let fnptr = {
        let Ok(reg) = REGISTRY.lock() else { return core::ptr::null_mut() };
        match reg.iter().find(|e| e.type_name == tn && e.method == m) {
            Some(e) => e.fnptr,
            None => return core::ptr::null_mut(),
        }
    };

    crate::gc::leak_obj(BoundMethodObj {
        fn_ptr: fnptr,
        kind_word: crate::heap::ObjKind::BoundMethod as u64,
        self_word: recv_word,
    })
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

    extern "C" fn dummy(_: i64) -> i64 {
        0
    }

    fn struct_word(type_name: &str) -> i64 {
        let s = crate::coll::StructObj::<i64>::new(type_name);
        JadeValue::from_ptr(crate::gc::leak_obj(s) as *const ()).bits() as i64
    }

    fn bind(recv: i64, method: &str) -> *mut c_void {
        let m = CString::new(method).unwrap();
        jrt_bind_method_new(recv, m.as_ptr())
    }

    fn register(type_name: &str, method: &str) {
        let (t, m) = (CString::new(type_name).unwrap(), CString::new(method).unwrap());
        jrt_method_register(t.as_ptr(), m.as_ptr(), dummy as *const c_void);
    }

    #[test]
    fn binding_captures_the_implementation_and_the_receiver() {
        let _c = counted();
        register("BM_greeter", "greet");
        let recv = struct_word("BM_greeter");
        let b = bind(recv, "greet");
        assert!(!b.is_null());
        let bm = unsafe { &*(b as *const BoundMethodObj) };
        assert_eq!(bm.fn_ptr, dummy as *const () as usize);
        assert_eq!(bm.self_word, recv, "the receiver is what will be passed as self");
    }

    // The kind byte at offset 8 is how a caller tells a bound method from a
    // plain function value, so its position is load-bearing, not incidental.
    #[test]
    fn the_kind_byte_sits_at_offset_eight() {
        let _c = counted();
        register("BM_kind", "m");
        let b = bind(struct_word("BM_kind"), "m");
        let kind = unsafe { *((b as *const u8).add(8)) };
        assert_eq!(kind, crate::heap::ObjKind::BoundMethod as u8);
    }

    #[test]
    fn the_receiver_sits_at_offset_sixteen() {
        let _c = counted();
        register("BM_off", "m");
        let recv = struct_word("BM_off");
        let b = bind(recv, "m");
        let got = unsafe { *((b as *const i64).add(2)) };
        assert_eq!(got, recv);
    }

    #[test]
    fn an_unknown_method_does_not_bind() {
        let _c = counted();
        register("BM_known", "yes");
        assert!(bind(struct_word("BM_known"), "no").is_null());
    }

    // Methods are registered per type, so a method of *another* type must not
    // bind — that would call an implementation with the wrong `self`.
    #[test]
    fn a_method_of_a_different_type_does_not_bind() {
        let _c = counted();
        register("BM_dog", "bark");
        register("BM_cat", "meow");
        assert!(bind(struct_word("BM_cat"), "bark").is_null());
    }

    #[test]
    fn a_non_struct_receiver_does_not_bind() {
        let _c = counted();
        register("BM_ns", "m");
        assert!(bind(JadeValue::from_int(7).bits() as i64, "m").is_null());
        assert!(bind(crate::value::NIL.bits() as i64, "m").is_null());
    }
}
