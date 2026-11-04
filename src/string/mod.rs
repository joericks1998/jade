use crate::builtins::Package;
use crate::compiler::tir::JadeType;
use crate::compiler::type_infer::TypeContext;

/// The methods a `str` receiver has, by name.
pub const STR_METHODS: &[&str] = &[
    "encode",
    "len",
    "upper",
    "lower",
    "trim",
    "split",
    "contains",
    "replace",
    "starts_with",
    "ends_with",
    "trim_start",
    "trim_end",
    "capitalize",
    "index_of",
    "last_index_of",
    "count",
    "is_empty",
    "slice",
    "repeat",
    "pad_start",
    "pad_end",
    "lines",
];

fn register_string_pkg_types(ctx: &mut TypeContext) {
    ctx.define("string".to_string(), JadeType::Unknown);
}

pub fn register_str_method_types(ctx: &mut TypeContext) {
    let ret_str = || Box::new(JadeType::Str);
    let ret_int = || Box::new(JadeType::Int);
    let ret_bool = || Box::new(JadeType::Bool);
    let ret_arr = || Box::new(JadeType::Array(Box::new(JadeType::Str)));

    let methods: &[(&str, JadeType)] = &[
        ("len", JadeType::Fn { params: vec![], ret: ret_int() }),
        ("upper", JadeType::Fn { params: vec![], ret: ret_str() }),
        ("lower", JadeType::Fn { params: vec![], ret: ret_str() }),
        ("trim", JadeType::Fn { params: vec![], ret: ret_str() }),
        ("split", JadeType::Fn { params: vec![JadeType::Str], ret: ret_arr() }),
        ("contains", JadeType::Fn { params: vec![JadeType::Str], ret: ret_bool() }),
        ("replace", JadeType::Fn { params: vec![JadeType::Str, JadeType::Str], ret: ret_str() }),
        ("starts_with", JadeType::Fn { params: vec![JadeType::Str], ret: ret_bool() }),
        ("ends_with", JadeType::Fn { params: vec![JadeType::Str], ret: ret_bool() }),
        ("encode", JadeType::Fn { params: vec![], ret: Box::new(JadeType::Bytes) }),
        // v1.3.23. The return type drives print and format lowering, so an
        // omission here shows up as a formatting divergence between the engines
        // rather than as a rejection.
        ("trim_start", JadeType::Fn { params: vec![], ret: ret_str() }),
        ("trim_end", JadeType::Fn { params: vec![], ret: ret_str() }),
        ("capitalize", JadeType::Fn { params: vec![], ret: ret_str() }),
        ("is_empty", JadeType::Fn { params: vec![], ret: ret_bool() }),
        ("index_of", JadeType::Fn { params: vec![JadeType::Str], ret: ret_int() }),
        ("last_index_of", JadeType::Fn { params: vec![JadeType::Str], ret: ret_int() }),
        ("count", JadeType::Fn { params: vec![JadeType::Str], ret: ret_int() }),
        ("repeat", JadeType::Fn { params: vec![JadeType::Int], ret: ret_str() }),
        ("slice", JadeType::Fn { params: vec![JadeType::Int, JadeType::Int], ret: ret_str() }),
        ("pad_start", JadeType::Fn { params: vec![JadeType::Int, JadeType::Str], ret: ret_str() }),
        ("pad_end", JadeType::Fn { params: vec![JadeType::Int, JadeType::Str], ret: ret_str() }),
        ("lines", JadeType::Fn { params: vec![], ret: ret_arr() }),
    ];
    for (name, ty) in methods {
        ctx.define_primitive_method("str", name, ty.clone());
    }
}

pub static STRING_PKG: Package = Package {
    import_name: "std/string",
    global_name: "string",
    fns: &[
        "split",
        "upper",
        "lower",
        "trim",
        "contains",
        "replace",
        "starts_with",
        "ends_with",
        "trim_start",
        "trim_end",
        "capitalize",
        "index_of",
        "last_index_of",
        "count",
        "is_empty",
        "slice",
        "repeat",
        "pad_start",
        "pad_end",
        "lines",
    ],
    register_types: register_string_pkg_types,
};
