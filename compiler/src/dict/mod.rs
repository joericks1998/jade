use crate::builtins::Package;
use crate::compiler::tir::JadeType;
use crate::compiler::type_infer::TypeContext;

/// The methods a `dict` receiver has, by name.
pub const DICT_METHODS: &[&str] = &["len", "keys", "values", "has", "get"];

fn register_dict_pkg_types(ctx: &mut TypeContext) {
    ctx.define("dict".to_string(), JadeType::Unknown);
}

pub fn register_dict_method_types(ctx: &mut TypeContext) {
    let methods: &[(&str, JadeType)] = &[
        ("len", JadeType::Fn { params: vec![], ret: Box::new(JadeType::Int) }),
        (
            "keys",
            JadeType::Fn {
                params: vec![],
                ret: Box::new(JadeType::Array(Box::new(JadeType::Str))),
            },
        ),
        (
            "values",
            JadeType::Fn {
                params: vec![],
                ret: Box::new(JadeType::Array(Box::new(JadeType::Unknown))),
            },
        ),
        ("has", JadeType::Fn { params: vec![JadeType::Str], ret: Box::new(JadeType::Bool) }),
        ("get", JadeType::Fn { params: vec![JadeType::Str], ret: Box::new(JadeType::Unknown) }),
    ];
    for (name, ty) in methods {
        ctx.define_primitive_method("dict", name, ty.clone());
    }
}

pub static DICT_PKG: Package = Package {
    import_name: "std/dict",
    global_name: "dict",
    fns: &["keys", "values", "has", "get", "merge"],
    register_types: register_dict_pkg_types,
};
