use super::*;
use crate::builtins::make_array;
use std::sync::Mutex;

// The RNG is a process-global. Seed-determinism tests must not run concurrently
// with one another (a reseed in one would perturb another). Serialize them.
static TEST_LOCK: Mutex<()> = Mutex::new(());

fn seed(n: i64) {
    random_seed(&[VmValue::Int(n)]).unwrap();
}

fn int_val(v: VmValue) -> i64 {
    match v {
        VmValue::Int(n) => n,
        other => panic!("expected Int, got {:?}", other),
    }
}

// ---- seed / int determinism ----

#[test]
fn int_is_reproducible_for_fixed_seed() {
    let _g = TEST_LOCK.lock();
    seed(42);
    let a = int_val(random_int(&[VmValue::Int(0), VmValue::Int(1_000_000)]).unwrap());
    let b = int_val(random_int(&[VmValue::Int(0), VmValue::Int(1_000_000)]).unwrap());
    seed(42);
    let a2 = int_val(random_int(&[VmValue::Int(0), VmValue::Int(1_000_000)]).unwrap());
    let b2 = int_val(random_int(&[VmValue::Int(0), VmValue::Int(1_000_000)]).unwrap());
    assert_eq!(a, a2);
    assert_eq!(b, b2);
}

#[test]
fn int_within_inclusive_bounds() {
    let _g = TEST_LOCK.lock();
    seed(7);
    for _ in 0..1000 {
        match random_int(&[VmValue::Int(3), VmValue::Int(9)]).unwrap() {
            VmValue::Int(n) => assert!((3..=9).contains(&n), "out of bounds: {}", n),
            other => panic!("expected Int, got {:?}", other),
        }
    }
}

#[test]
fn int_single_value_range() {
    let _g = TEST_LOCK.lock();
    seed(1);
    match random_int(&[VmValue::Int(5), VmValue::Int(5)]).unwrap() {
        VmValue::Int(n) => assert_eq!(n, 5),
        other => panic!("expected Int, got {:?}", other),
    }
}

#[test]
fn int_min_greater_than_max_errors() {
    let err = random_int(&[VmValue::Int(9), VmValue::Int(1)]).unwrap_err();
    assert!(matches!(err, JadeError::IoError { .. }));
}

#[test]
fn int_arity_error() {
    let err = random_int(&[VmValue::Int(0)]).unwrap_err();
    assert!(matches!(err, JadeError::ArityMismatch { expected: 2, got: 1, .. }));
}

#[test]
fn int_type_error() {
    let err = random_int(&[VmValue::Float(0.0), VmValue::Int(3)]).unwrap_err();
    assert!(matches!(err, JadeError::TypeError { .. }));
}

// ---- float ----

#[test]
fn float_is_reproducible_for_fixed_seed() {
    let _g = TEST_LOCK.lock();
    seed(99);
    let a = random_float(&[]).unwrap();
    seed(99);
    let a2 = random_float(&[]).unwrap();
    match (a, a2) {
        (VmValue::Float(x), VmValue::Float(y)) => assert_eq!(x, y),
        _ => panic!("expected floats"),
    }
}

#[test]
fn float_in_unit_range() {
    let _g = TEST_LOCK.lock();
    seed(3);
    for _ in 0..1000 {
        match random_float(&[]).unwrap() {
            VmValue::Float(f) => assert!((0.0..1.0).contains(&f), "out of [0,1): {}", f),
            other => panic!("expected Float, got {:?}", other),
        }
    }
}

#[test]
fn float_arity_error() {
    let err = random_float(&[VmValue::Int(1)]).unwrap_err();
    assert!(matches!(err, JadeError::ArityMismatch { expected: 0, got: 1, .. }));
}

// ---- choice ----

#[test]
fn choice_is_reproducible_for_fixed_seed() {
    let _g = TEST_LOCK.lock();
    let arr = make_array(vec![
        VmValue::Int(10),
        VmValue::Int(20),
        VmValue::Int(30),
        VmValue::Int(40),
    ]);
    seed(123);
    let a = random_choice(&[arr.clone()]).unwrap();
    seed(123);
    let b = random_choice(&[arr]).unwrap();
    assert_eq!(format!("{:?}", a), format!("{:?}", b));
}

#[test]
fn choice_returns_member() {
    let _g = TEST_LOCK.lock();
    seed(5);
    let arr = make_array(vec![VmValue::Int(1), VmValue::Int(2), VmValue::Int(3)]);
    for _ in 0..100 {
        match random_choice(&[arr.clone()]).unwrap() {
            VmValue::Int(n) => assert!((1..=3).contains(&n)),
            other => panic!("expected Int member, got {:?}", other),
        }
    }
}

#[test]
fn choice_empty_array_errors() {
    let arr = make_array(vec![]);
    let err = random_choice(&[arr]).unwrap_err();
    assert!(matches!(err, JadeError::IoError { .. }));
}

#[test]
fn choice_type_error() {
    let err = random_choice(&[VmValue::Int(1)]).unwrap_err();
    assert!(matches!(err, JadeError::TypeError { .. }));
}

#[test]
fn choice_arity_error() {
    let err = random_choice(&[]).unwrap_err();
    assert!(matches!(err, JadeError::ArityMismatch { expected: 1, got: 0, .. }));
}

// ---- shuffle ----

fn snapshot(v: &VmValue) -> Vec<i64> {
    match v {
        VmValue::Array(arc) => arc
            .lock()
            .iter()
            .map(|e| match e {
                VmValue::Int(n) => *n,
                _ => panic!("expected int elements"),
            })
            .collect(),
        _ => panic!("expected array"),
    }
}

#[test]
fn shuffle_preserves_length_and_multiset() {
    let _g = TEST_LOCK.lock();
    seed(17);
    let arr = make_array((0..20).map(VmValue::Int).collect());
    let out = random_shuffle(&[arr.clone()]).unwrap();
    assert!(matches!(out, VmValue::Nil));
    let mut after = snapshot(&arr);
    assert_eq!(after.len(), 20);
    after.sort();
    assert_eq!(after, (0..20).collect::<Vec<_>>());
}

#[test]
fn shuffle_is_reproducible_for_fixed_seed() {
    let _g = TEST_LOCK.lock();
    let a = make_array((0..10).map(VmValue::Int).collect());
    seed(2024);
    random_shuffle(&[a.clone()]).unwrap();
    let ordering_a = snapshot(&a);

    let b = make_array((0..10).map(VmValue::Int).collect());
    seed(2024);
    random_shuffle(&[b.clone()]).unwrap();
    let ordering_b = snapshot(&b);

    assert_eq!(ordering_a, ordering_b);
}

#[test]
fn shuffle_type_error() {
    let err = random_shuffle(&[VmValue::Int(1)]).unwrap_err();
    assert!(matches!(err, JadeError::TypeError { .. }));
}

#[test]
fn shuffle_arity_error() {
    let err = random_shuffle(&[]).unwrap_err();
    assert!(matches!(err, JadeError::ArityMismatch { expected: 1, got: 0, .. }));
}

// ---- seed ----

#[test]
fn seed_type_error() {
    let err = random_seed(&[VmValue::Str("x".to_string().into())]).unwrap_err();
    assert!(matches!(err, JadeError::TypeError { .. }));
}

#[test]
fn seed_arity_error() {
    let err = random_seed(&[]).unwrap_err();
    assert!(matches!(err, JadeError::ArityMismatch { expected: 1, got: 0, .. }));
}
