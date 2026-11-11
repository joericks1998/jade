use crate::builtins::Package;
use crate::typechecker::tir::JadeType;
use crate::typechecker::type_infer::TypeContext;

fn register_time_pkg_types(ctx: &mut TypeContext) {
    ctx.define("time".to_string(), JadeType::Unknown);
}

pub static TIME_PKG: Package = Package {
    import_name: "std/time",
    global_name: "time",
    fns: &["now", "now_ms", "monotonic", "sleep", "after", "local", "utc", "parts", "stamp"],
    register_types: register_time_pkg_types,
};
