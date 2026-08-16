//! The native built-in registry: shared types (`BuiltinFn`, `Package`,
//! `PrimType`), the `PACKAGES` table, and `seed_globals`. Each package is a
//! flat top-level module (`crate::array`, `crate::math`, …).

use std::{collections::HashMap, sync::Arc};

use jade_runtime::coll::{ArrayObj, DictObj};
use parking_lot::Mutex;

use crate::{
    compiler::type_infer::TypeContext,
    frontend::error::Result,
    vm::{NativeFnId, VmValue},
};

// The built-in packages are flat top-level modules; pull them into scope so the
// registry can refer to them by bare name.
use crate::uhttp;
use crate::{
    array, bytes, core, dict, env, fs, grammar, http, json, math, path, random, sh, string, time,
};

// ── BuiltinFn ─────────────────────────────────────────────────────────────────

/// A pure (no VmState mutation) Rust-backed function.
///
/// `vm_impl` receives all arguments as a slice: for primitive methods,
/// `args[0]` is the receiver (pre-pended by `NativeBoundMethod` dispatch).
pub type NativeFnPtr = fn(&[VmValue]) -> Result<VmValue>;

#[derive(Clone, Copy)]
pub struct BuiltinFn {
    pub name: &'static str,
    pub vm_impl: NativeFnPtr,
}

impl std::fmt::Debug for BuiltinFn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<builtin {}>", self.name)
    }
}

// ── NativeBoundMethod ─────────────────────────────────────────────────────────

/// A primitive method that has already captured its receiver.
///
/// Stored as `VmValue::NativeBoundMethod`. On call, the VM prepends
/// `receiver` as `args[0]` before invoking `method.vm_impl`.
pub struct NativeBoundMethod {
    pub receiver: VmValue,
    pub method: BuiltinFn,
}

// ── PrimType ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrimType {
    Str,
    Bytes,
    Array,
    Dict,
    Int,
    Float,
}

impl PrimType {
    pub fn from_value(v: &VmValue) -> Option<Self> {
        match v {
            VmValue::Str(_) => Some(PrimType::Str),
            VmValue::Bytes(_) => Some(PrimType::Bytes),
            VmValue::Array(_) => Some(PrimType::Array),
            VmValue::Dict(_) => Some(PrimType::Dict),
            VmValue::Int(_) => Some(PrimType::Int),
            VmValue::Float(_) => Some(PrimType::Float),
            _ => None,
        }
    }

    pub fn type_name(self) -> &'static str {
        match self {
            PrimType::Str => "str",
            PrimType::Bytes => "bytes",
            PrimType::Array => "array",
            PrimType::Dict => "dict",
            PrimType::Int => "int",
            PrimType::Float => "float",
        }
    }
}

// ── Package ───────────────────────────────────────────────────────────────────

/// A stdlib package injected via `use "..."`.
///
/// `import_name` is the string in the `use` statement ("llm", "std/string").
/// `global_name` is the variable name injected into scope ("llm", "string").
pub struct Package {
    pub import_name: &'static str,
    pub global_name: &'static str,
    pub fns: &'static [BuiltinFn],
    /// Functions the VM dispatches by id because they touch `VmState` — the
    /// token budget, the inference backend, the loaded model profile — and so
    /// cannot be expressed as a pure [`BuiltinFn`].
    ///
    /// Three packages needed these (`llm`, `std/array`, `std/uhttp`) and each
    /// grew its own `*_vm_dict_value()` override, hand-listed in the VM's
    /// `package_dict_value`. `llm` fared worst: with every one of its ten
    /// functions stateful, it declared each one *three* times — a
    /// `unreachable!()` stub, an entry in its `fns` table, and the real id in
    /// the override — so adding a function meant editing three lists and
    /// getting a runtime panic if you missed one.
    ///
    /// Listing the ids here instead makes that the package's own business. A
    /// package with no stateful functions passes `&[]`.
    pub natives: &'static [(&'static str, NativeFnId)],
    /// Register type information into the type checker when this package is imported.
    pub register_types: fn(ctx: &mut TypeContext),
}

impl Package {
    /// Build the VmValue::Dict for this package's functions.
    ///
    /// Natives are inserted after the pure functions, so a name appearing in
    /// both resolves to the stateful implementation. `std/array` relies on
    /// that: `map`/`filter` have pure entries used for their signatures but
    /// must dispatch through the VM to call a Jade function per element.
    pub fn vm_dict_value(&self) -> VmValue {
        let mut map = DictObj::new();
        for f in self.fns {
            map.insert(f.name.to_string(), VmValue::BuiltinFn(*f));
        }
        for (name, id) in self.natives {
            map.insert((*name).to_string(), VmValue::NativeFn(id.clone()));
        }
        VmValue::dict(map)
    }
}

// ── Registries ────────────────────────────────────────────────────────────────

/// All core globals (always available without import).
/// `print` and `stream` are excluded — they are state-mutating and dispatched
/// through `NativeFnId` variants injected directly in `seed_globals`.
static CORE_BUILTINS: &[BuiltinFn] = &[core::WRITE, core::LEN, core::INPUT];

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
];

// ── Primitive method tables ───────────────────────────────────────────────────

/// How many arguments a primitive method takes, not counting the receiver.
///
/// `BuiltinFn` carries none, and each implementation simply reads the arguments
/// it wants — so a surplus one was dropped in silence. `"abc".upper(1, 2, 3)`
/// answered `ABC` under `jade run` and was a build error compiled, and a stray
/// argument is exactly what gets left behind when a call is edited. Too *few*
/// was already caught, because the implementations pattern-match on what they
/// need and fall through to a type error.
///
/// Keyed by name alone: the four tables share `len` and `contains` across every
/// receiver kind, and neither differs in arity by kind. `primitive_methods_all_declare_an_arity`
/// fails if a method is added to one of those tables and not to this list.
pub fn primitive_method_arity(method: &str) -> Option<usize> {
    Some(match method {
        "len" | "upper" | "lower" | "trim" | "encode" | "pop" | "sort" | "reverse" | "keys"
        | "values" | "decode" => 0,
        "split" | "contains" | "starts_with" | "ends_with" | "push" | "map" | "filter" | "has"
        | "get" | "merge" => 1,
        "replace" | "slice" => 2,
        _ => return None,
    })
}

pub fn find_primitive_method(ty: PrimType, method: &str) -> Option<BuiltinFn> {
    match ty {
        PrimType::Str => string::find_str_method(method),
        PrimType::Bytes => bytes::find_bytes_method(method),
        PrimType::Array => array::find_array_method(method),
        PrimType::Dict => dict::find_dict_method(method),
        PrimType::Int => None,
        PrimType::Float => None,
    }
}

// ── Public lookup API ─────────────────────────────────────────────────────────

/// Every stdlib package, for callers that need to walk the whole set rather
/// than look one up. The tripwire tying these tables to the AOT backend's
/// lowering predicate uses it — see `codegen::tests`.
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

/// Pre-seed `globals` with all core built-in functions. Generic over the map's
/// hasher so the VM can seed a fast-hashed `globals` (see `VmState::globals`).
pub fn seed_globals<S: std::hash::BuildHasher>(globals: &mut HashMap<String, VmValue, S>) {
    for f in CORE_BUILTINS {
        globals.insert(f.name.to_string(), VmValue::BuiltinFn(*f));
    }
    // `print` needs async VmState access to drain a TokenStream, and `route`
    // needs it to call the method it dispatches to, so both go through
    // NativeFnId rather than the pure BuiltinFn path. (`stream` was here too
    // until v1.2.5, when `?p` folded onto the buffered stream and it went away.)
    globals.insert("print".to_string(), VmValue::NativeFn(NativeFnId::Print));
    globals.insert("route".to_string(), VmValue::NativeFn(NativeFnId::Route));
    // Primitive type constructors: callable with one arg like Python's int(), str(), etc.
    for name in &["int", "float", "bool", "str", "char", "func"] {
        globals.insert(name.to_string(), VmValue::TypeRef(name.to_string()));
    }
    // Grammar global: Grammar.new(pattern) → VmValue::Grammar(pattern)
    let mut grammar_fields = DictObj::new();
    grammar_fields.insert("new".to_string(), VmValue::BuiltinFn(grammar::GRAMMAR_NEW));
    globals.insert("Grammar".to_string(), VmValue::dict(grammar_fields));
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
}

// ── Helpers shared across package implementations ─────────────────────────────

/// Wrap a `Vec<VmValue>` as the standard Jade array value.
pub fn make_array(v: Vec<VmValue>) -> VmValue {
    VmValue::Array(Arc::new(Mutex::new(ArrayObj::from_vec(v))))
}

#[cfg(test)]
mod tests;
