use crate::builtins::Package;

/// The methods a `dict` receiver has, by name.
pub const DICT_METHODS: &[&str] = &["len", "keys", "values", "has", "get"];

pub static DICT_PKG: Package = Package {
    import_name: "std/dict",
    global_name: "dict",
    fns: &["keys", "values", "has", "get", "merge"],
};
