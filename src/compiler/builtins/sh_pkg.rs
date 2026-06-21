use std::collections::HashMap;

use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext, vm::{value_type_name, VmValue}},
    frontend::error::{JadeError, Result, Span},
};

use super::{BuiltinFn, Package};

const ZERO: Span = Span { line: 0, col: 0 };

fn require_str<'a>(args: &'a [VmValue], pos: usize, fn_name: &str) -> Result<&'a str> {
    match args.get(pos) {
        Some(VmValue::Str(s)) => Ok(s.as_str()),
        Some(other) => Err(JadeError::TypeError {
            message: format!("{}: expected a string command, got {}", fn_name, value_type_name(other)),
            span: ZERO,
        }),
        None => Err(JadeError::ArityMismatch { expected: pos + 1, got: args.len(), span: ZERO }),
    }
}

/// `sh.exec(cmd)` — run `cmd` via `sh -c`, return captured stdout as str.
/// Raises if the process exits non-zero, including stderr in the error message.
fn sh_exec(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let cmd = require_str(args, 0, "sh.exec")?;
    let out = std::process::Command::new("sh")
        .args(["-c", cmd])
        .output()
        .map_err(|e| JadeError::IoError {
            message: format!("sh.exec: could not spawn shell: {}", e),
            span: ZERO,
        })?;
    if out.status.success() {
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let trimmed = stdout.trim_end_matches('\n').to_string();
        Ok(VmValue::Str(trimmed))
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let code = out.status.code().unwrap_or(-1);
        Err(JadeError::IoError {
            message: format!(
                "sh.exec: command exited with code {}: {}",
                code,
                stderr.trim()
            ),
            span: ZERO,
        })
    }
}

/// `sh.run(cmd)` — run `cmd` via `sh -c`, inheriting stdio. Returns exit code as int.
fn sh_run(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let cmd = require_str(args, 0, "sh.run")?;
    let status = std::process::Command::new("sh")
        .args(["-c", cmd])
        .status()
        .map_err(|e| JadeError::IoError {
            message: format!("sh.run: could not spawn shell: {}", e),
            span: ZERO,
        })?;
    Ok(VmValue::Int(status.code().unwrap_or(-1) as i64))
}

/// `sh.output(cmd)` — run `cmd` via `sh -c`, capture all streams.
/// Returns a dict with `stdout`, `stderr` (strs) and `code` (int).
fn sh_output(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let cmd = require_str(args, 0, "sh.output")?;
    let out = std::process::Command::new("sh")
        .args(["-c", cmd])
        .output()
        .map_err(|e| JadeError::IoError {
            message: format!("sh.output: could not spawn shell: {}", e),
            span: ZERO,
        })?;
    let mut map = HashMap::new();
    map.insert("stdout".to_string(), VmValue::Str(String::from_utf8_lossy(&out.stdout).into_owned()));
    map.insert("stderr".to_string(), VmValue::Str(String::from_utf8_lossy(&out.stderr).into_owned()));
    map.insert("code".to_string(),   VmValue::Int(out.status.code().unwrap_or(-1) as i64));
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
