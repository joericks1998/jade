use jade_runtime::coll::DictObj;

use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext, vm::VmValue},
    frontend::error::{JadeError, Result, Span},
};

use crate::builtins::{BuiltinFn, Package};

#[cfg(test)]
mod tests;

const ZERO: Span = Span { line: 0, col: 0 };

fn require_str<'a>(args: &'a [VmValue], pos: usize, fn_name: &str) -> Result<&'a str> {
    match args.get(pos) {
        Some(VmValue::Str(s)) => Ok(s.as_str()),
        Some(_) => Err(JadeError::TypeError { message: fn_name.to_string(), span: ZERO }),
        None    => Err(JadeError::ArityMismatch { expected: pos + 1, got: args.len(), span: ZERO }),
    }
}

/// `sh.exec(cmd)` — run `cmd` via `sh -c`, return captured stdout as str.
/// Raises if the process exits non-zero, including stderr in the error message.
fn sh_exec(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let cmd = require_str(args, 0, "sh.exec")?;
    jade_runtime::shf::exec(cmd)
        .map(VmValue::Str)
        .map_err(|message| JadeError::IoError { message, span: ZERO })
}

/// `sh.run(cmd)` — run `cmd` via `sh -c`, inheriting stdio. Returns exit code as int.
fn sh_run(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let cmd = require_str(args, 0, "sh.run")?;
    jade_runtime::shf::run(cmd)
        .map(VmValue::Int)
        .map_err(|message| JadeError::IoError { message, span: ZERO })
}

/// `sh.output(cmd)` — run `cmd` via `sh -c`, capture all streams.
/// Returns a dict with `stdout`, `stderr` (strs) and `code` (int).
fn sh_output(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let cmd = require_str(args, 0, "sh.output")?;
    let (stdout, stderr, code) =
        jade_runtime::shf::output(cmd).map_err(|message| JadeError::IoError { message, span: ZERO })?;
    let mut map = DictObj::new();
    map.insert("stdout".to_string(), VmValue::Str(stdout));
    map.insert("stderr".to_string(), VmValue::Str(stderr));
    map.insert("code".to_string(), VmValue::Int(code));
    Ok(VmValue::Dict(map))
}

static SH_PKG_FNS: &[BuiltinFn] = &[
    BuiltinFn { name: "exec",   vm_impl: sh_exec },
    BuiltinFn { name: "run",    vm_impl: sh_run },
    BuiltinFn { name: "output", vm_impl: sh_output },
];

fn register_sh_pkg_types(ctx: &mut TypeContext) {
    ctx.define("sh".to_string(), JadeType::Unknown);
}

pub static SH_PKG: Package = Package {
    import_name: "std/sh",
    global_name: "sh",
    fns: SH_PKG_FNS,
    register_types: register_sh_pkg_types,
};
