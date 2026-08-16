use super::*;
use crate::builtins::make_array;

fn arr(vals: Vec<VmValue>) -> VmValue {
    make_array(vals)
}

fn ints(v: &VmValue) -> Vec<i64> {
    match v {
        VmValue::Array(arc) => arc
            .lock()
            .iter()
            .map(|e| match e {
                VmValue::Int(i) => *i,
                _ => panic!("not an int"),
            })
            .collect(),
        _ => panic!("not an array"),
    }
}

// ── arr_len ───────────────────────────────────────────────────────────────────

#[test]
fn len_empty() {
    let a = arr(vec![]);
    assert!(matches!(arr_len(&[a]), Ok(VmValue::Int(0))));
}

#[test]
fn len_nonempty() {
    let a = arr(vec![VmValue::Int(1), VmValue::Int(2), VmValue::Int(3)]);
    assert!(matches!(arr_len(&[a]), Ok(VmValue::Int(3))));
}

#[test]
fn len_wrong_type() {
    assert!(matches!(arr_len(&[VmValue::Int(1)]), Err(JadeError::TypeError { .. })));
}

// ── arr_push ──────────────────────────────────────────────────────────────────

#[test]
fn push_appends_and_returns_nil() {
    let a = arr(vec![VmValue::Int(1)]);
    let r = arr_push(&[a.clone(), VmValue::Int(2)]).unwrap();
    assert!(matches!(r, VmValue::Nil));
    assert_eq!(ints(&a), vec![1, 2]);
}

#[test]
fn push_reference_semantics() {
    // aliases share the same underlying vec
    let a = arr(vec![]);
    let alias = a.clone();
    arr_push(&[a.clone(), VmValue::Int(42)]).unwrap();
    assert_eq!(ints(&alias), vec![42]);
}

#[test]
fn push_missing_arg() {
    let a = arr(vec![]);
    assert!(matches!(arr_push(&[a]), Err(JadeError::TypeError { .. })));
}

#[test]
fn push_wrong_receiver() {
    assert!(matches!(
        arr_push(&[VmValue::Int(1), VmValue::Int(2)]),
        Err(JadeError::TypeError { .. })
    ));
}

// ── arr_pop ───────────────────────────────────────────────────────────────────

#[test]
fn pop_returns_last() {
    let a = arr(vec![VmValue::Int(1), VmValue::Int(9)]);
    assert!(matches!(arr_pop(std::slice::from_ref(&a)), Ok(VmValue::Int(9))));
    assert_eq!(ints(&a), vec![1]);
}

#[test]
fn pop_empty_returns_nil() {
    let a = arr(vec![]);
    assert!(matches!(arr_pop(&[a]), Ok(VmValue::Nil)));
}

#[test]
fn pop_wrong_type() {
    assert!(matches!(arr_pop(&[VmValue::Str("x".into())]), Err(JadeError::TypeError { .. })));
}

// ── arr_contains ──────────────────────────────────────────────────────────────

#[test]
fn contains_found() {
    let a = arr(vec![VmValue::Int(1), VmValue::Str("hi".into())]);
    assert!(matches!(
        arr_contains(&[a.clone(), VmValue::Str("hi".into())]),
        Ok(VmValue::Bool(true))
    ));
}

#[test]
fn contains_not_found() {
    let a = arr(vec![VmValue::Int(1)]);
    assert!(matches!(arr_contains(&[a, VmValue::Int(2)]), Ok(VmValue::Bool(false))));
}

#[test]
fn contains_type_mismatch_is_false() {
    // 1 (int) vs 1.0 (float) are not equal in vm_values_equal
    let a = arr(vec![VmValue::Int(1)]);
    assert!(matches!(arr_contains(&[a, VmValue::Float(1.0)]), Ok(VmValue::Bool(false))));
}

#[test]
fn contains_missing_needle() {
    let a = arr(vec![]);
    assert!(matches!(arr_contains(&[a]), Err(JadeError::TypeError { .. })));
}

// ── arr_sort (in place) ───────────────────────────────────────────────────────

#[test]
fn sort_ints_in_place() {
    let a = arr(vec![VmValue::Int(3), VmValue::Int(1), VmValue::Int(2)]);
    let r = arr_sort(std::slice::from_ref(&a)).unwrap();
    assert!(matches!(r, VmValue::Nil));
    assert_eq!(ints(&a), vec![1, 2, 3]);
}

#[test]
fn sort_wrong_type() {
    assert!(matches!(arr_sort(&[VmValue::Nil]), Err(JadeError::TypeError { .. })));
}

// ── arr_reverse (in place) ────────────────────────────────────────────────────

#[test]
fn reverse_in_place() {
    let a = arr(vec![VmValue::Int(1), VmValue::Int(2), VmValue::Int(3)]);
    let r = arr_reverse(std::slice::from_ref(&a)).unwrap();
    assert!(matches!(r, VmValue::Nil));
    assert_eq!(ints(&a), vec![3, 2, 1]);
}

#[test]
fn reverse_wrong_type() {
    assert!(matches!(arr_reverse(&[VmValue::Int(1)]), Err(JadeError::TypeError { .. })));
}

// ── pkg_sort / pkg_reverse (return a new array, leave source untouched) ────────

#[test]
fn pkg_sort_returns_new_sorted() {
    let a = arr(vec![VmValue::Int(2), VmValue::Int(1)]);
    let sorted = pkg_sort(std::slice::from_ref(&a)).unwrap();
    assert_eq!(ints(&sorted), vec![1, 2]);
    // original unchanged
    assert_eq!(ints(&a), vec![2, 1]);
}

#[test]
fn pkg_reverse_returns_new() {
    let a = arr(vec![VmValue::Int(1), VmValue::Int(2)]);
    let rev = pkg_reverse(std::slice::from_ref(&a)).unwrap();
    assert_eq!(ints(&rev), vec![2, 1]);
    assert_eq!(ints(&a), vec![1, 2]);
}

#[test]
fn pkg_sort_wrong_type() {
    assert!(matches!(pkg_sort(&[VmValue::Int(1)]), Err(JadeError::TypeError { .. })));
}

// ── sort helper: mixed int/float ordering ─────────────────────────────────────

#[test]
fn sort_mixed_numeric() {
    let a = arr(vec![VmValue::Float(2.5), VmValue::Int(1), VmValue::Int(3)]);
    arr_sort(std::slice::from_ref(&a)).unwrap();
    let guard = match &a {
        VmValue::Array(arc) => arc.lock().clone(),
        _ => unreachable!(),
    };
    // 1, 2.5, 3
    assert!(matches!(guard[0], VmValue::Int(1)));
    assert!(matches!(guard[1], VmValue::Float(f) if f == 2.5));
    assert!(matches!(guard[2], VmValue::Int(3)));
}

// ── method lookup ─────────────────────────────────────────────────────────────

#[test]
fn find_method_known() {
    assert!(find_array_method("push").is_some());
    assert!(find_array_method("sort").is_some());
}

#[test]
fn find_method_unknown() {
    assert!(find_array_method("nope").is_none());
}

// ── Both spellings ────────────────────────────────────────────────────────────

/// `map`/`filter` are the only array functions whose implementation sits behind
/// a `NativeFnId` rather than a `BuiltinFn`, because each runs a Jade function
/// per element. That is why they were the only two with no method spelling
/// until v1.3.21 — the primitive-method path could reach pure builtins only.
#[test]
fn map_and_filter_are_reachable_as_methods() {
    use crate::builtins::PrimType;
    for name in ["map", "filter"] {
        assert!(
            crate::builtins::find_primitive_method(PrimType::Array, name).is_none(),
            "{name} is not a pure BuiltinFn — if it becomes one, drop the bound-native path"
        );
    }
}

/// The type checker has to know the method spelling too, or `a.map(f)` is a
/// call on a type with no such method.
#[test]
fn the_method_spelling_is_registered_for_type_inference() {
    let mut ctx = crate::compiler::type_infer::TypeContext::new();
    register_array_method_types(&mut ctx);
    for name in ["map", "filter", "sort", "push", "len"] {
        assert!(
            ctx.primitive_methods.get("array").is_some_and(|m| m.contains_key(name)),
            "array method '{name}' is not registered"
        );
    }
}

/// The one pair that is deliberately *not* the same function. `std/array`'s
/// package entries are the functional style, so `array.sort(a)` answers with a
/// sorted copy while `a.sort()` sorts in place. Lowering the package form to
/// the in-place symbol would have made a compiled program mutate an array the
/// interpreter leaves alone.
#[test]
fn the_package_sort_copies_and_the_method_sorts_in_place() {
    let arr = make_array(vec![VmValue::Int(3), VmValue::Int(1)]);

    let sorted = pkg_sort(&[arr.clone()]).expect("array.sort");
    let VmValue::Array(out) = &sorted else { panic!("expected an array") };
    assert_eq!(out.lock().len(), 2);
    let VmValue::Array(src) = &arr else { panic!() };
    assert!(
        matches!(src.lock().as_slice()[0], VmValue::Int(3)),
        "array.sort(a) must leave its argument alone"
    );

    arr_sort(&[arr.clone()]).expect("a.sort()");
    assert!(matches!(src.lock().as_slice()[0], VmValue::Int(1)), "a.sort() must sort in place");
}
