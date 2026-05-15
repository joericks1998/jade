use std::time::{SystemTime, UNIX_EPOCH};

use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext, vm::VmValue},
    frontend::error::{JadeError, Result, Span},
};

use super::{BuiltinFn, Package};

const ZERO: Span = Span { line: 0, col: 0 };

fn time_now(args: &[VmValue]) -> Result<VmValue> {
    if !args.is_empty() {
        return Err(JadeError::ArityMismatch { expected: 0, got: args.len(), span: ZERO });
    }
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    Ok(VmValue::Int(secs as i64))
}

fn time_now_ms(args: &[VmValue]) -> Result<VmValue> {
    if !args.is_empty() {
        return Err(JadeError::ArityMismatch { expected: 0, got: args.len(), span: ZERO });
    }
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    Ok(VmValue::Int(ms as i64))
}

fn time_sleep(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let secs = match &args[0] {
        VmValue::Int(n)   => *n as f64,
        VmValue::Float(f) => *f,
        _ => return Err(JadeError::TypeError { op: "time.sleep".to_string(), span: ZERO }),
    };
    if secs > 0.0 {
        std::thread::sleep(std::time::Duration::from_secs_f64(secs));
    }
    Ok(VmValue::Nil)
}

static TIME_PKG_FNS: &[BuiltinFn] = &[
    BuiltinFn { name: "now",    vm_impl: time_now },
    BuiltinFn { name: "now_ms", vm_impl: time_now_ms },
    BuiltinFn { name: "sleep",  vm_impl: time_sleep },
];

fn register_time_pkg_types(ctx: &mut TypeContext) {
    ctx.define("time".to_string(), JadeType::Unknown);
}

pub static TIME_PKG: Package = Package {
    import_name: "std/time",
    global_name: "time",
    fns: TIME_PKG_FNS,
    register_types: register_time_pkg_types,
};
