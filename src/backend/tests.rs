//! Tests for the static backend: programs built to binaries and run.
//!
//! These link with the system `cc`, as `jade build` does. Each test builds in
//! a directory of its own, named for the process and a counter, so the tests
//! can run in parallel.

use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// Purpose: This function lowers a source program to MIR.
fn mir_of(src: &str) -> mir::Program {
    let tokens = crate::frontend::lexer::tokenize(src).expect("lex");
    let program = crate::frontend::parser::parse(tokens).expect("parse");
    let mut tir = crate::compiler::type_infer::infer(program).expect("infer");
    crate::compiler::type_infer::fill_struct_literal_defaults(&mut tir).expect("defaults");
    let typed = crate::compiler::infer::check_program(&tir).expect("check");
    mir::lower_program(&tir, &typed).expect("lowers")
}

/// Purpose: This function builds a program and runs it.
///
/// Inputs:
/// - `src`: the program.
///
/// Outputs: its standard output, standard error and exit status.
fn run(src: &str) -> (String, String, i32) {
    let dir = std::env::temp_dir().join(format!(
        "jade-static-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let bin = dir.join("prog");
    compile(&mir_of(src), &bin, Emit::Binary).expect("builds");
    let out = std::process::Command::new(&bin).output().expect("runs");
    let _ = std::fs::remove_dir_all(&dir);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn numbers_print_as_v1_printed_them() {
    let (out, _, rc) = run(
        "print(1 + 2)\nprint(48000.0)\nprint(0.1 + 0.2)\nprint(-0.0)\nprint(7 / 2)\nprint(-7 % 3)\n",
    );
    assert_eq!(rc, 0);
    assert_eq!(out, "3\n48000.0\n0.30000000000000004\n-0.0\n3\n-1\n");
}

#[test]
fn integer_overflow_stops_the_program_with_v1s_message() {
    let (out, err, rc) = run("let x = 4611686018427387903\nprint(x * 4)\nprint(\"unreached\")\n");
    assert_eq!(out, "");
    assert_eq!(err, "jade: integer overflow\n");
    assert_eq!(rc, 1);
}

#[test]
fn a_dict_passed_to_a_function_is_a_copy() {
    let (out, _, _) = run("fn w(d) { d[\"x\"] = 99 }\nlet d = {\"x\": 1}\nw(d)\nprint(d[\"x\"])\n");
    assert_eq!(out, "1\n");
}

#[test]
fn an_array_passed_to_a_function_is_shared() {
    let (out, _, _) = run("fn p(a) { a.push(2) }\nlet a = [1]\np(a)\nprint(a)\n");
    assert_eq!(out, "[1, 2]\n");
}

#[test]
fn strings_count_and_map_characters_not_bytes() {
    let (out, _, _) = run("print(len(\"café\"))\nprint(\"größe\".upper())\nprint(\"café\"[3])\n");
    assert_eq!(out, "4\nGRÖSSE\né\n");
}

#[test]
fn dicts_print_sorted_with_quoted_keys() {
    let (out, _, _) = run("let d = {\"b\": [1], \"a\": [2, 3]}\nprint(d)\n");
    assert_eq!(out, "{\"a\": [2, 3], \"b\": [1]}\n");
}

#[test]
fn a_child_struct_runs_its_own_method_through_a_parent() {
    let src = "class A {\n    @public n\n    @public fn who() { return \"A\" }\n}\n\
               class B(A) {\n    @public m\n    @public fn who() { return \"B\" }\n}\n\
               let xs = [A { n: 1 }, B { n: 2, m: 3 }]\n\
               for x in xs { print(x.who()) }\n";
    let (out, _, _) = run(src);
    assert_eq!(out, "A\nB\n");
}

/// The point of the static backend: a compiled program calls nothing but the
/// C library. Every external symbol the IR declares must be one of these.
#[test]
fn a_program_declares_nothing_but_c_library_functions() {
    let src = "let d = {\"k\": \"v\"}\nlet a = [1.5, 2.5]\na.sort()\nprint(f\"{d} {a} {\"x\".upper()}\")\nprint(int(\"3\") + len(\"abc\".split(\"\")))\n";
    let ctx = Context::create();
    let module = build_module(&ctx, &mir_of(src)).expect("builds");
    let allowed = [
        "malloc",
        "realloc",
        "free",
        "memcmp",
        "write",
        "read",
        "exit",
        "snprintf",
        "strtod",
        "qsort",
        "pthread_attr_init",
        "pthread_attr_destroy",
        "pthread_attr_setstacksize",
        "pthread_create",
        "pthread_join",
    ];
    let mut f = module.get_first_function();
    while let Some(func) = f {
        let name = func.get_name().to_string_lossy().into_owned();
        if func.count_basic_blocks() == 0 && !name.starts_with("llvm.") {
            assert!(allowed.contains(&name.as_str()), "the program declares {name}");
        }
        f = func.get_next_function();
    }
}

#[test]
fn a_field_read_through_a_second_parent_finds_the_childs_own() {
    let src = "class W {\n    @public span\n}\nclass F {\n    @public fins\n}\nclass D(W, F) {\n    @public name\n}\n\
               fn grow(f) {\n    f.fins = f.fins + 1\n    return f.fins\n}\n\
               let d = D { span: 2, fins: 5, name: \"dot\" }\nlet fs = [F { fins: 1 }, F(d)]\n\
               for f in fs {\n    print(grow(f))\n}\nprint(d.fins)\nprint(d.span)\n";
    let (out, _, _) = run(src);
    assert_eq!(out, "2\n6\n6\n2\n");
}

#[test]
fn destructors_run_child_first_and_globals_newest_first() {
    let src = "class R {\n    @public name\n    fn drop() { print(\"R \" + self.name) }\n}\n\
               class L(R) {\n    fn drop() { print(\"L \" + self.name) }\n}\n\
               fn f() {\n    let x = R { name: \"local\" }\n}\nf()\n\
               let a = R { name: \"a\" }\nlet b = L { name: \"b\" }\nprint(\"end\")\n";
    let (out, _, rc) = run(src);
    assert_eq!(rc, 0);
    assert_eq!(out, "R local\nend\nL b\nR b\nR a\n");
}

#[test]
fn is_tests_the_class_and_its_descendants() {
    let src = "class A {\n    @public n\n}\nclass B(A) { }\nclass C(B) { }\nclass D(A) { }\n\
               let xs = [A { n: 1 }, B { n: 2 }, C { n: 3 }, D { n: 4 }]\nfor x in xs {\n    print(x is B)\n}\n";
    let (out, _, _) = run(src);
    assert_eq!(out, "false\ntrue\ntrue\nfalse\n");
}
