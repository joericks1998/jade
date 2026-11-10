use crate::builtins::Package;
use crate::compiler::tir::JadeType;
use crate::compiler::type_infer::TypeContext;

fn register_path_pkg_types(ctx: &mut TypeContext) {
    ctx.define("path".to_string(), JadeType::Unknown);
}

pub static PATH_PKG: Package = Package {
    import_name: "std/path",
    global_name: "path",
    fns: &["join", "basename", "dirname", "ext", "stem", "abs", "is_abs"],
    register_types: register_path_pkg_types,
};
