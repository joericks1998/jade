pub mod core;
pub mod llm_pkg;
pub mod string_pkg;
pub mod array_pkg;
pub mod dict_pkg;
pub mod math_pkg;
pub mod fs_pkg;
pub mod time_pkg;
pub mod grammar_pkg;
pub mod http_pkg;
pub mod sh_pkg;
pub mod json_pkg;
pub mod env_pkg;
pub mod path_pkg;
pub mod random_pkg;

use std::{collections::HashMap, sync::Arc};

use parking_lot::Mutex;

use crate::{
    compiler::{type_infer::TypeContext, vm::{NativeFnId, VmValue}},
    frontend::error::Result,
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
    Array,
    Dict,
    Int,
    Float,
}

impl PrimType {
    pub fn from_value(v: &VmValue) -> Option<Self> {
        match v {
            VmValue::Str(_)   => Some(PrimType::Str),
            VmValue::Array(_) => Some(PrimType::Array),
            VmValue::Dict(_)  => Some(PrimType::Dict),
            VmValue::Int(_)   => Some(PrimType::Int),
            VmValue::Float(_) => Some(PrimType::Float),
            _ => None,
        }
    }

    pub fn type_name(self) -> &'static str {
        match self {
            PrimType::Str   => "str",
            PrimType::Array => "array",
            PrimType::Dict  => "dict",
            PrimType::Int   => "int",
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
    /// Register type information into the type checker when this package is imported.
    pub register_types: fn(ctx: &mut TypeContext),
}

impl Package {
    /// Build the VmValue::Dict for this package's functions.
    pub fn vm_dict_value(&self) -> VmValue {
        let mut map = HashMap::new();
        for f in self.fns {
            map.insert(f.name.to_string(), VmValue::BuiltinFn(*f));
        }
        VmValue::Dict(map)
    }
}

// ── Registries ────────────────────────────────────────────────────────────────

/// All core globals (always available without import).
/// `print` and `stream` are excluded — they are state-mutating and dispatched
/// through `NativeFnId` variants injected directly in `seed_globals`.
static CORE_BUILTINS: &[BuiltinFn] = &[
    core::WRITE,
    core::LEN,
    core::INPUT,
];

/// All stdlib packages (available via `use "..."`).
static PACKAGES: &[&Package] = &[
    &llm_pkg::LLM_PKG,
    &string_pkg::STRING_PKG,
    &math_pkg::MATH_PKG,
    &array_pkg::ARRAY_PKG,
    &dict_pkg::DICT_PKG,
    &fs_pkg::FS_PKG,
    &time_pkg::TIME_PKG,
    &http_pkg::HTTP_PKG,
    &sh_pkg::SH_PKG,
    &json_pkg::JSON_PKG,
    &env_pkg::ENV_PKG,
    &path_pkg::PATH_PKG,
    &random_pkg::RANDOM_PKG,
];

// ── Primitive method tables ───────────────────────────────────────────────────

pub fn find_primitive_method(ty: PrimType, method: &str) -> Option<BuiltinFn> {
    match ty {
        PrimType::Str   => string_pkg::find_str_method(method),
        PrimType::Array => array_pkg::find_array_method(method),
        PrimType::Dict  => dict_pkg::find_dict_method(method),
        PrimType::Int   => None,
        PrimType::Float => None,
    }
}

// ── Public lookup API ─────────────────────────────────────────────────────────

pub fn find_package(import_name: &str) -> Option<&'static Package> {
    PACKAGES.iter().find(|p| p.import_name == import_name).copied()
}

/// Returns true if `name` is the global binding name of any stdlib package
/// (e.g. "fs", "http", "json"). Used to separate stdlib imports from
/// user-defined exports when packaging module globals.
pub fn is_package_global_name(name: &str) -> bool {
    PACKAGES.iter().any(|p| p.global_name == name)
}

/// Pre-seed `globals` with all core built-in functions.
pub fn seed_globals(globals: &mut HashMap<String, VmValue>) {
    for f in CORE_BUILTINS {
        globals.insert(f.name.to_string(), VmValue::BuiltinFn(*f));
    }
    // `print` and `stream` need async VmState access to drain TokenStreams,
    // so they dispatch through NativeFnId rather than the pure BuiltinFn path.
    globals.insert("print".to_string(),  VmValue::NativeFn(NativeFnId::Print));
    globals.insert("stream".to_string(), VmValue::NativeFn(NativeFnId::Stream));
    globals.insert("route".to_string(),  VmValue::NativeFn(NativeFnId::Route));
    // Primitive type constructors: callable with one arg like Python's int(), str(), etc.
    for name in &["int", "float", "bool", "str", "func"] {
        globals.insert(name.to_string(), VmValue::TypeRef(name.to_string()));
    }
    // Grammar global: Grammar.new(pattern) → VmValue::Grammar(pattern)
    let mut grammar_fields = std::collections::HashMap::new();
    grammar_fields.insert("new".to_string(), VmValue::BuiltinFn(grammar_pkg::GRAMMAR_NEW));
    globals.insert("Grammar".to_string(), VmValue::Dict(grammar_fields));
}

/// Register type signatures for all core built-ins into the type checker.
pub fn register_core_types(ctx: &mut TypeContext) {
    core::register_types(ctx);
}

/// Register primitive method type information into the type checker.
pub fn register_primitive_method_types(ctx: &mut TypeContext) {
    string_pkg::register_str_method_types(ctx);
    array_pkg::register_array_method_types(ctx);
    dict_pkg::register_dict_method_types(ctx);
}

// ── Helpers shared across package implementations ─────────────────────────────

/// Wrap a `Vec<VmValue>` as the standard Jade array value.
pub fn make_array(v: Vec<VmValue>) -> VmValue {
    VmValue::Array(Arc::new(Mutex::new(v)))
}
