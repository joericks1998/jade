//! The built-in registry: the `PACKAGES` table and the names every program has.
//!
//! Each package is a flat top-level module (`crate::array`, `crate::math`, …)
//! that declares its function names and registers its types with the checker.
//! Nothing here runs: `compiler/infer/builtins.rs` types each call, `mir/`
//! lowers it, and `src/backend/` emits the code.

use crate::compiler::type_infer::TypeContext;

// The built-in packages are flat top-level modules; pull them into scope so the
// registry can refer to them by bare name.
use crate::{
    array, bytes, core, dict, env, fs, future, http, json, math, path, random, sh, string, time,
    uhttp,
};

// ── Package ───────────────────────────────────────────────────────────────────

/// A stdlib package injected via `use "..."`.
///
/// `import_name` is the string in the `use` statement ("std/string").
/// `global_name` is the variable name injected into scope ("string").
pub struct Package {
    pub import_name: &'static str,
    pub global_name: &'static str,
    /// Every function the package has, by name.
    pub fns: &'static [&'static str],
    pub register_types: fn(ctx: &mut TypeContext),
}

impl Package {
    /// Whether the package has a function of this name.
    pub fn has_fn(&self, name: &str) -> bool {
        self.fns.contains(&name)
    }
}

// ── Registries ────────────────────────────────────────────────────────────────

/// All stdlib packages (available via `use "..."`).
static PACKAGES: &[&Package] = &[
    &string::STRING_PKG,
    &math::MATH_PKG,
    &array::ARRAY_PKG,
    &dict::DICT_PKG,
    &fs::FS_PKG,
    &time::TIME_PKG,
    &http::HTTP_PKG,
    &uhttp::UHTTP_PKG,
    &sh::SH_PKG,
    &json::JSON_PKG,
    &env::ENV_PKG,
    &path::PATH_PKG,
    &random::RANDOM_PKG,
    &bytes::BYTES_PKG,
    &crate::infer::INFER_PKG,
];

// ── Primitive method tables ───────────────────────────────────────────────────

/// How many arguments a primitive method takes, not counting the receiver.
///
/// A surplus argument used to be dropped in silence: `"abc".upper(1, 2, 3)`
/// answered `ABC`, and a stray argument is exactly what gets left behind when a
/// call is edited.
///
/// Keyed by name alone: the method tables share `len` and `contains` across
/// every receiver kind, and neither differs in arity by kind.
/// `primitive_methods_all_declare_an_arity` fails if a method is added to one of
/// those tables and not to this list.
pub fn primitive_method_arity(method: &str) -> Option<usize> {
    Some(match method {
        "len" | "upper" | "lower" | "trim" | "encode" | "pop" | "sort" | "reverse" | "keys"
        | "values" | "decode" => 0,
        "split" | "contains" | "starts_with" | "ends_with" | "push" | "map" | "filter" | "has"
        | "get" | "merge" | "index_of" | "last_index_of" | "count" | "repeat" | "join" => 1,
        "replace" | "slice" | "pad_start" | "pad_end" => 2,
        "trim_start" | "trim_end" | "capitalize" | "is_empty" | "lines" => 0,
        "ready" | "cancel" => 0,
        _ => return None,
    })
}

/// Every primitive method table, by receiver kind.
pub fn primitive_method_tables() -> [(&'static str, &'static [&'static str]); 5] {
    [
        ("str", string::STR_METHODS),
        ("bytes", bytes::BYTES_METHODS),
        ("array", array::ARRAY_METHODS),
        ("dict", dict::DICT_METHODS),
        ("future", future::FUTURE_METHODS),
    ]
}

// ── Public lookup API ─────────────────────────────────────────────────────────

/// Every stdlib package, for callers that need to walk the whole set rather
/// than look one up. The static checker and MIR lowering read it to tell a
/// package name from a value.
pub fn all_packages() -> &'static [&'static Package] {
    PACKAGES
}

pub fn find_package(import_name: &str) -> Option<&'static Package> {
    PACKAGES.iter().find(|p| p.import_name == import_name).copied()
}

/// The package a stdlib global name belongs to (`"math"` → `std/math`).
pub fn package_by_global_name(name: &str) -> Option<&'static Package> {
    PACKAGES.iter().find(|p| p.global_name == name).copied()
}

/// Returns true if `name` is the global binding name of any stdlib package
/// (e.g. "fs", "http", "json"). Used to separate stdlib imports from
/// user-defined exports when packaging module globals.
pub fn is_package_global_name(name: &str) -> bool {
    PACKAGES.iter().any(|p| p.global_name == name)
}

/// Every global a program has without an import.
pub fn global_names() -> &'static [&'static str] {
    core::GLOBAL_NAMES
}

/// Register type signatures for all core built-ins into the type checker.
pub fn register_core_types(ctx: &mut TypeContext) {
    core::register_types(ctx);
}

/// Register primitive method type information into the type checker.
pub fn register_primitive_method_types(ctx: &mut TypeContext) {
    string::register_str_method_types(ctx);
    bytes::register_bytes_method_types(ctx);
    array::register_array_method_types(ctx);
    dict::register_dict_method_types(ctx);
    future::register_future_method_types(ctx);
}

#[cfg(test)]
mod tests;
