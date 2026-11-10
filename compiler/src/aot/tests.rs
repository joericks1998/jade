use super::*;

/// Purpose: This function parses and types a program with no file.
fn tir(src: &str) -> TProgram {
    let tokens = crate::frontend::lexer::tokenize(src).unwrap();
    let program = crate::frontend::parser::parse(tokens).unwrap();
    crate::compiler::type_infer::infer(program).unwrap()
}

#[test]
fn a_program_that_checks_would_build() {
    assert!(would_build(&tir("let x = 1\nprint(x + 2)\n"), None).is_ok());
}

#[test]
fn a_type_error_is_what_would_build_reports() {
    let err = would_build(&tir("let x = 1\nx = \"a\"\n"), None).unwrap_err();
    assert!(err.contains("type"), "{err}");
}

/// The task check runs on every build, not only on `jade check`.
#[test]
fn a_task_that_races_does_not_build() {
    let src = "let counter = 0\nasync fn bump() { counter = counter + 1 }\nawait bump()\n";
    let err = would_build(&tir(src), None).unwrap_err();
    assert!(err.contains("counter"), "{err}");
    let out = std::env::temp_dir().join(format!("jade_aot_race_{}", std::process::id()));
    assert!(compile(tir(src), None, &out, Emit::Mir).is_err());
}

#[test]
fn mir_is_text_and_writes_nothing() {
    let out = std::env::temp_dir().join(format!("jade_aot_mir_{}", std::process::id()));
    let r = compile(tir("print(1)\n"), None, &out, Emit::Mir).unwrap();
    assert!(r.ir.unwrap().contains("fn f0 main"));
    assert!(!out.exists());
}
