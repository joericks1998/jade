use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext, vm::VmValue},
    frontend::error::{JadeError, Result, Span},
};

use crate::builtins::{BuiltinFn, Package};

const ZERO: Span = Span { line: 0, col: 0 };

fn require_str<'a>(args: &'a [VmValue], pos: usize, fn_name: &str) -> Result<&'a str> {
    match args.get(pos) {
        Some(VmValue::Str(s)) => Ok(s.as_str()),
        Some(_) => Err(JadeError::TypeError { message: fn_name.to_string(), span: ZERO }),
        None    => Err(JadeError::ArityMismatch { expected: pos + 1, got: args.len(), span: ZERO }),
    }
}

/// `path.join(base, part, ...)` — variadic; joins two or more path segments.
fn path_join(args: &[VmValue]) -> Result<VmValue> {
    if args.len() < 2 {
        return Err(JadeError::ArityMismatch { expected: 2, got: args.len(), span: ZERO });
    }
    let mut p = std::path::PathBuf::from(require_str(args, 0, "path.join")?);
    for i in 1..args.len() {
        p.push(require_str(args, i, "path.join")?);
    }
    Ok(VmValue::Str(p.to_string_lossy().into_owned()))
}

/// `path.basename(p)` — last component of the path (filename + extension).
fn path_basename(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let p = require_str(args, 0, "path.basename")?;
    let name = std::path::Path::new(p)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(VmValue::Str(name))
}

/// `path.dirname(p)` — parent directory. Returns `"."` for bare filenames.
fn path_dirname(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let p = require_str(args, 0, "path.dirname")?;
    let dir = std::path::Path::new(p)
        .parent()
        .map(|d| {
            let s = d.to_string_lossy();
            if s.is_empty() { ".".to_string() } else { s.into_owned() }
        })
        .unwrap_or_else(|| ".".to_string());
    Ok(VmValue::Str(dir))
}

/// `path.ext(p)` — file extension including the dot (e.g. `".rs"`), or nil.
fn path_ext(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let p = require_str(args, 0, "path.ext")?;
    let val = std::path::Path::new(p)
        .extension()
        .map(|e| VmValue::Str(format!(".{}", e.to_string_lossy())))
        .unwrap_or(VmValue::Nil);
    Ok(val)
}

/// `path.stem(p)` — filename without extension.
fn path_stem(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let p = require_str(args, 0, "path.stem")?;
    let stem = std::path::Path::new(p)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(VmValue::Str(stem))
}

/// `path.abs(p)` — absolute path (does not resolve symlinks, path need not exist).
fn path_abs(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let p = require_str(args, 0, "path.abs")?;
    std::path::absolute(p)
        .map(|abs| VmValue::Str(abs.to_string_lossy().into_owned()))
        .map_err(|e| JadeError::IoError { message: format!("path.abs: {}", e), span: ZERO })
}

/// `path.is_abs(p)` — true if the path is absolute.
fn path_is_abs(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let p = require_str(args, 0, "path.is_abs")?;
    Ok(VmValue::Bool(std::path::Path::new(p).is_absolute()))
}

static PATH_PKG_FNS: &[BuiltinFn] = &[
    BuiltinFn { name: "join",     vm_impl: path_join },
    BuiltinFn { name: "basename", vm_impl: path_basename },
    BuiltinFn { name: "dirname",  vm_impl: path_dirname },
    BuiltinFn { name: "ext",      vm_impl: path_ext },
    BuiltinFn { name: "stem",     vm_impl: path_stem },
    BuiltinFn { name: "abs",      vm_impl: path_abs },
    BuiltinFn { name: "is_abs",   vm_impl: path_is_abs },
];

fn register_path_pkg_types(ctx: &mut TypeContext) {
    ctx.define("path".to_string(), JadeType::Unknown);
}

pub static PATH_PKG: Package = Package {
    import_name: "std/path",
    global_name: "path",
    fns: PATH_PKG_FNS,
    register_types: register_path_pkg_types,
};
