use crate::builtins::Package;
use crate::compiler::tir::JadeType;
use crate::compiler::type_infer::TypeContext;

fn register_json_pkg_types(ctx: &mut TypeContext) {
    ctx.define("json".to_string(), JadeType::Unknown);
}

pub static JSON_PKG: Package = Package {
    import_name: "std/json",
    global_name: "json",
    fns: &["parse", "stringify", "stringify_pretty"],
    register_types: register_json_pkg_types,
};
