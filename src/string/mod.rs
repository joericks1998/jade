#[cfg(test)]
mod tests;

use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext},
    frontend::error::{JadeError, Result, Span},
    vm::VmValue,
};

use crate::builtins::{BuiltinFn, Package, make_array};
use jade_runtime::trust::JStr;

const ZERO: Span = Span { line: 0, col: 0 };

// ── Primitive str methods (args[0] = self) ────────────────────────────────────

fn str_len(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Str(s) => Ok(VmValue::Int(s.chars().count() as i64)),
        _ => Err(JadeError::TypeError { message: "str.len".to_string(), span: ZERO }),
    }
}

fn str_upper(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Str(s) => Ok(VmValue::Str(s.derive(s.to_uppercase()))),
        _ => Err(JadeError::TypeError { message: "str.upper".to_string(), span: ZERO }),
    }
}

fn str_lower(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Str(s) => Ok(VmValue::Str(s.derive(s.to_lowercase()))),
        _ => Err(JadeError::TypeError { message: "str.lower".to_string(), span: ZERO }),
    }
}

fn str_trim(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Str(s) => Ok(VmValue::Str(s.derive(s.trim()))),
        _ => Err(JadeError::TypeError { message: "str.trim".to_string(), span: ZERO }),
    }
}

fn str_split(args: &[VmValue]) -> Result<VmValue> {
    match (&args[0], args.get(1)) {
        (VmValue::Str(s), Some(VmValue::Str(sep))) => {
            let parts: Vec<VmValue> =
                s.split(sep.as_str()).map(|p| VmValue::Str(s.derive(p))).collect();
            Ok(make_array(parts))
        }
        _ => Err(JadeError::TypeError { message: "str.split".to_string(), span: ZERO }),
    }
}

fn str_contains(args: &[VmValue]) -> Result<VmValue> {
    match (&args[0], args.get(1)) {
        (VmValue::Str(s), Some(VmValue::Str(sub))) => Ok(VmValue::Bool(s.contains(sub.as_str()))),
        _ => Err(JadeError::TypeError { message: "str.contains".to_string(), span: ZERO }),
    }
}

fn str_replace(args: &[VmValue]) -> Result<VmValue> {
    match (&args[0], args.get(1), args.get(2)) {
        (VmValue::Str(s), Some(VmValue::Str(from)), Some(VmValue::Str(to))) => {
            let trust = jade_runtime::trust::combine(s.trust(), to.trust());
            Ok(VmValue::Str(JStr::with_trust(s.replace(from.as_str(), to.as_str()), trust)))
        }
        _ => Err(JadeError::TypeError { message: "str.replace".to_string(), span: ZERO }),
    }
}

fn str_starts_with(args: &[VmValue]) -> Result<VmValue> {
    match (&args[0], args.get(1)) {
        (VmValue::Str(s), Some(VmValue::Str(prefix))) => {
            Ok(VmValue::Bool(s.starts_with(prefix.as_str())))
        }
        _ => Err(JadeError::TypeError { message: "str.starts_with".to_string(), span: ZERO }),
    }
}

fn str_ends_with(args: &[VmValue]) -> Result<VmValue> {
    match (&args[0], args.get(1)) {
        (VmValue::Str(s), Some(VmValue::Str(suffix))) => {
            Ok(VmValue::Bool(s.ends_with(suffix.as_str())))
        }
        _ => Err(JadeError::TypeError { message: "str.ends_with".to_string(), span: ZERO }),
    }
}

/// `s.encode()` — the UTF-8 octets of a string, as `bytes`.
///
/// Explicit, never implicit: a string and a blob are different types and
/// converting between them can lose information in one direction, so the
/// program says when it means to. The trust byte travels with the octets.
fn str_encode(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Str(s) => Ok(VmValue::Bytes(std::sync::Arc::new(
            jade_runtime::bytesf::BytesObj::new(s.as_bytes().to_vec(), s.trust()),
        ))),
        _ => Err(JadeError::TypeError { message: "str.encode".to_string(), span: ZERO }),
    }
}

// ── v1.3.23 additions ─────────────────────────────────────────────────────────
//
// One body each, shared by both spellings and by both engines: the core lives
// in `jade_runtime::strf`, so the interpreter and compiled code cannot drift on
// what a character index means. Every index below is a character index, because
// `len()` counts characters and `s[i]` walks them.

/// Both spellings of a string operation, defined once.
///
/// `str_x` is the method and `pkg_x` the package function, and they are the
/// same function — `src/builtins/README.md` requires both, and writing them out
/// twice by hand is how they drift.
macro_rules! str_op {
    // (self) -> Str
    ($m:ident, $p:ident, $core:ident, $name:literal, str) => {
        fn $m(args: &[VmValue]) -> Result<VmValue> {
            match &args[0] {
                VmValue::Str(s) => {
                    Ok(VmValue::Str(s.derive(jade_runtime::strf::$core(s.as_str()))))
                }
                _ => Err(JadeError::TypeError { message: $name.to_string(), span: ZERO }),
            }
        }
        fn $p(args: &[VmValue]) -> Result<VmValue> {
            $m(args)
        }
    };
    // (self, str) -> Int
    ($m:ident, $p:ident, $core:ident, $name:literal, int_of_str) => {
        fn $m(args: &[VmValue]) -> Result<VmValue> {
            match (&args[0], args.get(1)) {
                (VmValue::Str(s), Some(VmValue::Str(sub))) => {
                    Ok(VmValue::Int(jade_runtime::strf::$core(s.as_str(), sub.as_str())))
                }
                _ => Err(JadeError::TypeError { message: $name.to_string(), span: ZERO }),
            }
        }
        fn $p(args: &[VmValue]) -> Result<VmValue> {
            $m(args)
        }
    };
}

str_op!(str_trim_start, pkg_trim_start, trim_start, "str.trim_start", str);
str_op!(str_trim_end, pkg_trim_end, trim_end, "str.trim_end", str);
str_op!(str_capitalize, pkg_capitalize, capitalize, "str.capitalize", str);
str_op!(str_index_of, pkg_index_of, index_of, "str.index_of", int_of_str);
str_op!(str_last_index_of, pkg_last_index_of, last_index_of, "str.last_index_of", int_of_str);
str_op!(str_count, pkg_count, count, "str.count", int_of_str);

fn str_is_empty(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Str(s) => Ok(VmValue::Bool(s.is_empty())),
        _ => Err(JadeError::TypeError { message: "str.is_empty".to_string(), span: ZERO }),
    }
}

fn pkg_is_empty(args: &[VmValue]) -> Result<VmValue> {
    str_is_empty(args)
}

/// `s.slice(start, end)` — character indices, clamped rather than raising.
///
/// The name is shared with `bytes.slice`, which is why the compiled backend has
/// to dispatch this one on the receiver's runtime kind rather than on the method
/// name. See `emit_val_method`.
fn str_slice(args: &[VmValue]) -> Result<VmValue> {
    match (&args[0], args.get(1), args.get(2)) {
        (VmValue::Str(s), Some(VmValue::Int(a)), Some(VmValue::Int(b))) => {
            Ok(VmValue::Str(s.derive(jade_runtime::strf::slice(s.as_str(), *a, *b))))
        }
        _ => Err(JadeError::TypeError { message: "str.slice".to_string(), span: ZERO }),
    }
}

fn pkg_slice(args: &[VmValue]) -> Result<VmValue> {
    str_slice(args)
}

fn str_repeat(args: &[VmValue]) -> Result<VmValue> {
    match (&args[0], args.get(1)) {
        (VmValue::Str(s), Some(VmValue::Int(n))) => {
            Ok(VmValue::Str(s.derive(jade_runtime::strf::repeat(s.as_str(), *n))))
        }
        _ => Err(JadeError::TypeError { message: "str.repeat".to_string(), span: ZERO }),
    }
}

fn pkg_repeat(args: &[VmValue]) -> Result<VmValue> {
    str_repeat(args)
}

/// `s.pad_start(width, pad)`. Trust comes from the receiver, not the padding —
/// padding a tainted string with a literal must not launder it.
fn str_pad_start(args: &[VmValue]) -> Result<VmValue> {
    match (&args[0], args.get(1), args.get(2)) {
        (VmValue::Str(s), Some(VmValue::Int(w)), Some(VmValue::Str(pad))) => {
            Ok(VmValue::Str(s.derive(jade_runtime::strf::pad_start(s.as_str(), *w, pad.as_str()))))
        }
        _ => Err(JadeError::TypeError { message: "str.pad_start".to_string(), span: ZERO }),
    }
}

fn pkg_pad_start(args: &[VmValue]) -> Result<VmValue> {
    str_pad_start(args)
}

/// See [`str_pad_start`].
fn str_pad_end(args: &[VmValue]) -> Result<VmValue> {
    match (&args[0], args.get(1), args.get(2)) {
        (VmValue::Str(s), Some(VmValue::Int(w)), Some(VmValue::Str(pad))) => {
            Ok(VmValue::Str(s.derive(jade_runtime::strf::pad_end(s.as_str(), *w, pad.as_str()))))
        }
        _ => Err(JadeError::TypeError { message: "str.pad_end".to_string(), span: ZERO }),
    }
}

fn pkg_pad_end(args: &[VmValue]) -> Result<VmValue> {
    str_pad_end(args)
}

/// `s.lines()` — split on newlines, tolerating both line endings.
///
/// A trailing newline does not produce an empty final element, which is the
/// difference from `split("\n")` and the whole point: a file read off disk
/// almost always ends in one, and `split` gives a phantom empty line every time.
fn str_lines(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Str(s) => {
            let parts: Vec<VmValue> = s.lines().map(|l| VmValue::Str(s.derive(l))).collect();
            Ok(make_array(parts))
        }
        _ => Err(JadeError::TypeError { message: "str.lines".to_string(), span: ZERO }),
    }
}

fn pkg_lines(args: &[VmValue]) -> Result<VmValue> {
    str_lines(args)
}

pub(crate) static STR_METHODS: &[BuiltinFn] = &[
    BuiltinFn { name: "encode", vm_impl: str_encode },
    BuiltinFn { name: "len", vm_impl: str_len },
    BuiltinFn { name: "upper", vm_impl: str_upper },
    BuiltinFn { name: "lower", vm_impl: str_lower },
    BuiltinFn { name: "trim", vm_impl: str_trim },
    BuiltinFn { name: "split", vm_impl: str_split },
    BuiltinFn { name: "contains", vm_impl: str_contains },
    BuiltinFn { name: "replace", vm_impl: str_replace },
    BuiltinFn { name: "starts_with", vm_impl: str_starts_with },
    BuiltinFn { name: "ends_with", vm_impl: str_ends_with },
    BuiltinFn { name: "trim_start", vm_impl: str_trim_start },
    BuiltinFn { name: "trim_end", vm_impl: str_trim_end },
    BuiltinFn { name: "capitalize", vm_impl: str_capitalize },
    BuiltinFn { name: "index_of", vm_impl: str_index_of },
    BuiltinFn { name: "last_index_of", vm_impl: str_last_index_of },
    BuiltinFn { name: "count", vm_impl: str_count },
    BuiltinFn { name: "is_empty", vm_impl: str_is_empty },
    BuiltinFn { name: "slice", vm_impl: str_slice },
    BuiltinFn { name: "repeat", vm_impl: str_repeat },
    BuiltinFn { name: "pad_start", vm_impl: str_pad_start },
    BuiltinFn { name: "pad_end", vm_impl: str_pad_end },
    BuiltinFn { name: "lines", vm_impl: str_lines },
];

pub fn find_str_method(name: &str) -> Option<BuiltinFn> {
    STR_METHODS.iter().find(|m| m.name == name).copied()
}

// ── std/string package functions (functional style, args[0] = first arg) ─────

fn pkg_split(args: &[VmValue]) -> Result<VmValue> {
    match (&args[0], args.get(1)) {
        (VmValue::Str(s), Some(VmValue::Str(sep))) => {
            let parts: Vec<VmValue> =
                s.split(sep.as_str()).map(|p| VmValue::Str(s.derive(p))).collect();
            Ok(make_array(parts))
        }
        _ => Err(JadeError::TypeError { message: "string.split".to_string(), span: ZERO }),
    }
}

fn pkg_upper(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Str(s) => Ok(VmValue::Str(s.derive(s.to_uppercase()))),
        _ => Err(JadeError::TypeError { message: "string.upper".to_string(), span: ZERO }),
    }
}

fn pkg_lower(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Str(s) => Ok(VmValue::Str(s.derive(s.to_lowercase()))),
        _ => Err(JadeError::TypeError { message: "string.lower".to_string(), span: ZERO }),
    }
}

fn pkg_trim(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Str(s) => Ok(VmValue::Str(s.derive(s.trim()))),
        _ => Err(JadeError::TypeError { message: "string.trim".to_string(), span: ZERO }),
    }
}

fn pkg_contains(args: &[VmValue]) -> Result<VmValue> {
    match (&args[0], args.get(1)) {
        (VmValue::Str(s), Some(VmValue::Str(sub))) => Ok(VmValue::Bool(s.contains(sub.as_str()))),
        _ => Err(JadeError::TypeError { message: "string.contains".to_string(), span: ZERO }),
    }
}

fn pkg_replace(args: &[VmValue]) -> Result<VmValue> {
    match (&args[0], args.get(1), args.get(2)) {
        (VmValue::Str(s), Some(VmValue::Str(from)), Some(VmValue::Str(to))) => {
            let trust = jade_runtime::trust::combine(s.trust(), to.trust());
            Ok(VmValue::Str(JStr::with_trust(s.replace(from.as_str(), to.as_str()), trust)))
        }
        _ => Err(JadeError::TypeError { message: "string.replace".to_string(), span: ZERO }),
    }
}

fn pkg_starts_with(args: &[VmValue]) -> Result<VmValue> {
    match (&args[0], args.get(1)) {
        (VmValue::Str(s), Some(VmValue::Str(prefix))) => {
            Ok(VmValue::Bool(s.starts_with(prefix.as_str())))
        }
        _ => Err(JadeError::TypeError { message: "string.starts_with".to_string(), span: ZERO }),
    }
}

fn pkg_ends_with(args: &[VmValue]) -> Result<VmValue> {
    match (&args[0], args.get(1)) {
        (VmValue::Str(s), Some(VmValue::Str(suffix))) => {
            Ok(VmValue::Bool(s.ends_with(suffix.as_str())))
        }
        _ => Err(JadeError::TypeError { message: "string.ends_with".to_string(), span: ZERO }),
    }
}

static STRING_PKG_FNS: &[BuiltinFn] = &[
    BuiltinFn { name: "split", vm_impl: pkg_split },
    BuiltinFn { name: "upper", vm_impl: pkg_upper },
    BuiltinFn { name: "lower", vm_impl: pkg_lower },
    BuiltinFn { name: "trim", vm_impl: pkg_trim },
    BuiltinFn { name: "contains", vm_impl: pkg_contains },
    BuiltinFn { name: "replace", vm_impl: pkg_replace },
    BuiltinFn { name: "starts_with", vm_impl: pkg_starts_with },
    BuiltinFn { name: "ends_with", vm_impl: pkg_ends_with },
    BuiltinFn { name: "trim_start", vm_impl: pkg_trim_start },
    BuiltinFn { name: "trim_end", vm_impl: pkg_trim_end },
    BuiltinFn { name: "capitalize", vm_impl: pkg_capitalize },
    BuiltinFn { name: "index_of", vm_impl: pkg_index_of },
    BuiltinFn { name: "last_index_of", vm_impl: pkg_last_index_of },
    BuiltinFn { name: "count", vm_impl: pkg_count },
    BuiltinFn { name: "is_empty", vm_impl: pkg_is_empty },
    BuiltinFn { name: "slice", vm_impl: pkg_slice },
    BuiltinFn { name: "repeat", vm_impl: pkg_repeat },
    BuiltinFn { name: "pad_start", vm_impl: pkg_pad_start },
    BuiltinFn { name: "pad_end", vm_impl: pkg_pad_end },
    BuiltinFn { name: "lines", vm_impl: pkg_lines },
];

fn register_string_pkg_types(ctx: &mut TypeContext) {
    ctx.define("string".to_string(), JadeType::Unknown);
}

pub static STRING_PKG: Package = Package {
    import_name: "std/string",
    global_name: "string",
    fns: STRING_PKG_FNS,
    natives: &[],
    register_types: register_string_pkg_types,
};

// ── Type checker primitive method registration ────────────────────────────────

pub fn register_str_method_types(ctx: &mut TypeContext) {
    let ret_str = || Box::new(JadeType::Str);
    let ret_int = || Box::new(JadeType::Int);
    let ret_bool = || Box::new(JadeType::Bool);
    let ret_arr = || Box::new(JadeType::Array(Box::new(JadeType::Str)));

    let methods: &[(&str, JadeType)] = &[
        ("len", JadeType::Fn { params: vec![], ret: ret_int() }),
        ("upper", JadeType::Fn { params: vec![], ret: ret_str() }),
        ("lower", JadeType::Fn { params: vec![], ret: ret_str() }),
        ("trim", JadeType::Fn { params: vec![], ret: ret_str() }),
        ("split", JadeType::Fn { params: vec![JadeType::Str], ret: ret_arr() }),
        ("contains", JadeType::Fn { params: vec![JadeType::Str], ret: ret_bool() }),
        ("replace", JadeType::Fn { params: vec![JadeType::Str, JadeType::Str], ret: ret_str() }),
        ("starts_with", JadeType::Fn { params: vec![JadeType::Str], ret: ret_bool() }),
        ("ends_with", JadeType::Fn { params: vec![JadeType::Str], ret: ret_bool() }),
        ("encode", JadeType::Fn { params: vec![], ret: Box::new(JadeType::Bytes) }),
        // v1.3.23. The return type drives print and format lowering, so an
        // omission here shows up as a formatting divergence between the engines
        // rather than as a rejection.
        ("trim_start", JadeType::Fn { params: vec![], ret: ret_str() }),
        ("trim_end", JadeType::Fn { params: vec![], ret: ret_str() }),
        ("capitalize", JadeType::Fn { params: vec![], ret: ret_str() }),
        ("is_empty", JadeType::Fn { params: vec![], ret: ret_bool() }),
        ("index_of", JadeType::Fn { params: vec![JadeType::Str], ret: ret_int() }),
        ("last_index_of", JadeType::Fn { params: vec![JadeType::Str], ret: ret_int() }),
        ("count", JadeType::Fn { params: vec![JadeType::Str], ret: ret_int() }),
        ("repeat", JadeType::Fn { params: vec![JadeType::Int], ret: ret_str() }),
        ("slice", JadeType::Fn { params: vec![JadeType::Int, JadeType::Int], ret: ret_str() }),
        ("pad_start", JadeType::Fn { params: vec![JadeType::Int, JadeType::Str], ret: ret_str() }),
        ("pad_end", JadeType::Fn { params: vec![JadeType::Int, JadeType::Str], ret: ret_str() }),
        ("lines", JadeType::Fn { params: vec![], ret: ret_arr() }),
    ];
    for (name, ty) in methods {
        ctx.define_primitive_method("str", name, ty.clone());
    }
}
