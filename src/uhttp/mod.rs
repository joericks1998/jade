//! `std/uhttp`: HTTP/1.1 over a Unix domain socket.
//!
//! Mirrors the `std/http` API (`get`/`post`/`put`/`delete`/`head`, returning a
//! `{status, body}` dict) but connects over a Unix socket path instead of a TCP
//! host. The single string argument is a pseudo-URL:
//!
//! ```text
//! unix://<socket-path>:<request-path>
//! unix:///var/run/docker.sock:/v1.43/containers/json
//! ```
//!
//! The transport lives in `jade_runtime::uhttpf`, behind the `jrt_uhttp_*`
//! symbols compiled programs call, including `uhttpf::Stream` for `uhttp.stream`.
//! This module only names the package's functions and registers their types.

use crate::builtins::Package;
use crate::compiler::tir::JadeType;
use crate::compiler::type_infer::TypeContext;

fn register_uhttp_pkg_types(ctx: &mut TypeContext) {
    ctx.define("uhttp".to_string(), JadeType::Unknown);
}

pub static UHTTP_PKG: Package = Package {
    import_name: "std/uhttp",
    global_name: "uhttp",
    fns: &["get", "post", "put", "delete", "head", "get_bytes", "post_bytes", "stream"],
    register_types: register_uhttp_pkg_types,
};
