use super::*;

fn sym(args: &[&str], ret: &str) -> CSymbol {
    CSymbol {
        args: args.iter().map(|s| s.to_string()).collect(),
        ret: ret.to_string(),
        fails_when: None,
    }
}

/// The same, with a declared failure convention.
fn failing_sym(args: &[&str], ret: &str, f: crate::project::CFailure) -> CSymbol {
    CSymbol { fails_when: Some(f), ..sym(args, ret) }
}

fn symbols(pairs: &[(&str, CSymbol)]) -> HashMap<String, CSymbol> {
    pairs.iter().map(|(n, s)| (n.to_string(), s.clone())).collect()
}

#[test]
fn generates_a_pkg_init_and_binding_table() {
    let src = generate("zlib", &symbols(&[("crc32", sym(&["int", "str"], "int"))])).unwrap();

    assert!(src.contains("int jade_pkg_init(JadeNativePkg* out)"), "no init:\n{src}");
    assert!(src.contains(r#"out->name = "zlib";"#), "package name missing:\n{src}");
    assert!(src.contains(r#"{ "crc32", jade_shim_crc32 }"#), "binding missing:\n{src}");
}

#[test]
fn declares_the_target_symbol_with_its_prototype() {
    let src = generate("m", &symbols(&[("hypot", sym(&["float", "float"], "float"))])).unwrap();
    assert!(src.contains("extern double hypot(double, double);"), "bad decl:\n{src}");
}

#[test]
fn a_zero_arg_function_declares_void() {
    let src = generate("t", &symbols(&[("now", sym(&[], "int"))])).unwrap();
    assert!(src.contains("extern int64_t now(void);"), "bad decl:\n{src}");
    assert!(src.contains("if (argc != 0) return 1;"), "missing arity check:\n{src}");
}

#[test]
fn wrappers_check_arity_and_tags_before_calling() {
    // Without the tag check the union would reinterpret the bytes and hand the
    // C function garbage.
    let src = generate("z", &symbols(&[("f", sym(&["int", "str"], "bool"))])).unwrap();

    assert!(src.contains("if (argc != 2) return 1;"), "missing arity check:\n{src}");
    assert!(src.contains("if (argv[0].tag != JADE_FFI_INT) return 1;"), "missing tag check:\n{src}");
    assert!(src.contains("if (argv[1].tag != JADE_FFI_STR) return 1;"), "missing tag check:\n{src}");
    assert!(src.contains("f(argv[0].data.as_int, argv[1].data.as_str)"), "bad call:\n{src}");
}

#[test]
fn a_nil_return_calls_without_capturing_a_result() {
    let src = generate("z", &symbols(&[("reset", sym(&["int"], "nil"))])).unwrap();
    assert!(src.contains("extern void reset(int64_t);"), "bad decl:\n{src}");
    assert!(src.contains("out->tag = JADE_FFI_NIL;"), "should return nil:\n{src}");
    assert!(!src.contains("= reset("), "void call must not be assigned:\n{src}");
}

#[test]
fn rejects_an_unrepresentable_argument_type() {
    // The FFI has no array; silently marshalling it to nil is exactly the
    // failure mode this generator exists to avoid.
    let err = generate("z", &symbols(&[("f", sym(&["array"], "int"))])).unwrap_err();
    assert!(err.contains("'f'"), "error should name the symbol: {err}");
    assert!(err.contains("array"), "error should name the type: {err}");
    assert!(err.contains("Supported types"), "error should list what works: {err}");
}

#[test]
fn rejects_an_unrepresentable_return_type() {
    let err = generate("z", &symbols(&[("f", sym(&[], "dict"))])).unwrap_err();
    assert!(err.contains("dict"), "error should name the type: {err}");
}

#[test]
fn rejects_nil_as_an_argument_type() {
    // `nil` is meaningful only as "returns nothing".
    assert!(generate("z", &symbols(&[("f", sym(&["nil"], "int"))])).is_err());
}

#[test]
fn rejects_an_empty_symbol_table() {
    let err = generate("z", &HashMap::new()).unwrap_err();
    assert!(err.contains("no symbols"), "unexpected message: {err}");
}

#[test]
fn output_is_deterministic() {
    // HashMap iteration order must not leak into the generated file, or every
    // reinstall would recompile and churn the shim.
    let syms = symbols(&[
        ("zeta", sym(&["int"], "int")),
        ("alpha", sym(&["str"], "bool")),
        ("mid", sym(&[], "float")),
    ]);
    let first = generate("z", &syms).unwrap();
    for _ in 0..5 {
        assert_eq!(generate("z", &syms).unwrap(), first);
    }
    // ...and sorted, so the order is predictable rather than merely stable.
    let a = first.find("jade_shim_alpha").unwrap();
    let m = first.find("jade_shim_mid").unwrap();
    let z = first.find("jade_shim_zeta").unwrap();
    assert!(a < m && m < z, "symbols should be emitted in sorted order");
}

#[test]
fn every_ffi_type_maps() {
    for t in ["int", "float", "bool", "str"] {
        assert!(map_type(t).is_some(), "{t} should map");
    }
    assert!(map_type("nil").is_none(), "nil is a return-only spelling");
}

// ── Failure conventions and errno ─────────────────────────────────────────
//
// Without these, a failed C call returns its raw sentinel and the reason — which
// the library already put in errno — is thrown away. The Jade program sees -1
// and nothing else.

use crate::project::CFailure;

#[test]
fn a_null_convention_tests_the_return_and_reports_errno() {
    let src = generate("z", &symbols(&[("gzopen", failing_sym(&["str", "str"], "int", CFailure::Null))])).unwrap();
    assert!(src.contains("errno = 0;"), "errno must be cleared before the call:\n{src}");
    assert!(src.contains("if (!(r)) {"), "missing null test:\n{src}");
    assert!(src.contains("out->tag = JADE_FFI_ERROR;"), "failure must raise:\n{src}");
    assert!(src.contains("jade_shim_errmsg()"), "must report the reason:\n{src}");
}

#[test]
fn each_convention_emits_its_own_test() {
    let cases = [
        (CFailure::Null, "if (!(r)) {"),
        (CFailure::Negative, "if ((r) < 0) {"),
        (CFailure::Nonzero, "if ((r) != 0) {"),
    ];
    for (conv, expect) in cases {
        let src = generate("l", &symbols(&[("f", failing_sym(&["int"], "int", conv))])).unwrap();
        assert!(src.contains(expect), "{conv:?} should emit `{expect}`:\n{src}");
    }
}

#[test]
fn a_symbol_that_cannot_fail_does_not_touch_errno() {
    // Every call paying for an errno read would be a cost on the common path,
    // and a symbol with no convention has no sentinel to test anyway.
    let src = generate("m", &symbols(&[("hypot", sym(&["float", "float"], "float"))])).unwrap();
    assert!(!src.contains("errno = 0;"), "no convention means no errno handling:\n{src}");
    assert!(!src.contains("JADE_FFI_ERROR;"), "nothing should raise:\n{src}");
}

#[test]
fn never_is_the_same_as_omitting_the_key() {
    let never = generate("m", &symbols(&[("f", failing_sym(&["int"], "int", CFailure::Never))])).unwrap();
    let absent = generate("m", &symbols(&[("f", sym(&["int"], "int"))])).unwrap();
    assert_eq!(never, absent);
}

#[test]
fn a_void_symbol_cannot_declare_a_failure_convention() {
    // There is no return value to test, so the declaration could only be
    // silently ignored. Naming it is better.
    let err = generate("l", &symbols(&[("f", failing_sym(&["int"], "nil", CFailure::Negative))]))
        .expect_err("nil + fails_when should be refused");
    assert!(err.contains("returns nil"), "message should say why: {err}");
    assert!(err.contains("drop `fails_when`"), "message should name a fix: {err}");

    // `never` on a void symbol is fine — it asserts what is already true.
    assert!(generate("l", &symbols(&[("f", failing_sym(&["int"], "nil", CFailure::Never))])).is_ok());
}

#[test]
fn the_generated_shim_compiles() {
    // The rest of these tests assert on the text of the C. This one asserts the
    // C is *valid*, which is the property that actually matters and the one a
    // string check cannot reach — an unbalanced brace or a missing include
    // passes every assertion above and fails at install time.
    let syms = symbols(&[
        ("gzopen", failing_sym(&["str", "str"], "int", CFailure::Null)),
        ("gzread", failing_sym(&["int", "int"], "int", CFailure::Negative)),
        ("gzclose", failing_sym(&["int"], "int", CFailure::Nonzero)),
        ("crc32", sym(&["int", "str"], "int")),
        ("noop", sym(&[], "nil")),
    ]);
    let src = generate("z", &syms).unwrap();

    let dir = std::env::temp_dir().join(format!("jade-cshim-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let c = dir.join("shim.c");
    std::fs::write(&c, &src).unwrap();

    let out = std::process::Command::new("cc")
        .args(["-c", "-Wall", "-Werror", "-o"])
        .arg(dir.join("shim.o"))
        .arg(&c)
        .output()
        .expect("cc must be available — it is already required to bind a C library");

    assert!(
        out.status.success(),
        "generated shim does not compile:\n{}\n--- source ---\n{src}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}
