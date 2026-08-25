//! `bytes` — the primitive methods on a binary blob.
//!
//! A `bytes` value is a counted sequence of raw octets, distinct from `str`
//! because Jade strings are UTF-8 and NUL-terminated and arbitrary bytes are
//! neither. The value itself lives in `jade_runtime::bytesf::BytesObj`, shared
//! with the AOT heap; this module is only the method surface the VM dispatches.
//!
//! There are deliberately few methods. `bytes` exists to carry data through a
//! program unchanged — from a file, over a socket, out to stdout — not to be a
//! second string type with a parallel set of operations.

#[cfg(test)]
mod tests;

use crate::builtins::{BuiltinFn, Package, make_bytes, make_trusted_bytes};
use crate::compiler::{tir::JadeType, type_infer::TypeContext};
use crate::frontend::error::{JadeError, Result, Span};
use crate::vm::VmValue;

const ZERO: Span = Span { line: 0, col: 0 };

/// `b.len()` — octet count.
fn bytes_len(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Bytes(b) => Ok(VmValue::Int(b.lock().len() as i64)),
        _ => Err(JadeError::TypeError { message: "bytes.len".to_string(), span: ZERO }),
    }
}

/// `b.decode()` — interpret the octets as UTF-8 text.
///
/// Raises on invalid UTF-8 rather than substituting replacement characters,
/// because silently corrupting data is worse than a catchable error: a caller
/// that wants lossy behavior can ask for it, but one that assumed the bytes
/// were text needs to hear that they were not.
///
/// The trust byte travels with the text. Without that, `fs.read_bytes(p)
/// .decode()` would hand back a *clean* string and walk straight past the check
/// in `sh.exec` that `fs.read(p)` cannot.
fn bytes_decode(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Bytes(b) => {
            let g = b.lock();
            match std::str::from_utf8(g.as_slice()) {
                Ok(s) => Ok(VmValue::Str(jade_runtime::trust::JStr::with_trust(s, g.trust))),
                // A `TypeError`, not an `Exception`. `Exception` means a `raise`
                // the program wrote, and the VM answers one by handing the catch
                // block `state.raised_exception` — which a built-in never sets,
                // so a caught `bytes.decode()` used to bind the bare string
                // "unknown exception" while the compiled backend bound a proper
                // `RuntimeError`. Every other variant routes through
                // `make_vm_runtime_error`, which is what the two engines agree on.
                Err(e) => Err(JadeError::TypeError {
                    message: format!("bytes.decode(): not valid UTF-8 at byte {}", e.valid_up_to()),
                    span: ZERO,
                }),
            }
        }
        _ => Err(JadeError::TypeError { message: "bytes.decode".to_string(), span: ZERO }),
    }
}

/// `b.slice(start, end)` — a sub-blob, `end` exclusive.
///
/// Clamped rather than raising: a slice past the end of a buffer is how you
/// read the tail of one, and every caller would otherwise write the same
/// min() by hand.
fn bytes_slice(args: &[VmValue]) -> Result<VmValue> {
    match (&args[0], args.get(1), args.get(2)) {
        (VmValue::Bytes(b), Some(VmValue::Int(s)), Some(VmValue::Int(e))) => {
            let g = b.lock();
            let len = g.len() as i64;
            let start = (*s).clamp(0, len) as usize;
            let end = (*e).clamp(start as i64, len) as usize;
            Ok(make_bytes(g.as_slice()[start..end].to_vec(), g.trust))
        }
        _ => Err(JadeError::TypeError { message: "bytes.slice".to_string(), span: ZERO }),
    }
}

// ── std::bytes ────────────────────────────────────────────────────────────────
//
// The three ways to *make* a blob. They are package functions and not methods
// because they have no receiver, and because the method surface above is three
// on purpose: `bytes` carries data through a program, it is not a second string
// type with a parallel set of operations.

/// `bytes.zeros(n)` — `n` zeroed octets, trusted.
///
/// Trusted because the program wrote them. Nothing here came from a file, a
/// socket, or an argument.
fn pkg_zeros(args: &[VmValue]) -> Result<VmValue> {
    let n = match args.first() {
        Some(VmValue::Int(n)) => *n,
        Some(other) => {
            return Err(JadeError::TypeError {
                message: format!(
                    "bytes.zeros() expects an int, got {}",
                    crate::vm::value_type_name(other)
                ),
                span: ZERO,
            });
        }
        None => return Err(JadeError::ArityMismatch { expected: 1, got: 0, span: ZERO }),
    };
    jade_runtime::bytesf::zeros(n)
        .map(make_trusted_bytes)
        .map_err(|message| JadeError::TypeError { message, span: ZERO })
}

/// `bytes.from_ints(arr)` — a blob from an array of octet values.
///
/// The one way to build a blob holding octets a string cannot carry: a zero
/// terminates a Jade string, and anything above 127 encodes as two octets
/// through `str.encode()` rather than one.
///
/// The result is trusted, and an int carries no trust anywhere in Jade, so a
/// program that walks a tainted blob out into ints and back gets a trusted one.
/// That edge is real and documented rather than closed: trust follows values,
/// and an int is not one that holds any.
fn pkg_from_ints(args: &[VmValue]) -> Result<VmValue> {
    let arr = match args.first() {
        Some(VmValue::Array(a)) => a,
        Some(_) => {
            return Err(JadeError::TypeError {
                message: jade_runtime::bytesf::not_an_array(),
                span: ZERO,
            });
        }
        None => return Err(JadeError::ArityMismatch { expected: 1, got: 0, span: ZERO }),
    };
    let guard = arr.lock();
    let mut data = Vec::with_capacity(guard.len());
    for (i, el) in guard.as_slice().iter().enumerate() {
        let VmValue::Int(n) = el else {
            return Err(JadeError::TypeError {
                message: jade_runtime::bytesf::non_int_element(i),
                span: ZERO,
            });
        };
        match jade_runtime::bytesf::octet(i, *n) {
            Ok(b) => data.push(b),
            Err(message) => return Err(JadeError::TypeError { message, span: ZERO }),
        }
    }
    Ok(make_trusted_bytes(data))
}

/// `bytes.concat(a, b)` — the octets of `a` then those of `b`, in a new blob.
///
/// Trust is the more restrictive of the two. See `jade_runtime::bytesf::concat`
/// for why the other choice would be a laundering path.
fn pkg_concat(args: &[VmValue]) -> Result<VmValue> {
    match (args.first(), args.get(1)) {
        (Some(VmValue::Bytes(a)), Some(VmValue::Bytes(b))) => {
            // One guard at a time, never two.
            //
            // The obvious spelling, `let (ga, gb) = (a.lock(), b.lock())`, takes
            // the two locks in *argument* order, and that is a deadlock. Tasks
            // run on real threads over one heap, so `concat(x, y)` on one thread
            // and `concat(y, x)` on another each end up holding what the other
            // is waiting for. `parking_lot` parks with no timeout, no panic and
            // no message, so the process simply stops. Ordering the two
            // acquisitions by address would fix that too, but taking one at a
            // time leaves no ordering rule for anyone to get wrong later, and it
            // costs nothing: each guard is held for one append.
            //
            // It also removes the `concat(b, b)` special case this used to
            // need. Two guards on one blob would have hung on the spot, since
            // `parking_lot::Mutex` is not reentrant.
            //
            // The lengths are read before the payloads and are not re-read.
            // Nothing in the language changes a blob's length: there is no
            // `push` and no `truncate`, only `b[i] = v`, which writes in place.
            let a_len = a.lock().len();
            let b_len = b.lock().len();
            let n = jade_runtime::bytesf::joined_len(a_len, b_len)
                .map_err(|message| JadeError::TypeError { message, span: ZERO })?;
            let mut data = Vec::with_capacity(n);
            let a_trust = {
                let g = a.lock();
                data.extend_from_slice(g.as_slice());
                g.trust
            };
            let b_trust = {
                let g = b.lock();
                data.extend_from_slice(g.as_slice());
                g.trust
            };
            Ok(make_bytes(data, jade_runtime::bytesf::concat_trust(a_trust, b_trust)))
        }
        // One argument at a time, and the first failure wins. The wording is
        // `require_bytes_body`'s in `src/runtime_aot/common.c`, because both
        // engines have to answer a bad argument the same way.
        (Some(a), Some(b)) => {
            let bad = if matches!(a, VmValue::Bytes(_)) { b } else { a };
            Err(JadeError::TypeError {
                message: format!(
                    "bytes.concat expects bytes, got {}",
                    crate::vm::value_type_name(bad)
                ),
                span: ZERO,
            })
        }
        _ => Err(JadeError::ArityMismatch { expected: 2, got: args.len(), span: ZERO }),
    }
}

static BYTES_PKG_FNS: &[BuiltinFn] = &[
    BuiltinFn { name: "zeros", vm_impl: pkg_zeros },
    BuiltinFn { name: "from_ints", vm_impl: pkg_from_ints },
    BuiltinFn { name: "concat", vm_impl: pkg_concat },
];

fn register_bytes_pkg_types(ctx: &mut TypeContext) {
    ctx.define("bytes".to_string(), JadeType::Unknown);
}

/// The package binds a global named `bytes`, which is also the name of the
/// type. The two do not collide, because `bytes` is not a type *constructor*:
/// `int`, `float`, `bool`, `str`, `char` and `func` are seeded as `TypeRef`
/// globals and callable, and `bytes` deliberately is not. Keep it that way. If
/// `bytes` ever joins that list, importing the package would silently overwrite
/// the constructor and `bytes(x)` would work only in files that did *not*
/// import it.
pub static BYTES_PKG: Package = Package {
    import_name: "std/bytes",
    global_name: "bytes",
    fns: BYTES_PKG_FNS,
    natives: &[],
    register_types: register_bytes_pkg_types,
};

pub(crate) static BYTES_METHODS: &[BuiltinFn] = &[
    BuiltinFn { name: "len", vm_impl: bytes_len },
    BuiltinFn { name: "decode", vm_impl: bytes_decode },
    BuiltinFn { name: "slice", vm_impl: bytes_slice },
];

pub fn find_bytes_method(name: &str) -> Option<BuiltinFn> {
    BYTES_METHODS.iter().find(|m| m.name == name).copied()
}

pub fn register_bytes_method_types(ctx: &mut TypeContext) {
    let methods: &[(&str, JadeType)] = &[
        ("len", JadeType::Fn { params: vec![], ret: Box::new(JadeType::Int) }),
        ("decode", JadeType::Fn { params: vec![], ret: Box::new(JadeType::Str) }),
        (
            "slice",
            JadeType::Fn {
                params: vec![JadeType::Int, JadeType::Int],
                ret: Box::new(JadeType::Bytes),
            },
        ),
    ];
    for (name, ty) in methods {
        ctx.define_primitive_method("bytes", name, ty.clone());
    }
}
