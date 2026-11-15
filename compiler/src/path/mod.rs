use crate::builtins::Package;

pub static PATH_PKG: Package = Package {
    import_name: "std/path",
    global_name: "path",
    fns: &["join", "basename", "dirname", "ext", "stem", "abs", "is_abs"],
};
