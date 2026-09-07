#[cfg(test)]
mod tests;

use jade_runtime::coll::DictObj;

use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext},
    frontend::error::{JadeError, Result, Span},
    vm::VmValue,
};

use crate::builtins::{BuiltinFn, Package, make_array};
use jade_runtime::trust::JStr;

const ZERO: Span = Span { line: 0, col: 0 };

/// Convert a parsed JSON value to a `VmValue`, tagging every string it
/// contains with `trust`.
///
/// The trust is the *input's*. A model reply or an HTTP body is tainted, and
/// pulling a field out of it does not make that field trustworthy — but this
/// used to build every string with `.into()` (`JStr::trusted`), so
/// `sh.exec(json.parse(reply)["cmd"])` was refused when compiled and executed
/// under `jade run`. Parsing is the widest path from an LLM to a sink in the
/// language, so it is the one that most needs to carry the taint. The compiled
/// backend (`jsonf.rs`) already threaded the input's trust through.
fn json_to_vm(val: serde_json::Value, trust: u8) -> VmValue {
    match val {
        serde_json::Value::Null => VmValue::Nil,
        serde_json::Value::Bool(b) => VmValue::Bool(b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                VmValue::Int(i)
            } else {
                VmValue::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        serde_json::Value::String(s) => VmValue::Str(JStr::with_trust(s, trust)),
        serde_json::Value::Array(arr) => {
            make_array(arr.into_iter().map(|v| json_to_vm(v, trust)).collect())
        }
        serde_json::Value::Object(map) => {
            let mut d = DictObj::new();
            for (k, v) in map {
                d.insert(k, json_to_vm(v, trust));
            }
            VmValue::dict(d)
        }
    }
}

/// Convert a `VmValue` to JSON, or `None` when it nests past
/// `jade_runtime::render::MAX_DEPTH`.
///
/// Two problems, both from a value that can contain itself — arrays and dicts
/// are reference-semantic and nothing collects cycles. This held an array's
/// lock while recursing over its elements, so a self-containing array re-locked
/// a `parking_lot` mutex the same thread already held, and
/// `json.stringify(a)` hung with no output rather than raising. And the
/// recursion had no bound, so even without a cycle a deep value overflowed the
/// stack. Elements are copied out of the lock first (the copies are `Arc`
/// clones), and depth is bounded by the same constant both renderers use.
///
/// Refusing past the bound rather than emitting `null`: JSON cannot represent a
/// cycle, and quietly writing `null` where a value was would hand back a
/// document that does not say what it holds.
fn vm_to_json(val: &VmValue) -> Option<serde_json::Value> {
    vm_to_json_at(val, 0)
}

fn vm_to_json_at(val: &VmValue, depth: usize) -> Option<serde_json::Value> {
    if depth > jade_runtime::render::MAX_DEPTH {
        return None;
    }
    Some(match val {
        VmValue::Nil => serde_json::Value::Null,
        VmValue::Bool(b) => serde_json::Value::Bool(*b),
        VmValue::Int(i) => serde_json::Value::Number((*i).into()),
        VmValue::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        VmValue::Str(s) => serde_json::Value::String(s.to_string()),
        VmValue::Array(arc) => {
            let items: Vec<VmValue> = arc.lock().iter().cloned().collect();
            let mut out = Vec::with_capacity(items.len());
            for v in &items {
                out.push(vm_to_json_at(v, depth + 1)?);
            }
            serde_json::Value::Array(out)
        }
        VmValue::Dict(map) => {
            let mut obj = serde_json::Map::new();
            for (k, v) in map.iter() {
                obj.insert(k.clone(), vm_to_json_at(v, depth + 1)?);
            }
            serde_json::Value::Object(obj)
        }
        _ => serde_json::Value::Null,
    })
}

/// The error a value too deep to serialize raises, on both engines.
fn too_deep(what: &str) -> JadeError {
    JadeError::IoError {
        message: format!(
            "{what}: value nests deeper than {} levels (a value that contains itself cannot be \
             represented as JSON)",
            jade_runtime::render::MAX_DEPTH
        ),
        span: ZERO,
    }
}

fn json_parse(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let (s, trust) = match &args[0] {
        VmValue::Str(s) => (s.as_str(), s.trust()),
        _ => return Err(JadeError::TypeError { message: "json.parse".to_string(), span: ZERO }),
    };
    let val: serde_json::Value = serde_json::from_str(s)
        .map_err(|e| JadeError::IoError { message: format!("json.parse: {}", e), span: ZERO })?;
    Ok(json_to_vm(val, trust))
}

fn json_stringify(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let j = vm_to_json(&args[0]).ok_or_else(|| too_deep("json.stringify"))?;
    serde_json::to_string(&j)
        .map(|s| VmValue::Str(JStr::trusted(s)))
        .map_err(|e| JadeError::IoError { message: format!("json.stringify: {}", e), span: ZERO })
}

fn json_stringify_pretty(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let j = vm_to_json(&args[0]).ok_or_else(|| too_deep("json.stringify_pretty"))?;
    serde_json::to_string_pretty(&j).map(|s| VmValue::Str(JStr::trusted(s))).map_err(|e| {
        JadeError::IoError { message: format!("json.stringify_pretty: {}", e), span: ZERO }
    })
}

static JSON_PKG_FNS: &[BuiltinFn] = &[
    BuiltinFn { name: "parse", vm_impl: json_parse },
    BuiltinFn { name: "stringify", vm_impl: json_stringify },
    BuiltinFn { name: "stringify_pretty", vm_impl: json_stringify_pretty },
];

fn register_json_pkg_types(ctx: &mut TypeContext) {
    ctx.define("json".to_string(), JadeType::Unknown);
}

pub static JSON_PKG: Package = Package {
    import_name: "std/json",
    global_name: "json",
    fns: JSON_PKG_FNS,
    natives: &[],
    register_types: register_json_pkg_types,
};
