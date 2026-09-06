use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext},
    frontend::error::{JadeError, Result, Span},
    vm::VmValue,
};

use crate::builtins::{BuiltinFn, Package};
use jade_runtime::trust::JStr;

#[cfg(test)]
mod tests;

const ZERO: Span = Span { line: 0, col: 0 };

/// The trust of a string argument, so a pure path transform can carry it
/// through: `path.abs(tainted)` is still tainted — the characters came from the
/// same place.
fn str_trust(args: &[VmValue], pos: usize) -> u8 {
    match args.get(pos) {
        Some(VmValue::Str(s)) => s.trust(),
        _ => jade_runtime::trust::TRUSTED,
    }
}

fn require_str<'a>(args: &'a [VmValue], pos: usize, fn_name: &str) -> Result<&'a str> {
    match args.get(pos) {
        Some(VmValue::Str(s)) => Ok(s.as_str()),
        Some(_) => Err(JadeError::TypeError { message: fn_name.to_string(), span: ZERO }),
        None => Err(JadeError::ArityMismatch { expected: pos + 1, got: args.len(), span: ZERO }),
    }
}

/// `path.join(base, part, ...)` — variadic; joins two or more path segments.
fn path_join(args: &[VmValue]) -> Result<VmValue> {
    if args.len() < 2 {
        return Err(JadeError::ArityMismatch { expected: 2, got: args.len(), span: ZERO });
    }
    let mut segs: Vec<&str> = Vec::with_capacity(args.len());
    for i in 0..args.len() {
        segs.push(require_str(args, i, "path.join")?);
    }
    // The join is as tainted as its most tainted segment: one untrusted
    // component is enough to steer the whole path.
    let trust =
        jade_runtime::trust::combine_all((0..args.len()).map(|i| str_trust(args, i)));
    Ok(VmValue::Str(JStr::with_trust(jade_runtime::pathf::join(&segs), trust)))
}

/// `path.basename(p)` — last component of the path (filename + extension).
fn path_basename(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let p = require_str(args, 0, "path.basename")?;
    // Trust travels with the derived path. These used to go through `.into()`
    // (`JStr::trusted`), so taking a tainted path apart and putting it back
    // together laundered it, and `fs.read(path.dirname(tainted))` was accepted
    // where `fs.read(tainted)` was refused. `path.abs` always did this
    // correctly, and the compiled backend does it for all of them.
    Ok(VmValue::Str(JStr::with_trust(jade_runtime::pathf::basename(p), str_trust(args, 0))))
}

/// `path.dirname(p)` — parent directory. Returns `"."` for bare filenames.
fn path_dirname(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let p = require_str(args, 0, "path.dirname")?;
    // Trust travels with the derived path. These used to go through `.into()`
    // (`JStr::trusted`), so taking a tainted path apart and putting it back
    // together laundered it, and `fs.read(path.dirname(tainted))` was accepted
    // where `fs.read(tainted)` was refused. `path.abs` always did this
    // correctly, and the compiled backend does it for all of them.
    Ok(VmValue::Str(JStr::with_trust(jade_runtime::pathf::dirname(p), str_trust(args, 0))))
}

/// `path.ext(p)` — file extension including the dot (e.g. `".rs"`), or nil.
fn path_ext(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let p = require_str(args, 0, "path.ext")?;
    Ok(match jade_runtime::pathf::ext(p) {
        Some(e) => VmValue::Str(JStr::with_trust(e, str_trust(args, 0))),
        None => VmValue::Nil,
    })
}

/// `path.stem(p)` — filename without extension.
fn path_stem(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let p = require_str(args, 0, "path.stem")?;
    // Trust travels with the derived path. These used to go through `.into()`
    // (`JStr::trusted`), so taking a tainted path apart and putting it back
    // together laundered it, and `fs.read(path.dirname(tainted))` was accepted
    // where `fs.read(tainted)` was refused. `path.abs` always did this
    // correctly, and the compiled backend does it for all of them.
    Ok(VmValue::Str(JStr::with_trust(jade_runtime::pathf::stem(p), str_trust(args, 0))))
}

/// `path.abs(p)` — absolute path (does not resolve symlinks, path need not exist).
fn path_abs(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let p = require_str(args, 0, "path.abs")?;
    let trust = str_trust(args, 0);
    jade_runtime::pathf::abs(p)
        .map(|s| VmValue::Str(JStr::with_trust(s, trust)))
        .map_err(|e| JadeError::IoError { message: format!("path.abs: {}", e), span: ZERO })
}

/// `path.is_abs(p)` — true if the path is absolute.
fn path_is_abs(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let p = require_str(args, 0, "path.is_abs")?;
    Ok(VmValue::Bool(jade_runtime::pathf::is_abs(p)))
}

static PATH_PKG_FNS: &[BuiltinFn] = &[
    BuiltinFn { name: "join", vm_impl: path_join },
    BuiltinFn { name: "basename", vm_impl: path_basename },
    BuiltinFn { name: "dirname", vm_impl: path_dirname },
    BuiltinFn { name: "ext", vm_impl: path_ext },
    BuiltinFn { name: "stem", vm_impl: path_stem },
    BuiltinFn { name: "abs", vm_impl: path_abs },
    BuiltinFn { name: "is_abs", vm_impl: path_is_abs },
];

fn register_path_pkg_types(ctx: &mut TypeContext) {
    ctx.define("path".to_string(), JadeType::Unknown);
}

pub static PATH_PKG: Package = Package {
    import_name: "std/path",
    global_name: "path",
    fns: PATH_PKG_FNS,
    natives: &[],
    register_types: register_path_pkg_types,
};
