use super::*;
use crate::compiler::type_infer::infer;
use crate::frontend::{lexer, parser};

/// Parse + type-check `src` and return the body of its first `fn`.
fn fn_body(src: &str) -> Vec<TStmt> {
    let tokens = lexer::tokenize(src).expect("lex");
    let program = parser::parse(tokens).expect("parse");
    let tp = infer(program).expect("infer");
    for st in tp.stmts {
        if let TStmt::FnDef { body, .. } = st {
            return body;
        }
    }
    panic!("no fn in source");
}

fn plan(src: &str) -> ArenaPlan {
    analyze(&fn_body(src))
}

#[test]
fn eligible_local_scalar_array() {
    let p = plan("fn f() {\n  let a = [1, 2, 3]\n  let x = a[0]\n  return x\n}");
    assert_eq!(p.eligible.len(), 1, "a local scalar array used only by index is eligible");
    assert!(p.arena_vars.contains("a"));
}

#[test]
fn eligible_in_loop() {
    let p = plan(
        "fn f(n) {\n  let acc = 0\n  let i = 0\n  while i < n {\n    let a = [i, i + 1, i + 2]\n    acc = acc + a[0] + a[2]\n    i = i + 1\n  }\n  return acc\n}",
    );
    assert_eq!(p.eligible.len(), 1, "a loop-body-local scalar array is eligible");
    assert!(p.arena_vars.contains("a"));
}

// ── Escape cases: each of these MUST be rejected (a false eligible is a UAF) ──

#[test]
fn returned_array_escapes() {
    let p = plan("fn f() {\n  let a = [1, 2, 3]\n  return a\n}");
    assert!(p.is_empty(), "a returned array escapes");
}

#[test]
fn passed_to_call_escapes() {
    let p = plan("fn f() {\n  let a = [1, 2, 3]\n  print(a)\n  return 0\n}");
    assert!(p.is_empty(), "an array passed to a call escapes");
}

#[test]
fn aliased_to_another_var_escapes() {
    let p = plan("fn f() {\n  let a = [1, 2, 3]\n  let b = a\n  return b[0]\n}");
    assert!(p.is_empty(), "aliasing the array to another variable escapes it");
}

#[test]
fn stored_in_dict_escapes() {
    let p = plan("fn f() {\n  let a = [1, 2, 3]\n  let d = {\"k\": a}\n  return d\n}");
    assert!(p.is_empty(), "an array stored into a dict escapes");
}

#[test]
fn stored_in_array_escapes() {
    let p = plan("fn f() {\n  let a = [1, 2, 3]\n  let b = [a]\n  return b\n}");
    assert!(p.is_empty(), "an array nested into another array literal escapes");
}

#[test]
fn bare_use_in_arithmetic_context_escapes() {
    // `a` used without indexing (here as an equality operand) is not a scalar
    // index read, so it must be treated as escaping.
    let p = plan("fn f() {\n  let a = [1, 2, 3]\n  let b = [1, 2, 3]\n  let c = a == b\n  return c\n}");
    assert!(p.is_empty(), "comparing the array by value is not an allowed use");
}

#[test]
fn string_elements_excluded() {
    let p = plan("fn f() {\n  let a = [\"x\", \"y\"]\n  let z = a[0]\n  return z\n}");
    assert!(p.is_empty(), "Str elements are heap pointers and are excluded in v1");
}

#[test]
fn nested_array_elements_excluded() {
    let p = plan("fn f() {\n  let a = [[1], [2]]\n  let z = a[0]\n  return z[0]\n}");
    assert!(p.is_empty(), "array-of-arrays elements are not immediate scalars");
}

#[test]
fn dict_literals_not_targeted_in_v1() {
    let p = plan("fn f() {\n  let d = {\"x\": 1}\n  let v = d[\"x\"]\n  return v\n}");
    assert!(p.is_empty(), "dicts are excluded (String keys would leak at reset)");
}
