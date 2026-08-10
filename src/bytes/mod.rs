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

use jade_runtime::bytesf::BytesObj;
use std::sync::Arc;

use crate::builtins::BuiltinFn;
use crate::compiler::{tir::JadeType, type_infer::TypeContext};
use crate::frontend::error::{JadeError, Result, Span};
use crate::vm::VmValue;

const ZERO: Span = Span { line: 0, col: 0 };

/// `b.len()` — octet count.
fn bytes_len(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Bytes(b) => Ok(VmValue::Int(b.len() as i64)),
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
        VmValue::Bytes(b) => match std::str::from_utf8(b.as_slice()) {
            Ok(s) => Ok(VmValue::Str(jade_runtime::trust::JStr::with_trust(s, b.trust))),
            Err(e) => Err(JadeError::Exception {
                message: format!("bytes.decode(): not valid UTF-8 at byte {}", e.valid_up_to()),
                span: ZERO,
            }),
        },
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
            let len = b.len() as i64;
            let start = (*s).clamp(0, len) as usize;
            let end = (*e).clamp(start as i64, len) as usize;
            Ok(VmValue::Bytes(Arc::new(BytesObj::new(b.as_slice()[start..end].to_vec(), b.trust))))
        }
        _ => Err(JadeError::TypeError { message: "bytes.slice".to_string(), span: ZERO }),
    }
}

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
