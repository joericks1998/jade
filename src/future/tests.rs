use super::*;
use crate::builtins::{PrimType, find_primitive_method, primitive_method_arity};

#[test]
fn ready_resolves_as_a_future_method() {
    let f = find_primitive_method(PrimType::Future, "ready");
    assert!(f.is_some(), "future.ready should resolve");
    assert_eq!(f.unwrap().name, "ready");
}

/// A future has one method and no other. The dispatch arm in `vm::dispatch`
/// reports "future has no method '<name>'" for anything else, which it can only
/// do because the lookup returns `None` rather than falling through to the
/// generic "value is not a struct".
#[test]
fn a_future_has_no_other_methods() {
    for m in ["len", "upper", "push", "keys", "await", "bogus"] {
        assert!(
            find_primitive_method(PrimType::Future, m).is_none(),
            "future.{m} should not resolve"
        );
    }
}

/// The arity table is what lets the compiled backend say "`ready` takes 0
/// arguments" instead of "no method named `ready`" — the same distinction
/// `upper` gets. Missing from it, a wrong-arity call reads as a typo.
#[test]
fn ready_is_in_the_arity_table() {
    assert_eq!(primitive_method_arity("ready"), Some(0));
}

#[test]
fn ready_rejects_a_receiver_that_is_not_a_future() {
    let err = future_ready(&[crate::vm::VmValue::Int(5)]);
    assert!(err.is_err(), "a non-future receiver must not answer");
}
