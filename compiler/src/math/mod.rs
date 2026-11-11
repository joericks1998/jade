use crate::builtins::Package;
use crate::typechecker::tir::JadeType;
use crate::typechecker::type_infer::TypeContext;

fn register_math_pkg_types(ctx: &mut TypeContext) {
    ctx.define("math".to_string(), JadeType::Unknown);
}

pub static MATH_PKG: Package = Package {
    import_name: "std/math",
    global_name: "math",
    fns: &[
        "floor", "ceil", "abs", "sqrt", "min", "max", "pow", "round", "trunc", "sign", "ln",
        "log2", "log10", "exp", "sin", "cos", "tan", "asin", "acos", "atan", "atan2", "hypot",
        "clamp", "is_nan", "is_inf", "pi", "e", "tau", "inf", "nan",
    ],
    register_types: register_math_pkg_types,
};
