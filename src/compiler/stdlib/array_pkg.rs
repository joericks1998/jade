use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext, vm::VmValue},
    frontend::error::{JadeError, Result, Span},
};

use super::{BuiltinFn, Package, make_array};

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
            guard.sort_by(|a, b| vm_cmp_for_sort(a, b));
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

static ARRAY_METHODS: &[BuiltinFn] = &[
    BuiltinFn { name: "len",      vm_impl: arr_len },
    BuiltinFn { name: "push",     vm_impl: arr_push },
    BuiltinFn { name: "pop",      vm_impl: arr_pop },
    BuiltinFn { name: "contains", vm_impl: arr_contains },
    BuiltinFn { name: "sort",     vm_impl: arr_sort },
    BuiltinFn { name: "reverse",  vm_impl: arr_reverse },
];

pub fn find_array_method(name: &str) -> Option<BuiltinFn> {
    ARRAY_METHODS.iter().find(|m| m.name == name).copied()
}

// ── std/array package functions (functional style) ────────────────────────────

fn pkg_map(_args: &[VmValue]) -> Result<VmValue> {
    // map(arr, f) is async-capable but we need the VM call context.
    // For now, return an error explaining this must be done with a for-loop.
    // Full async-capable map would require VmState access.
    Err(JadeError::TypeError {
        message: "array.map: use a for-loop to transform arrays".to_string(),
        span: ZERO,
    })
}

fn pkg_filter(_args: &[VmValue]) -> Result<VmValue> {
    Err(JadeError::TypeError {
        message: "array.filter: use a for-loop to filter arrays".to_string(),
        span: ZERO,
    })
}

fn pkg_sort(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Array(arc) => {
            let guard = arc.lock();
            let mut v: Vec<VmValue> = guard.clone();
            drop(guard);
            v.sort_by(|a, b| vm_cmp_for_sort(a, b));
            Ok(make_array(v))
        }
        _ => Err(JadeError::TypeError { message: "array.sort".to_string(), span: ZERO }),
    }
}

fn pkg_reverse(args: &[VmValue]) -> Result<VmValue> {
    match &args[0] {
        VmValue::Array(arc) => {
            let guard = arc.lock();
            let mut v: Vec<VmValue> = guard.clone();
            drop(guard);
            v.reverse();
            Ok(make_array(v))
        }
        _ => Err(JadeError::TypeError { message: "array.reverse".to_string(), span: ZERO }),
    }
}

static ARRAY_PKG_FNS: &[BuiltinFn] = &[
    BuiltinFn { name: "map",     vm_impl: pkg_map },
    BuiltinFn { name: "filter",  vm_impl: pkg_filter },
    BuiltinFn { name: "sort",    vm_impl: pkg_sort },
    BuiltinFn { name: "reverse", vm_impl: pkg_reverse },
];

fn register_array_pkg_types(ctx: &mut TypeContext) {
    ctx.define("array".to_string(), JadeType::Unknown);
}

pub static ARRAY_PKG: Package = Package {
    import_name: "std/array",
    global_name: "array",
    fns: ARRAY_PKG_FNS,
    register_types: register_array_pkg_types,
};

// ── Type checker primitive method registration ────────────────────────────────

pub fn register_array_method_types(ctx: &mut TypeContext) {
    let unk = JadeType::Unknown;
    let methods: &[(&str, JadeType)] = &[
        ("len",      JadeType::Fn { params: vec![], ret: Box::new(JadeType::Int) }),
        ("push",     JadeType::Fn { params: vec![unk.clone()], ret: Box::new(JadeType::Nil) }),
        ("pop",      JadeType::Fn { params: vec![], ret: Box::new(JadeType::Unknown) }),
        ("contains", JadeType::Fn { params: vec![unk.clone()], ret: Box::new(JadeType::Bool) }),
        ("sort",     JadeType::Fn { params: vec![], ret: Box::new(JadeType::Nil) }),
        ("reverse",  JadeType::Fn { params: vec![], ret: Box::new(JadeType::Nil) }),
    ];
    for (name, ty) in methods {
        ctx.define_primitive_method("array", name, ty.clone());
    }
}

// ── Comparison helper for sort ────────────────────────────────────────────────

fn vm_values_equal(a: &VmValue, b: &VmValue) -> bool {
    match (a, b) {
        (VmValue::Int(x), VmValue::Int(y))     => x == y,
        (VmValue::Float(x), VmValue::Float(y)) => x == y,
        (VmValue::Bool(x), VmValue::Bool(y))   => x == y,
        (VmValue::Str(x), VmValue::Str(y))     => x == y,
        (VmValue::Nil, VmValue::Nil)           => true,
        _ => false,
    }
}

fn vm_cmp_for_sort(a: &VmValue, b: &VmValue) -> std::cmp::Ordering {
    match (a, b) {
        (VmValue::Int(x), VmValue::Int(y))         => x.cmp(y),
        (VmValue::Float(x), VmValue::Float(y))     => x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal),
        (VmValue::Int(x), VmValue::Float(y))       => (*x as f64).partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal),
        (VmValue::Float(x), VmValue::Int(y))       => x.partial_cmp(&(*y as f64)).unwrap_or(std::cmp::Ordering::Equal),
        (VmValue::Str(x), VmValue::Str(y))         => x.cmp(y),
        (VmValue::Bool(x), VmValue::Bool(y))       => x.cmp(y),
        _ => std::cmp::Ordering::Equal,
    }
}
