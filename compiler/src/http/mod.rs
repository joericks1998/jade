use crate::builtins::Package;

pub static HTTP_PKG: Package = Package {
    import_name: "std/http",
    global_name: "http",
    fns: &["get", "get_bytes", "post_bytes", "post", "put", "delete", "head"],
};
