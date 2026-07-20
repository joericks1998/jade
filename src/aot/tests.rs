//! Tests for the two compilation modes. These stop at IR (`emit_ir = true`) so
//! they need LLVM but not a linker or a build daemon.

use super::*;

fn tir(src: &str) -> TProgram {
    let tokens = crate::frontend::lexer::tokenize(src).expect("lex");
    let program = crate::frontend::parser::parse(tokens).expect("parse");
    crate::compiler::type_infer::infer(program).expect("infer")
}

fn ir(src: &str, mode: CompileMode) -> String {
    compile_with_mode(tir(src), None, Path::new("mylib"), true, mode)
        .expect("compilation should succeed")
        .expect("emit_ir returns the IR")
}

const LIB: &str = "fn add(a, b) { return a + b }\nfn triple(x) { return x * 3 }\n";

// ── Binary mode ───────────────────────────────────────────────────────────────

#[test]
fn binary_mode_emits_main_and_no_pkg_init() {
    let out = ir(LIB, CompileMode::Binary);
    assert!(out.contains("define i32 @main("), "no main:\n{out}");
    assert!(!out.contains("@jade_pkg_init"), "a binary must not export jade_pkg_init");
}

// ── Shared-library mode ───────────────────────────────────────────────────────

#[test]
fn shared_lib_emits_pkg_init_and_no_main() {
    let out = ir(LIB, CompileMode::SharedLib { exports: vec![] });
    assert!(out.contains("define i32 @jade_pkg_init("), "no pkg_init:\n{out}");
    assert!(
        !out.contains("define i32 @main("),
        "a shared library must not define main:\n{out}"
    );
}

#[test]
fn an_empty_export_list_exports_every_function() {
    // Jade has no `pub`, so everything top-level is public by construction.
    let out = ir(LIB, CompileMode::SharedLib { exports: vec![] });
    assert!(out.contains(r#""jade_export$add""#), "add not exported:\n{out}");
    assert!(out.contains(r#""jade_export$triple""#), "triple not exported:\n{out}");
}

#[test]
fn an_export_list_narrows_the_bindings() {
    let out = ir(LIB, CompileMode::SharedLib { exports: vec!["add".to_string()] });
    assert!(out.contains(r#""jade_export$add""#), "add not exported:\n{out}");
    assert!(!out.contains(r#""jade_export$triple""#), "triple should have been filtered out:\n{out}");
}

#[test]
fn exporting_an_unknown_function_names_it() {
    let err = compile_with_mode(
        tir(LIB),
        None,
        Path::new("mylib"),
        true,
        CompileMode::SharedLib { exports: vec!["nope".to_string()] },
    )
    .unwrap_err();
    assert!(err.contains("nope"), "error should name the function: {err}");
}

#[test]
fn a_file_with_no_functions_cannot_be_a_package() {
    let err = compile_with_mode(
        tir("let x = 1\n"),
        None,
        Path::new("mylib"),
        true,
        CompileMode::SharedLib { exports: vec![] },
    )
    .unwrap_err();
    assert!(err.contains("no top-level functions"), "unexpected message: {err}");
}

#[test]
fn wrappers_marshal_through_the_ffi_helpers() {
    // The lowered functions speak the tagged word; the host speaks JadeVal.
    let out = ir(LIB, CompileMode::SharedLib { exports: vec![] });
    assert!(out.contains("@jrt_ffi_to_tagged"), "no inbound marshalling:\n{out}");
    assert!(out.contains("@jrt_ffi_from_tagged"), "no outbound marshalling:\n{out}");
}

#[test]
fn both_modes_share_one_initializer() {
    // Binary and package must initialize identically, or a package would skip
    // the native-package dlopen prologue that a binary runs.
    for mode in [CompileMode::Binary, CompileMode::SharedLib { exports: vec![] }] {
        let out = ir(LIB, mode);
        assert!(out.contains("define void @jade_mod_init()"), "missing initializer:\n{out}");
        assert!(out.contains("call void @jade_mod_init()"), "initializer never called:\n{out}");
    }
}

#[test]
fn pkg_init_runs_the_module_body_only_once() {
    // A host may call jade_pkg_init more than once; re-running the top level
    // would repeat its side effects.
    let out = ir(LIB, CompileMode::SharedLib { exports: vec![] });
    assert!(out.contains("@jade_pkg_inited"), "no once-guard:\n{out}");
}

// ── Integration: a Jade package that actually loads ───────────────────────────

/// End-to-end for `jade build --lib`: lower a Jade file to a real shared
/// library, then load it through the same loader a consumer project uses. This
/// is the only test that exercises linking and `dlopen` together.
#[test]
fn a_compiled_jade_package_loads_and_binds_its_exports() {
    let dir = std::env::temp_dir().join(format!("jade_libmode_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ext = if cfg!(target_os = "macos") { "dylib" } else { "so" };
    let out = dir.join(format!("mathpkg.{ext}"));

    let result = compile_with_mode(
        tir(LIB),
        None,
        &out,
        false,
        CompileMode::SharedLib { exports: vec![] },
    );
    assert!(result.is_ok(), "linking a package failed: {:?}", result.err());
    assert!(out.exists(), "no shared library was produced at {}", out.display());

    let span = crate::frontend::error::Span { line: 0, col: 0 };
    let pkg = crate::native::load_native_package(&out, span)
        .expect("a compiled Jade package must satisfy the native package ABI");

    assert!(pkg.contains_key("add"), "missing export: {:?}", pkg.keys().collect::<Vec<_>>());
    assert!(pkg.contains_key("triple"), "missing export: {:?}", pkg.keys().collect::<Vec<_>>());

    let _ = std::fs::remove_dir_all(&dir);
}
