use crate::builtins::Package;
use crate::compiler::tir::JadeType;
use crate::compiler::type_infer::TypeContext;

fn register_http_pkg_types(ctx: &mut TypeContext) {
    ctx.define("http".to_string(), JadeType::Unknown);
}

pub static HTTP_PKG: Package = Package {
    import_name: "std/http",
    global_name: "http",
    fns: &["get", "get_bytes", "post_bytes", "post", "put", "delete", "head"],
    register_types: register_http_pkg_types,
};
