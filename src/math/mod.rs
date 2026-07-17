use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext, vm::VmValue},
    frontend::error::{JadeError, Result, Span},
};

use crate::builtins::{BuiltinFn, Package};

const ZERO: Span = Span { line: 0, col: 0 };

fn math_floor(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Float(f) => Ok(VmValue::Int(f.floor() as i64)),
        VmValue::Int(i)   => Ok(VmValue::Int(*i)),
        _ => Err(JadeError::TypeError { message: "math.floor".to_string(), span: ZERO }),
    }
}

fn math_ceil(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Float(f) => Ok(VmValue::Int(f.ceil() as i64)),
        VmValue::Int(i)   => Ok(VmValue::Int(*i)),
        _ => Err(JadeError::TypeError { message: "math.ceil".to_string(), span: ZERO }),
    }
}

fn math_abs(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Int(i)   => Ok(VmValue::Int(i.abs())),
        VmValue::Float(f) => Ok(VmValue::Float(f.abs())),
        _ => Err(JadeError::TypeError { message: "math.abs".to_string(), span: ZERO }),
    }
}

fn math_sqrt(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Float(f) => Ok(VmValue::Float(f.sqrt())),
        VmValue::Int(i)   => Ok(VmValue::Float((*i as f64).sqrt())),
        _ => Err(JadeError::TypeError { message: "math.sqrt".to_string(), span: ZERO }),
    }
}

fn math_min(args: &[VmValue]) -> Result<VmValue> {
    match (args.get(0), args.get(1)) {
        (Some(VmValue::Int(a)), Some(VmValue::Int(b)))     => Ok(VmValue::Int(*a.min(b))),
        (Some(VmValue::Float(a)), Some(VmValue::Float(b))) => Ok(VmValue::Float(a.min(*b))),
        (Some(VmValue::Int(a)), Some(VmValue::Float(b)))   => Ok(VmValue::Float((*a as f64).min(*b))),
        (Some(VmValue::Float(a)), Some(VmValue::Int(b)))   => Ok(VmValue::Float(a.min(*b as f64))),
        _ => Err(JadeError::TypeError { message: "math.min".to_string(), span: ZERO }),
    }
}

fn math_max(args: &[VmValue]) -> Result<VmValue> {
    match (args.get(0), args.get(1)) {
        (Some(VmValue::Int(a)), Some(VmValue::Int(b)))     => Ok(VmValue::Int(*a.max(b))),
        (Some(VmValue::Float(a)), Some(VmValue::Float(b))) => Ok(VmValue::Float(a.max(*b))),
        (Some(VmValue::Int(a)), Some(VmValue::Float(b)))   => Ok(VmValue::Float((*a as f64).max(*b))),
        (Some(VmValue::Float(a)), Some(VmValue::Int(b)))   => Ok(VmValue::Float(a.max(*b as f64))),
        _ => Err(JadeError::TypeError { message: "math.max".to_string(), span: ZERO }),
    }
}

fn math_pow(args: &[VmValue]) -> Result<VmValue> {
    match (args.get(0), args.get(1)) {
        (Some(VmValue::Int(base)), Some(VmValue::Int(exp))) => {
            if *exp >= 0 {
                Ok(VmValue::Int(base.pow(*exp as u32)))
            } else {
                Ok(VmValue::Float((*base as f64).powi(*exp as i32)))
            }
        }
        (Some(VmValue::Float(base)), Some(VmValue::Float(exp))) => Ok(VmValue::Float(base.powf(*exp))),
        (Some(VmValue::Int(base)), Some(VmValue::Float(exp)))   => Ok(VmValue::Float((*base as f64).powf(*exp))),
        (Some(VmValue::Float(base)), Some(VmValue::Int(exp)))   => Ok(VmValue::Float(base.powi(*exp as i32))),
        _ => Err(JadeError::TypeError { message: "math.pow".to_string(), span: ZERO }),
    }
}

static MATH_PKG_FNS: &[BuiltinFn] = &[
    BuiltinFn { name: "floor", vm_impl: math_floor },
    BuiltinFn { name: "ceil",  vm_impl: math_ceil },
    BuiltinFn { name: "abs",   vm_impl: math_abs },
    BuiltinFn { name: "sqrt",  vm_impl: math_sqrt },
    BuiltinFn { name: "min",   vm_impl: math_min },
    BuiltinFn { name: "max",   vm_impl: math_max },
    BuiltinFn { name: "pow",   vm_impl: math_pow },
];

fn register_math_pkg_types(ctx: &mut TypeContext) {
    ctx.define("math".to_string(), JadeType::Unknown);
}

pub static MATH_PKG: Package = Package {
    import_name: "std/math",
    global_name: "math",
    fns: MATH_PKG_FNS,
    register_types: register_math_pkg_types,
};
