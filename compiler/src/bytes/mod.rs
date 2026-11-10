//! `bytes` — the primitive methods on a binary blob.
//!
//! A `bytes` value is a counted sequence of raw octets, distinct from `str`
//! because Jade strings are UTF-8 and NUL-terminated and arbitrary bytes are
//! neither. The value and its methods are support code `src/backend/bytes.rs`
//! emits; this module only names the methods and registers their types.
//!
//! There are deliberately few methods. `bytes` exists to carry data through a
//! program unchanged — from a file, over a socket, out to stdout — not to be a
//! second string type with a parallel set of operations.

use crate::builtins::Package;
use crate::compiler::tir::JadeType;
use crate::compiler::type_infer::TypeContext;

/// The methods a `bytes` receiver has, by name.
pub const BYTES_METHODS: &[&str] = &["len", "decode", "slice"];

fn register_bytes_pkg_types(ctx: &mut TypeContext) {
    ctx.define("bytes".to_string(), JadeType::Unknown);
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

pub static BYTES_PKG: Package = Package {
    import_name: "std/bytes",
    global_name: "bytes",
    fns: &["zeros", "from_ints", "concat"],
    register_types: register_bytes_pkg_types,
};
