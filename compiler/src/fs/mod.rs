use crate::builtins::Package;
use crate::compiler::tir::JadeType;
use crate::compiler::type_infer::TypeContext;

fn register_fs_pkg_types(ctx: &mut TypeContext) {
    ctx.define("fs".to_string(), JadeType::Unknown);
}

pub static FS_PKG: Package = Package {
    import_name: "std/fs",
    global_name: "fs",
    fns: &[
        "read_stdin_bytes",
        "write_stdout_bytes",
        "read_bytes",
        "write_bytes",
        "append_bytes",
        "read",
        "write",
        "append",
        "exists",
        "delete",
        "list_dir",
        "mkdir",
        "is_file",
        "is_dir",
        "copy",
        "rename",
        "rmdir",
        "size",
    ],
    register_types: register_fs_pkg_types,
};
