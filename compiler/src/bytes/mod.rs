//! `bytes` — the primitive methods on a binary blob.
//!
//! A `bytes` value is a counted sequence of raw octets, distinct from `str`
//! because Jade strings are UTF-8 and NUL-terminated and arbitrary bytes are
//! neither. The value and its methods are support code `src/codegen/support/bytes.rs`
//! emits; this module only names the methods and the package functions.
//!
//! There are deliberately few methods. `bytes` exists to carry data through a
//! program unchanged — from a file, over a socket, out to stdout — not to be a
//! second string type with a parallel set of operations.

use crate::builtins::Package;

/// The methods a `bytes` receiver has, by name.
pub const BYTES_METHODS: &[&str] = &["len", "decode", "slice"];

pub static BYTES_PKG: Package = Package {
    import_name: "std/bytes",
    global_name: "bytes",
    fns: &["zeros", "from_ints", "concat"],
};
