use std::time::{SystemTime, UNIX_EPOCH};

use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext, vm::VmValue},
    frontend::error::{JadeError, Result, Span},
};

use crate::builtins::{BuiltinFn, Package};

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
        _ => return Err(JadeError::TypeError { message: "time.sleep".to_string(), span: ZERO }),
    };
    if secs > 0.0 {
        std::thread::sleep(std::time::Duration::from_secs_f64(secs));
    }
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
        VmValue::Nil    => String::new(),
        _ => return Err(JadeError::TypeError { message: "time.local: tz must be str".to_string(), span: ZERO }),
    };
    let mut cmd = std::process::Command::new("date");
    cmd.arg("+%a %b %e %H:%M:%S %Z %Y");
    if !tz.is_empty() {
        cmd.env("TZ", &tz);
    }
    let out = cmd.output().map_err(|e| JadeError::IoError {
        message: format!("time.local: could not spawn date: {}", e),
        span: ZERO,
    })?;
    let s = String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_string();
    Ok(VmValue::Str(s))
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
