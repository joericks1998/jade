use crate::builtins::Package;

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
};
