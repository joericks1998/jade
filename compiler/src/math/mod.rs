use crate::builtins::Package;

pub static MATH_PKG: Package = Package {
    import_name: "std/math",
    global_name: "math",
    fns: &[
        "floor", "ceil", "abs", "sqrt", "min", "max", "pow", "round", "trunc", "sign", "ln",
        "log2", "log10", "exp", "sin", "cos", "tan", "asin", "acos", "atan", "atan2", "hypot",
        "clamp", "is_nan", "is_inf", "pi", "e", "tau", "inf", "nan",
    ],
};
