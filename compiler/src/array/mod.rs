use crate::builtins::Package;
use crate::typechecker::tir::JadeType;
use crate::typechecker::type_infer::TypeContext;

/// The methods a `array` receiver has, by name.
pub const ARRAY_METHODS: &[&str] =
    &["len", "push", "pop", "contains", "sort", "reverse", "join", "map", "filter"];

fn register_array_pkg_types(ctx: &mut TypeContext) {
    ctx.define("array".to_string(), JadeType::Unknown);
}

pub fn register_array_method_types(ctx: &mut TypeContext) {
    let unk = JadeType::Unknown;
    let methods: &[(&str, JadeType)] = &[
        ("len", JadeType::Fn { params: vec![], ret: Box::new(JadeType::Int) }),
        ("push", JadeType::Fn { params: vec![unk.clone()], ret: Box::new(JadeType::Nil) }),
        ("pop", JadeType::Fn { params: vec![], ret: Box::new(JadeType::Unknown) }),
        ("contains", JadeType::Fn { params: vec![unk.clone()], ret: Box::new(JadeType::Bool) }),
        ("sort", JadeType::Fn { params: vec![], ret: Box::new(JadeType::Nil) }),
        ("reverse", JadeType::Fn { params: vec![], ret: Box::new(JadeType::Nil) }),
        // The method spelling of `array.map` / `array.filter`. Unlike the rest
        // of this table they run a Jade function per element, which MIR lowers
        // as a loop that calls it. They were the only array functions with no
        // method spelling until v1.3.21.
        (
            "map",
            JadeType::Fn {
                params: vec![unk.clone()],
                ret: Box::new(JadeType::Array(Box::new(JadeType::Unknown))),
            },
        ),
        (
            "filter",
            JadeType::Fn {
                params: vec![unk.clone()],
                ret: Box::new(JadeType::Array(Box::new(JadeType::Unknown))),
            },
        ),
    ];
    for (name, ty) in methods {
        ctx.define_primitive_method("array", name, ty.clone());
    }
}

pub static ARRAY_PKG: Package = Package {
    import_name: "std/array",
    global_name: "array",
    fns: &["map", "filter", "sort", "reverse", "join"],
    register_types: register_array_pkg_types,
};
