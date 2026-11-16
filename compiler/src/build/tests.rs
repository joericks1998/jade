//! Tests for the in-process build path.
//!
//! Most stop at IR (`Emit::Ir`), so they need LLVM but no linker; one links a
//! binary with the LLD inside `jade`.

use std::path::Path;

use super::*;

fn resolved(src: &str) -> Program {
    let tokens = crate::frontend::lexer::tokenize(src).expect("lex");
    let program = crate::frontend::parser::parse(tokens).expect("parse");
    crate::typechecker::resolve::resolve(program).expect("resolve")
}

#[test]
fn emit_defaults_to_binary() {
    assert_eq!(Emit::default(), Emit::Binary);
}

// ── build() ───────────────────────────────────────────────────────────────────

/// A real on-disk source file — `build` canonicalizes `source_path` to anchor
/// import resolution, so a nonexistent path fails before reaching the backend.
struct Source {
    dir: std::path::PathBuf,
    file: std::path::PathBuf,
}

impl Source {
    fn new(tag: &str, body: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("jade_build_test_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.jde");
        std::fs::write(&file, body).unwrap();
        Source { dir, file }
    }

    fn out(&self) -> std::path::PathBuf {
        self.dir.join("out")
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn building_ir_does_not_write_an_output_file() {
    let src = Source::new("ir", "fn f() { return 1 }\n");
    build(&resolved("fn f() { return 1 }\n"), &src.file, &src.out(), Emit::Ir, None)
        .expect("IR emission should succeed");

    assert!(!src.out().exists(), "emitting IR must not produce an artifact");
}

#[test]
fn building_a_binary_writes_an_artifact() {
    let src = Source::new("bin", "fn f() { return 1 }\n");
    let cache = src.dir.join("lib");
    build(&resolved("fn f() { return 1 }\n"), &src.file, &src.out(), Emit::Binary, Some(&cache))
        .expect("binary build should succeed");

    assert!(src.out().exists(), "no binary produced at {}", src.out().display());
}

#[test]
fn a_missing_source_file_is_an_error() {
    // source_path anchors import resolution, so it has to exist.
    let result = build(
        &resolved("fn f() { return 1 }\n"),
        Path::new("/nonexistent/nope.jde"),
        Path::new("/tmp/jade_build_never"),
        Emit::Ir,
        None,
    );
    assert!(result.is_err(), "expected an error for a missing source file");
}

#[test]
fn a_program_that_checks_would_build() {
    assert!(would_build(&resolved("let x = 1\nprint(x + 2)\n"), None).is_ok());
}

#[test]
fn a_type_error_is_what_would_build_reports() {
    let err = would_build(&resolved("let x = 1\nx = \"a\"\n"), None).unwrap_err();
    assert!(err.contains("type"), "{err}");
}

/// The task check runs on every build, not only on `jade check`.
#[test]
fn a_task_that_races_does_not_build() {
    let src = "let counter = 0\nasync fn bump() { counter = counter + 1 }\nawait bump()\n";
    let err = would_build(&resolved(src), None).unwrap_err();
    assert!(err.contains("counter"), "{err}");
    let out = std::env::temp_dir().join(format!("jade_build_race_{}", std::process::id()));
    assert!(compile(resolved(src), None, &out, Emit::Mir, None).is_err());
}

#[test]
fn mir_is_text_and_writes_nothing() {
    let out = std::env::temp_dir().join(format!("jade_build_mir_{}", std::process::id()));
    let r = compile(resolved("print(1)\n"), None, &out, Emit::Mir, None).unwrap();
    assert!(r.ir.unwrap().contains("fn f0 main"));
    assert!(!out.exists());
}
