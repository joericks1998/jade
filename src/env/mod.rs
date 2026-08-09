use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext},
    frontend::error::{JadeError, Result, Span},
    vm::VmValue,
};

use crate::builtins::{BuiltinFn, Package, make_array};
use jade_runtime::trust::JStr;

#[cfg(test)]
mod tests;

const ZERO: Span = Span { line: 0, col: 0 };

fn env_get(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let name = match &args[0] {
        VmValue::Str(s) => s.as_str(),
        _ => return Err(JadeError::TypeError { message: "env.get".to_string(), span: ZERO }),
    };
    Ok(match jade_runtime::envf::get(name) {
        Some(val) => VmValue::Str(val.into()),
        None => VmValue::Nil,
    })
}

fn env_set(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 2 {
        return Err(JadeError::ArityMismatch { expected: 2, got: args.len(), span: ZERO });
    }
    let name = match &args[0] {
        VmValue::Str(s) => s.clone(),
        _ => return Err(JadeError::TypeError { message: "env.set".to_string(), span: ZERO }),
    };
    let val = match &args[1] {
        VmValue::Str(s) => s.clone(),
        _ => return Err(JadeError::TypeError { message: "env.set".to_string(), span: ZERO }),
    };
    jade_runtime::envf::set(&name, &val);
    Ok(VmValue::Nil)
}

fn env_args(args: &[VmValue]) -> Result<VmValue> {
    if !args.is_empty() {
        return Err(JadeError::ArityMismatch { expected: 0, got: args.len(), span: ZERO });
    }
    let argv: Vec<VmValue> =
        jade_runtime::envf::args().into_iter().map(|a| VmValue::Str(JStr::trusted(a))).collect();
    Ok(make_array(argv))
}

fn env_cwd(args: &[VmValue]) -> Result<VmValue> {
    if !args.is_empty() {
        return Err(JadeError::ArityMismatch { expected: 0, got: args.len(), span: ZERO });
    }
    jade_runtime::envf::cwd()
        .map(|v| VmValue::Str(JStr::tainted(v)))
        .map_err(|e| JadeError::IoError { message: format!("env.cwd: {}", e), span: ZERO })
}

static ENV_PKG_FNS: &[BuiltinFn] = &[
    BuiltinFn { name: "get", vm_impl: env_get },
    BuiltinFn { name: "set", vm_impl: env_set },
    BuiltinFn { name: "args", vm_impl: env_args },
    BuiltinFn { name: "cwd", vm_impl: env_cwd },
];

fn register_env_pkg_types(ctx: &mut TypeContext) {
    ctx.define("env".to_string(), JadeType::Unknown);
}

pub static ENV_PKG: Package = Package {
    import_name: "std/env",
    global_name: "env",
    fns: ENV_PKG_FNS,
    natives: &[],
    register_types: register_env_pkg_types,
};
