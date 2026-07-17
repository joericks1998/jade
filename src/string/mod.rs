#[cfg(test)]
mod tests;

use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext, vm::VmValue},
    frontend::error::{JadeError, Result, Span},
};

use crate::builtins::{BuiltinFn, Package, make_array};

const ZERO: Span = Span { line: 0, col: 0 };

// ── Primitive str methods (args[0] = self) ────────────────────────────────────

fn str_len(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Str(s) => Ok(VmValue::Int(s.chars().count() as i64)),
        _ => Err(JadeError::TypeError { message: "str.len".to_string(), span: ZERO }),
    }
}

fn str_upper(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Str(s) => Ok(VmValue::Str(s.to_uppercase())),
        _ => Err(JadeError::TypeError { message: "str.upper".to_string(), span: ZERO }),
    }
}

fn str_lower(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Str(s) => Ok(VmValue::Str(s.to_lowercase())),
        _ => Err(JadeError::TypeError { message: "str.lower".to_string(), span: ZERO }),
    }
}

fn str_trim(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Str(s) => Ok(VmValue::Str(s.trim().to_string())),
        _ => Err(JadeError::TypeError { message: "str.trim".to_string(), span: ZERO }),
    }
}

fn str_split(args: &[VmValue]) -> Result<VmValue> {
    match (&args[0], args.get(1)) {
        (VmValue::Str(s), Some(VmValue::Str(sep))) => {
            let parts: Vec<VmValue> = s.split(sep.as_str()).map(|p| VmValue::Str(p.to_string())).collect();
            Ok(make_array(parts))
        }
        _ => Err(JadeError::TypeError { message: "str.split".to_string(), span: ZERO }),
    }
}

fn str_contains(args: &[VmValue]) -> Result<VmValue> {
    match (&args[0], args.get(1)) {
        (VmValue::Str(s), Some(VmValue::Str(sub))) => Ok(VmValue::Bool(s.contains(sub.as_str()))),
        _ => Err(JadeError::TypeError { message: "str.contains".to_string(), span: ZERO }),
    }
}

fn str_replace(args: &[VmValue]) -> Result<VmValue> {
    match (&args[0], args.get(1), args.get(2)) {
        (VmValue::Str(s), Some(VmValue::Str(from)), Some(VmValue::Str(to))) => {
            Ok(VmValue::Str(s.replace(from.as_str(), to.as_str())))
        }
        _ => Err(JadeError::TypeError { message: "str.replace".to_string(), span: ZERO }),
    }
}

fn str_starts_with(args: &[VmValue]) -> Result<VmValue> {
    match (&args[0], args.get(1)) {
        (VmValue::Str(s), Some(VmValue::Str(prefix))) => Ok(VmValue::Bool(s.starts_with(prefix.as_str()))),
        _ => Err(JadeError::TypeError { message: "str.starts_with".to_string(), span: ZERO }),
    }
}

fn str_ends_with(args: &[VmValue]) -> Result<VmValue> {
    match (&args[0], args.get(1)) {
        (VmValue::Str(s), Some(VmValue::Str(suffix))) => Ok(VmValue::Bool(s.ends_with(suffix.as_str()))),
        _ => Err(JadeError::TypeError { message: "str.ends_with".to_string(), span: ZERO }),
    }
}

static STR_METHODS: &[BuiltinFn] = &[
    BuiltinFn { name: "len",         vm_impl: str_len },
    BuiltinFn { name: "upper",       vm_impl: str_upper },
    BuiltinFn { name: "lower",       vm_impl: str_lower },
    BuiltinFn { name: "trim",        vm_impl: str_trim },
    BuiltinFn { name: "split",       vm_impl: str_split },
    BuiltinFn { name: "contains",    vm_impl: str_contains },
    BuiltinFn { name: "replace",     vm_impl: str_replace },
    BuiltinFn { name: "starts_with", vm_impl: str_starts_with },
    BuiltinFn { name: "ends_with",   vm_impl: str_ends_with },
];

pub fn find_str_method(name: &str) -> Option<BuiltinFn> {
    STR_METHODS.iter().find(|m| m.name == name).copied()
}

// ── std/string package functions (functional style, args[0] = first arg) ─────

fn pkg_split(args: &[VmValue]) -> Result<VmValue> {
    match (&args[0], args.get(1)) {
        (VmValue::Str(s), Some(VmValue::Str(sep))) => {
            let parts: Vec<VmValue> = s.split(sep.as_str()).map(|p| VmValue::Str(p.to_string())).collect();
            Ok(make_array(parts))
        }
        _ => Err(JadeError::TypeError { message: "string.split".to_string(), span: ZERO }),
    }
}

fn pkg_upper(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Str(s) => Ok(VmValue::Str(s.to_uppercase())),
        _ => Err(JadeError::TypeError { message: "string.upper".to_string(), span: ZERO }),
    }
}

fn pkg_lower(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Str(s) => Ok(VmValue::Str(s.to_lowercase())),
        _ => Err(JadeError::TypeError { message: "string.lower".to_string(), span: ZERO }),
    }
}

fn pkg_trim(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Str(s) => Ok(VmValue::Str(s.trim().to_string())),
        _ => Err(JadeError::TypeError { message: "string.trim".to_string(), span: ZERO }),
    }
}

fn pkg_contains(args: &[VmValue]) -> Result<VmValue> {
    match (&args[0], args.get(1)) {
        (VmValue::Str(s), Some(VmValue::Str(sub))) => Ok(VmValue::Bool(s.contains(sub.as_str()))),
        _ => Err(JadeError::TypeError { message: "string.contains".to_string(), span: ZERO }),
    }
}

fn pkg_replace(args: &[VmValue]) -> Result<VmValue> {
    match (&args[0], args.get(1), args.get(2)) {
        (VmValue::Str(s), Some(VmValue::Str(from)), Some(VmValue::Str(to))) => {
            Ok(VmValue::Str(s.replace(from.as_str(), to.as_str())))
        }
        _ => Err(JadeError::TypeError { message: "string.replace".to_string(), span: ZERO }),
    }
}

fn pkg_starts_with(args: &[VmValue]) -> Result<VmValue> {
    match (&args[0], args.get(1)) {
        (VmValue::Str(s), Some(VmValue::Str(prefix))) => Ok(VmValue::Bool(s.starts_with(prefix.as_str()))),
        _ => Err(JadeError::TypeError { message: "string.starts_with".to_string(), span: ZERO }),
    }
}

fn pkg_ends_with(args: &[VmValue]) -> Result<VmValue> {
    match (&args[0], args.get(1)) {
        (VmValue::Str(s), Some(VmValue::Str(suffix))) => Ok(VmValue::Bool(s.ends_with(suffix.as_str()))),
        _ => Err(JadeError::TypeError { message: "string.ends_with".to_string(), span: ZERO }),
    }
}

static STRING_PKG_FNS: &[BuiltinFn] = &[
    BuiltinFn { name: "split",       vm_impl: pkg_split },
    BuiltinFn { name: "upper",       vm_impl: pkg_upper },
    BuiltinFn { name: "lower",       vm_impl: pkg_lower },
    BuiltinFn { name: "trim",        vm_impl: pkg_trim },
    BuiltinFn { name: "contains",    vm_impl: pkg_contains },
    BuiltinFn { name: "replace",     vm_impl: pkg_replace },
    BuiltinFn { name: "starts_with", vm_impl: pkg_starts_with },
    BuiltinFn { name: "ends_with",   vm_impl: pkg_ends_with },
];

fn register_string_pkg_types(ctx: &mut TypeContext) {
    ctx.define("string".to_string(), JadeType::Unknown);
}

pub static STRING_PKG: Package = Package {
    import_name: "std/string",
    global_name: "string",
    fns: STRING_PKG_FNS,
    register_types: register_string_pkg_types,
};

// ── Type checker primitive method registration ────────────────────────────────

pub fn register_str_method_types(ctx: &mut TypeContext) {
    let ret_str  = || Box::new(JadeType::Str);
    let ret_int  = || Box::new(JadeType::Int);
    let ret_bool = || Box::new(JadeType::Bool);
    let ret_arr  = || Box::new(JadeType::Array(Box::new(JadeType::Str)));

    let methods: &[(&str, JadeType)] = &[
        ("len",         JadeType::Fn { params: vec![], ret: ret_int() }),
        ("upper",       JadeType::Fn { params: vec![], ret: ret_str() }),
        ("lower",       JadeType::Fn { params: vec![], ret: ret_str() }),
        ("trim",        JadeType::Fn { params: vec![], ret: ret_str() }),
        ("split",       JadeType::Fn { params: vec![JadeType::Str], ret: ret_arr() }),
        ("contains",    JadeType::Fn { params: vec![JadeType::Str], ret: ret_bool() }),
        ("replace",     JadeType::Fn { params: vec![JadeType::Str, JadeType::Str], ret: ret_str() }),
        ("starts_with", JadeType::Fn { params: vec![JadeType::Str], ret: ret_bool() }),
        ("ends_with",   JadeType::Fn { params: vec![JadeType::Str], ret: ret_bool() }),
    ];
    for (name, ty) in methods {
        ctx.define_primitive_method("str", name, ty.clone());
    }
}
