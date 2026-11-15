use crate::builtins::Package;

pub static SH_PKG: Package =
    Package { import_name: "std/sh", global_name: "sh", fns: &["exec", "run", "output"] };
