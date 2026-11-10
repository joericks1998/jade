use crate::builtins::Package;
use crate::compiler::tir::JadeType;
use crate::compiler::type_infer::TypeContext;

fn register_random_pkg_types(ctx: &mut TypeContext) {
    ctx.define("random".to_string(), JadeType::Unknown);
}

pub static RANDOM_PKG: Package = Package {
    import_name: "std/random",
    global_name: "random",
    fns: &["int", "float", "choice", "shuffle", "seed"],
    register_types: register_random_pkg_types,
};
