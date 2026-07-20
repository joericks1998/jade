use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext, vm::VmValue},
    frontend::error::{JadeError, Result, Span},
};

use crate::builtins::{BuiltinFn, Package};
use jade_runtime::trust::JStr;

#[cfg(test)]
mod tests;

const ZERO: Span = Span { line: 0, col: 0 };

fn time_now(args: &[VmValue]) -> Result<VmValue> {
    if !args.is_empty() {
        return Err(JadeError::ArityMismatch { expected: 0, got: args.len(), span: ZERO });
    }
    Ok(VmValue::Int(jade_runtime::timef::now()))
}

fn time_now_ms(args: &[VmValue]) -> Result<VmValue> {
    if !args.is_empty() {
        return Err(JadeError::ArityMismatch { expected: 0, got: args.len(), span: ZERO });
    }
    Ok(VmValue::Int(jade_runtime::timef::now_ms()))
}

fn time_sleep(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let secs = match &args[0] {
        VmValue::Int(n)   => *n as f64,
        VmValue::Float(f) => *f,
        _ => return Err(JadeError::TypeError { message: "time.sleep".to_string(), span: ZERO }),
    };
    jade_runtime::timef::sleep(secs);
    Ok(VmValue::Nil)
}

/// `time.local(tz)` — formatted local time in an IANA timezone (e.g. "Asia/Tokyo").
/// Empty/nil tz uses the system default. VM path runs `date` via execvp-style
/// Command (no shell parsing); the AOT path uses libc tzset/strftime directly.
fn time_local(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let tz = match &args[0] {
        VmValue::Str(s) => s.clone(),
        VmValue::Nil    => String::new().into(),
        _ => return Err(JadeError::TypeError { message: "time.local: tz must be str".to_string(), span: ZERO }),
    };
    jade_runtime::timef::local(&tz)
        .map(|s| VmValue::Str(JStr::tainted(s)))
        .map_err(|e| JadeError::IoError {
            message: format!("time.local: could not spawn date: {}", e),
            span: ZERO,
        })
}

static TIME_PKG_FNS: &[BuiltinFn] = &[
    BuiltinFn { name: "now",    vm_impl: time_now },
    BuiltinFn { name: "now_ms", vm_impl: time_now_ms },
    BuiltinFn { name: "sleep",  vm_impl: time_sleep },
    BuiltinFn { name: "local",  vm_impl: time_local },
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
