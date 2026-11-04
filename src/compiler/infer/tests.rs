//! Tests for whole-program inference: the rules in design/v2-static-values.md,
//! each pinned by a program that should check and one that should not.

use super::*;

/// Purpose: This function runs a source string through the frontend and the checker.
///
/// Inputs:
/// - `src`: a Jade program.
///
/// Outputs: the checker's answer.
fn check(src: &str) -> Result<Typed, JadeError> {
    let tokens = crate::frontend::lexer::tokenize(src).expect("lexes");
    let program = crate::frontend::parser::parse(tokens).expect("parses");
    let mut tp = crate::compiler::type_infer::infer(program).expect("v1 checker accepts it");
    crate::compiler::type_infer::fill_struct_literal_defaults(&mut tp).expect("defaults fill");
    check_program(&tp)
}

/// Purpose: This function asserts a program checks.
///
/// Inputs:
/// - `src`: a Jade program.
///
/// Outputs: the checker's answer.
fn ok(src: &str) -> Typed {
    match check(src) {
        Ok(t) => t,
        Err(e) => panic!("expected the program to check, got: {e}\n{src}"),
    }
}

/// Purpose: This function asserts a program is rejected with a message
/// containing `want`.
///
/// Inputs:
/// - `src`: a Jade program.
/// - `want`: part of the expected message.
///
/// Outputs: none.
fn rejects(src: &str, want: &str) {
    match check(src) {
        Ok(_) => panic!("expected a type error containing '{want}', but it checked:\n{src}"),
        Err(e) => {
            let m = e.to_string();
            assert!(m.contains(want), "expected '{want}' in the error, got: {m}");
        }
    }
}

/// Purpose: This function finds a global's resolved type.
///
/// Inputs:
/// - `t`: the checker's answer.
/// - `name`: the global.
///
/// Outputs: its type.
fn global(t: &Typed, name: &str) -> Ty {
    t.globals.get(name).cloned().unwrap_or_else(|| panic!("no global {name}"))
}

#[test]
fn literals_and_arithmetic() {
    let t = ok("let a = 1 + 2\nlet b = 1.5 * 2\nlet c = \"x\" + \"y\"\nlet d = 1 < 2\n");
    assert_eq!(global(&t, "a"), Ty::Int);
    assert_eq!(global(&t, "b"), Ty::Float);
    assert_eq!(global(&t, "c"), Ty::Str);
    assert_eq!(global(&t, "d"), Ty::Bool);
}

#[test]
fn a_function_is_checked_once_per_argument_types() {
    let t = ok(
        "fn add(a, b) { return a + b }\nlet i = add(1, 2)\nlet f = add(1.5, 2.0)\nlet s = add(\"a\", \"b\")\n",
    );
    assert_eq!(global(&t, "i"), Ty::Int);
    assert_eq!(global(&t, "f"), Ty::Float);
    assert_eq!(global(&t, "s"), Ty::Str);
    let adds = t.instances.iter().filter(|(k, _, _)| k.func == FnKey::Named("add".into())).count();
    assert_eq!(adds, 3);
}

#[test]
fn a_bad_call_is_reported_at_its_instance() {
    rejects(
        "fn add(a, b) { return a + b }\nlet x = add(1, \"b\")\n",
        "'+' cannot combine int and str",
    );
}

#[test]
fn an_empty_array_learns_its_type_from_a_push() {
    let t = ok("let xs = []\nxs.push(1)\n");
    assert_eq!(global(&t, "xs"), Ty::Array(Box::new(Ty::Int)));
}

#[test]
fn a_push_through_a_function_reaches_the_callers_array() {
    let t = ok("fn fill(a) { a.push(\"x\") }\nlet xs = []\nfill(xs)\n");
    assert_eq!(global(&t, "xs"), Ty::Array(Box::new(Ty::Str)));
}

#[test]
fn an_array_nothing_fills_holds_nil() {
    let t = ok("let xs = []\nprint(xs)\n");
    assert_eq!(global(&t, "xs"), Ty::Array(Box::new(Ty::Nil)));
}

#[test]
fn a_variable_keeps_one_type() {
    rejects("let v = \"text\"\nv = 7\n", "do not mix");
}

#[test]
fn nil_then_a_value_is_optional() {
    let t = ok("let first = nil\nfirst = 5\n");
    assert_eq!(global(&t, "first"), Ty::Opt(Box::new(Ty::Int)));
}

#[test]
fn an_optional_must_be_checked_before_use() {
    rejects("use std::env\nlet p = env.get(\"PORT\")\nprint(p.len())\n", "may be nil");
}

#[test]
fn checking_against_nil_narrows() {
    ok("use std::env\nlet p = env.get(\"PORT\")\nif p != nil {\n    print(p.len())\n}\n");
    ok("use std::env\nlet p = env.get(\"PORT\")\nif p != nil && p.len() > 0 {\n    print(p)\n}\n");
}

#[test]
fn falling_off_the_end_returns_nil() {
    let t = ok("fn maybe(x) {\n    if x > 0 {\n        return x\n    }\n}\nlet m = maybe(3)\n");
    assert_eq!(global(&t, "m"), Ty::Opt(Box::new(Ty::Int)));
}

#[test]
fn returning_two_kinds_is_an_error() {
    rejects(
        "fn pick(x) {\n    if x {\n        return \"s\"\n    }\n    return 1\n}\nprint(pick(true))\n",
        "do not mix",
    );
}

#[test]
fn a_dict_holds_one_kind_of_value() {
    let t = ok("let ages = {\"ana\": 31}\nages[\"bo\"] = 28\nlet a = ages.get(\"x\")\n");
    assert_eq!(global(&t, "ages"), Ty::Map(Box::new(Ty::Int)));
    assert_eq!(global(&t, "a"), Ty::Opt(Box::new(Ty::Int)));
    rejects("let d = {\"name\": \"jade\", \"version\": 1}\n", "use a struct");
}

#[test]
fn a_dict_has_no_fields() {
    rejects("let d = {\"code\": 0}\nprint(d.code)\n", "a dict has no fields");
}

#[test]
fn mixed_arrays_are_an_error() {
    rejects("let m = [1, \"a\"]\n", "do not mix");
}

#[test]
fn structs_join_to_their_common_parent() {
    let src = "struct Frame {}\nstruct Token(Frame) { text }\nstruct Done(Frame) { reason }\n\
               let frames = [Token { text: \"hi\" }, Done { reason: \"stop\" }]\n";
    let t = ok(src);
    assert_eq!(global(&t, "frames"), Ty::Array(Box::new(Ty::Struct("Frame".into()))));
    rejects("struct A { x }\nstruct B { y }\nlet m = [A { x: 1 }, B { y: 2 }]\n", "shared parent");
}

#[test]
fn a_method_dispatches_on_the_actual_struct() {
    let src = "struct Animal { name }\nstruct Dog(Animal) {}\nstruct Cat(Animal) {}\n\
               extend Dog { fn speak(self) { return \"woof\" } }\n\
               extend Cat { fn speak(self) { return \"meow\" } }\n\
               let pets = [Dog { name: \"rex\" }, Cat { name: \"tom\" }]\n\
               for p in pets {\n    print(p.speak())\n}\n";
    ok(src);
}

#[test]
fn a_struct_field_is_typed_by_every_literal() {
    rejects("struct P { let x = 0 }\nlet p = P { x: \"s\" }\n", "do not mix");
}

#[test]
fn recursion_settles() {
    let t = ok(
        "fn fact(n) {\n    if n <= 1 {\n        return 1\n    }\n    return n * fact(n - 1)\n}\nlet f = fact(5)\n",
    );
    assert_eq!(global(&t, "f"), Ty::Int);
}

#[test]
fn recursion_that_changes_its_own_types_is_refused() {
    rejects("fn f(x) {\n    return f([x])\n}\nprint(f(1))\n", "different type each time");
}

#[test]
fn closures_take_their_types_from_their_use() {
    let t = ok("fn apply(f, x) { return f(x) }\nlet sq = apply(|x| x * x, 6)\n");
    assert_eq!(global(&t, "sq"), Ty::Int);
}

#[test]
fn map_and_filter_are_typed() {
    let t = ok(
        "let xs = [1, 2, 3]\nlet ys = xs.map(|x| x * 2)\nlet zs = xs.filter(|x| x > 1)\nlet ss = xs.map(str)\n",
    );
    assert_eq!(global(&t, "ys"), Ty::Array(Box::new(Ty::Int)));
    assert_eq!(global(&t, "zs"), Ty::Array(Box::new(Ty::Int)));
    assert_eq!(global(&t, "ss"), Ty::Array(Box::new(Ty::Str)));
}

#[test]
fn different_functions_in_one_array_share_a_function_type() {
    let t = ok("fn double(x) { return x * 2 }\nfn square(x) { return x * x }\n\
                let fs = [double, square]\nfor g in fs {\n    print(g(3))\n}\n");
    match global(&t, "fs") {
        Ty::Array(inner) => assert!(matches!(*inner, Ty::Fn(_)), "got {inner}"),
        other => panic!("expected an array, got {other}"),
    }
}

#[test]
fn raise_takes_a_struct_or_a_string() {
    ok(
        "struct Oops { code }\ntry {\n    raise Oops { code: 1 }\n} catch Oops e {\n    print(e.code)\n} catch e {\n    print(e.message)\n}\n",
    );
    rejects("raise 5\n", "cannot raise int");
}

#[test]
fn a_catch_all_only_has_a_message() {
    rejects("try {\n    raise \"x\"\n} catch e {\n    print(e.code)\n}\n", "only has 'message'");
}

#[test]
fn async_functions_give_futures() {
    let t = ok(
        "async fn work(x) { return x + 1 }\nlet f = work(1)\nlet r = await f\nlet all = join(work(1), work(2))\n",
    );
    assert_eq!(global(&t, "f"), Ty::Future(Box::new(Ty::Int)));
    assert_eq!(global(&t, "r"), Ty::Int);
    assert_eq!(global(&t, "all"), Ty::Array(Box::new(Ty::Int)));
}

#[test]
fn generators_give_streams() {
    let t = ok("fn nums() {\n    yield 1\n    yield 2\n}\nlet s = nums()\n");
    assert_eq!(global(&t, "s"), Ty::Stream(Box::new(Ty::Int)));
    rejects("fn mixed() {\n    yield 1\n    yield \"two\"\n}\nprint(mixed())\n", "do not mix");
}

#[test]
fn std_results_are_structs_that_read_both_ways() {
    ok("use std::sh\nlet r = sh.output(\"true\")\nprint(r.code)\nprint(r[\"stdout\"])\n");
}

#[test]
fn json_is_its_own_type() {
    let t =
        ok("use std::json\nlet v = json.parse(\"{}\")\nlet a = v[\"Action\"]\nlet s = str(a)\n");
    assert_eq!(global(&t, "v"), Ty::Json);
    assert_eq!(global(&t, "s"), Ty::Str);
}

#[test]
fn the_v1_soundness_holes_are_closed() {
    // An assignment in one branch.
    rejects("let x = \"s\"\nif true {\n    x = 1\n}\nprint(x)\n", "do not mix");
    // A push of another kind.
    rejects("let a = [1]\na.push(\"s\")\n", "do not mix");
}

#[test]
fn an_array_cannot_contain_itself() {
    rejects("let a = []\na.push(a)\n", "cannot contain itself");
}

#[test]
fn an_early_exit_on_nil_narrows_the_rest_of_the_block() {
    ok(
        "use std::env\nfn port() {\n    let p = env.get(\"PORT\")\n    if p == nil {\n        return \"none\"\n    }\n    return p\n}\nprint(port())\n",
    );
}

#[test]
fn a_trailing_expression_is_the_return_value() {
    let t = ok("fn double(x) {\n    x * 2\n}\nlet d = double(5) + 1\n");
    assert_eq!(global(&t, "d"), Ty::Int);
}
