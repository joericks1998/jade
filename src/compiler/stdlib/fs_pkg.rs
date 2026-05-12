use std::io::Write as IoWrite;

use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext, vm::VmValue},
    frontend::error::{JadeError, Result, Span},
};

use super::{BuiltinFn, Package, make_array};

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
        Some(_) => Err(JadeError::TypeError { op: fn_name.to_string(), span: ZERO }),
        None    => Err(JadeError::ArityMismatch { expected: pos + 1, got: args.len(), span: ZERO }),
    }
}

fn fs_read(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let path = require_str(args, 0, "fs.read")?;
    std::fs::read_to_string(path)
        .map(VmValue::Str)
        .map_err(|e| io_err("read", path, e))
}

fn fs_write(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 2 {
        return Err(JadeError::ArityMismatch { expected: 2, got: args.len(), span: ZERO });
    }
    let path    = require_str(args, 0, "fs.write")?;
    let content = require_str(args, 1, "fs.write")?;
    std::fs::write(path, content)
        .map(|_| VmValue::Nil)
        .map_err(|e| io_err("write", path, e))
}

fn fs_append(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 2 {
        return Err(JadeError::ArityMismatch { expected: 2, got: args.len(), span: ZERO });
    }
    let path    = require_str(args, 0, "fs.append")?;
    let content = require_str(args, 1, "fs.append")?;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .map_err(|e| io_err("append", path, e))?;
    file.write_all(content.as_bytes())
        .map(|_| VmValue::Nil)
        .map_err(|e| io_err("append", path, e))
}

fn fs_exists(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let path = require_str(args, 0, "fs.exists")?;
    Ok(VmValue::Bool(std::path::Path::new(path).exists()))
}

fn fs_delete(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let path = require_str(args, 0, "fs.delete")?;
    std::fs::remove_file(path)
        .map(|_| VmValue::Nil)
        .map_err(|e| io_err("delete", path, e))
}

fn fs_list_dir(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let path = require_str(args, 0, "fs.list_dir")?;
    let entries = std::fs::read_dir(path)
        .map_err(|e| io_err("list_dir", path, e))?
        .map(|entry| {
            entry
                .map(|e| VmValue::Str(e.file_name().to_string_lossy().into_owned()))
                .map_err(|e| io_err("list_dir", path, e))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(make_array(entries))
}

fn fs_mkdir(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let path = require_str(args, 0, "fs.mkdir")?;
    std::fs::create_dir_all(path)
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
