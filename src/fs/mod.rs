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
    if !vouched
        && let Some(VmValue::Str(s)) = args.first()
        && s.is_tainted()
    {
        return Err(JadeError::Exception {
            message: jade_runtime::trust::refusal_message("fs.read(path)"),
            span: ZERO,
        });
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

/// `fs.is_file(path)` / `fs.is_dir(path)`.
///
/// Neither refuses a tainted path. `fs.exists` does not either, and a rule
/// applied to two of the three questions you can ask about a path is worse than
/// one applied to none — the read sinks are where the refusal earns its keep.
fn fs_is_file(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    Ok(VmValue::Bool(jade_runtime::fsf::is_file(require_str(args, 0, "fs.is_file")?)))
}

/// See [`fs_is_file`].
fn fs_is_dir(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    Ok(VmValue::Bool(jade_runtime::fsf::is_dir(require_str(args, 0, "fs.is_dir")?)))
}

/// `fs.copy(src, dst)` — creating or truncating `dst`.
fn fs_copy(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 2 {
        return Err(JadeError::ArityMismatch { expected: 2, got: args.len(), span: ZERO });
    }
    let (src, dst) = (require_str(args, 0, "fs.copy")?, require_str(args, 1, "fs.copy")?);
    jade_runtime::fsf::copy(src, dst).map(|_| VmValue::Nil).map_err(|e| io_err("copy", src, e))
}

/// `fs.rename(src, dst)` — rename or move.
fn fs_rename(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 2 {
        return Err(JadeError::ArityMismatch { expected: 2, got: args.len(), span: ZERO });
    }
    let (src, dst) = (require_str(args, 0, "fs.rename")?, require_str(args, 1, "fs.rename")?);
    jade_runtime::fsf::rename(src, dst).map(|_| VmValue::Nil).map_err(|e| io_err("rename", src, e))
}

/// `fs.rmdir(path)` — an **empty** directory only. Deliberately not recursive;
/// see [`jade_runtime::fsf::rmdir`].
fn fs_rmdir(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let path = require_str(args, 0, "fs.rmdir")?;
    jade_runtime::fsf::rmdir(path).map(|_| VmValue::Nil).map_err(|e| io_err("rmdir", path, e))
}

/// `fs.size(path)` — bytes.
fn fs_size(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let path = require_str(args, 0, "fs.size")?;
    jade_runtime::fsf::size(path).map(VmValue::Int).map_err(|e| io_err("size", path, e))
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
    if !vouched
        && let Some(VmValue::Str(s)) = args.first()
        && s.is_tainted()
    {
        return Err(JadeError::Exception {
            message: jade_runtime::trust::refusal_message("fs.read_bytes(path)"),
            span: ZERO,
        });
    }
    let path = require_str(args, 0, "fs.read_bytes")?;
    jade_runtime::fsf::read_bytes(path)
        .map(|d| crate::builtins::make_bytes(d, jade_runtime::trust::TAINTED))
        .map_err(|e| io_err("read_bytes", path, e))
}

/// Run `f` over the octets a `bytes` argument carries, for the write paths.
///
/// A callback rather than a returned slice, because a blob lives behind a lock
/// and a borrow of the payload cannot outlive the guard that made it safe to
/// read. Returning an owned `Vec` would compile just as well and would copy the
/// whole buffer on every call, which is the wrong shape for a function whose
/// entire job is writing a large one to disk.
///
/// The guard is held across `f`, so `f` must not touch the same blob again:
/// `parking_lot::Mutex` is not reentrant. Every caller here hands the octets
/// straight to `jade_runtime::fsf`, which knows nothing about Jade values.
fn with_bytes<R>(args: &[VmValue], pos: usize, who: &str, f: impl FnOnce(&[u8]) -> R) -> Result<R> {
    match args.get(pos) {
        Some(VmValue::Bytes(b)) => Ok(f(b.lock().as_slice())),
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
    with_bytes(args, 1, "fs.write_bytes", |data| jade_runtime::fsf::write_bytes(path, data))?
        .map(|_| VmValue::Nil)
        .map_err(|e| io_err("write_bytes", path, e))
}

fn fs_append_bytes(args: &[VmValue]) -> Result<VmValue> {
    if args.len() < 2 || args.len() > 3 {
        return Err(JadeError::ArityMismatch { expected: 2, got: args.len(), span: ZERO });
    }
    let path = require_str(args, 0, "fs.append_bytes")?;
    with_bytes(args, 1, "fs.append_bytes", |data| jade_runtime::fsf::append_bytes(path, data))?
        .map(|_| VmValue::Nil)
        .map_err(|e| io_err("append_bytes", path, e))
}

/// `fs.read_stdin_bytes()` — all of stdin as raw octets, tainted.
fn fs_read_stdin_bytes(args: &[VmValue]) -> Result<VmValue> {
    if !args.is_empty() {
        return Err(JadeError::ArityMismatch { expected: 0, got: args.len(), span: ZERO });
    }
    jade_runtime::fsf::read_stdin_bytes()
        .map(|d| crate::builtins::make_bytes(d, jade_runtime::trust::TAINTED))
        .map_err(|e| io_err("read_stdin_bytes", "<stdin>", e))
}

/// `fs.write_stdout_bytes(b)` — raw octets to stdout, no newline, flushed.
fn fs_write_stdout_bytes(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    with_bytes(args, 0, "fs.write_stdout_bytes", jade_runtime::fsf::write_stdout_bytes)?
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
    BuiltinFn { name: "is_file", vm_impl: fs_is_file },
    BuiltinFn { name: "is_dir", vm_impl: fs_is_dir },
    BuiltinFn { name: "copy", vm_impl: fs_copy },
    BuiltinFn { name: "rename", vm_impl: fs_rename },
    BuiltinFn { name: "rmdir", vm_impl: fs_rmdir },
    BuiltinFn { name: "size", vm_impl: fs_size },
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
