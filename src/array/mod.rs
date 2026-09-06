#[cfg(test)]
mod tests;

use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext},
    frontend::error::{JadeError, Result, Span},
    vm::{NativeFnId, VmValue},
};

use crate::builtins::{BuiltinFn, Package, make_array};

const ZERO: Span = Span { line: 0, col: 0 };

// ── Primitive array methods (args[0] = self as Array arc) ─────────────────────

fn arr_len(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Array(arc) => Ok(VmValue::Int(arc.lock().len() as i64)),
        _ => Err(JadeError::TypeError { message: "array.len".to_string(), span: ZERO }),
    }
}

fn arr_push(args: &[VmValue]) -> Result<VmValue> {
    match (&args[0], args.get(1)) {
        (VmValue::Array(arc), Some(val)) => {
            arc.lock().push(val.clone());
            Ok(VmValue::Nil)
        }
        _ => Err(JadeError::TypeError { message: "array.push".to_string(), span: ZERO }),
    }
}

fn arr_pop(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Array(arc) => {
            let v = arc.lock().pop().unwrap_or(VmValue::Nil);
            Ok(v)
        }
        _ => Err(JadeError::TypeError { message: "array.pop".to_string(), span: ZERO }),
    }
}

fn arr_contains(args: &[VmValue]) -> Result<VmValue> {
    match (&args[0], args.get(1)) {
        (VmValue::Array(arc), Some(needle)) => {
            let guard = arc.lock();
            let found = guard.iter().any(|elem| vm_values_equal(elem, needle));
            Ok(VmValue::Bool(found))
        }
        _ => Err(JadeError::TypeError { message: "array.contains".to_string(), span: ZERO }),
    }
}

fn arr_sort(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Array(arc) => {
            let mut guard = arc.lock();
            guard.sort_by(vm_cmp_for_sort);
            Ok(VmValue::Nil)
        }
        _ => Err(JadeError::TypeError { message: "array.sort".to_string(), span: ZERO }),
    }
}

fn arr_reverse(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Array(arc) => {
            arc.lock().reverse();
            Ok(VmValue::Nil)
        }
        _ => Err(JadeError::TypeError { message: "array.reverse".to_string(), span: ZERO }),
    }
}

/// `a.join(sep)` — the elements rendered and joined.
///
/// Non-string elements render the way `print` renders them, so
/// `[1, 2].join("-")` is `"1-2"` rather than an error. Trust is the union of
/// every part's: joining a tainted string into a literal separator must not
/// launder it.
///
/// It lives on `std::array` rather than `std::string` because a package
/// function's first argument is the type the package is named for — the rule
/// the codegen bridge between the two spellings depends on.
fn arr_join(args: &[VmValue]) -> Result<VmValue> {
    match (&args[0], args.get(1)) {
        (VmValue::Array(arc), Some(VmValue::Str(sep))) => {
            // Copied out of the lock before rendering, for the reason
            // `value_to_display` documents: an array that contains itself made
            // this re-lock a mutex the thread already held, and `a.join(",")`
            // hung instead of answering.
            let items: Vec<VmValue> = arc.lock().iter().cloned().collect();
            let mut trust = sep.trust();
            let mut parts: Vec<String> = Vec::with_capacity(items.len());
            for v in items.iter() {
                if let VmValue::Str(s) = v {
                    trust = jade_runtime::trust::combine(trust, s.trust());
                }
                parts.push(crate::vm::value_to_display(v));
            }
            let joined = parts.join(sep.as_str());
            Ok(VmValue::Str(match jade_runtime::trust::is_tainted(trust) {
                true => jade_runtime::trust::JStr::tainted(joined),
                false => jade_runtime::trust::JStr::trusted(joined),
            }))
        }
        _ => Err(JadeError::TypeError { message: "array.join".to_string(), span: ZERO }),
    }
}

fn pkg_join(args: &[VmValue]) -> Result<VmValue> {
    crate::builtins::check_pkg_arity("join", args)?;
    arr_join(args)
}

pub(crate) static ARRAY_METHODS: &[BuiltinFn] = &[
    BuiltinFn { name: "len", vm_impl: arr_len },
    BuiltinFn { name: "push", vm_impl: arr_push },
    BuiltinFn { name: "pop", vm_impl: arr_pop },
    BuiltinFn { name: "contains", vm_impl: arr_contains },
    BuiltinFn { name: "sort", vm_impl: arr_sort },
    BuiltinFn { name: "reverse", vm_impl: arr_reverse },
    BuiltinFn { name: "join", vm_impl: arr_join },
];

pub fn find_array_method(name: &str) -> Option<BuiltinFn> {
    ARRAY_METHODS.iter().find(|m| m.name == name).copied()
}

// ── std/array package functions (functional style) ────────────────────────────

// array.map / array.filter call a user function per element, which needs the VM
// call context (VmState + async). They are dispatched through
// NativeFnId::ArrayMap / ArrayFilter, listed in ARRAY_PKG_NATIVES, which shadow
// these entries in the package dict. These BuiltinFn stubs exist only so the
// package table still lists the names — they are never invoked.
fn pkg_map(_args: &[VmValue]) -> Result<VmValue> {
    unreachable!("array.map is handled by NativeFnId::ArrayMap dispatch in the VM")
}

fn pkg_filter(_args: &[VmValue]) -> Result<VmValue> {
    unreachable!("array.filter is handled by NativeFnId::ArrayFilter dispatch in the VM")
}

fn pkg_sort(args: &[VmValue]) -> Result<VmValue> {
    crate::builtins::check_pkg_arity("sort", args)?;
    match &args[0] {
        VmValue::Array(arc) => {
            let guard = arc.lock();
            let mut v: Vec<VmValue> = guard.as_slice().to_vec();
            drop(guard);
            v.sort_by(vm_cmp_for_sort);
            Ok(make_array(v))
        }
        _ => Err(JadeError::TypeError { message: "array.sort".to_string(), span: ZERO }),
    }
}

fn pkg_reverse(args: &[VmValue]) -> Result<VmValue> {
    crate::builtins::check_pkg_arity("reverse", args)?;
    match &args[0] {
        VmValue::Array(arc) => {
            let guard = arc.lock();
            let mut v: Vec<VmValue> = guard.as_slice().to_vec();
            drop(guard);
            v.reverse();
            Ok(make_array(v))
        }
        _ => Err(JadeError::TypeError { message: "array.reverse".to_string(), span: ZERO }),
    }
}

static ARRAY_PKG_FNS: &[BuiltinFn] = &[
    BuiltinFn { name: "map", vm_impl: pkg_map },
    BuiltinFn { name: "filter", vm_impl: pkg_filter },
    BuiltinFn { name: "sort", vm_impl: pkg_sort },
    BuiltinFn { name: "reverse", vm_impl: pkg_reverse },
    BuiltinFn { name: "join", vm_impl: pkg_join },
];

fn register_array_pkg_types(ctx: &mut TypeContext) {
    ctx.define("array".to_string(), JadeType::Unknown);
}

/// `map`/`filter` run a Jade function per element, so they dispatch through the
/// VM; the pure entries in `ARRAY_PKG_FNS` are shadowed by these (natives are
/// inserted last). `sort`/`reverse` need no VM state and stay pure.
static ARRAY_PKG_NATIVES: &[(&str, NativeFnId)] =
    &[("map", NativeFnId::ArrayMap), ("filter", NativeFnId::ArrayFilter)];

pub static ARRAY_PKG: Package = Package {
    import_name: "std/array",
    global_name: "array",
    fns: ARRAY_PKG_FNS,
    natives: ARRAY_PKG_NATIVES,
    register_types: register_array_pkg_types,
};

// ── Type checker primitive method registration ────────────────────────────────

pub fn register_array_method_types(ctx: &mut TypeContext) {
    let unk = JadeType::Unknown;
    let methods: &[(&str, JadeType)] = &[
        ("len", JadeType::Fn { params: vec![], ret: Box::new(JadeType::Int) }),
        ("push", JadeType::Fn { params: vec![unk.clone()], ret: Box::new(JadeType::Nil) }),
        ("pop", JadeType::Fn { params: vec![], ret: Box::new(JadeType::Unknown) }),
        ("contains", JadeType::Fn { params: vec![unk.clone()], ret: Box::new(JadeType::Bool) }),
        ("sort", JadeType::Fn { params: vec![], ret: Box::new(JadeType::Nil) }),
        ("reverse", JadeType::Fn { params: vec![], ret: Box::new(JadeType::Nil) }),
        // The method spelling of `array.map` / `array.filter`. Unlike the rest
        // of this table these are not `BuiltinFn`s — they run a Jade function
        // per element, so the VM binds the receiver to their `NativeFnId`
        // instead (see `array_fn_method`). They were the only array functions
        // with no method spelling until v1.3.21.
        (
            "map",
            JadeType::Fn {
                params: vec![unk.clone()],
                ret: Box::new(JadeType::Array(Box::new(JadeType::Unknown))),
            },
        ),
        (
            "filter",
            JadeType::Fn {
                params: vec![unk.clone()],
                ret: Box::new(JadeType::Array(Box::new(JadeType::Unknown))),
            },
        ),
    ];
    for (name, ty) in methods {
        ctx.define_primitive_method("array", name, ty.clone());
    }
}

// ── Comparison helper for sort ────────────────────────────────────────────────

fn vm_values_equal(a: &VmValue, b: &VmValue) -> bool {
    match (a, b) {
        (VmValue::Int(x), VmValue::Int(y)) => x == y,
        (VmValue::Float(x), VmValue::Float(y)) => x == y,
        (VmValue::Bool(x), VmValue::Bool(y)) => x == y,
        (VmValue::Str(x), VmValue::Str(y)) => x == y,
        (VmValue::Nil, VmValue::Nil) => true,
        _ => false,
    }
}

fn vm_cmp_for_sort(a: &VmValue, b: &VmValue) -> std::cmp::Ordering {
    match (a, b) {
        (VmValue::Int(x), VmValue::Int(y)) => x.cmp(y),
        (VmValue::Float(x), VmValue::Float(y)) => {
            x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal)
        }
        (VmValue::Int(x), VmValue::Float(y)) => {
            (*x as f64).partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal)
        }
        (VmValue::Float(x), VmValue::Int(y)) => {
            x.partial_cmp(&(*y as f64)).unwrap_or(std::cmp::Ordering::Equal)
        }
        (VmValue::Str(x), VmValue::Str(y)) => x.cmp(y),
        (VmValue::Bool(x), VmValue::Bool(y)) => x.cmp(y),
        _ => std::cmp::Ordering::Equal,
    }
}
