use crate::builtins::Package;

/// The methods a `array` receiver has, by name.
pub const ARRAY_METHODS: &[&str] =
    &["len", "push", "pop", "contains", "sort", "reverse", "join", "map", "filter"];

pub static ARRAY_PKG: Package = Package {
    import_name: "std/array",
    global_name: "array",
    fns: &["map", "filter", "sort", "reverse", "join"],
};
