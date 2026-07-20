use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext}, vm::VmValue,
    frontend::error::{JadeError, Result, Span},
};

use crate::builtins::{BuiltinFn, Package, make_array};
use jade_runtime::trust::JStr;

#[cfg(test)]
mod tests;

const ZERO: Span = Span { line: 0, col: 0 };

fn io_err(op: &str, path: &str, e: std::io::Error) -> JadeError {
    JadeError::IoError {
        message: format!("{} '{}': {}", op, path, e),
        span: ZERO,
    }
}

fn require_str<'a>(args: &'a [VmValue], pos: usize, fn_name: &str) -> Result<&'a str> {
    match args.get(pos) {
        Some(VmValue::Str(s)) => Ok(s.as_str()),
        Some(_) => Err(JadeError::TypeError { message: fn_name.to_string(), span: ZERO }),
        None    => Err(JadeError::ArityMismatch { expected: pos + 1, got: args.len(), span: ZERO }),
    }
}

// fs.read/write/append accept an optional trailing `trust` bool (positional or
// `trust=`). It only affects the AOT taint model (trust=true → TRUSTED content,
// skip the tainted-path refusal); the VM has no trust model, so it accepts and
// ignores the flag — keeping a program that passes `trust=true` portable.
fn fs_read(args: &[VmValue]) -> Result<VmValue> {
    if args.is_empty() || args.len() > 2 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    // A tainted path is refused unless the caller explicitly vouches for it
    // with `trust=true` — the same rule as the AOT forwarder in common.c.
    let vouched = matches!(args.get(1), Some(VmValue::Bool(true)));
    if !vouched {
        if let Some(VmValue::Str(s)) = args.first() {
            if s.is_tainted() {
                return Err(JadeError::Exception {
                    message: jade_runtime::trust::refusal_message("fs.read(path)"),
                    span: ZERO,
                });
            }
        }
    }
    let path = require_str(args, 0, "fs.read")?;
    jade_runtime::fsf::read(path)
        .map(|s| VmValue::Str(JStr::tainted(s)))
        .map_err(|e| io_err("read", path, e))
}

fn fs_write(args: &[VmValue]) -> Result<VmValue> {
    if args.len() < 2 || args.len() > 3 {
        return Err(JadeError::ArityMismatch { expected: 2, got: args.len(), span: ZERO });
    }
    let path    = require_str(args, 0, "fs.write")?;
    let content = require_str(args, 1, "fs.write")?;
    jade_runtime::fsf::write(path, content)
        .map(|_| VmValue::Nil)
        .map_err(|e| io_err("write", path, e))
}

fn fs_append(args: &[VmValue]) -> Result<VmValue> {
    if args.len() < 2 || args.len() > 3 {
        return Err(JadeError::ArityMismatch { expected: 2, got: args.len(), span: ZERO });
    }
    let path    = require_str(args, 0, "fs.append")?;
    let content = require_str(args, 1, "fs.append")?;
    jade_runtime::fsf::append(path, content)
        .map(|_| VmValue::Nil)
        .map_err(|e| io_err("append", path, e))
}

fn fs_exists(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let path = require_str(args, 0, "fs.exists")?;
    Ok(VmValue::Bool(jade_runtime::fsf::exists(path)))
}

fn fs_delete(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let path = require_str(args, 0, "fs.delete")?;
    jade_runtime::fsf::delete(path)
        .map(|_| VmValue::Nil)
        .map_err(|e| io_err("delete", path, e))
}

fn fs_list_dir(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let path = require_str(args, 0, "fs.list_dir")?;
    jade_runtime::fsf::list_dir(path)
        .map(|names| make_array(names.into_iter().map(|s| VmValue::Str(JStr::tainted(s))).collect()))
        .map_err(|e| io_err("list_dir", path, e))
}

fn fs_mkdir(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let path = require_str(args, 0, "fs.mkdir")?;
    jade_runtime::fsf::mkdir(path)
        .map(|_| VmValue::Nil)
        .map_err(|e| io_err("mkdir", path, e))
}

static FS_PKG_FNS: &[BuiltinFn] = &[
    BuiltinFn { name: "read",     vm_impl: fs_read },
    BuiltinFn { name: "write",    vm_impl: fs_write },
    BuiltinFn { name: "append",   vm_impl: fs_append },
    BuiltinFn { name: "exists",   vm_impl: fs_exists },
    BuiltinFn { name: "delete",   vm_impl: fs_delete },
    BuiltinFn { name: "list_dir", vm_impl: fs_list_dir },
    BuiltinFn { name: "mkdir",    vm_impl: fs_mkdir },
];

fn register_fs_pkg_types(ctx: &mut TypeContext) {
    ctx.define("fs".to_string(), JadeType::Unknown);
}

pub static FS_PKG: Package = Package {
    import_name: "std/fs",
    global_name: "fs",
    fns: FS_PKG_FNS,
    register_types: register_fs_pkg_types,
};
