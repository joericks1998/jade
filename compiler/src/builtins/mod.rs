//! The built-in registry: the `PACKAGES` table and the names every program has.
//!
//! Each package is a flat top-level module (`crate::array`, `crate::bytes`, …)
//! that declares its function names. Nothing here runs: the per-file pass in
//! `typechecker/resolve.rs` binds a package's name where a `use` names it,
//! `typechecker/infer/builtins.rs` types each call, `mir/` lowers it, and
//! `src/codegen/` emits the code. Since 2.0.8 the table holds only the value
//! types' own packages, `string`, `array`, `dict` and `bytes`, and the
//! inference prelude's `__infer`.
//!
//! Every other std package, such as `std::fs`, is written in Jade: a `.jde`
//! module in `src/lib/`, and `crate::stdlib` is its registry. The functions at
//! the bottom of this file answer for both kinds, for the places that only
//! need to know whether a `use` names a std package and what it has.

use crate::typechecker::resolve::Names;

// The built-in packages are flat top-level modules; pull them into scope so the
// registry can refer to them by bare name.
use crate::{array, bytes, core, dict, future, string};

// ── Package ───────────────────────────────────────────────────────────────────

/// A std package a program brings in with `use std::name`.
///
/// `import_name` is the path the `use` names ("std/string").
/// `global_name` is the name it binds in the program ("string"), which the
/// per-file pass records as a package.
pub struct Package {
    pub import_name: &'static str,
    pub global_name: &'static str,
    /// Every function the package has, by name.
    pub fns: &'static [&'static str],
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
    &array::ARRAY_PKG,
    &dict::DICT_PKG,
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

/// Returns true if `name` is the global binding name of a package in this
/// table (e.g. "string", "bytes"). Used to separate stdlib imports from
/// user-defined exports when packaging module globals.
pub fn is_package_global_name(name: &str) -> bool {
    PACKAGES.iter().any(|p| p.global_name == name)
}

/// Purpose: This function says whether a `use` names a std package, of
/// either kind: one the backend writes, or one written in Jade.
///
/// Inputs:
/// - `import_name`: the path the `use` names, such as `std/path`.
///
/// Outputs: true for a std package.
pub fn is_std_package(import_name: &str) -> bool {
    find_package(import_name).is_some() || crate::stdlib::find(import_name).is_some()
}

/// Purpose: This function returns the name a std package binds, of either kind.
///
/// Inputs:
/// - `import_name`: the path the `use` names.
///
/// Outputs: the name, such as `path`, or `None` for a path that is not a std
/// package.
pub fn std_global_name(import_name: &str) -> Option<&'static str> {
    find_package(import_name)
        .map(|p| p.global_name)
        .or_else(|| crate::stdlib::find(import_name).map(|l| l.name))
}

/// Purpose: This function lists the functions a std package bound under a
/// name has, of either kind, in the order a message lists them.
///
/// Inputs:
/// - `global_name`: the name the package binds, such as `math`.
///
/// Outputs: the package's path as a program writes it, `std::math`, and its
/// functions, or `None` for a name no std package binds.
pub fn std_functions(global_name: &str) -> Option<(String, Vec<String>)> {
    if let Some(p) = package_by_global_name(global_name) {
        return Some((
            p.import_name.replace('/', "::"),
            p.fns.iter().map(|f| f.to_string()).collect(),
        ));
    }
    let lib = crate::stdlib::by_name(global_name)?;
    Some((format!("std::{}", lib.name), lib.public_fns().to_vec()))
}

/// Every global a program has without an import.
pub fn global_names() -> &'static [&'static str] {
    core::GLOBAL_NAMES
}

/// Bind every global a program has without an import in the per-file pass's
/// name table.
pub fn register_core_names(names: &mut Names) {
    core::register_names(names);
}

#[cfg(test)]
mod tests;
