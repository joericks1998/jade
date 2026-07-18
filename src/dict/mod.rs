#[cfg(test)]
mod tests;

use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext, vm::VmValue},
    frontend::error::{JadeError, Result, Span},
};

use crate::builtins::{BuiltinFn, Package, make_array};

const ZERO: Span = Span { line: 0, col: 0 };

// ── Primitive dict methods (args[0] = self as Dict) ───────────────────────────

fn dict_len(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Dict(m) => Ok(VmValue::Int(m.len() as i64)),
        _ => Err(JadeError::TypeError { message: "dict.len".to_string(), span: ZERO }),
    }
}

fn dict_keys(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Dict(m) => {
            let mut keys: Vec<String> = m.keys().cloned().collect();
            keys.sort();
            Ok(make_array(keys.into_iter().map(VmValue::Str).collect()))
        }
        _ => Err(JadeError::TypeError { message: "dict.keys".to_string(), span: ZERO }),
    }
}

fn dict_values(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Dict(m) => {
            let mut pairs: Vec<(&String, &VmValue)> = m.iter().collect();
            pairs.sort_by_key(|(k, _)| k.as_str());
            Ok(make_array(pairs.into_iter().map(|(_, v)| v.clone()).collect()))
        }
        _ => Err(JadeError::TypeError { message: "dict.values".to_string(), span: ZERO }),
    }
}

fn dict_has(args: &[VmValue]) -> Result<VmValue> {
    match (&args[0], args.get(1)) {
        (VmValue::Dict(m), Some(VmValue::Str(k))) => Ok(VmValue::Bool(m.contains_key(k.as_str()))),
        _ => Err(JadeError::TypeError { message: "dict.has".to_string(), span: ZERO }),
    }
}

fn dict_get(args: &[VmValue]) -> Result<VmValue> {
    match (&args[0], args.get(1)) {
        (VmValue::Dict(m), Some(VmValue::Str(k))) => Ok(m.get(k.as_str()).cloned().unwrap_or(VmValue::Nil)),
        _ => Err(JadeError::TypeError { message: "dict.get".to_string(), span: ZERO }),
    }
}

static DICT_METHODS: &[BuiltinFn] = &[
    BuiltinFn { name: "len",    vm_impl: dict_len },
    BuiltinFn { name: "keys",   vm_impl: dict_keys },
    BuiltinFn { name: "values", vm_impl: dict_values },
    BuiltinFn { name: "has",    vm_impl: dict_has },
    BuiltinFn { name: "get",    vm_impl: dict_get },
];

pub fn find_dict_method(name: &str) -> Option<BuiltinFn> {
    DICT_METHODS.iter().find(|m| m.name == name).copied()
}

// ── std/dict package functions ────────────────────────────────────────────────

fn pkg_keys(args: &[VmValue]) -> Result<VmValue> { dict_keys(args) }
fn pkg_values(args: &[VmValue]) -> Result<VmValue> { dict_values(args) }
fn pkg_has(args: &[VmValue]) -> Result<VmValue> { dict_has(args) }
fn pkg_get(args: &[VmValue]) -> Result<VmValue> { dict_get(args) }

fn pkg_merge(args: &[VmValue]) -> Result<VmValue> {
    match (&args[0], args.get(1)) {
        (VmValue::Dict(d1), Some(VmValue::Dict(d2))) => {
            let mut merged = d1.clone();
            for (k, v) in d2.iter() {
                merged.insert(k.clone(), v.clone());
            }
            Ok(VmValue::Dict(merged))
        }
        _ => Err(JadeError::TypeError { message: "dict.merge".to_string(), span: ZERO }),
    }
}

static DICT_PKG_FNS: &[BuiltinFn] = &[
    BuiltinFn { name: "keys",   vm_impl: pkg_keys },
    BuiltinFn { name: "values", vm_impl: pkg_values },
    BuiltinFn { name: "has",    vm_impl: pkg_has },
    BuiltinFn { name: "get",    vm_impl: pkg_get },
    BuiltinFn { name: "merge",  vm_impl: pkg_merge },
];

fn register_dict_pkg_types(ctx: &mut TypeContext) {
    ctx.define("dict".to_string(), JadeType::Unknown);
}

pub static DICT_PKG: Package = Package {
    import_name: "std/dict",
    global_name: "dict",
    fns: DICT_PKG_FNS,
    register_types: register_dict_pkg_types,
};

// ── Type checker primitive method registration ────────────────────────────────

pub fn register_dict_method_types(ctx: &mut TypeContext) {
    let methods: &[(&str, JadeType)] = &[
        ("len",    JadeType::Fn { params: vec![], ret: Box::new(JadeType::Int) }),
        ("keys",   JadeType::Fn { params: vec![], ret: Box::new(JadeType::Array(Box::new(JadeType::Str))) }),
        ("values", JadeType::Fn { params: vec![], ret: Box::new(JadeType::Array(Box::new(JadeType::Unknown))) }),
        ("has",    JadeType::Fn { params: vec![JadeType::Str], ret: Box::new(JadeType::Bool) }),
        ("get",    JadeType::Fn { params: vec![JadeType::Str], ret: Box::new(JadeType::Unknown) }),
    ];
    for (name, ty) in methods {
        ctx.define_primitive_method("dict", name, ty.clone());
    }
}
