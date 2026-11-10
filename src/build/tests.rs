//! Tests for the in-process build path.
//!
//! Most stop at IR (`Emit::Ir`), so they need LLVM but no linker; one links a
//! binary with the LLD inside `jade`.

use std::path::Path;

use super::*;

fn tir(src: &str) -> TProgram {
    let tokens = crate::frontend::lexer::tokenize(src).expect("lex");
    let program = crate::frontend::parser::parse(tokens).expect("parse");
    crate::compiler::type_infer::infer(program).expect("infer")
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
    build(&tir("fn f() { return 1 }\n"), &src.file, &src.out(), Emit::Ir)
        .expect("IR emission should succeed");

    assert!(!src.out().exists(), "emitting IR must not produce an artifact");
}

#[test]
fn building_a_binary_writes_an_artifact() {
    let src = Source::new("bin", "fn f() { return 1 }\n");
    build(&tir("fn f() { return 1 }\n"), &src.file, &src.out(), Emit::Binary)
        .expect("binary build should succeed");

    assert!(src.out().exists(), "no binary produced at {}", src.out().display());
}

#[test]
fn a_missing_source_file_is_an_error() {
    // source_path anchors import resolution, so it has to exist.
    let result = build(
        &tir("fn f() { return 1 }\n"),
        Path::new("/nonexistent/nope.jde"),
        Path::new("/tmp/jade_build_never"),
        Emit::Ir,
    );
    assert!(result.is_err(), "expected an error for a missing source file");
}
