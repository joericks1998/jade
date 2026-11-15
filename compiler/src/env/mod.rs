use crate::builtins::Package;

pub static ENV_PKG: Package =
    Package { import_name: "std/env", global_name: "env", fns: &["get", "set", "args", "cwd"] };
