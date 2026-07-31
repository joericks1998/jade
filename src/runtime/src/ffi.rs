//! The C-ABI surface of the runtime: `#[no_mangle] extern "C"` shims that
//! AOT-compiled binaries (and the residual C runtime) call by their historical
//! `jrt_*` names. Each is a thin forwarder to the pure Rust implementation in
//! the sibling modules — the semantics live there, once, so the VM (which calls
//! the Rust API directly) and AOT binaries (which call these symbols) cannot
//! diverge.
//!
//! `jade_value_t` is C's `int64_t`; it crosses this boundary as `i64` and is
//! reinterpreted into a [`JadeValue`] on the Rust side.
//!
//! As symbols move here from `runtime_aot/common.c`, the C definitions are
//! deleted (their declarations stay in `runtime.h`) and the linker resolves the
//! calls against this crate's staticlib instead.

use crate::dynop::DynErr;
use crate::ops;
use crate::value::JadeValue;

// ── Dynamic-op error codes (shared with common.c's forwarders) ─────────────
//
// The `jrt_core_*` ops report failure through a `*mut u32` out-parameter using
// these codes instead of raising, so no `longjmp` crosses a Rust frame. The C
// forwarder inspects the code and calls `jade_exc_throw_typed` itself. Keep in
// lockstep with the JRT_OP_* macros in runtime.h.

/// No error; the returned value is valid.
pub const JRT_OP_OK: u32 = 0;
/// Division by zero.
pub const JRT_OP_DIVZERO: u32 = 1;
/// A non-numeric operand where a number was required.
pub const JRT_OP_TYPE: u32 = 2;
/// Integer `+ - *` overflowed.
pub const JRT_OP_OVERFLOW: u32 = 3;
/// Modulo by zero.
pub const JRT_OP_REMZERO: u32 = 4;

#[inline]
fn err_code(e: DynErr) -> u32 {
    match e {
        DynErr::DivZero => JRT_OP_DIVZERO,
        DynErr::Type => JRT_OP_TYPE,
        DynErr::Overflow => JRT_OP_OVERFLOW,
        DynErr::RemZero => JRT_OP_REMZERO,
    }
}

/// Write the outcome of a value-producing op to the `err` out-param and return
/// the tagged value bits (0 on error).
#[inline]
unsafe fn finish(r: ops::OpResult, err: *mut u32) -> i64 {
    match r {
        Ok(v) => {
            unsafe { *err = JRT_OP_OK };
            v.bits() as i64
        }
        Err(e) => {
            unsafe { *err = err_code(e) };
            0
        }
    }
}

// ── Dynamic arithmetic (was common.c jrt_*_any, now thin C forwarders) ─────

#[unsafe(no_mangle)]
pub extern "C" fn jrt_core_add(a: i64, b: i64, err: *mut u32) -> i64 {
    unsafe { finish(ops::add(JadeValue::from_bits(a as u64), JadeValue::from_bits(b as u64)), err) }
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_core_sub(a: i64, b: i64, err: *mut u32) -> i64 {
    unsafe { finish(ops::sub(JadeValue::from_bits(a as u64), JadeValue::from_bits(b as u64)), err) }
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_core_mul(a: i64, b: i64, err: *mut u32) -> i64 {
    unsafe { finish(ops::mul(JadeValue::from_bits(a as u64), JadeValue::from_bits(b as u64)), err) }
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_core_div(a: i64, b: i64, err: *mut u32) -> i64 {
    unsafe { finish(ops::div(JadeValue::from_bits(a as u64), JadeValue::from_bits(b as u64)), err) }
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_core_mod(a: i64, b: i64, err: *mut u32) -> i64 {
    unsafe { finish(ops::rem(JadeValue::from_bits(a as u64), JadeValue::from_bits(b as u64)), err) }
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_core_pow(a: i64, b: i64, err: *mut u32) -> i64 {
    unsafe { finish(ops::pow(JadeValue::from_bits(a as u64), JadeValue::from_bits(b as u64)), err) }
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_core_neg(a: i64, err: *mut u32) -> i64 {
    unsafe { finish(ops::neg(JadeValue::from_bits(a as u64)), err) }
}

/// Ordering `-1/0/1`; sets `*err` to `JRT_OP_TYPE` on non-numeric operands.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_core_cmp(a: i64, b: i64, err: *mut u32) -> i32 {
    match ops::cmp(JadeValue::from_bits(a as u64), JadeValue::from_bits(b as u64)) {
        Ok(c) => {
            unsafe { *err = JRT_OP_OK };
            c
        }
        Err(e) => {
            unsafe { *err = err_code(e) };
            0
        }
    }
}

/// Coerce to `f64` for `math.*`; sets `*err` to `JRT_OP_TYPE` on non-numeric.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_core_to_double(v: i64, err: *mut u32) -> f64 {
    match ops::to_double(JadeValue::from_bits(v as u64)) {
        Ok(d) => {
            unsafe { *err = JRT_OP_OK };
            d
        }
        Err(e) => {
            unsafe { *err = err_code(e) };
            0.0
        }
    }
}

/// Equality (`1`/`0`); sets `*err` to `JRT_OP_TYPE` on cross-kind operands
/// (VM-strict: `2 == 2.0` errors). codegen uses this for both `==` and `!=`.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_core_eq(a: i64, b: i64, err: *mut u32) -> i32 {
    match ops::eq(JadeValue::from_bits(a as u64), JadeValue::from_bits(b as u64)) {
        Ok(v) => {
            unsafe { *err = JRT_OP_OK };
            v
        }
        Err(e) => {
            unsafe { *err = err_code(e) };
            0
        }
    }
}

/// Membership equality (`1`/`0`). Never raises: operands of different kinds are
/// not equal. See [`ops::eq_total`] for why this is not [`jrt_core_eq`].
#[unsafe(no_mangle)]
pub extern "C" fn jrt_core_eq_total(a: i64, b: i64) -> i32 {
    ops::eq_total(JadeValue::from_bits(a as u64), JadeValue::from_bits(b as u64)) as i32
}

/// The name of a value's type, as a static NUL-terminated string the caller must
/// not free.
///
/// The spellings are the VM's `value_type_name`, so an error message built in C
/// reads the same as the one the interpreter would have printed. Kinds a tagged
/// word cannot distinguish (a closure from a plain fn, say) are not reachable
/// here — this exists for scalar operand errors.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_core_type_name(v: i64) -> *const core::ffi::c_char {
    let val = JadeValue::from_bits(v as u64);
    let name: &str = if val.is_int() {
        "int\0"
    } else if val.is_float() {
        "float\0"
    } else if val.is_bool() {
        "bool\0"
    } else if val.is_str() {
        "str\0"
    } else if val.is_nil() {
        "nil\0"
    } else if val.is_ptr() {
        let kind = unsafe { (*(val.as_ptr() as *const crate::heap::ObjHeader)).kind };
        match kind {
            k if k == crate::heap::ObjKind::Array as u8 => "array\0",
            k if k == crate::heap::ObjKind::Dict as u8 => "dict\0",
            k if k == crate::heap::ObjKind::Struct as u8 => "struct\0",
            _ => "value\0",
        }
    } else {
        "value\0"
    };
    name.as_ptr() as *const core::ffi::c_char
}

/// Truthiness (`1`/`0`). Never raises. Was `common.c jrt_to_bool`.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_to_bool(v: i64) -> i32 {
    ops::to_bool(JadeValue::from_bits(v as u64))
}

// ── Boxed float (was common.c jrt_box_float / jrt_unbox_float) ─────────────

/// Heap-box a double, returning a `TAG_FLOAT`-tagged value.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_box_float(d: f64) -> i64 {
    crate::float::box_float(d).bits() as i64
}

/// Load a boxed double from a `TAG_FLOAT`-tagged value.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_unbox_float(v: i64) -> f64 {
    crate::float::unbox_float(JadeValue::from_bits(v as u64))
}

// ── Integer pow (was common.c jrt_ipow) ────────────────────────────────────

/// Integer exponentiation by squaring.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_ipow(base: i64, exp: i64) -> i64 {
    crate::num::ipow(base, exp)
}

// ── Tagged-string allocator (was common.c jrt_str_*) ───────────────────────

/// Allocate `len` data bytes tagged `trust` (8-byte header, trust at data[-1]).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_str_new(len: usize, trust: u8) -> *mut u8 {
    crate::string::new(len, trust)
}

/// Read a tagged string's trust byte (NULL → TRUSTED).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_trust_of(s: *const u8) -> u8 {
    crate::string::trust_of(s)
}

/// Allocate a tagged copy of a NUL-terminated string.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_str_dup(s: *const u8, trust: u8) -> *mut u8 {
    crate::string::dup(s, trust)
}

/// Free a string returned by `jrt_str_new`/`jrt_str_dup`/`jrt_str_concat`.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_str_free(s: *mut u8) {
    crate::string::free_str(s)
}

/// Concatenate two tagged strings (trust = max of inputs).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_str_concat(a: *const u8, b: *const u8) -> *mut u8 {
    crate::string::concat(a, b)
}
