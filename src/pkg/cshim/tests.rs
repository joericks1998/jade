use super::*;

fn sym(args: &[&str], ret: &str) -> CSymbol {
    CSymbol { args: args.iter().map(|s| s.to_string()).collect(), ret: ret.to_string() }
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
