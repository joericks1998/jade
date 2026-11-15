//! The globals every program has without an import: `print`, `len`, `write`,
//! `input`, the task controls, the type functions and their converters, and
//! `Grammar`.

use crate::typechecker::resolve::{NameKind, Names};

/// Every global a program can name without importing anything.
///
/// The static checker keeps its own list of the callable ones, in
/// `is_builtin_value` in `typechecker/infer/builtins.rs`.
pub const GLOBAL_NAMES: &[&str] = &[
    "print",
    "len",
    "write",
    "input",
    "cancelled",
    "max_tasks",
    "set_max_tasks",
    "max_tokens",
    "set_max_tokens",
    "wait",
    "route",
    "int",
    "float",
    "bool",
    "str",
    "char",
    "toInt",
    "toFloat",
    "toString",
    "toBool",
    "toChar",
    "toDict",
    "byte",
    "short",
    "ushort",
    "int32",
    "uint32",
    "long",
    "ulong",
    "float32",
    "double",
    "toByte",
    "toShort",
    "toUshort",
    "toInt32",
    "toUint32",
    "toLong",
    "toUlong",
    "toFloat32",
    "toDouble",
    "func",
    "Grammar",
];

// ── Name registration ─────────────────────────────────────────────────────────

/// Purpose: This function binds every global in the per-file pass's name
/// table, so a call to one is not an undefined name.
///
/// Inputs:
/// - `names`: the table a file starts with.
///
/// Outputs: none.
pub fn register_names(names: &mut Names) {
    // `join` is a builtin the checker types, though not one a program lists.
    for name in GLOBAL_NAMES.iter().chain(&["join"]) {
        // `Grammar` is a namespace, `Grammar.new(…)`, not a function to call.
        let kind = if *name == "Grammar" { NameKind::Value } else { NameKind::Function };
        names.define(name.to_string(), kind);
    }
    // nil / None / null literals — all three spellings are valid
    for name in ["nil", "None", "null"] {
        names.define(name.to_string(), NameKind::Value);
    }
}
