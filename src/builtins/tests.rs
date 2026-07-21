//! Tests for the built-in registry: `seed_globals`, package lookup APIs,
//! primitive-method lookup, the `PACKAGES`/`CORE_BUILTINS` tables, and the
//! `PrimType` / `Package` helpers.

use jade_runtime::coll::DictObj;
use super::*;
use crate::vm::VmValue;

// ── PrimType ──────────────────────────────────────────────────────────────────

#[test]
fn prim_type_from_value_covers_all_primitives() {
    assert_eq!(PrimType::from_value(&VmValue::Str("x".into())), Some(PrimType::Str));
    assert_eq!(PrimType::from_value(&VmValue::Int(1)), Some(PrimType::Int));
    assert_eq!(PrimType::from_value(&VmValue::Float(1.0)), Some(PrimType::Float));
    assert_eq!(
        PrimType::from_value(&make_array(vec![VmValue::Int(1)])),
        Some(PrimType::Array)
    );
    assert_eq!(
        PrimType::from_value(&VmValue::Dict(DictObj::new())),
        Some(PrimType::Dict)
    );
}

#[test]
fn prim_type_from_value_none_for_non_primitive() {
    assert_eq!(PrimType::from_value(&VmValue::Nil), None);
    assert_eq!(PrimType::from_value(&VmValue::Bool(true)), None);
}

#[test]
fn prim_type_type_name() {
    assert_eq!(PrimType::Str.type_name(), "str");
    assert_eq!(PrimType::Array.type_name(), "array");
    assert_eq!(PrimType::Dict.type_name(), "dict");
    assert_eq!(PrimType::Int.type_name(), "int");
    assert_eq!(PrimType::Float.type_name(), "float");
}

// ── seed_globals ──────────────────────────────────────────────────────────────

fn seeded() -> HashMap<String, VmValue> {
    let mut g = HashMap::new();
    seed_globals(&mut g);
    g
}

#[test]
fn seed_globals_registers_core_builtins() {
    let g = seeded();
    // CORE_BUILTINS: write, len, input as pure BuiltinFn.
    for name in ["write", "len", "input"] {
        match g.get(name) {
            Some(VmValue::BuiltinFn(f)) => assert_eq!(f.name, name),
            other => panic!("expected BuiltinFn for {name}, got {other:?}"),
        }
    }
}

#[test]
fn seed_globals_registers_native_dispatched_globals() {
    let g = seeded();
    // print / stream / route dispatch through NativeFnId, not BuiltinFn.
    for name in ["print", "stream", "route"] {
        match g.get(name) {
            Some(VmValue::NativeFn(_)) => {}
            other => panic!("expected NativeFn for {name}, got {other:?}"),
        }
    }
}

#[test]
fn seed_globals_registers_type_constructors() {
    let g = seeded();
    for name in ["int", "float", "bool", "str", "func"] {
        match g.get(name) {
            Some(VmValue::TypeRef(t)) => assert_eq!(t, name),
            other => panic!("expected TypeRef for {name}, got {other:?}"),
        }
    }
}

#[test]
fn seed_globals_registers_grammar_global() {
    let g = seeded();
    match g.get("Grammar") {
        Some(VmValue::Dict(fields)) => {
            assert!(fields.contains_key("new"), "Grammar dict should have `new`");
        }
        other => panic!("expected Dict for Grammar, got {other:?}"),
    }
}

#[test]
fn seed_globals_does_not_register_arbitrary_names() {
    let g = seeded();
    assert!(g.get("no_such_builtin").is_none());
}

// ── Package lookup ────────────────────────────────────────────────────────────

#[test]
fn find_package_by_import_path() {
    // Every registered package resolves by its `use` import name.
    let expected = [
        ("llm", "llm"),
        ("std/string", "string"),
        ("std/math", "math"),
        ("std/array", "array"),
        ("std/dict", "dict"),
        ("std/fs", "fs"),
        ("std/time", "time"),
        ("std/http", "http"),
        ("std/sh", "sh"),
        ("std/json", "json"),
        ("std/env", "env"),
        ("std/path", "path"),
        ("std/random", "random"),
    ];
    for (import, global) in expected {
        let pkg = find_package(import).unwrap_or_else(|| panic!("missing package {import}"));
        assert_eq!(pkg.import_name, import);
        assert_eq!(pkg.global_name, global);
    }
}

#[test]
fn find_package_uhttp_present_on_unix() {
    let pkg = find_package("std/uhttp").expect("uhttp should be registered on unix");
    assert_eq!(pkg.global_name, "uhttp");
}

#[test]
fn find_package_unknown_returns_none() {
    assert!(find_package("std/nonexistent").is_none());
    assert!(find_package("string").is_none()); // global name, not import name
}

#[test]
fn is_package_global_name_matches_globals() {
    assert!(is_package_global_name("string"));
    assert!(is_package_global_name("math"));
    assert!(is_package_global_name("json"));
    assert!(is_package_global_name("llm"));
    assert!(!is_package_global_name("std/string")); // that's the import name
    assert!(!is_package_global_name("not_a_package"));
}

#[test]
fn package_vm_dict_value_exposes_fns() {
    let pkg = find_package("std/math").expect("math package");
    match pkg.vm_dict_value() {
        VmValue::Dict(map) => {
            assert!(!map.is_empty(), "math package should expose functions");
            // Each entry must be a BuiltinFn.
            for (name, v) in map.iter() {
                match v {
                    VmValue::BuiltinFn(f) => assert_eq!(&f.name, name),
                    other => panic!("expected BuiltinFn for {name}, got {other:?}"),
                }
            }
        }
        other => panic!("expected Dict, got {other:?}"),
    }
}

// ── Primitive method lookup ───────────────────────────────────────────────────

#[test]
fn find_primitive_method_str() {
    for m in ["len", "upper", "lower", "trim", "split", "contains", "replace"] {
        let f = find_primitive_method(PrimType::Str, m);
        assert!(f.is_some(), "str.{m} should resolve");
        assert_eq!(f.unwrap().name, m);
    }
}

#[test]
fn find_primitive_method_array() {
    for m in ["len", "push", "pop", "contains", "sort", "reverse"] {
        let f = find_primitive_method(PrimType::Array, m);
        assert!(f.is_some(), "array.{m} should resolve");
    }
}

#[test]
fn find_primitive_method_dict() {
    for m in ["len", "keys", "values", "has", "get"] {
        let f = find_primitive_method(PrimType::Dict, m);
        assert!(f.is_some(), "dict.{m} should resolve");
    }
}

#[test]
fn find_primitive_method_int_and_float_have_none() {
    assert!(find_primitive_method(PrimType::Int, "len").is_none());
    assert!(find_primitive_method(PrimType::Float, "sqrt").is_none());
}

#[test]
fn find_primitive_method_unknown_returns_none() {
    assert!(find_primitive_method(PrimType::Str, "no_such_method").is_none());
    assert!(find_primitive_method(PrimType::Array, "flatten").is_none());
}

// ── NativeBoundMethod / BuiltinFn helpers ─────────────────────────────────────

#[test]
fn native_bound_method_captures_receiver_and_method() {
    let method = find_primitive_method(PrimType::Str, "upper").unwrap();
    let bound = NativeBoundMethod {
        receiver: VmValue::Str("hi".into()),
        method,
    };
    assert_eq!(bound.method.name, "upper");
    match &bound.receiver {
        VmValue::Str(s) => assert_eq!(s, "hi"),
        other => panic!("expected Str receiver, got {other:?}"),
    }
    // The captured method actually runs: receiver is prepended as args[0].
    let out = (bound.method.vm_impl)(&[bound.receiver.clone()]).unwrap();
    match out {
        VmValue::Str(s) => assert_eq!(s, "HI"),
        other => panic!("expected uppercased Str, got {other:?}"),
    }
}

#[test]
fn builtin_fn_debug_format() {
    let f = find_primitive_method(PrimType::Str, "trim").unwrap();
    assert_eq!(format!("{f:?}"), "<builtin trim>");
}

// ── make_array helper ─────────────────────────────────────────────────────────

#[test]
fn make_array_wraps_vec() {
    let v = make_array(vec![VmValue::Int(1), VmValue::Int(2)]);
    match v {
        VmValue::Array(arc) => {
            let guard = arc.lock();
            assert_eq!(guard.len(), 2);
        }
        other => panic!("expected Array, got {other:?}"),
    }
}
