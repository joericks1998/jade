#[cfg(test)]
mod tests;

use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext},
    frontend::error::{JadeError, Result, Span},
    vm::VmValue,
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

fn math_round(args: &[VmValue]) -> Result<VmValue> {
    Ok(value_of(mathf::round(num_of(args.first(), "round")?)))
}

fn math_trunc(args: &[VmValue]) -> Result<VmValue> {
    Ok(value_of(mathf::trunc(num_of(args.first(), "trunc")?)))
}

fn math_sign(args: &[VmValue]) -> Result<VmValue> {
    Ok(value_of(mathf::sign(num_of(args.first(), "sign")?)))
}

fn math_ln(args: &[VmValue]) -> Result<VmValue> {
    Ok(value_of(mathf::ln(num_of(args.first(), "ln")?)))
}

fn math_log2(args: &[VmValue]) -> Result<VmValue> {
    Ok(value_of(mathf::log2(num_of(args.first(), "log2")?)))
}

fn math_log10(args: &[VmValue]) -> Result<VmValue> {
    Ok(value_of(mathf::log10(num_of(args.first(), "log10")?)))
}

fn math_exp(args: &[VmValue]) -> Result<VmValue> {
    Ok(value_of(mathf::exp(num_of(args.first(), "exp")?)))
}

fn math_sin(args: &[VmValue]) -> Result<VmValue> {
    Ok(value_of(mathf::sin(num_of(args.first(), "sin")?)))
}

fn math_cos(args: &[VmValue]) -> Result<VmValue> {
    Ok(value_of(mathf::cos(num_of(args.first(), "cos")?)))
}

fn math_tan(args: &[VmValue]) -> Result<VmValue> {
    Ok(value_of(mathf::tan(num_of(args.first(), "tan")?)))
}

fn math_asin(args: &[VmValue]) -> Result<VmValue> {
    Ok(value_of(mathf::asin(num_of(args.first(), "asin")?)))
}

fn math_acos(args: &[VmValue]) -> Result<VmValue> {
    Ok(value_of(mathf::acos(num_of(args.first(), "acos")?)))
}

fn math_atan(args: &[VmValue]) -> Result<VmValue> {
    Ok(value_of(mathf::atan(num_of(args.first(), "atan")?)))
}

fn math_atan2(args: &[VmValue]) -> Result<VmValue> {
    Ok(value_of(mathf::atan2(num_of(args.first(), "atan2")?, num_of(args.get(1), "atan2")?)))
}

fn math_hypot(args: &[VmValue]) -> Result<VmValue> {
    Ok(value_of(mathf::hypot(num_of(args.first(), "hypot")?, num_of(args.get(1), "hypot")?)))
}

fn math_clamp(args: &[VmValue]) -> Result<VmValue> {
    Ok(value_of(mathf::clamp(
        num_of(args.first(), "clamp")?,
        num_of(args.get(1), "clamp")?,
        num_of(args.get(2), "clamp")?,
    )))
}

fn math_is_nan(args: &[VmValue]) -> Result<VmValue> {
    Ok(VmValue::Bool(mathf::is_nan(num_of(args.first(), "is_nan")?)))
}

fn math_is_inf(args: &[VmValue]) -> Result<VmValue> {
    Ok(VmValue::Bool(mathf::is_inf(num_of(args.first(), "is_inf")?)))
}

// The constants. Spelled as calls because a package namespace is only ever a
// field access a call immediately consumes — reading `math.pi` in value
// position has no lowering, so it would build on one engine and not the other.
// `inf` and `nan` are reachable no other way at all: the lexer caps a numeric
// literal, so neither can be written down.

fn math_pi(args: &[VmValue]) -> Result<VmValue> {
    if !args.is_empty() {
        return Err(JadeError::ArityMismatch { expected: 0, got: args.len(), span: ZERO });
    }
    Ok(value_of(mathf::pi()))
}

fn math_e(args: &[VmValue]) -> Result<VmValue> {
    if !args.is_empty() {
        return Err(JadeError::ArityMismatch { expected: 0, got: args.len(), span: ZERO });
    }
    Ok(value_of(mathf::e()))
}

fn math_tau(args: &[VmValue]) -> Result<VmValue> {
    if !args.is_empty() {
        return Err(JadeError::ArityMismatch { expected: 0, got: args.len(), span: ZERO });
    }
    Ok(value_of(mathf::tau()))
}

fn math_inf(args: &[VmValue]) -> Result<VmValue> {
    if !args.is_empty() {
        return Err(JadeError::ArityMismatch { expected: 0, got: args.len(), span: ZERO });
    }
    Ok(value_of(mathf::inf()))
}

fn math_nan(args: &[VmValue]) -> Result<VmValue> {
    if !args.is_empty() {
        return Err(JadeError::ArityMismatch { expected: 0, got: args.len(), span: ZERO });
    }
    Ok(value_of(mathf::nan()))
}

static MATH_PKG_FNS: &[BuiltinFn] = &[
    BuiltinFn { name: "floor", vm_impl: math_floor },
    BuiltinFn { name: "ceil", vm_impl: math_ceil },
    BuiltinFn { name: "abs", vm_impl: math_abs },
    BuiltinFn { name: "sqrt", vm_impl: math_sqrt },
    BuiltinFn { name: "min", vm_impl: math_min },
    BuiltinFn { name: "max", vm_impl: math_max },
    BuiltinFn { name: "pow", vm_impl: math_pow },
    BuiltinFn { name: "round", vm_impl: math_round },
    BuiltinFn { name: "trunc", vm_impl: math_trunc },
    BuiltinFn { name: "sign", vm_impl: math_sign },
    BuiltinFn { name: "ln", vm_impl: math_ln },
    BuiltinFn { name: "log2", vm_impl: math_log2 },
    BuiltinFn { name: "log10", vm_impl: math_log10 },
    BuiltinFn { name: "exp", vm_impl: math_exp },
    BuiltinFn { name: "sin", vm_impl: math_sin },
    BuiltinFn { name: "cos", vm_impl: math_cos },
    BuiltinFn { name: "tan", vm_impl: math_tan },
    BuiltinFn { name: "asin", vm_impl: math_asin },
    BuiltinFn { name: "acos", vm_impl: math_acos },
    BuiltinFn { name: "atan", vm_impl: math_atan },
    BuiltinFn { name: "atan2", vm_impl: math_atan2 },
    BuiltinFn { name: "hypot", vm_impl: math_hypot },
    BuiltinFn { name: "clamp", vm_impl: math_clamp },
    BuiltinFn { name: "is_nan", vm_impl: math_is_nan },
    BuiltinFn { name: "is_inf", vm_impl: math_is_inf },
    BuiltinFn { name: "pi", vm_impl: math_pi },
    BuiltinFn { name: "e", vm_impl: math_e },
    BuiltinFn { name: "tau", vm_impl: math_tau },
    BuiltinFn { name: "inf", vm_impl: math_inf },
    BuiltinFn { name: "nan", vm_impl: math_nan },
];

fn register_math_pkg_types(ctx: &mut TypeContext) {
    ctx.define("math".to_string(), JadeType::Unknown);
}

pub static MATH_PKG: Package = Package {
    import_name: "std/math",
    global_name: "math",
    fns: MATH_PKG_FNS,
    natives: &[],
    register_types: register_math_pkg_types,
};
