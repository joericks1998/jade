use crate::builtins::Package;

pub static RANDOM_PKG: Package = Package {
    import_name: "std/random",
    global_name: "random",
    fns: &["int", "float", "choice", "shuffle", "seed"],
};
