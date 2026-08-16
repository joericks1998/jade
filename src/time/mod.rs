use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext},
    frontend::error::{JadeError, Result, Span},
    vm::VmValue,
};

use crate::builtins::{BuiltinFn, Package};
use jade_runtime::{coll::DictObj, trust::JStr};

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
        VmValue::Int(n) => *n as f64,
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
        VmValue::Nil => String::new().into(),
        _ => {
            return Err(JadeError::TypeError {
                message: "time.local: tz must be str".to_string(),
                span: ZERO,
            });
        }
    };
    jade_runtime::timef::local(&tz).map(|s| VmValue::Str(JStr::tainted(s))).map_err(|e| {
        JadeError::IoError {
            message: format!("time.local: could not spawn date: {}", e),
            span: ZERO,
        }
    })
}

/// `time.monotonic()` — seconds from a fixed origin, as a float. Never jumps,
/// so it is the one to subtract two readings of; see the note in `timef.rs`.
fn time_monotonic(args: &[VmValue]) -> Result<VmValue> {
    if !args.is_empty() {
        return Err(JadeError::ArityMismatch { expected: 0, got: args.len(), span: ZERO });
    }
    Ok(VmValue::Float(jade_runtime::timef::monotonic()))
}

/// The one integer argument shared by `parts` and `utc`.
fn require_ts(args: &[VmValue], fn_name: &str) -> Result<i64> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    match &args[0] {
        VmValue::Int(n) => Ok(*n),
        _ => Err(JadeError::TypeError {
            message: format!("{}: timestamp must be int", fn_name),
            span: ZERO,
        }),
    }
}

/// `time.parts(ts)` — UTC calendar fields as a dict.
fn time_parts(args: &[VmValue]) -> Result<VmValue> {
    let ts = require_ts(args, "time.parts")?;
    let mut map = DictObj::new();
    for (k, v) in jade_runtime::timef::parts(ts).fields() {
        map.insert(k.to_string(), VmValue::Int(v));
    }
    Ok(VmValue::dict(map))
}

/// `time.utc(ts)` — ISO 8601 UTC. Trusted: computed from an int, not read from
/// a subprocess the way `time.local` is.
fn time_utc(args: &[VmValue]) -> Result<VmValue> {
    let ts = require_ts(args, "time.utc")?;
    Ok(VmValue::Str(JStr::trusted(jade_runtime::timef::utc(ts))))
}

/// `time.stamp(y, mo, d[, h[, mi[, s]]])` — UTC fields to a Unix timestamp.
/// The three time-of-day fields default to zero, so three arguments mean
/// midnight UTC on that date.
fn time_stamp(args: &[VmValue]) -> Result<VmValue> {
    if !(3..=6).contains(&args.len()) {
        return Err(JadeError::ArityMismatch { expected: 3, got: args.len(), span: ZERO });
    }
    let mut f = [0i64; 6];
    for (i, arg) in args.iter().enumerate() {
        match arg {
            VmValue::Int(n) => f[i] = *n,
            _ => {
                return Err(JadeError::TypeError {
                    message: "time.stamp: every field must be int".to_string(),
                    span: ZERO,
                });
            }
        }
    }
    Ok(VmValue::Int(jade_runtime::timef::stamp(f[0], f[1], f[2], f[3], f[4], f[5])))
}

static TIME_PKG_FNS: &[BuiltinFn] = &[
    BuiltinFn { name: "now", vm_impl: time_now },
    BuiltinFn { name: "now_ms", vm_impl: time_now_ms },
    BuiltinFn { name: "monotonic", vm_impl: time_monotonic },
    BuiltinFn { name: "sleep", vm_impl: time_sleep },
    BuiltinFn { name: "local", vm_impl: time_local },
    BuiltinFn { name: "utc", vm_impl: time_utc },
    BuiltinFn { name: "parts", vm_impl: time_parts },
    BuiltinFn { name: "stamp", vm_impl: time_stamp },
];

fn register_time_pkg_types(ctx: &mut TypeContext) {
    ctx.define("time".to_string(), JadeType::Unknown);
}

pub static TIME_PKG: Package = Package {
    import_name: "std/time",
    global_name: "time",
    fns: TIME_PKG_FNS,
    natives: &[],
    register_types: register_time_pkg_types,
};
