use crate::builtins::Package;

pub static JSON_PKG: Package = Package {
    import_name: "std/json",
    global_name: "json",
    fns: &["parse", "stringify", "stringify_pretty"],
};
