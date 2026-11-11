use crate::builtins::Package;
use crate::typechecker::tir::JadeType;
use crate::typechecker::type_infer::TypeContext;

fn register_env_pkg_types(ctx: &mut TypeContext) {
    ctx.define("env".to_string(), JadeType::Unknown);
}

pub static ENV_PKG: Package = Package {
    import_name: "std/env",
    global_name: "env",
    fns: &["get", "set", "args", "cwd"],
    register_types: register_env_pkg_types,
};
