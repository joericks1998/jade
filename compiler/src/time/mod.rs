use crate::builtins::Package;

pub static TIME_PKG: Package = Package {
    import_name: "std/time",
    global_name: "time",
    fns: &["now", "now_ms", "monotonic", "sleep", "after", "local", "utc", "parts", "stamp"],
};
