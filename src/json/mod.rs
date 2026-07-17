use std::collections::HashMap;

use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext, vm::VmValue},
    frontend::error::{JadeError, Result, Span},
};

use crate::builtins::{BuiltinFn, Package, make_array};

const ZERO: Span = Span { line: 0, col: 0 };

fn json_to_vm(val: serde_json::Value) -> VmValue {
    match val {
        serde_json::Value::Null      => VmValue::Nil,
        serde_json::Value::Bool(b)   => VmValue::Bool(b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() { VmValue::Int(i) }
            else { VmValue::Float(n.as_f64().unwrap_or(0.0)) }
        }
        serde_json::Value::String(s) => VmValue::Str(s),
        serde_json::Value::Array(arr) => {
            make_array(arr.into_iter().map(json_to_vm).collect())
        }
        serde_json::Value::Object(map) => {
            let mut d = HashMap::new();
            for (k, v) in map { d.insert(k, json_to_vm(v)); }
            VmValue::Dict(d)
        }
    }
}

fn vm_to_json(val: &VmValue) -> serde_json::Value {
    match val {
        VmValue::Nil      => serde_json::Value::Null,
        VmValue::Bool(b)  => serde_json::Value::Bool(*b),
        VmValue::Int(i)   => serde_json::Value::Number((*i).into()),
        VmValue::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        VmValue::Str(s)   => serde_json::Value::String(s.clone()),
        VmValue::Array(arc) => {
            let guard = arc.lock();
            serde_json::Value::Array(guard.iter().map(vm_to_json).collect())
        }
        VmValue::Dict(map) => {
            let obj: serde_json::Map<_, _> = map.iter()
                .map(|(k, v)| (k.clone(), vm_to_json(v)))
                .collect();
            serde_json::Value::Object(obj)
        }
        _ => serde_json::Value::Null,
    }
}

fn json_parse(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let s = match &args[0] {
        VmValue::Str(s) => s.as_str(),
        _ => return Err(JadeError::TypeError { message: "json.parse".to_string(), span: ZERO }),
    };
    let val: serde_json::Value = serde_json::from_str(s).map_err(|e| JadeError::IoError {
        message: format!("json.parse: {}", e),
        span: ZERO,
    })?;
    Ok(json_to_vm(val))
}

fn json_stringify(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let j = vm_to_json(&args[0]);
    serde_json::to_string(&j)
        .map(VmValue::Str)
        .map_err(|e| JadeError::IoError { message: format!("json.stringify: {}", e), span: ZERO })
}

fn json_stringify_pretty(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let j = vm_to_json(&args[0]);
    serde_json::to_string_pretty(&j)
        .map(VmValue::Str)
        .map_err(|e| JadeError::IoError { message: format!("json.stringify_pretty: {}", e), span: ZERO })
}

static JSON_PKG_FNS: &[BuiltinFn] = &[
    BuiltinFn { name: "parse",            vm_impl: json_parse },
    BuiltinFn { name: "stringify",        vm_impl: json_stringify },
    BuiltinFn { name: "stringify_pretty", vm_impl: json_stringify_pretty },
];

fn register_json_pkg_types(ctx: &mut TypeContext) {
    ctx.define("json".to_string(), JadeType::Unknown);
}

pub static JSON_PKG: Package = Package {
    import_name: "std/json",
    global_name: "json",
    fns: JSON_PKG_FNS,
    register_types: register_json_pkg_types,
};
