use crate::builtins::Package;
use crate::compiler::tir::JadeType;
use crate::compiler::type_infer::TypeContext;

fn register_sh_pkg_types(ctx: &mut TypeContext) {
    ctx.define("sh".to_string(), JadeType::Unknown);
}

pub static SH_PKG: Package = Package {
    import_name: "std/sh",
    global_name: "sh",
    fns: &["exec", "run", "output"],
    register_types: register_sh_pkg_types,
};
