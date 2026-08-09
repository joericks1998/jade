use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext},
    frontend::error::{JadeError, Result, Span},
    vm::VmValue,
};

use crate::builtins::{BuiltinFn, Package, make_array};
use jade_runtime::trust::JStr;

#[cfg(test)]
mod tests;

const ZERO: Span = Span { line: 0, col: 0 };

fn io_err(op: &str, path: &str, e: std::io::Error) -> JadeError {
    JadeError::IoError { message: format!("{} '{}': {}", op, path, e), span: ZERO }
}

fn require_str<'a>(args: &'a [VmValue], pos: usize, fn_name: &str) -> Result<&'a str> {
    match args.get(pos) {
        Some(VmValue::Str(s)) => Ok(s.as_str()),
        Some(_) => Err(JadeError::TypeError { message: fn_name.to_string(), span: ZERO }),
        None => Err(JadeError::ArityMismatch { expected: pos + 1, got: args.len(), span: ZERO }),
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
    let path = require_str(args, 0, "fs.write")?;
    let content = require_str(args, 1, "fs.write")?;
    jade_runtime::fsf::write(path, content)
        .map(|_| VmValue::Nil)
        .map_err(|e| io_err("write", path, e))
}

fn fs_append(args: &[VmValue]) -> Result<VmValue> {
    if args.len() < 2 || args.len() > 3 {
        return Err(JadeError::ArityMismatch { expected: 2, got: args.len(), span: ZERO });
    }
    let path = require_str(args, 0, "fs.append")?;
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
    jade_runtime::fsf::delete(path).map(|_| VmValue::Nil).map_err(|e| io_err("delete", path, e))
}

fn fs_list_dir(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let path = require_str(args, 0, "fs.list_dir")?;
    jade_runtime::fsf::list_dir(path)
        .map(|names| {
            make_array(names.into_iter().map(|s| VmValue::Str(JStr::tainted(s))).collect())
        })
        .map_err(|e| io_err("list_dir", path, e))
}

fn fs_mkdir(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let path = require_str(args, 0, "fs.mkdir")?;
    jade_runtime::fsf::mkdir(path).map(|_| VmValue::Nil).map_err(|e| io_err("mkdir", path, e))
}

/// `fs.read_bytes(path)` — a file as raw octets.
///
/// The content is *tainted*, exactly as `fs.read` is: it comes from outside the
/// program. That the trust byte lives on the blob rather than in a string
/// header is what stops `fs.read_bytes(p).decode()` laundering it.
fn fs_read_bytes(args: &[VmValue]) -> Result<VmValue> {
    if args.is_empty() || args.len() > 2 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let vouched = matches!(args.get(1), Some(VmValue::Bool(true)));
    if !vouched {
        if let Some(VmValue::Str(s)) = args.first() {
            if s.is_tainted() {
                return Err(JadeError::Exception {
                    message: jade_runtime::trust::refusal_message("fs.read_bytes(path)"),
                    span: ZERO,
                });
            }
        }
    }
    let path = require_str(args, 0, "fs.read_bytes")?;
    jade_runtime::fsf::read_bytes(path)
        .map(|d| {
            VmValue::Bytes(std::sync::Arc::new(jade_runtime::bytesf::BytesObj::new(
                d,
                jade_runtime::trust::TAINTED,
            )))
        })
        .map_err(|e| io_err("read_bytes", path, e))
}

/// The octets a `bytes` argument carries, for the write paths.
fn require_bytes<'a>(args: &'a [VmValue], pos: usize, who: &str) -> Result<&'a [u8]> {
    match args.get(pos) {
        Some(VmValue::Bytes(b)) => Ok(b.as_slice()),
        Some(other) => Err(JadeError::TypeError {
            message: format!("{who} expects bytes, got {}", crate::vm::value_type_name(other)),
            span: ZERO,
        }),
        None => Err(JadeError::ArityMismatch { expected: pos + 1, got: args.len(), span: ZERO }),
    }
}

fn fs_write_bytes(args: &[VmValue]) -> Result<VmValue> {
    if args.len() < 2 || args.len() > 3 {
        return Err(JadeError::ArityMismatch { expected: 2, got: args.len(), span: ZERO });
    }
    let path = require_str(args, 0, "fs.write_bytes")?;
    let data = require_bytes(args, 1, "fs.write_bytes")?;
    jade_runtime::fsf::write_bytes(path, data)
        .map(|_| VmValue::Nil)
        .map_err(|e| io_err("write_bytes", path, e))
}

fn fs_append_bytes(args: &[VmValue]) -> Result<VmValue> {
    if args.len() < 2 || args.len() > 3 {
        return Err(JadeError::ArityMismatch { expected: 2, got: args.len(), span: ZERO });
    }
    let path = require_str(args, 0, "fs.append_bytes")?;
    let data = require_bytes(args, 1, "fs.append_bytes")?;
    jade_runtime::fsf::append_bytes(path, data)
        .map(|_| VmValue::Nil)
        .map_err(|e| io_err("append_bytes", path, e))
}

/// `fs.read_stdin_bytes()` — all of stdin as raw octets, tainted.
fn fs_read_stdin_bytes(args: &[VmValue]) -> Result<VmValue> {
    if !args.is_empty() {
        return Err(JadeError::ArityMismatch { expected: 0, got: args.len(), span: ZERO });
    }
    jade_runtime::fsf::read_stdin_bytes()
        .map(|d| {
            VmValue::Bytes(std::sync::Arc::new(jade_runtime::bytesf::BytesObj::new(
                d,
                jade_runtime::trust::TAINTED,
            )))
        })
        .map_err(|e| io_err("read_stdin_bytes", "<stdin>", e))
}

/// `fs.write_stdout_bytes(b)` — raw octets to stdout, no newline, flushed.
fn fs_write_stdout_bytes(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let data = require_bytes(args, 0, "fs.write_stdout_bytes")?;
    jade_runtime::fsf::write_stdout_bytes(data)
        .map(|_| VmValue::Nil)
        .map_err(|e| io_err("write_stdout_bytes", "<stdout>", e))
}

static FS_PKG_FNS: &[BuiltinFn] = &[
    BuiltinFn { name: "read_stdin_bytes", vm_impl: fs_read_stdin_bytes },
    BuiltinFn { name: "write_stdout_bytes", vm_impl: fs_write_stdout_bytes },
    BuiltinFn { name: "read_bytes", vm_impl: fs_read_bytes },
    BuiltinFn { name: "write_bytes", vm_impl: fs_write_bytes },
    BuiltinFn { name: "append_bytes", vm_impl: fs_append_bytes },
    BuiltinFn { name: "read", vm_impl: fs_read },
    BuiltinFn { name: "write", vm_impl: fs_write },
    BuiltinFn { name: "append", vm_impl: fs_append },
    BuiltinFn { name: "exists", vm_impl: fs_exists },
    BuiltinFn { name: "delete", vm_impl: fs_delete },
    BuiltinFn { name: "list_dir", vm_impl: fs_list_dir },
    BuiltinFn { name: "mkdir", vm_impl: fs_mkdir },
];

fn register_fs_pkg_types(ctx: &mut TypeContext) {
    ctx.define("fs".to_string(), JadeType::Unknown);
}

pub static FS_PKG: Package = Package {
    import_name: "std/fs",
    global_name: "fs",
    fns: FS_PKG_FNS,
    natives: &[],
    register_types: register_fs_pkg_types,
};
