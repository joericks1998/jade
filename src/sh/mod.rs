use jade_runtime::coll::DictObj;

use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext},
    frontend::error::{JadeError, Result, Span},
    vm::VmValue,
};

use crate::builtins::{BuiltinFn, Package};
use jade_runtime::trust::JStr;

#[cfg(test)]
mod tests;

const ZERO: Span = Span { line: 0, col: 0 };

fn require_str<'a>(args: &'a [VmValue], pos: usize, fn_name: &str) -> Result<&'a str> {
    match args.get(pos) {
        Some(VmValue::Str(s)) => Ok(s.as_str()),
        Some(_) => Err(JadeError::TypeError { message: fn_name.to_string(), span: ZERO }),
        None => Err(JadeError::ArityMismatch { expected: pos + 1, got: args.len(), span: ZERO }),
    }
}

/// `sh.exec(cmd)` — run `cmd` via `sh -c`, return captured stdout as str.
/// Raises if the process exits non-zero, including stderr in the error message.
/// Refuse a tainted string at a sink that would execute or fetch it.
///
/// The compiled runtime has always done this (`jrt_refuse_if_tainted`); the
/// interpreter tracked no trust at all, so the same program ran an untrusted
/// command under `jade run` and was refused under `jade build`. The message
/// comes from the shared runtime so both engines word it identically.
fn refuse_if_tainted(args: &[VmValue], pos: usize, sink: &str) -> Result<()> {
    if let Some(VmValue::Str(s)) = args.get(pos)
        && s.is_tainted()
    {
        return Err(JadeError::Exception {
            message: jade_runtime::trust::refusal_message(sink),
            span: ZERO,
        });
    }
    Ok(())
}

fn sh_exec(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    refuse_if_tainted(args, 0, "sh.exec(cmd)")?;
    let cmd = require_str(args, 0, "sh.exec")?;
    jade_runtime::shf::exec(cmd)
        .map(|s| VmValue::Str(JStr::tainted(s)))
        .map_err(|message| JadeError::IoError { message, span: ZERO })
}

/// `sh.run(cmd)` — run `cmd` via `sh -c`, inheriting stdio. Returns exit code as int.
fn sh_run(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    refuse_if_tainted(args, 0, "sh.run(cmd)")?;
    let cmd = require_str(args, 0, "sh.run")?;
    jade_runtime::shf::run(cmd)
        .map(VmValue::Int)
        .map_err(|message| JadeError::IoError { message, span: ZERO })
}

/// `sh.output(cmd)` — run `cmd` via `sh -c`, capture all streams.
/// Returns a dict with `stdout`, `stderr` (strs) and `code` (int).
///
/// Refuses a tainted command exactly as `exec` and `run` do. It did not until
/// v1.3.3, which made it the way around the trust model rather than a third
/// member of it: all three reach the same `sh -c`, so refusing two of them and
/// not the third only meant an untrusted command had to be spelled
/// `sh.output(x).stdout`.
fn sh_output(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    refuse_if_tainted(args, 0, "sh.output(cmd)")?;
    let cmd = require_str(args, 0, "sh.output")?;
    let (stdout, stderr, code) = jade_runtime::shf::output(cmd)
        .map_err(|message| JadeError::IoError { message, span: ZERO })?;
    let mut map = DictObj::new();
    map.insert("stdout".to_string(), VmValue::Str(stdout.into()));
    map.insert("stderr".to_string(), VmValue::Str(stderr.into()));
    map.insert("code".to_string(), VmValue::Int(code));
    Ok(VmValue::dict(map))
}

static SH_PKG_FNS: &[BuiltinFn] = &[
    BuiltinFn { name: "exec", vm_impl: sh_exec },
    BuiltinFn { name: "run", vm_impl: sh_run },
    BuiltinFn { name: "output", vm_impl: sh_output },
];

fn register_sh_pkg_types(ctx: &mut TypeContext) {
    ctx.define("sh".to_string(), JadeType::Unknown);
}

pub static SH_PKG: Package = Package {
    import_name: "std/sh",
    global_name: "sh",
    fns: SH_PKG_FNS,
    natives: &[],
    register_types: register_sh_pkg_types,
};
