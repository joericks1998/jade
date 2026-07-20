#[cfg(test)]
mod tests;

use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext, vm::VmValue},
    frontend::error::{JadeError, Result, Span},
};

use crate::builtins::{BuiltinFn, Package};

const ZERO: Span = Span { line: 0, col: 0 };

// Every `math.*` operation is implemented once, in `jade_runtime::mathf`. This
// module only converts `VmValue` to and from that core's `Num` and maps its one
// error onto Jade's `IntegerOverflow`.
//
// It used to be a second implementation, with the AOT copy carrying a comment
// promising the two "mirror" each other. They did not: `math.pow(2, 64)`
// panicked with a raw Rust overflow message here and silently printed 0 under
// `jade build`, and neither matched `a + 1`, which raises "integer overflow".

use jade_runtime::mathf::{self, MathErr, Num};

/// `VmValue` → `Num`, or a `math.<op>` type error for a non-number.
fn num_of(v: Option<&VmValue>, op: &str) -> Result<Num> {
    match v {
        Some(VmValue::Int(i)) => Ok(Num::Int(*i)),
        Some(VmValue::Float(f)) => Ok(Num::Float(*f)),
        _ => Err(JadeError::TypeError { message: format!("math.{op}"), span: ZERO }),
    }
}

fn value_of(n: Num) -> VmValue {
    match n {
        Num::Int(i) => VmValue::Int(i),
        Num::Float(f) => VmValue::Float(f),
    }
}

/// The core's only failure is overflow, which is the same condition `+`/`-`/`*`
/// already report as `IntegerOverflow`.
fn checked(r: std::result::Result<Num, MathErr>) -> Result<VmValue> {
    match r {
        Ok(n) => Ok(value_of(n)),
        Err(MathErr::Overflow) => Err(JadeError::IntegerOverflow { span: ZERO }),
    }
}

fn math_floor(args: &[VmValue]) -> Result<VmValue> {
    Ok(value_of(mathf::floor(num_of(args.first(), "floor")?)))
}

fn math_ceil(args: &[VmValue]) -> Result<VmValue> {
    Ok(value_of(mathf::ceil(num_of(args.first(), "ceil")?)))
}

fn math_abs(args: &[VmValue]) -> Result<VmValue> {
    checked(mathf::abs(num_of(args.first(), "abs")?))
}

fn math_sqrt(args: &[VmValue]) -> Result<VmValue> {
    Ok(value_of(mathf::sqrt(num_of(args.first(), "sqrt")?)))
}

fn math_min(args: &[VmValue]) -> Result<VmValue> {
    Ok(value_of(mathf::min(num_of(args.first(), "min")?, num_of(args.get(1), "min")?)))
}

fn math_max(args: &[VmValue]) -> Result<VmValue> {
    Ok(value_of(mathf::max(num_of(args.first(), "max")?, num_of(args.get(1), "max")?)))
}

fn math_pow(args: &[VmValue]) -> Result<VmValue> {
    checked(mathf::pow(num_of(args.first(), "pow")?, num_of(args.get(1), "pow")?))
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
