//! Tests for lowering checked programs into MIR.
//!
//! Each test lowers a small program and reads the MIR as text, so a failure
//! shows the code that was produced.

use super::*;
use crate::frontend::error::JadeError;

/// Purpose: This function runs a source program through the frontend, the
/// checker and the lowering.
///
/// Inputs:
/// - `src`: the program.
///
/// Outputs: the MIR program, or the error.
fn lower(src: &str) -> Result<Program, JadeError> {
    let tokens = crate::frontend::lexer::tokenize(src).expect("lex");
    let program = crate::frontend::parser::parse(tokens).expect("parse");
    let mut tir = crate::compiler::type_infer::infer(program).expect("infer");
    crate::compiler::type_infer::fill_struct_literal_defaults(&mut tir).expect("defaults");
    let typed = crate::compiler::infer::check_program(&tir).expect("check");
    lower_program(&tir, &typed)
}

/// Purpose: This function lowers a program that must lower and prints it.
fn text(src: &str) -> String {
    lower(src).expect("lowers").to_string()
}

#[test]
fn each_set_of_argument_types_gets_its_own_function() {
    let t = text("fn add(a, b) { return a + b }\nprint(add(1, 2))\nprint(add(1.5, 2.0))\n");
    assert!(t.contains("add(int, int)(_0: int, _1: int) -> int"), "{t}");
    assert!(t.contains("add(float, float)(_0: float, _1: float) -> float"), "{t}");
    assert!(t.contains("add.int"), "int addition in the int copy: {t}");
    assert!(t.contains("add.float"), "float addition in the float copy: {t}");
}

#[test]
fn main_is_function_zero_and_globals_are_named() {
    let p = lower("let x = 1\nprint(x)\n").expect("lowers");
    assert_eq!(p.main, FuncId(0));
    assert_eq!(p.funcs[0].name, "main");
    assert!(p.globals.iter().any(|g| g.name == "x" && g.ty == Ty::Int));
}

#[test]
fn a_narrowed_read_unwraps_the_optional() {
    let t = text(
        "fn f(n) {\n    let x = nil\n    if n > 0 { x = n }\n    if x != nil { print(x + 1) }\n}\nf(2)\n",
    );
    assert!(t.contains("isnil"), "the nil test: {t}");
    assert!(t.contains("unwrap"), "the narrowed read: {t}");
    assert!(t.contains("wrap.int?"), "storing an int into an int?: {t}");
}

#[test]
fn a_statements_temporaries_are_dropped_when_it_ends() {
    let t = text("let a = \"x\"\nprint(a + \"y\")\n");
    assert!(t.contains("concat"), "{t}");
    assert!(t.contains("drop _"), "the joined string is freed: {t}");
}

#[test]
fn storing_a_fresh_value_hands_its_reference_over() {
    let t = text("fn f(a) {\n    let s = a + \"!\"\n    print(s)\n}\nf(\"x\")\n");
    assert!(t.contains("= move _"), "the temporary is moved, not copied: {t}");
}

#[test]
fn writing_into_a_dict_is_a_dict_set() {
    let p = lower("fn f(d) { d[\"k\"] = 1 }\nf({\"a\": 0})\n").expect("lowers");
    let found = p
        .funcs
        .iter()
        .flat_map(|f| &f.blocks)
        .flat_map(|b| &b.stmts)
        .any(|s| matches!(s, Stmt::DictSet { .. }));
    assert!(found, "{p}");
}

#[test]
fn a_function_that_can_fall_off_returns_nil() {
    let t = text("fn f(n) {\n    if n > 0 { return n }\n}\nprint(f(1))\n");
    assert!(t.contains("return nil: int?"), "{t}");
}

#[test]
fn a_method_on_a_parent_dispatches_on_the_type_id() {
    let src = "struct Shape { name }\nstruct Sq(Shape) { side }\nstruct Ci(Shape) { r }\n\
               extend Sq { fn area(self) { return self.side * self.side } }\n\
               extend Ci { fn area(self) { return self.r * 3 } }\n\
               let shapes = [Sq { name: \"s\", side: 2 }, Ci { name: \"c\", r: 1 }]\n\
               for s in shapes { print(s.area()) }\n";
    let t = text(src);
    assert!(t.contains("typeid"), "{t}");
    assert!(t.contains("switch"), "{t}");
}

#[test]
fn a_for_loop_over_a_string_walks_its_characters() {
    let t = text("for c in \"hé\" { print(c) }\n");
    assert!(t.contains("strchars"), "{t}");
}

#[test]
fn features_from_later_phases_say_so() {
    for (src, what) in [
        ("try { print(1) } catch e { print(2) }\n", "try and catch"),
        ("async fn f() { return 1 }\nprint(await f())\n", "await"),
        ("fn g(x) { yield x }\nlet s = g(1)\nprint(s[0])\n", "stream"),
    ] {
        match lower(src) {
            Err(JadeError::NotYetStatic { what: w, .. }) => {
                assert!(w.contains(what), "{src}: named {w}, expected {what}")
            }
            other => panic!("{src}: expected a not-yet error, got {other:?}"),
        }
    }
}

#[test]
fn heap_types_are_the_reference_counted_ones() {
    assert!(is_heap(&Ty::Str));
    assert!(is_heap(&Ty::Opt(Box::new(Ty::Array(Box::new(Ty::Int))))));
    assert!(!is_heap(&Ty::Int));
    assert!(!is_heap(&Ty::Opt(Box::new(Ty::Float))));
}
