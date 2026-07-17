use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext, vm::VmValue},
    frontend::error::{JadeError, Result, Span},
};

use crate::builtins::{BuiltinFn, Package, make_array};

const ZERO: Span = Span { line: 0, col: 0 };

fn env_get(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let name = match &args[0] {
        VmValue::Str(s) => s.as_str(),
        _ => return Err(JadeError::TypeError { message: "env.get".to_string(), span: ZERO }),
    };
    Ok(match std::env::var(name) {
        Ok(val) => VmValue::Str(val),
        Err(_)  => VmValue::Nil,
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
    // Safety: jade programs are single-threaded at the OS/process level.
    #[allow(deprecated)]
    unsafe { std::env::set_var(name, val) };
    Ok(VmValue::Nil)
}

fn env_args(args: &[VmValue]) -> Result<VmValue> {
    if !args.is_empty() {
        return Err(JadeError::ArityMismatch { expected: 0, got: args.len(), span: ZERO });
    }
    let argv: Vec<VmValue> = std::env::args().map(VmValue::Str).collect();
    Ok(make_array(argv))
}

fn env_cwd(args: &[VmValue]) -> Result<VmValue> {
    if !args.is_empty() {
        return Err(JadeError::ArityMismatch { expected: 0, got: args.len(), span: ZERO });
    }
    std::env::current_dir()
        .map(|p| VmValue::Str(p.to_string_lossy().into_owned()))
        .map_err(|e| JadeError::IoError { message: format!("env.cwd: {}", e), span: ZERO })
}

static ENV_PKG_FNS: &[BuiltinFn] = &[
    BuiltinFn { name: "get",  vm_impl: env_get },
    BuiltinFn { name: "set",  vm_impl: env_set },
    BuiltinFn { name: "args", vm_impl: env_args },
    BuiltinFn { name: "cwd",  vm_impl: env_cwd },
];

fn register_env_pkg_types(ctx: &mut TypeContext) {
    ctx.define("env".to_string(), JadeType::Unknown);
}

pub static ENV_PKG: Package = Package {
    import_name: "std/env",
    global_name: "env",
    fns: ENV_PKG_FNS,
    register_types: register_env_pkg_types,
};
