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

/// The struct type of `recv_word` has a method called `method`.
///
/// The guard a devirtualized method call emits. Codegen resolves `obj.m(...)`
/// to one implementation when only one type declares an `m` that accepts that
/// many arguments — but the receiver's *type* is not part of that decision,
/// because bytecode carries no types. So the call site checks here that the
/// receiver really is the type the resolution assumed, and dispatches
/// dynamically when it is not.
///
/// Without it, `fn call(o) { return o.go() }` ran type A's `go` on a B, which
/// answered with a value computed from another type's fields. Reading the type
/// name in place rather than through `jrt_get_type_name` keeps the guard free
/// of an allocation per call.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_struct_is_type(recv_word: i64, type_name: *const c_char) -> i32 {
    let v = JadeValue::from_bits(recv_word as u64);
    if !v.is_ptr() {
        return 0;
    }
    let p = v.as_ptr();
    if p.is_null() {
        return 0;
    }
    if unsafe { (*(p as *const crate::heap::ObjHeader)).kind } != crate::heap::ObjKind::Struct as u8
    {
        return 0;
    }
    let actual = unsafe { &*(p as *const crate::coll::StructObj<i64>) }.type_name();
    let want = unsafe { cstr::to_string(type_name) };
    i32::from(actual == want)
}

/// Whether `recv_word` is a struct instance at all.
///
/// Lets a call site tell "the receiver is some other struct" from "the receiver
/// is an array or a string", which decide differently: the first goes to method
/// dispatch, the second to the primitive method of the same name.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_is_struct(recv_word: i64) -> i32 {
    let v = JadeValue::from_bits(recv_word as u64);
    if !v.is_ptr() {
        return 0;
    }
    let p = v.as_ptr();
    if p.is_null() {
        return 0;
    }
    i32::from(
        unsafe { (*(p as *const crate::heap::ObjHeader)).kind }
            == crate::heap::ObjKind::Struct as u8,
    )
}

/// Why [`jrt_method_resolve`] could not find an implementation.
pub mod resolve_status {
    /// Found; the returned pointer is the implementation.
    pub const OK: i32 = 0;
    /// The receiver is not a struct at all.
    pub const NOT_A_STRUCT: i32 = 1;
    /// It is a struct, but its type declares no such method.
    pub const NO_SUCH_METHOD: i32 = 2;
}

/// Resolve `recv.method` against the receiver's *runtime* type, reporting why
/// through `status` rather than raising — a Jade raise is a longjmp and must not
/// cross a Rust frame, so the C forwarder raises on the way out.
///
/// Replaces the old `jrt_get_type_name` + `jrt_method_lookup` pair at a dynamic
/// call site. That pair allocated a fresh tagged string for the type name on
/// every call and never freed it, and returned null on a miss which the call
/// site then jumped through.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_method_resolve(
    recv_word: i64,
    method: *const c_char,
    status: *mut i32,
) -> *const c_void {
    let set = |s: i32| {
        if !status.is_null() {
            unsafe { *status = s };
        }
    };
    let v = JadeValue::from_bits(recv_word as u64);
    if !v.is_ptr() {
        set(resolve_status::NOT_A_STRUCT);
        return core::ptr::null();
    }
    let p = v.as_ptr();
    if p.is_null()
        || unsafe { (*(p as *const crate::heap::ObjHeader)).kind }
            != crate::heap::ObjKind::Struct as u8
    {
        set(resolve_status::NOT_A_STRUCT);
        return core::ptr::null();
    }
    let tn = unsafe { &*(p as *const crate::coll::StructObj<i64>) }.type_name();
    let m = unsafe { cstr::to_string(method) };
    let Ok(reg) = REGISTRY.lock() else {
        set(resolve_status::NO_SUCH_METHOD);
        return core::ptr::null();
    };
    match reg.iter().find(|e| e.type_name == tn && e.method == m) {
        Some(e) => {
            set(resolve_status::OK);
            e.fnptr as *const c_void
        }
        None => {
            set(resolve_status::NO_SUCH_METHOD);
            core::ptr::null()
        }
    }
}

/// A method bound to a receiver: `let greet = person.greet`.
///
/// ```text
///   offset  0   header     ObjHeader — rc at 0, kind (9) at 8
///   offset 16   fn_ptr     the jf_<uid> implementation
///   offset 24   self_word  the tagged receiver, passed as param 0
/// ```
///
/// The kind byte at offset 8 is the discriminator, and it sits at the same
/// offset here as in every other heap object because it is *in* the header.
/// Static fn boxes and native fn values put `ObjKind::Fn` there; a caller reads
/// the byte and, seeing `9`, prepends `self_word` to the argument list. Note the
/// older discriminator for native fn values is a *sentinel address* at offset 0
/// — this uses the kind byte instead, which is the invariant every `TAG_PTR`
/// value already has to satisfy, and does not burn a symbol address.
///
/// **Why the header, when a static fn box gets by without one.** A fn box is an
/// LLVM global constant (see `fn_box_word`): it allocates nothing and it holds
/// nothing, so leaving it outside the refcount is free. A bound method is
/// neither. It is a real allocation, and it *owns a reference to its receiver*.
/// Laying it out fn-pointer-first, the way a fn box is laid out, left it with
/// nowhere to keep a refcount — so it was never reclaimed and, far worse, the
/// receiver it pointed at was freed the moment the frame that built it returned:
///
/// ```text
///   fn mk(v) { let c = C { n: v }; return c.get }   // c dies here
///   let f = mk(42)
///   print(f())                                       // reads freed memory
/// ```
///
/// That ran fine under `jade run` and crashed under `jade build`. Giving the
/// object a header costs 16 bytes and moves `fn_ptr` off zero; `indirect_call`
/// already branches on the kind byte, so it reads the pointer from the right
/// offset in the branch it was taking anyway.
#[repr(C)]
pub struct BoundMethodObj {
    header: crate::heap::ObjHeader,
    fn_ptr: usize,
    /// The receiver, retained at bind time. `gc::free_one` releases it.
    pub(crate) self_word: i64,
}

/// Bind `method` on the receiver `recv_word` (a tagged struct pointer).
///
/// Returns the bound-method object, or null when the receiver's type has no
/// such method — the C forwarder raises, since a Jade error is a longjmp and
/// must not cross a Rust frame.
///
/// **The bound object retains the receiver.** It outlives the expression that
/// built it — that is the whole point of a first-class method — so the receiver
/// has to outlive it too. `gc::free_obj` releases that reference when the bound
/// method itself is reclaimed.
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

    // The bound object is about to own `recv_word`; balanced by the decref in
    // `gc::free_obj`'s BoundMethod arm.
    crate::gc::jrt_incref(recv_word);
    crate::gc::leak_obj(BoundMethodObj {
        header: crate::heap::ObjHeader::new(crate::heap::ObjKind::BoundMethod, 0),
        fn_ptr: fnptr,
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

    // The header occupies 0..16, so the implementation pointer is at 16 and the
    // receiver at 24. `indirect_call` hard-codes both, which is why they are
    // pinned here rather than left to the struct definition.
    #[test]
    fn the_implementation_sits_at_offset_sixteen() {
        let _c = counted();
        register("BM_fnoff", "m");
        let b = bind(struct_word("BM_fnoff"), "m");
        let got = unsafe { *((b as *const usize).add(2)) };
        assert_eq!(got, dummy as *const () as usize);
    }

    #[test]
    fn the_receiver_sits_at_offset_twentyfour() {
        let _c = counted();
        register("BM_off", "m");
        let recv = struct_word("BM_off");
        let b = bind(recv, "m");
        let got = unsafe { *((b as *const i64).add(3)) };
        assert_eq!(got, recv);
    }

    // The bug this layout exists to fix: a bound method outlives the expression
    // that built it, so it has to keep the receiver alive on its own.
    #[test]
    fn binding_retains_the_receiver() {
        let _c = counted();
        register("BM_ret", "m");
        let recv = struct_word("BM_ret");
        let rc_before = unsafe {
            (*(JadeValue::from_bits(recv as u64).as_ptr() as *const crate::heap::ObjHeader))
                .rc
                .load(std::sync::atomic::Ordering::Relaxed)
        };
        let b = bind(recv, "m");
        assert!(!b.is_null());
        let rc_after = unsafe {
            (*(JadeValue::from_bits(recv as u64).as_ptr() as *const crate::heap::ObjHeader))
                .rc
                .load(std::sync::atomic::Ordering::Relaxed)
        };
        assert_eq!(rc_after, rc_before + 1, "the bound method takes a reference");
    }

    // And releasing the bound method gives that reference back, so the pair is
    // balanced and a program that makes many of them does not grow.
    #[test]
    fn releasing_a_bound_method_frees_it_and_its_receiver() {
        let _c = counted();
        register("BM_bal", "m");
        let before = crate::gc::jrt_heap_live_count();
        let recv = struct_word("BM_bal");
        let b = bind(recv, "m");
        let bw = JadeValue::from_ptr(b as *const ()).bits() as i64;
        assert_eq!(crate::gc::jrt_heap_live_count(), before + 2, "receiver + bound method");
        // Drop the caller's own reference to the receiver, then the bound
        // method's. Only the second one should reclaim anything.
        crate::gc::jrt_decref(recv);
        assert_eq!(crate::gc::jrt_heap_live_count(), before + 2, "the binding still holds it");
        crate::gc::jrt_decref(bw);
        assert_eq!(crate::gc::jrt_heap_live_count(), before, "both reclaimed");
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
