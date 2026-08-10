use super::*;

fn sym(args: &[&str], ret: &str) -> CSymbol {
    CSymbol {
        args: args.iter().map(|s| s.to_string()).collect(),
        ret: ret.to_string(),
        fails_when: None,
        frees_with: None,
    }
}

/// The same, with a declared failure convention.
fn failing_sym(args: &[&str], ret: &str, f: crate::project::CFailure) -> CSymbol {
    CSymbol { fails_when: Some(f), ..sym(args, ret) }
}

fn symbols(pairs: &[(&str, CSymbol)]) -> HashMap<String, CSymbol> {
    pairs.iter().map(|(n, s)| (n.to_string(), s.clone())).collect()
}

/// The common case: no struct out-parameters, so no structs table and no
/// headers. Shadows the glob-imported `generate` so the tests that predate
/// out-parameters read the way they always did.
fn generate(name: &str, syms: &HashMap<String, CSymbol>) -> Result<String, String> {
    super::generate(name, syms, &HashMap::new(), &[])
}

/// With a structs table and headers, for the out-parameter tests.
fn generate_with(
    name: &str,
    syms: &HashMap<String, CSymbol>,
    structs: &[(&str, &[(&str, &str)])],
    headers: &[&str],
) -> Result<String, String> {
    let structs: HashMap<String, crate::project::CStruct> = structs
        .iter()
        .map(|(n, fields)| {
            (
                n.to_string(),
                crate::project::CStruct {
                    fields: fields.iter().map(|(f, t)| (f.to_string(), t.to_string())).collect(),
                    held: false,
                    buffers: Vec::new(),
                },
            )
        })
        .collect();
    let headers: Vec<String> = headers.iter().map(|h| h.to_string()).collect();
    super::generate(name, syms, &structs, &headers)
}

/// The same, for a struct Jade holds rather than one it builds.
fn generate_held(
    name: &str,
    syms: &HashMap<String, CSymbol>,
    type_name: &str,
    fields: &[(&str, &str)],
    buffers: &[(&str, &str, bool)],
) -> Result<String, String> {
    let def = crate::project::CStruct {
        fields: fields.iter().map(|(f, t)| (f.to_string(), t.to_string())).collect(),
        held: true,
        buffers: buffers
            .iter()
            .map(|(p, l, w)| crate::project::CBuffer {
                ptr: p.to_string(),
                len: l.to_string(),
                writable: *w,
            })
            .collect(),
    };
    let structs: HashMap<String, crate::project::CStruct> =
        [(type_name.to_string(), def)].into_iter().collect();
    super::generate(name, syms, &structs, &["fixture.h".to_string()])
}

#[test]
fn generates_a_pkg_init_and_binding_table() {
    let src =
        generate("zlib", &symbols(&[("crc32", sym(&["scalar:int64_t", "str"], "scalar:int64_t"))]))
            .unwrap();

    assert!(src.contains("int jade_pkg_init(JadeNativePkg* out)"), "no init:\n{src}");
    assert!(src.contains(r#"out->name = "zlib";"#), "package name missing:\n{src}");
    assert!(src.contains(r#"{ "crc32", jade_shim_crc32 }"#), "binding missing:\n{src}");
}

#[test]
fn declares_the_target_symbol_with_its_prototype() {
    let src = generate(
        "m",
        &symbols(&[("hypot", sym(&["scalar:double", "scalar:double"], "scalar:double"))]),
    )
    .unwrap();
    assert!(src.contains("extern double hypot(double, double);"), "bad decl:\n{src}");
}

#[test]
fn a_zero_arg_function_declares_void() {
    let src = generate("t", &symbols(&[("now", sym(&[], "scalar:int64_t"))])).unwrap();
    assert!(src.contains("extern int64_t now(void);"), "bad decl:\n{src}");
    assert!(src.contains("if (argc != 0) return 1;"), "missing arity check:\n{src}");
}

#[test]
fn wrappers_check_arity_and_tags_before_calling() {
    // Without the tag check the union would reinterpret the bytes and hand the
    // C function garbage.
    let src =
        generate("z", &symbols(&[("f", sym(&["scalar:int64_t", "str"], "scalar:bool"))])).unwrap();

    assert!(src.contains("if (argc != 2) return 1;"), "missing arity check:\n{src}");
    assert!(
        src.contains("if (argv[0].tag != JADE_FFI_INT) return 1;"),
        "missing tag check:\n{src}"
    );
    assert!(
        src.contains("if (argv[1].tag != JADE_FFI_STR) return 1;"),
        "missing tag check:\n{src}"
    );
    assert!(
        src.contains("(f)((int64_t)argv[0].data.as_int, argv[1].data.as_str)"),
        "bad call:\n{src}"
    );
}

#[test]
fn a_nil_return_calls_without_capturing_a_result() {
    let src = generate("z", &symbols(&[("reset", sym(&["scalar:int64_t"], "nil"))])).unwrap();
    assert!(src.contains("extern void reset(int64_t);"), "bad decl:\n{src}");
    assert!(src.contains("out->tag = JADE_FFI_NIL;"), "should return nil:\n{src}");
    assert!(!src.contains("= reset("), "void call must not be assigned:\n{src}");
}

#[test]
fn rejects_an_unrepresentable_argument_type() {
    // The FFI has no array; silently marshalling it to nil is exactly the
    // failure mode this generator exists to avoid.
    let err = generate("z", &symbols(&[("f", sym(&["array"], "scalar:int64_t"))])).unwrap_err();
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
    assert!(generate("z", &symbols(&[("f", sym(&["nil"], "scalar:int64_t"))])).is_err());
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
        ("zeta", sym(&["scalar:int64_t"], "scalar:int64_t")),
        ("alpha", sym(&["str"], "scalar:bool")),
        ("mid", sym(&[], "scalar:double")),
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
    let src = generate(
        "z",
        &symbols(&[("gzopen", failing_sym(&["str", "str"], "scalar:int64_t", CFailure::Null))]),
    )
    .unwrap();
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
        let src = generate(
            "l",
            &symbols(&[("f", failing_sym(&["scalar:int64_t"], "scalar:int64_t", conv))]),
        )
        .unwrap();
        assert!(src.contains(expect), "{conv:?} should emit `{expect}`:\n{src}");
    }
}

#[test]
fn a_symbol_that_cannot_fail_does_not_touch_errno() {
    // Every call paying for an errno read would be a cost on the common path,
    // and a symbol with no convention has no sentinel to test anyway.
    let src = generate(
        "m",
        &symbols(&[("hypot", sym(&["scalar:double", "scalar:double"], "scalar:double"))]),
    )
    .unwrap();
    assert!(!src.contains("errno = 0;"), "no convention means no errno handling:\n{src}");
    assert!(!src.contains("JADE_FFI_ERROR;"), "nothing should raise:\n{src}");
}

#[test]
fn never_is_the_same_as_omitting_the_key() {
    let never = generate(
        "m",
        &symbols(&[("f", failing_sym(&["scalar:int64_t"], "scalar:int64_t", CFailure::Never))]),
    )
    .unwrap();
    let absent =
        generate("m", &symbols(&[("f", sym(&["scalar:int64_t"], "scalar:int64_t"))])).unwrap();
    assert_eq!(never, absent);
}

#[test]
fn a_void_symbol_cannot_declare_a_failure_convention() {
    // There is no return value to test, so the declaration could only be
    // silently ignored. Naming it is better.
    let err = generate(
        "l",
        &symbols(&[("f", failing_sym(&["scalar:int64_t"], "nil", CFailure::Negative))]),
    )
    .expect_err("nil + fails_when should be refused");
    assert!(err.contains("returns nil"), "message should say why: {err}");
    assert!(err.contains("drop `fails_when`"), "message should name a fix: {err}");

    // `never` on a void symbol is fine — it asserts what is already true.
    assert!(
        generate("l", &symbols(&[("f", failing_sym(&["scalar:int64_t"], "nil", CFailure::Never))]))
            .is_ok()
    );
}

#[test]
fn the_generated_shim_compiles() {
    // The rest of these tests assert on the text of the C. This one asserts the
    // C is *valid*, which is the property that actually matters and the one a
    // string check cannot reach — an unbalanced brace or a missing include
    // passes every assertion above and fails at install time.
    let syms = symbols(&[
        ("gzopen", failing_sym(&["str", "str"], "scalar:int64_t", CFailure::Null)),
        (
            "gzread",
            failing_sym(
                &["scalar:int64_t", "scalar:int64_t"],
                "scalar:int64_t",
                CFailure::Negative,
            ),
        ),
        ("gzclose", failing_sym(&["scalar:int64_t"], "scalar:int64_t", CFailure::Nonzero)),
        ("crc32", sym(&["scalar:int64_t", "str"], "scalar:int64_t")),
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

// ── Buffers ──────────────────────────────────────────────────────────────
//
// The Jade-facing arity is deliberately smaller than the C one. `x_read(h, buf,
// n)` is called from Jade as `x_read(h, n)` and hands back the bytes, because a
// Jade blob is immutable — three methods, none of them a write — and letting a
// C library scribble into one would break that for the FFI's convenience.

#[test]
fn an_input_blob_becomes_a_pointer_and_a_length() {
    let src = generate("z", &symbols(&[("put", sym(&["bytes"], "scalar:int64_t"))])).unwrap();
    assert!(src.contains("extern int64_t put(const void*, size_t);"), "bad decl:\n{src}");
    assert!(
        src.contains("if (argv[0].tag != JADE_FFI_BYTES) return 1;"),
        "missing tag check:\n{src}"
    );
    assert!(src.contains("as_bytes->data"), "should pass the pointer:\n{src}");
    assert!(src.contains("as_bytes->len"), "should pass the length:\n{src}");
    // One Jade argument, two C parameters.
    assert!(src.contains("if (argc != 1) return 1;"), "arity should be the Jade one:\n{src}");
}

#[test]
fn a_blob_with_no_length_becomes_one_pointer() {
    // The libfdt shape: the extent is written inside the blob, so there is
    // nowhere to pass a size and the pointer goes on its own.
    let src =
        generate("fdt", &symbols(&[("check", sym(&["bytes_ptr"], "scalar:int64_t"))])).unwrap();
    assert!(src.contains("extern int64_t check(const void*);"), "bad decl:\n{src}");
    assert!(src.contains("if (argv[0].tag != JADE_FFI_BYTES) return 1;"), "no tag check:\n{src}");
    assert!(src.contains("(check)(argv[0].data.as_bytes"), "should pass the pointer:\n{src}");
    assert!(!src.contains("as_bytes->len"), "must not invent a length:\n{src}");
}

#[test]
fn a_lengthless_blob_shim_compiles() {
    let syms = symbols(&[
        ("check", sym(&["bytes_ptr"], "scalar:int64_t")),
        ("at", sym(&["bytes_ptr", "str"], "scalar:int64_t")),
    ]);
    let src = generate("fdt", &syms).unwrap();
    if let Err(e) = compiles(&src, &[]) {
        panic!("lengthless blob shim does not compile:\n{e}\n--- source ---\n{src}");
    }
}

#[test]
fn a_blob_revised_in_place_is_copied_and_handed_back() {
    // A Jade blob is immutable, so the library gets scratch of the caller's
    // bytes rather than the caller's bytes, and the edit comes back as a return.
    let src = generate(
        "fdt",
        &symbols(&[("nop", sym(&["inout_bytes", "scalar:int64_t"], "scalar:int64_t"))]),
    )
    .unwrap();
    assert!(src.contains("extern int64_t nop(void*, int64_t);"), "bad decl:\n{src}");
    assert!(
        src.contains("memcpy(iobuf0, argv[0].data.as_bytes->data, iolen0);"),
        "no copy:\n{src}"
    );
    assert!(src.contains("jade_shim_bytes(iobuf0, iolen0)"), "not handed back:\n{src}");
    assert!(src.contains("free(iobuf0);"), "scratch leaked:\n{src}");
    // One argument in, and a pair back: the status and the edited blob.
    assert!(src.contains(r#"jade_shim_struct("nop_result", 2)"#), "should pair:\n{src}");
}

#[test]
fn two_revised_blobs_free_both_when_the_call_raises() {
    // The cleanup string used to be assigned rather than appended, so whichever
    // buffer was declared first leaked on the raise path.
    let s = failing_sym(&["inout_bytes@a", "inout_bytes@b"], "scalar:int64_t", CFailure::Nonzero);
    let src = generate("fdt", &symbols(&[("apply", s)])).unwrap();
    let raise = src.split("out->tag = JADE_FFI_ERROR").next().unwrap_or_default();
    assert!(raise.contains("free(iobuf0);") && raise.contains("free(iobuf1);"), "leak:\n{src}");
}

#[test]
fn a_revised_blob_shim_compiles() {
    let syms =
        symbols(&[("nop", sym(&["inout_bytes", "scalar:int64_t", "str"], "scalar:int64_t"))]);
    let src = generate("fdt", &syms).unwrap();
    if let Err(e) = compiles(&src, &[]) {
        panic!("in-place blob shim does not compile:\n{e}\n--- source ---\n{src}");
    }
}

#[test]
fn a_null_blob_passes_null_and_zero_rather_than_dereferencing() {
    let src = generate("z", &symbols(&[("put", sym(&["bytes"], "scalar:int64_t"))])).unwrap();
    assert!(src.contains("? (const void*)argv[0].data.as_bytes->data : NULL"), "unguarded:\n{src}");
}

#[test]
fn an_out_buffer_takes_no_jade_argument_and_returns_bytes() {
    let s = failing_sym(
        &["scalar:int64_t", "out_buffer:short", "scalar:int64_t"],
        "scalar:int64_t",
        CFailure::Negative,
    );
    let src = generate("snd", &symbols(&[("sf_read_short", s)])).unwrap();

    // Three C parameters, two Jade arguments: the buffer is the shim's.
    assert!(
        src.contains("extern int64_t sf_read_short(int64_t, short*, int64_t);"),
        "bad decl:\n{src}"
    );
    assert!(
        src.contains("if (argc != 2) return 1;"),
        "buffer must not consume an argument:\n{src}"
    );

    // Sized from the argument after it, which is the element count.
    assert!(src.contains("int64_t n_elem1 = argv[1].data.as_int;"), "wrong count source:\n{src}");
    assert!(src.contains("sizeof(short)"), "must size by the element type:\n{src}");

    // The return value is the fill count, so it sizes the blob rather than
    // coming back separately.
    assert!(src.contains("out->tag = JADE_FFI_BYTES;"), "should return bytes:\n{src}");
    assert!(src.contains("free(obuf1);"), "scratch must be released:\n{src}");
}

#[test]
fn a_failing_out_buffer_call_frees_its_scratch_before_raising() {
    // A raise that leaks the scratch would leak once per failed call, which on
    // a read loop hitting EOF is every iteration.
    let s = failing_sym(
        &["scalar:int64_t", "out_buffer:char", "scalar:int64_t"],
        "scalar:int64_t",
        CFailure::Negative,
    );
    let src = generate("z", &symbols(&[("rd", s)])).unwrap();
    let fail_block = &src[src.find("if ((r) < 0)").expect("failure test")..];
    let raise_at = fail_block.find("JADE_FFI_ERROR").unwrap();
    let free_at = fail_block.find("free(obuf1);").unwrap();
    assert!(free_at < raise_at, "scratch must be freed before the error return:\n{src}");
}

#[test]
fn a_short_read_is_clamped_to_what_was_allocated() {
    // A library reporting more than it was given would otherwise make the copy
    // read past the scratch.
    let s = sym(&["scalar:int64_t", "out_buffer:char", "scalar:int64_t"], "scalar:int64_t");
    let src = generate("z", &symbols(&[("rd", s)])).unwrap();
    assert!(src.contains("r > n_elem1 ? n_elem1 : r"), "missing clamp:\n{src}");
}

#[test]
fn an_out_buffer_needs_a_count_after_it() {
    let s = sym(&["scalar:int64_t", "out_buffer:char"], "scalar:int64_t");
    let err = generate("z", &symbols(&[("rd", s)])).unwrap_err();
    assert!(err.contains("followed by an `int`"), "unexpected: {err}");
    assert!(err.contains("how many"), "should say what the count is for: {err}");
}

#[test]
fn an_out_buffer_symbol_must_return_the_count() {
    let s = sym(&["out_buffer:char", "scalar:int64_t"], "str");
    let err = generate("z", &symbols(&[("rd", s)])).unwrap_err();
    assert!(err.contains("number of elements written"), "unexpected: {err}");
}

#[test]
fn at_most_one_out_parameter_may_read_the_c_return_value() {
    // Two out_buffers would both want the return value as their element count,
    // and there is only one of it.
    let s = sym(
        &["out_buffer:char", "scalar:int64_t", "out_buffer:char", "scalar:int64_t"],
        "scalar:int64_t",
    );
    let err = generate("z", &symbols(&[("rd", s)])).unwrap_err();
    assert!(err.contains("both read the C return value"), "unexpected: {err}");
}

#[test]
fn two_out_parameters_must_each_be_named() {
    let s = sym(&["out_scalar:int", "out_scalar:int"], "nil");
    let err = generate("z", &symbols(&[("f", s)])).unwrap_err();
    assert!(err.contains("needs \na name") || err.contains("needs a name"), "unexpected: {err}");
}

#[test]
fn two_out_parameters_may_not_share_a_name() {
    let s = sym(&["out_scalar:int@a", "out_scalar:int@a"], "nil");
    let err = generate("z", &symbols(&[("f", s)])).unwrap_err();
    assert!(err.contains("names two out-parameters"), "unexpected: {err}");
}

#[test]
fn ret_is_reserved_as_an_out_parameter_name() {
    let s = sym(&["out_scalar:int@ret", "out_scalar:int@b"], "scalar:int64_t");
    let err = generate("z", &symbols(&[("f", s)])).unwrap_err();
    assert!(err.contains("reserved"), "unexpected: {err}");
}

#[test]
fn a_c_type_that_is_not_an_identifier_is_refused() {
    // The text goes straight into generated C, so this is an injection guard as
    // much as a typo guard.
    for bad in ["short; evil()", "char*", "1int", ""] {
        let s = sym(&[format!("out_buffer:{bad}").as_str(), "scalar:int64_t"], "scalar:int64_t");
        assert!(generate("z", &symbols(&[("rd", s)])).is_err(), "should refuse out_buffer:{bad}");
    }
}

// ── Struct out-parameters ────────────────────────────────────────────────

const SF_INFO: &[(&str, &str)] = &[("frames", "int"), ("samplerate", "int"), ("channels", "int")];

#[test]
fn a_struct_out_parameter_is_a_zeroed_local_passed_by_address() {
    let s = failing_sym(&["str", "int", "out_struct:SF_INFO"], "int", CFailure::Null);
    let src =
        generate_with("snd", &symbols(&[("sf_open", s)]), &[("SF_INFO", SF_INFO)], &["sndfile.h"])
            .unwrap();

    assert!(src.contains("#include <sndfile.h>"), "must include the header:\n{src}");
    assert!(src.contains("SF_INFO ostruct2;"), "must declare a real local:\n{src}");
    assert!(src.contains("memset(&ostruct2, 0, sizeof ostruct2);"), "must zero it:\n{src}");
    assert!(src.contains("&ostruct2"), "must pass its address:\n{src}");
    assert!(src.contains("if (argc != 2) return 1;"), "out-param takes no Jade arg:\n{src}");
}

#[test]
fn the_header_declares_the_symbol_rather_than_the_shim() {
    // A hand-written prototype that disagrees with the real one — `int` where
    // the library says `long` — truncates silently at run time. Letting the
    // header win turns that into a compile error, which is the whole reason to
    // require one.
    let s = sym(&["str", "int", "out_struct:SF_INFO"], "int");
    let src =
        generate_with("snd", &symbols(&[("sf_open", s)]), &[("SF_INFO", SF_INFO)], &["sndfile.h"])
            .unwrap();
    assert!(!src.contains("extern int64_t sf_open"), "must not redeclare:\n{src}");
}

#[test]
fn a_returned_value_and_a_filled_struct_come_back_as_ret_and_out() {
    let s = sym(&["str", "int", "out_struct:SF_INFO"], "int");
    let src =
        generate_with("snd", &symbols(&[("sf_open", s)]), &[("SF_INFO", SF_INFO)], &["sndfile.h"])
            .unwrap();
    assert!(src.contains(r#"jade_shim_struct("sf_open_result", 2)"#), "missing pair:\n{src}");
    assert!(src.contains(r#"res->keys[0] = strdup("ret");"#), "missing ret:\n{src}");
    assert!(src.contains(r#"res->keys[1] = strdup("out");"#), "missing out:\n{src}");
}

#[test]
fn a_void_call_returns_the_filled_struct_directly() {
    // With nothing else to report there is no pair to make, so the common case
    // stays clean rather than paying for the general one.
    let s = sym(&["out_struct:SF_INFO"], "nil");
    let src =
        generate_with("snd", &symbols(&[("stat_it", s)]), &[("SF_INFO", SF_INFO)], &["sndfile.h"])
            .unwrap();
    assert!(!src.contains("_result"), "should not wrap:\n{src}");
    assert!(src.contains("out->data.as_struct = ostruct0_j;"), "should return it directly:\n{src}");
}

#[test]
fn struct_field_strings_are_copied_not_borrowed() {
    // A value inside a container is container-owned, so Jade's ffi_free frees
    // it. Handing over a pointer into a stack local would be a free of the
    // stack.
    let s = sym(&["out_struct:INFO"], "nil");
    let src = generate_with("z", &symbols(&[("f", s)]), &[("INFO", &[("name", "str")])], &["z.h"])
        .unwrap();
    assert!(src.contains("strdup((ostruct0.name)"), "field string must be copied:\n{src}");
}

#[test]
fn a_struct_out_parameter_needs_its_fields_declared() {
    let s = sym(&["out_struct:SF_INFO"], "nil");
    let err = generate_with("snd", &symbols(&[("f", s)]), &[], &["sndfile.h"]).unwrap_err();
    assert!(err.contains("structs.SF_INFO"), "should name the missing table: {err}");
}

#[test]
fn a_struct_out_parameter_needs_a_header() {
    // Without one the shim would have to synthesize the layout from the field
    // list, and any disagreement writes at the wrong offset.
    let s = sym(&["out_struct:SF_INFO"], "nil");
    let err =
        generate_with("snd", &symbols(&[("f", s)]), &[("SF_INFO", SF_INFO)], &[]).unwrap_err();
    assert!(err.contains("headers"), "should ask for a header: {err}");
    assert!(err.contains("wrong offsets"), "should say why: {err}");
}

// ── A returned pointer, sized by a parameter ─────────────────────────────

#[test]
fn a_returned_pointer_is_sized_by_its_ret_len_parameter() {
    // The mirror of out_buffer: there the return value is the count, here the
    // bytes are the return value and the count comes back through a parameter.
    let mut s = sym(&["bytes_ptr", "str", "ret_len:int"], "bytes");
    s.ret = "bytes".to_string();
    let src = generate("fdt", &symbols(&[("getprop", s)])).unwrap();
    assert!(src.contains("extern const void* getprop("), "bad decl:\n{src}");
    assert!(src.contains("int rlen2 = (int)0;"), "no length local:\n{src}");
    assert!(src.contains("jade_shim_bytes(r, (size_t)rlen2)"), "not sized by it:\n{src}");
    // The parameter is not a Jade argument and not a result of its own.
    assert!(src.contains("if (argc != 2) return 1;"), "wrong arity:\n{src}");
    assert!(!src.contains("_result"), "should not pair:\n{src}");
}

#[test]
fn a_returned_pointer_that_is_null_or_negative_comes_back_as_nil() {
    // How these signal "nothing": libfdt returns NULL and writes a negative
    // error code through the length.
    let s = sym(&["bytes_ptr", "ret_len:int"], "bytes");
    let src = generate("fdt", &symbols(&[("getprop", s)])).unwrap();
    assert!(src.contains("if (!r || (int64_t)rlen1 < 0)"), "unguarded:\n{src}");
}

#[test]
fn a_returned_blob_and_its_length_only_mean_anything_together() {
    let lone = sym(&["bytes_ptr", "ret_len:int"], "scalar:int64_t");
    let err = generate("fdt", &symbols(&[("f", lone)])).unwrap_err();
    assert!(err.contains("must be `bytes`"), "should refuse a stray length: {err}");

    let bare = sym(&["bytes_ptr"], "bytes");
    let err = generate("fdt", &symbols(&[("f", bare)])).unwrap_err();
    assert!(err.contains("how long it is"), "should refuse a stray blob: {err}");

    let two = sym(&["ret_len:int", "ret_len:int"], "bytes");
    let err = generate("fdt", &symbols(&[("f", two)])).unwrap_err();
    assert!(err.contains("only one of them"), "should refuse two lengths: {err}");
}

#[test]
fn a_returned_blob_shim_compiles() {
    let syms = symbols(&[(
        "getprop",
        sym(&["bytes_ptr", "scalar:int64_t", "str", "ret_len:int"], "bytes"),
    )]);
    let src = generate("fdt", &syms).unwrap();
    if let Err(e) = compiles(&src, &[]) {
        panic!("returned blob shim does not compile:\n{e}\n--- source ---\n{src}");
    }
}

#[test]
fn a_struct_input_is_copied_into_a_real_c_local() {
    let s = sym(&["in_struct:SF_INFO", "int"], "int");
    let src = generate_with("snd", &symbols(&[("f", s)]), &[("SF_INFO", SF_INFO)], &["sndfile.h"])
        .unwrap();
    assert!(src.contains("SF_INFO istruct0;"), "no local of the real type:\n{src}");
    assert!(src.contains("memset(&istruct0, 0, sizeof istruct0);"), "not zeroed:\n{src}");
    assert!(src.contains("(f)(&istruct0, argv[1].data.as_int)"), "not passed by address:\n{src}");
}

#[test]
fn a_struct_input_takes_one_jade_argument() {
    // Unlike an out_struct, which takes none: the caller supplies this one.
    let s = sym(&["in_struct:SF_INFO"], "int");
    let src = generate_with("snd", &symbols(&[("f", s)]), &[("SF_INFO", SF_INFO)], &["sndfile.h"])
        .unwrap();
    assert!(src.contains("if (argc != 1) return 1;"), "wrong arity:\n{src}");
    assert!(src.contains("if (argv[0].tag != JADE_FFI_STRUCT) return 1;"), "no tag check:\n{src}");
}

#[test]
fn a_field_left_out_of_a_struct_input_stays_zero() {
    // What the C it stands in for does: declare, zero, set what matters. A
    // struct with fifteen reserved fields the library requires to be zero would
    // be unusable if every one had to be written out.
    let s = sym(&["in_struct:SF_INFO"], "int");
    let src = generate_with("snd", &symbols(&[("f", s)]), &[("SF_INFO", SF_INFO)], &["sndfile.h"])
        .unwrap();
    assert!(src.contains("if (istruct0_0) {"), "a missing field must not fail:\n{src}");
}

#[test]
fn a_field_the_struct_does_not_have_is_refused_by_name() {
    // The mistake worth catching. Without this a misspelling is indistinguishable
    // from an omission, and silently becomes a zero the caller believed they set.
    let s = sym(&["in_struct:SF_INFO"], "int");
    let src = generate_with("snd", &symbols(&[("f", s)]), &[("SF_INFO", SF_INFO)], &["sndfile.h"])
        .unwrap();
    assert!(src.contains("jade_shim_nofield(\"SF_INFO\""), "no unknown-key check:\n{src}");
    assert!(src.contains("jade_shim_known(istruct0_names"), "no name table:\n{src}");
}

#[test]
fn a_struct_input_needs_its_fields_declared_and_a_header() {
    // Exactly what an out_struct needs, and for the same reason: the layout has
    // to come from the compiler rather than from a hand-written field list.
    let s = sym(&["in_struct:SF_INFO"], "int");
    let err = generate_with("snd", &symbols(&[("f", s.clone())]), &[], &["sndfile.h"]).unwrap_err();
    assert!(err.contains("structs.SF_INFO"), "should name the missing table: {err}");

    let err =
        generate_with("snd", &symbols(&[("f", s)]), &[("SF_INFO", SF_INFO)], &[]).unwrap_err();
    assert!(err.contains("headers"), "should ask for a header: {err}");
}

#[test]
fn a_struct_input_cannot_be_named_like_a_result() {
    // `@name` is the key an out-parameter comes back under. An in_struct is an
    // argument, so accepting one would read as a result that never arrives.
    let s = sym(&["in_struct:SF_INFO@cfg"], "int");
    let err = generate_with("snd", &symbols(&[("f", s)]), &[("SF_INFO", SF_INFO)], &["sndfile.h"])
        .unwrap_err();
    assert!(err.contains("produces nothing"), "should say why: {err}");
}

#[test]
fn a_header_name_that_is_not_a_path_is_refused() {
    let s = sym(&["int"], "int");
    for bad in ["foo.h>\n#include <evil.h", "a\"b", ""] {
        assert!(
            generate_with("z", &symbols(&[("f", s.clone())]), &[], &[bad]).is_err(),
            "should refuse header {bad:?}"
        );
    }
}

#[test]
fn helpers_are_emitted_only_when_something_uses_them() {
    let plain = generate(
        "m",
        &symbols(&[("hypot", sym(&["scalar:double", "scalar:double"], "scalar:double"))]),
    )
    .unwrap();
    assert!(!plain.contains("jade_shim_bytes"), "unused helper emitted:\n{plain}");
    assert!(!plain.contains("jade_shim_struct"), "unused helper emitted:\n{plain}");

    let buf = generate(
        "z",
        &symbols(&[("rd", sym(&["out_buffer:char", "scalar:int64_t"], "scalar:int64_t"))]),
    )
    .unwrap();
    assert!(buf.contains("static JadeBytes* jade_shim_bytes"), "helper missing:\n{buf}");
    assert!(!buf.contains("jade_shim_struct"), "struct helper not needed:\n{buf}");
}

/// Compile generated C, with an optional extra include directory. Returns
/// cc's stderr on failure.
///
/// The string assertions above check that the right text is present; only this
/// checks that the result is valid C. An unbalanced brace or a missing include
/// satisfies every `contains` in this file and then fails on a user's machine
/// at install time.
fn compiles(src: &str, extra: &[(&str, &str)]) -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!(
        "jade-cshim-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, body) in extra {
        std::fs::write(dir.join(name), body).unwrap();
    }
    let c = dir.join("shim.c");
    std::fs::write(&c, src).unwrap();

    let out = std::process::Command::new("cc")
        .args(["-c", "-Wall", "-Werror"])
        .arg(format!("-I{}", dir.display()))
        .arg("-o")
        .arg(dir.join("shim.o"))
        .arg(&c)
        .output()
        .expect("cc must be available — it is already required to bind a C library");

    let result = if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    };
    let _ = std::fs::remove_dir_all(&dir);
    result
}

#[test]
fn a_buffer_shim_compiles() {
    let syms = symbols(&[
        (
            "rd",
            failing_sym(
                &["scalar:int64_t", "out_buffer:short", "scalar:int64_t"],
                "scalar:int64_t",
                CFailure::Negative,
            ),
        ),
        ("put", sym(&["bytes", "scalar:int64_t"], "scalar:int64_t")),
        ("put2", sym(&["bytes"], "nil")),
    ]);
    let src = generate("z", &syms).unwrap();
    if let Err(e) = compiles(&src, &[]) {
        panic!("buffer shim does not compile:\n{e}\n--- source ---\n{src}");
    }
}

#[test]
fn a_struct_out_parameter_shim_compiles_against_a_real_header() {
    // The header is what owns the layout, so the test supplies a real one and
    // lets the compiler check the field reads against it — which is the whole
    // argument for requiring headers rather than a declared field layout.
    let header = r#"
#ifndef FIXTURE_H
#define FIXTURE_H
#include <stdint.h>
typedef struct { int64_t frames; int samplerate; int channels; const char* title; } SF_INFO;
extern int sf_open(const char* path, int mode, SF_INFO* info);
extern void sf_stat(SF_INFO* info);
#endif
"#;
    let syms = symbols(&[
        ("sf_open", failing_sym(&["str", "int", "out_struct:SF_INFO"], "int", CFailure::Nonzero)),
        ("sf_stat", sym(&["out_struct:SF_INFO"], "nil")),
    ]);
    let fields: &[(&str, &str)] =
        &[("frames", "int"), ("samplerate", "int"), ("channels", "int"), ("title", "str")];
    let src = generate_with("snd", &syms, &[("SF_INFO", fields)], &["fixture.h"]).unwrap();
    if let Err(e) = compiles(&src, &[("fixture.h", header)]) {
        panic!("struct shim does not compile:\n{e}\n--- source ---\n{src}");
    }
}

#[test]
fn a_struct_input_shim_compiles_against_a_real_header() {
    // Same argument as the out_struct case: the compiler places the fields, so
    // an assignment at a wrong offset is impossible rather than merely unlikely.
    let header = r#"
#ifndef FIXTURE_H
#define FIXTURE_H
#include <stdint.h>
typedef struct { int version; int64_t backward_size; int check; const char* tag; } FLAGS;
extern int cmp(const FLAGS* a, const FLAGS* b);
extern int use_one(const FLAGS* f, int mode);
#endif
"#;
    let syms = symbols(&[
        ("cmp", sym(&["in_struct:FLAGS", "in_struct:FLAGS"], "int")),
        ("use_one", sym(&["in_struct:FLAGS", "int"], "int")),
    ]);
    let fields: &[(&str, &str)] =
        &[("version", "int"), ("backward_size", "int"), ("check", "int"), ("tag", "str")];
    let src = generate_with("lz", &syms, &[("FLAGS", fields)], &["fixture.h"]).unwrap();
    // Two of them in one call must not collide on a local name.
    assert!(src.contains("FLAGS istruct0;") && src.contains("FLAGS istruct1;"), "collide:\n{src}");
    if let Err(e) = compiles(&src, &[("fixture.h", header)]) {
        panic!("struct input shim does not compile:\n{e}\n--- source ---\n{src}");
    }
}

#[test]
fn a_caller_sized_buffer_allocates_what_it_was_asked_for_and_hands_it_all_back() {
    // For the writes whose extent only the documentation gives.
    // `lzma_stream_header_encode(const lzma_stream_flags *, uint8_t *out)`
    // writes exactly twelve bytes and says so nowhere a generator can read.
    let src = generate(
        "lz",
        &symbols(&[("enc", sym(&["sized_buffer:unsigned char"], "scalar:int64_t"))]),
    )
    .unwrap();
    assert!(src.contains("if (argv[0].tag != JADE_FFI_INT) return 1;"), "no count check:\n{src}");
    assert!(src.contains("calloc((size_t)(n_want0 ? n_want0 : 1)"), "not allocated:\n{src}");
    // All of it: the call reports a status, so there is nothing to trim by.
    assert!(src.contains("jade_shim_bytes(sbuf0, (size_t)n_want0"), "not handed back:\n{src}");
    assert!(src.contains("free(sbuf0);"), "leaked:\n{src}");
}

#[test]
fn a_negative_or_absurd_count_is_refused_before_anything_is_allocated() {
    let src = generate(
        "lz",
        &symbols(&[("enc", sym(&["sized_buffer:unsigned char"], "scalar:int64_t"))]),
    )
    .unwrap();
    assert!(src.contains("if (n_want0 < 0)"), "unguarded:\n{src}");
}

#[test]
fn a_caller_sized_buffer_shim_compiles() {
    let syms = symbols(&[(
        "enc",
        sym(&["scalar:int64_t", "sized_buffer:unsigned char"], "scalar:int64_t"),
    )]);
    let src = generate("lz", &syms).unwrap();
    if let Err(e) = compiles(&src, &[]) {
        panic!("sized buffer shim does not compile:\n{e}\n--- source ---\n{src}");
    }
}

// ── Strings handed back through a parameter ──────────────────────────────

#[test]
fn a_borrowed_string_is_copied_inside_a_struct_and_lent_at_the_top() {
    // The ABI's rule rather than a choice: a value inside a container is
    // container-owned and Jade's `ffi_free` releases it, while a top-level
    // string is copied by both engines before the call returns.
    let alone = generate("fdt", &symbols(&[("f", sym(&["out_str:char"], "nil"))])).unwrap();
    assert!(alone.contains("out->data.as_str = (const char*)ostr0;"), "should lend:\n{alone}");

    let paired =
        generate("fdt", &symbols(&[("f", sym(&["out_str:char"], "scalar:int64_t"))])).unwrap();
    assert!(paired.contains("strdup((const char*)ostr0)"), "should copy:\n{paired}");
}

#[test]
fn an_owned_string_is_always_copied_and_always_released() {
    // Borrowing at top level is only safe while the pointer stays valid, and
    // this one stops being valid on the next line.
    // Alone, so it is the whole result and would otherwise be lent out.
    let mut s = sym(&["out_alloc_str:char"], "nil");
    s.frees_with = Some("ares_free_string".to_string());
    let src = generate_with("ar", &symbols(&[("f", s)]), &[], &["ares.h"]).unwrap();
    assert!(src.contains("jade_shim_owned(oastr0)"), "should copy before freeing:\n{src}");
    assert!(src.contains("ares_free_string(oastr0);"), "should release:\n{src}");
}

#[test]
fn an_owned_string_and_its_free_function_only_mean_anything_together() {
    let bare = sym(&["out_alloc_str:char"], "int");
    let err = generate_with("ar", &symbols(&[("f", bare)]), &[], &["ares.h"]).unwrap_err();
    assert!(err.contains("who releases it"), "should ask: {err}");

    let mut stray = sym(&["int"], "int");
    stray.frees_with = Some("free".to_string());
    let err = generate_with("ar", &symbols(&[("f", stray)]), &[], &["ares.h"]).unwrap_err();
    assert!(err.contains("hands nothing back"), "should refuse a stray rule: {err}");
}

#[test]
fn a_string_out_parameter_shim_compiles() {
    let header = "extern int borrowed(const char** namep);\n\
                  extern void take_owned(char** str);\n";
    let mut o = sym(&["out_alloc_str:char"], "nil");
    o.frees_with = Some("free".to_string());
    let syms = symbols(&[("borrowed", sym(&["out_str:char"], "int")), ("take_owned", o)]);
    let src = generate_with("z", &syms, &[], &["fixture.h"]).unwrap();
    if let Err(e) = compiles(&src, &[("fixture.h", header)]) {
        panic!("string out shim does not compile:\n{e}\n--- source ---\n{src}");
    }
}

// ── Strings handed back as the return value ──────────────────────────────

#[test]
fn a_returned_string_is_lent_when_the_library_keeps_it_and_released_when_it_does_not() {
    // The same C and the opposite ownership. `g_basename` points into its
    // argument, `g_strdup` mallocs, and both are written `gchar *` — so the two
    // spellings differ and only one of them frees.
    let lent = generate("glib", &symbols(&[("f", sym(&["str"], "str"))])).unwrap();
    assert!(lent.contains("out->data.as_str = r;"), "should lend:\n{lent}");
    assert!(!lent.contains("jade_shim_owned"), "nothing to release:\n{lent}");

    let mut owned = sym(&["str"], "alloc_str");
    owned.frees_with = Some("g_free".to_string());
    let src = generate("glib", &symbols(&[("f", owned)])).unwrap();
    assert!(src.contains("jade_shim_owned(r)"), "should copy before freeing:\n{src}");
    assert!(src.contains("g_free(r);"), "should release:\n{src}");
    // The copy, not the library's pointer, is what Jade is handed.
    assert!(src.contains("out->data.as_str = r_c;"), "should hand back the copy:\n{src}");
}

#[test]
fn a_returned_string_the_caller_owns_needs_a_free_function() {
    let bare = sym(&["str"], "alloc_str");
    let err = generate("glib", &symbols(&[("f", bare)])).unwrap_err();
    assert!(err.contains("who releases it"), "should ask: {err}");
}

#[test]
fn an_owned_string_return_is_released_on_the_failure_path_too() {
    // A raise must not leak. `fails_when = "null"` leaves nothing to free, but
    // it is not the only convention a pointer return can carry.
    let mut s = failing_sym(&["str"], "alloc_str", crate::project::CFailure::Null);
    s.frees_with = Some("g_free".to_string());
    let src = generate("glib", &symbols(&[("f", s)])).unwrap();
    let fail_at = src.find("out->tag = JADE_FFI_ERROR;").expect("a failure branch");
    assert!(src[..fail_at].contains("if (r) g_free(r);"), "should release first:\n{src}");
}

#[test]
fn an_owned_string_return_is_refused_where_the_return_value_is_already_spoken_for() {
    // An out_buffer reads the return as its element count, so the string would
    // be allocated and then dropped.
    let mut s = sym(&["out_buffer:char"], "alloc_str");
    s.frees_with = Some("free".to_string());
    let err = generate("z", &symbols(&[("f", s)])).unwrap_err();
    assert!(err.contains("already reads the return value"), "should refuse: {err}");
}

#[test]
fn an_owned_string_return_is_copied_into_a_result_struct_beside_another_value() {
    // Two results mean a struct, and a string inside a container is
    // container-owned — `strdup`, which Jade's `ffi_free` reclaims.
    let mut s = sym(&["str", "out_scalar:int"], "alloc_str");
    s.frees_with = Some("g_free".to_string());
    let src = generate("glib", &symbols(&[("f", s)])).unwrap();
    assert!(src.contains("strdup((const char*)r)"), "should copy into the tree:\n{src}");
    assert!(src.contains("g_free(r);"), "should still release:\n{src}");
}

#[test]
fn an_owned_string_return_shim_compiles() {
    let header = "extern char* dup_it(const char* s);\n\
                  extern void lib_free(void* p);\n";
    let mut s = sym(&["str"], "alloc_str");
    s.frees_with = Some("lib_free".to_string());
    let src = generate_with("z", &symbols(&[("dup_it", s)]), &[], &["fixture.h"]).unwrap();
    if let Err(e) = compiles(&src, &[("fixture.h", header)]) {
        panic!("owned string return shim does not compile:\n{e}\n--- source ---\n{src}");
    }
}

#[test]
fn a_free_function_is_declared_when_the_dependency_has_no_header() {
    // The shim calls it by name and nothing else declares it: a call taking a
    // lone `void *` and reporting nothing is refused as a binding, which is
    // exactly a free function's shape.
    let mut s = sym(&["str"], "alloc_str");
    s.frees_with = Some("lib_free".to_string());
    let src = generate("z", &symbols(&[("f", s)])).unwrap();
    assert!(src.contains("extern void lib_free(void*);"), "should declare it:\n{src}");
    if let Err(e) = compiles(&src, &[]) {
        panic!("headerless owned string shim does not compile:\n{e}\n--- source ---\n{src}");
    }

    // libc's own is already declared by <stdlib.h>, and a second one is noise.
    let mut plain = sym(&["str"], "alloc_str");
    plain.frees_with = Some("free".to_string());
    let src = generate("z", &symbols(&[("f", plain)])).unwrap();
    assert!(!src.contains("extern void free(void*);"), "stdlib.h has it:\n{src}");
}

#[test]
fn a_copied_owned_string_is_not_truncated() {
    // It used to land in a fixed 4096 buffer, which is the worst answer
    // available: a URL-escaped path came back silently short and nothing said
    // so. The buffer grows to fit now, and is reused rather than leaked.
    let mut s = sym(&["str"], "alloc_str");
    s.frees_with = Some("free".to_string());
    let src = generate("z", &symbols(&[("f", s)])).unwrap();
    assert!(!src.contains("char buf[4096]"), "should not be fixed:\n{src}");
    assert!(src.contains("realloc(b->buf, n)"), "should grow to fit:\n{src}");
}

#[test]
fn a_copied_owned_string_is_released_when_the_thread_ends() {
    // A _Thread_local pointer has no destructor in C11, so the buffer a thread
    // held last used to be lost when the thread went. Both engines retire idle
    // pool workers after ten seconds, so threads come and go for as long as the
    // program runs and the lost buffers add up. A pthread key is the hook.
    let mut s = sym(&["str"], "alloc_str");
    s.frees_with = Some("free".to_string());
    let src = generate("z", &symbols(&[("f", s)])).unwrap();
    assert!(src.contains("#include <pthread.h>"), "should include it:\n{src}");
    assert!(
        src.contains("pthread_key_create(&jade_owned_key, jade_owned_release)"),
        "should register a destructor:\n{src}"
    );

    // The destructor takes the buffer through its argument. It must not read a
    // _Thread_local: on macOS the thread's thread-local storage is already torn
    // down when key destructors run, so that spelling frees a null pointer and
    // reclaims nothing — the hook runs and the leak stays exactly as it was.
    // This was written the wrong way first, and only measuring caught it.
    let at = src.find("static void jade_owned_release").expect("a release hook");
    let hook = &src[at..at + src[at..].find("\n}").expect("a body") + 2];
    assert!(hook.contains("(JadeOwnedBuf*)p;"), "should take the buffer as an argument:\n{hook}");
    assert!(!hook.contains("_Thread_local"), "a destructor cannot read one:\n{hook}");

    // And what the key holds is the holder, never the buffer: realloc moves the
    // buffer, and a key still naming the released block would be a double free
    // at thread exit. The holder is allocated once per thread and never moves.
    assert!(src.contains("pthread_setspecific(jade_owned_key, b)"), "should hold it:\n{src}");
    assert!(src.contains("free(b->buf);") && src.contains("free(b);"), "frees both:\n{src}");

    // The key is created once for the process, and a creation that failed leaves
    // an indeterminate key that must not be used.
    assert!(src.contains("pthread_once(&jade_owned_once"), "should arm once:\n{src}");
    assert!(src.contains("if (!jade_owned_key_ok) return NULL;"), "no unusable key:\n{src}");
}

/// The branch a failed copy takes, from its test to the `return` that ends it.
///
/// Bounded at the `return` rather than by a character count: the line right
/// after the branch assigns the string into its target, and a window wide enough
/// to catch it would make "the error is not written into a field" pass for the
/// wrong reason.
fn failed_copy_branch(src: &str) -> &str {
    let at = src.find("if (!r_c)").expect("a failed-copy branch");
    let branch = &src[at..];
    let end = branch.find("return 1;").expect("the branch fails the call") + "return 1;".len();
    &branch[..end]
}

#[test]
fn a_failed_copy_of_an_owned_string_says_what_went_wrong() {
    // It used to be a bare `return 1` leaving `out` untouched, and neither
    // engine can read a cause that is not there: the compiled runtime said
    // "returned a non-zero status" and the VM "returned error code 1", for what
    // is simply out of memory.
    let mut s = sym(&["str"], "alloc_str");
    s.frees_with = Some("g_free".to_string());
    let src = generate("glib", &symbols(&[("dup_it", s)])).unwrap();
    let branch = failed_copy_branch(&src);
    assert!(branch.contains("out->tag = JADE_FFI_ERROR;"), "should be an error:\n{branch}");
    assert!(branch.contains("dup_it: out of memory"), "should name the cause:\n{branch}");
    assert!(branch.contains("return 1;"), "should still fail the call:\n{branch}");
}

#[test]
fn a_failed_copy_reports_through_out_even_when_the_string_lands_in_a_struct() {
    // Two results mean the string lands in a struct field, so `target` is that
    // field — but a failure is always reported through the top-level `out`, and
    // writing an error tag into a field of a half-built tree would say nothing
    // to either engine.
    let mut s = sym(&["str", "out_scalar:int"], "alloc_str");
    s.frees_with = Some("g_free".to_string());
    let src = generate("glib", &symbols(&[("f", s)])).unwrap();
    let branch = failed_copy_branch(&src);
    assert!(branch.contains("out->tag = JADE_FFI_ERROR;"), "should report on out:\n{branch}");
    assert!(branch.contains("out->data.as_str = \"f: out of memory"), "on out:\n{branch}");
    // The string's own target is a field of the result struct, and nothing about
    // the failure belongs there — that is the half that was easy to get wrong.
    assert!(src.contains("res->vals["), "the string should land in a field:\n{src}");
    assert!(!branch.contains("res->vals["), "the error is not a field:\n{branch}");
}

#[test]
fn a_symbol_that_would_shadow_a_shim_helper_is_refused_by_name() {
    // Every wrapper is `jade_shim_<symbol>`, and so is every helper. A library
    // exporting `bytes` would define one of them twice, and the C compiler
    // reports that against generated source hundreds of lines from anything the
    // reader wrote.
    let err = generate("z", &symbols(&[("bytes", sym(&["scalar:int64_t"], "scalar:int64_t"))]))
        .unwrap_err();
    assert!(err.contains("defined twice"), "should say why: {err}");
    assert!(err.contains("'bytes'"), "should name it: {err}");
}

// ── A callback's user-data slot ──────────────────────────────────────────

#[test]
fn a_callbacks_user_data_is_accepted_and_not_forwarded() {
    // The library will pass one, so the C signature must have it. Jade has
    // nothing to do with it, so nothing is forwarded.
    let s = sym(&["callback:int(int, void*)"], "scalar:int64_t");
    let src = generate("ar", &symbols(&[("go", s)])).unwrap();
    assert!(src.contains("static int jade_cbt_go_0(int a0, void* a1)"), "bad signature:\n{src}");
    assert!(src.contains("(void)a1;"), "should be explicitly unused:\n{src}");
    // One forwarded argument, and it is the first.
    assert!(src.contains("cbargs[0].data.as_int = (int64_t)a0;"), "bad marshal:\n{src}");
    assert!(src.contains("invoke(cb->host, 1, cbargs"), "wrong arity:\n{src}");
}

#[test]
fn a_null_pointer_stands_in_for_what_cannot_be_carried() {
    let s = sym(&["null_ptr", "int"], "int");
    let src = generate_with("br", &symbols(&[("go", s)]), &[], &["brotli.h"]).unwrap();
    assert!(src.contains("(go)(NULL, argv[0].data.as_int)"), "should pass null:\n{src}");
    assert!(src.contains("if (argc != 1) return 1;"), "should take no argument:\n{src}");
}

#[test]
fn a_null_pointer_needs_a_header_to_stand_in_against() {
    // Without one the shim declares the symbol itself, and it does not know what
    // type the null is standing in for.
    let err =
        generate("br", &symbols(&[("go", sym(&["null_ptr"], "scalar:int64_t"))])).unwrap_err();
    assert!(err.contains("headers"), "should ask for a header: {err}");
}

// ── A struct returned by value ───────────────────────────────────────────

#[test]
fn a_struct_returned_by_value_is_read_straight_out_of_the_return() {
    // Nothing crosses the boundary but the value: no allocation, no ownership,
    // nothing to release. Which register or stack slot it lands in is the ABI's
    // business, which is why this needs the header like the others.
    let fields: &[(&str, &str)] = &[("error", "int"), ("lowerBound", "int")];
    let s = sym(&["int"], "struct:BOUNDS");
    let src =
        generate_with("z", &symbols(&[("bounds", s)]), &[("BOUNDS", fields)], &["z.h"]).unwrap();
    assert!(src.contains("BOUNDS r = (bounds)("), "not received by value:\n{src}");
    assert!(src.contains("rs->vals[0].data.as_int = (int64_t)r.error;"), "field not read:\n{src}");
}

#[test]
fn a_struct_return_needs_its_fields_declared_and_a_header() {
    let s = sym(&["int"], "struct:BOUNDS");
    let err = generate_with("z", &symbols(&[("f", s.clone())]), &[], &["z.h"]).unwrap_err();
    assert!(err.contains("structs.BOUNDS"), "should name the missing table: {err}");

    let fields: &[(&str, &str)] = &[("error", "int")];
    let err = generate_with("z", &symbols(&[("f", s)]), &[("BOUNDS", fields)], &[]).unwrap_err();
    assert!(err.contains("headers"), "should ask for a header: {err}");
}

#[test]
fn a_struct_return_shim_compiles_against_a_real_header() {
    let header = "typedef struct { size_t error; int lowerBound; int upperBound; } BOUNDS;\n\
                  extern BOUNDS bounds(int p);\n";
    let fields: &[(&str, &str)] = &[("error", "int"), ("lowerBound", "int"), ("upperBound", "int")];
    let syms = symbols(&[("bounds", sym(&["int"], "struct:BOUNDS"))]);
    let src = generate_with("z", &syms, &[("BOUNDS", fields)], &["fixture.h"]).unwrap();
    if let Err(e) = compiles(&src, &[("fixture.h", &format!("#include <stddef.h>\n{header}"))]) {
        panic!("struct return shim does not compile:\n{e}\n--- source ---\n{src}");
    }
}

// ── A struct Jade holds ──────────────────────────────────────────────────

#[test]
fn a_held_struct_gets_its_own_bindings() {
    let syms = symbols(&[("code", sym(&["handle<strm>", "int"], "int"))]);
    let src = generate_held("lz", &syms, "strm", &[("avail_in", "int")], &[]).unwrap();
    for f in ["strm_new", "strm_free", "strm_get", "strm_set"] {
        assert!(src.contains(&format!(r#"{{ "{f}", jade_shim_{f} }}"#)), "no {f}:\n{src}");
    }
    assert!(src.contains("calloc(1, sizeof(strm))"), "not heap allocated:\n{src}");
}

#[test]
fn a_held_struct_with_buffers_owns_what_its_pointers_point_at() {
    // The library expects the memory to still be there on the next call, and a
    // Jade blob makes no such promise — it is the caller's, and Jade may collect
    // it the moment the call returns.
    let syms = symbols(&[("code", sym(&["handle<strm>", "int"], "int"))]);
    let bufs = &[("next_in", "avail_in", false), ("next_out", "avail_out", true)][..];
    let src = generate_held("lz", &syms, "strm", &[("avail_in", "int")], bufs).unwrap();

    assert!(src.contains("typedef struct { strm s; void* owned[2];"), "no wrapper:\n{src}");
    // A read-only buffer is set from a blob; a writable one is allocated to a
    // size and then taken from.
    assert!(src.contains("jade_shim_strm_set_next_in"), "no input setter:\n{src}");
    assert!(src.contains("jade_shim_strm_alloc_next_out"), "no output allocator:\n{src}");
    assert!(src.contains("jade_shim_strm_take_next_out"), "no output taker:\n{src}");
    assert!(!src.contains("jade_shim_strm_alloc_next_in"), "input needs no allocator:\n{src}");
    // Both allocations are released with the struct.
    assert!(src.contains("free(((jade_held_strm*)sp)->owned[0]);"), "leaks 0:\n{src}");
    assert!(src.contains("free(((jade_held_strm*)sp)->owned[1]);"), "leaks 1:\n{src}");
    // And a caller cannot read past what was allocated.
    assert!(src.contains("if ((size_t)n > w->owned_len[1])"), "unclamped take:\n{src}");
}

#[test]
fn a_held_struct_with_nothing_carryable_gets_no_getter_or_setter() {
    // There would be nothing for either to do — an empty struct out, and every
    // key refused on the way in.
    let syms = symbols(&[("code", sym(&["handle<opaque_state>", "int"], "int"))]);
    let src = generate_held("lz", &syms, "opaque_state", &[], &[]).unwrap();
    assert!(src.contains(r#"{ "opaque_state_new""#), "still needs a constructor:\n{src}");
    assert!(!src.contains("opaque_state_get"), "should not emit a getter:\n{src}");
}

#[test]
fn a_held_structs_binding_name_may_not_collide_with_the_library() {
    let syms = symbols(&[("strm_new", sym(&["int"], "int"))]);
    let err = generate_held("lz", &syms, "strm", &[("avail_in", "int")], &[]).unwrap_err();
    assert!(err.contains("Rename one of them"), "should name the clash: {err}");
}

#[test]
fn a_held_struct_shim_compiles_against_a_real_header() {
    let header = r#"
#ifndef FIXTURE_H
#define FIXTURE_H
#include <stddef.h>
typedef struct {
    const unsigned char* next_in;  size_t avail_in;
    unsigned char* next_out;       size_t avail_out;
    void* internal;
} strm;
extern int strm_code(strm* s, int action);
#endif
"#;
    let syms = symbols(&[("strm_code", sym(&["handle<strm>", "int"], "int"))]);
    let bufs = &[("next_in", "avail_in", false), ("next_out", "avail_out", true)][..];
    let fields = &[("avail_in", "int"), ("avail_out", "int")][..];
    let src = generate_held("lz", &syms, "strm", fields, bufs).unwrap();
    if let Err(e) = compiles(&src, &[("fixture.h", header)]) {
        panic!("held struct shim does not compile:\n{e}\n--- source ---\n{src}");
    }
}

#[test]
fn a_field_the_struct_does_not_have_fails_at_compile_time() {
    // The failure mode you want: naming a field that is not there is caught by
    // the C compiler against the real header, not by writing at a wrong offset.
    let header = "typedef struct { int frames; } SF_INFO;\nextern void f(SF_INFO*);\n";
    let syms = symbols(&[("f", sym(&["out_struct:SF_INFO"], "nil"))]);
    let src =
        generate_with("snd", &syms, &[("SF_INFO", &[("nosuch", "int")])], &["fixture.h"]).unwrap();
    let err = compiles(&src, &[("fixture.h", header)]).expect_err("should not compile");
    assert!(err.contains("nosuch"), "the error should name the field: {err}");
}

// ── Handles ──────────────────────────────────────────────────────────────
//
// Stage 1 put handles in the value ABI and both marshallers, but the shim had
// no way to produce or consume one — so the libraries handles exist for were
// still unbindable through `abi = "c"`. These three forms close that.

#[test]
fn a_handle_argument_is_unwrapped_with_its_type_checked() {
    let src = generate("db", &symbols(&[("close", sym(&["handle<sqlite3>"], "scalar:int64_t"))]))
        .unwrap();
    assert!(
        src.contains(r#"jade_shim_unwrap(&argv[0], "sqlite3", &h0)"#),
        "missing unwrap:\n{src}"
    );
    assert!(src.contains("(close)((sqlite3*)h0)"), "should pass the unwrapped pointer:\n{src}");
    // Checked before the call, so the library never sees a wrong-typed pointer.
    let unwrap_at = src.find("jade_shim_unwrap").unwrap();
    let call_at = src.find("(close)((sqlite3*)").unwrap();
    assert!(unwrap_at < call_at, "the type check must precede the call:\n{src}");
}

#[test]
fn the_wrong_handle_type_is_refused_rather_than_dereferenced() {
    // The check is the entire reason a handle carries a name.
    let src =
        generate("db", &symbols(&[("step", sym(&["handle<sqlite3_stmt>"], "scalar:int64_t"))]))
            .unwrap();
    assert!(src.contains(r#""sqlite3_stmt""#), "must check the exact type:\n{src}");
    assert!(src.contains("return 1;"), "a mismatch must fail the call:\n{src}");
}

#[test]
fn a_handle_return_is_wrapped_with_its_type() {
    let src = generate("db", &symbols(&[("open", sym(&["str"], "handle<sqlite3>"))])).unwrap();
    assert!(src.contains("extern sqlite3* open(const char*);"), "bad decl:\n{src}");
    assert!(src.contains(r#"jade_shim_handle((void*)r, "sqlite3")"#), "missing wrap:\n{src}");
    assert!(src.contains("out->tag = JADE_FFI_HANDLE;"), "should return a handle:\n{src}");
}

#[test]
fn an_out_handle_takes_no_jade_argument_and_returns_the_handle() {
    // sqlite3_open(path, &db) — the shape of every SQLite connection.
    let s = failing_sym(&["str", "out_handle:sqlite3"], "scalar:int64_t", CFailure::Nonzero);
    let src = generate("db", &symbols(&[("sqlite3_open", s)])).unwrap();

    assert!(
        src.contains("extern int64_t sqlite3_open(const char*, sqlite3**);"),
        "bad decl:\n{src}"
    );
    assert!(src.contains("if (argc != 1) return 1;"), "out-handle takes no Jade arg:\n{src}");
    assert!(src.contains("sqlite3* ohandle1 = NULL;"), "must start null:\n{src}");
    assert!(src.contains("&ohandle1"), "must pass its address:\n{src}");
    assert!(
        src.contains(r#"jade_shim_handle((void*)ohandle1, "sqlite3")"#),
        "missing wrap:\n{src}"
    );
    // The status is consumed by the failure convention, not returned.
    assert!(src.contains("if ((r) != 0) {"), "status should drive fails_when:\n{src}");
}

#[test]
fn an_out_handle_that_was_never_written_comes_back_nil() {
    // With a failure convention, which is the shape the generator emits: the
    // return is a status, so the handle is the whole result.
    let s = failing_sym(&["out_handle:T"], "scalar:int64_t", CFailure::Nonzero);
    let src = generate("db", &symbols(&[("op", s)])).unwrap();
    assert!(src.contains("if (!ohandle0) {"), "must check it was written:\n{src}");
    assert!(src.contains("out->tag = JADE_FFI_NIL;"), "should be nil, not a null handle:\n{src}");
}

#[test]
fn a_count_returned_beside_a_handle_is_not_swallowed() {
    // An out_handle used to consume the C return unconditionally, on the
    // reasoning that the handle is the result so the return can only be a
    // status. `size_t cs_disasm(…, cs_insn **insn)` returns how many
    // instructions were written, and discarding it leaves the caller a pointer
    // to a row whose length they cannot know.
    //
    // A failure convention is what says the return is a status. Without one it
    // is a value, and comes back beside the handle.
    let src =
        generate("cs", &symbols(&[("disasm", sym(&["out_handle:cs_insn"], "scalar:int64_t"))]))
            .unwrap();
    assert!(src.contains(r#"jade_shim_struct("disasm_result", 2)"#), "should pair:\n{src}");
    assert!(src.contains(r#"res->keys[0] = strdup("ret");"#), "count missing:\n{src}");
}

#[test]
fn a_handle_and_a_scalar_keep_their_argument_positions() {
    // The unwrap uses the Jade index, which is easy to get wrong once some
    // arguments consume a slot and others do not.
    let s = sym(&["scalar:int64_t", "handle<sqlite3>", "str"], "scalar:int64_t");
    let src = generate("db", &symbols(&[("f", s)])).unwrap();
    assert!(src.contains(r#"jade_shim_unwrap(&argv[1], "sqlite3", &h1)"#), "wrong index:\n{src}");
    assert!(
        src.contains("(f)((int64_t)argv[0].data.as_int, (sqlite3*)h1, argv[2].data.as_str)"),
        "bad call:\n{src}"
    );
}

#[test]
fn the_handle_helper_is_emitted_for_any_of_the_three_forms() {
    let plain =
        generate("m", &symbols(&[("f", sym(&["scalar:int64_t"], "scalar:int64_t"))])).unwrap();
    assert!(!plain.contains("jade_shim_handle"), "unused helper emitted:\n{plain}");

    for spec in [
        sym(&["handle<T>"], "scalar:int64_t"),
        sym(&["scalar:int64_t"], "handle<T>"),
        sym(&["out_handle:T"], "scalar:int64_t"),
    ] {
        let src = generate("z", &symbols(&[("f", spec)])).unwrap();
        assert!(src.contains("static JadeHandle* jade_shim_handle"), "helper missing:\n{src}");
    }
}

#[test]
fn a_handle_shim_compiles() {
    // A whole SQLite-shaped surface: open through an out-handle, a connection
    // argument, a statement of a different type, and a handle return.
    let header = r#"
#ifndef DBFIX_H
#define DBFIX_H
typedef struct sqlite3 sqlite3;
typedef struct sqlite3_stmt sqlite3_stmt;
extern int sqlite3_open(const char* path, sqlite3** db);
extern int sqlite3_prepare(sqlite3* db, const char* sql, sqlite3_stmt** stmt);
extern int sqlite3_step(sqlite3_stmt* s);
extern const char* sqlite3_errmsg(sqlite3* db);
extern sqlite3* sqlite3_dup(sqlite3* db);
extern int sqlite3_close(sqlite3* db);
#endif
"#;
    let syms = symbols(&[
        ("sqlite3_open", failing_sym(&["str", "out_handle:sqlite3"], "int", CFailure::Nonzero)),
        (
            "sqlite3_prepare",
            failing_sym(
                &["handle<sqlite3>", "str", "out_handle:sqlite3_stmt"],
                "int",
                CFailure::Nonzero,
            ),
        ),
        ("sqlite3_step", sym(&["handle<sqlite3_stmt>"], "int")),
        ("sqlite3_errmsg", sym(&["handle<sqlite3>"], "str")),
        ("sqlite3_dup", sym(&["handle<sqlite3>"], "handle<sqlite3>")),
        ("sqlite3_close", sym(&["handle<sqlite3>"], "int")),
    ]);
    let src = generate_with("db", &syms, &[], &["dbfix.h"]).unwrap();
    if let Err(e) = compiles(&src, &[("dbfix.h", header)]) {
        panic!("handle shim does not compile:\n{e}\n--- source ---\n{src}");
    }
}

// ── Callbacks ────────────────────────────────────────────────────────────
//
// The shim declares a real static C function of the declared shape, which is
// only possible because the shape is known when the C is written. Synthesising
// one at run time would need libffi and a trampoline compiler.

#[test]
fn a_callback_becomes_a_static_function_of_the_declared_shape() {
    let s = sym(&["scalar:int64_t", "callback:int(int, const char*)"], "scalar:int64_t");
    let src = generate("z", &symbols(&[("each", s)])).unwrap();
    // The library's own C types, not Jade's widened ones: this declares a
    // function pointer the library will store and call, so `int` must be `int`.
    // Widening it is not a truncation but an incompatible function pointer.
    assert!(
        src.contains("static int jade_cbt_each_1(int a0, const char* a1)"),
        "bad trampoline:\n{src}"
    );
    assert!(
        src.contains("extern int64_t each(int64_t, int (*)(int, const char*));"),
        "bad decl:\n{src}"
    );
    assert!(
        src.contains("(each)((int64_t)argv[0].data.as_int, jade_cbt_each_1)"),
        "should pass the trampoline:\n{src}"
    );
}

#[test]
fn a_registration_outlives_the_call_that_made_it() {
    // A library that stores the callback invokes it from a later call entirely:
    // `ares_search` registers and the answer arrives during `ares_process`.
    // Clearing the slot on return is what made that never fire.
    let s = sym(&["callback:int(int)"], "scalar:int64_t");
    let src = generate("z", &symbols(&[("go", s)])).unwrap();
    assert!(src.contains("jade_cb_go_0 = argv[0].data.as_fn;"), "should register:\n{src}");
    // The declaration says `= NULL` too, so look only at the wrapper body.
    let body = &src[src.find("jade_shim_go(size_t").unwrap()..];
    assert!(!body.contains("jade_cb_go_0 = NULL;"), "must not unregister:\n{body}");
    assert!(src.contains("if (!cb) {"), "should answer neutrally when empty:\n{src}");
}

#[test]
fn the_registration_slot_is_not_thread_local() {
    // Under the VM every native call runs on its own worker thread, so a slot
    // set while `ares_search` ran would read empty during `ares_process` even
    // if nothing ever cleared it.
    let s = sym(&["callback:int(int)"], "scalar:int64_t");
    let src = generate("z", &symbols(&[("go", s)])).unwrap();
    let decl = src.lines().find(|l| l.contains("jade_cb_go_0 = NULL")).unwrap_or("");
    assert!(!decl.contains("_Thread_local"), "the slot must be shared across threads: {decl}");
}

#[test]
fn a_raise_inside_a_callback_is_deferred_rather_than_unwound() {
    // Longjmping out of the trampoline would unwind through the C library's
    // own frames, past whatever it was in the middle of.
    let s = sym(&["callback:int(int)"], "scalar:int64_t");
    let src = generate("z", &symbols(&[("go", s)])).unwrap();
    assert!(src.contains("jade_cb_failed = 1;"), "should record the failure:\n{src}");
    assert!(src.contains("if (jade_cb_failed) {"), "should check after the call:\n{src}");
    assert!(src.contains("the callback raised"), "should surface it:\n{src}");
    // Recorded inside the trampoline, surfaced only after the library returns.
    let record = src.find("jade_cb_failed = 1;").unwrap();
    let surface = src.find("if (jade_cb_failed) {").unwrap();
    assert!(record < surface, "the raise must surface after the call, not during it");
}

#[test]
fn every_wrapper_reports_a_raise_even_if_it_took_no_callback() {
    // Once a registration outlives its call, the symbol that registered is not
    // the symbol that was running when the callback raised. A function given to
    // `ares_search` raises during `ares_process`, and that is the call that has
    // to report it — so the flag is one per shim and every wrapper checks it.
    let syms = symbols(&[
        ("go", sym(&["callback:int(int)"], "scalar:int64_t")),
        ("pump", sym(&["scalar:int64_t"], "scalar:int64_t")),
    ]);
    let src = generate("z", &syms).unwrap();
    let pump = &src[src.find("jade_shim_pump").unwrap()..];
    assert!(pump.contains("if (jade_cb_failed) {"), "a pumping call must report:\n{pump}");
}

#[test]
fn a_shim_with_no_callbacks_declares_no_raised_flag() {
    let src =
        generate("z", &symbols(&[("f", sym(&["scalar:int64_t"], "scalar:int64_t"))])).unwrap();
    assert!(!src.contains("jade_cb_failed"), "nothing to check:\n{src}");
}

#[test]
fn a_void_callback_and_a_zero_argument_callback_both_work() {
    let src = generate(
        "z",
        &symbols(&[
            ("a", sym(&["callback:void(int)"], "scalar:int64_t")),
            ("b", sym(&["callback:int()"], "scalar:int64_t")),
        ]),
    )
    .unwrap();
    assert!(src.contains("static void jade_cbt_a_0(int a0)"), "void callback:\n{src}");
    assert!(src.contains("static int jade_cbt_b_0(void)"), "no-arg callback:\n{src}");
}

#[test]
fn a_malformed_or_unrepresentable_callback_is_refused() {
    for bad in
        ["callback:int", "callback:int(", "callback:int(struct foo)", "callback:double*(int)"]
    {
        let s = sym(&[bad], "scalar:int64_t");
        assert!(generate("z", &symbols(&[("f", s)])).is_err(), "should refuse {bad}");
    }
}

#[test]
fn a_callback_shim_compiles() {
    let header = "typedef int (*each_cb)(int, const char*);\n\
                  extern int each(int n, each_cb cb);\n\
                  extern int walk(void (*cb)(int));\n";
    let syms = symbols(&[
        ("each", sym(&["int", "callback:int(int, const char*)"], "int")),
        ("walk", sym(&["callback:nil(int)"], "int")),
    ]);
    let src = generate_with("z", &syms, &[], &["cbfix.h"]).unwrap();
    if let Err(e) = compiles(&src, &[("cbfix.h", header)]) {
        panic!("callback shim does not compile:\n{e}\n--- source ---\n{src}");
    }
}

// ── Scalars written through a pointer ────────────────────────────────────

#[test]
fn an_out_scalar_takes_no_jade_argument_and_is_the_result() {
    let src =
        generate("z", &symbols(&[("f", sym(&["scalar:int64_t", "out_scalar:uint32_t"], "nil"))]))
            .unwrap();
    assert!(src.contains("extern void f(int64_t, uint32_t*);"), "bad decl:\n{src}");
    assert!(
        src.contains("if (argc != 1) return 1;"),
        "the out must not consume an argument:\n{src}"
    );
    assert!(src.contains("uint32_t oscalar1 = (uint32_t)0;"), "must be a zeroed local:\n{src}");
    assert!(src.contains("&oscalar1"), "must pass its address:\n{src}");
    assert!(src.contains("out->tag = JADE_FFI_INT;"), "should come back as an int:\n{src}");
}

#[test]
fn an_out_scalar_keeps_the_librarys_own_c_type() {
    // Widening it to int64_t would take the address of a differently-sized
    // object and let the library write past it.
    let src = generate("z", &symbols(&[("f", sym(&["out_scalar:uint16_t"], "nil"))])).unwrap();
    assert!(src.contains("uint16_t oscalar0"), "must declare the real type:\n{src}");
    assert!(!src.contains("int64_t oscalar0"), "must not widen:\n{src}");
}

#[test]
fn an_inout_scalar_consumes_an_argument_and_is_seeded_from_it() {
    let src =
        generate("z", &symbols(&[("f", sym(&["scalar:int64_t", "inout_scalar:int"], "nil"))]))
            .unwrap();
    assert!(src.contains("if (argc != 2) return 1;"), "it does take an argument:\n{src}");
    assert!(
        src.contains("if (argv[1].tag != JADE_FFI_INT) return 1;"),
        "missing tag check:\n{src}"
    );
    assert!(src.contains("int oscalar1 = (int)argv[1].data.as_int;"), "must be seeded:\n{src}");
}

#[test]
fn a_string_written_through_a_pointer_is_refused() {
    // Ownership is unresolvable from the header: nothing says who frees it.
    // `char*` is caught by the identifier check, before the scalar one.
    let err = generate("z", &symbols(&[("f", sym(&["out_scalar:char*"], "nil"))])).unwrap_err();
    assert!(err.contains("not a C type name"), "unexpected: {err}");
}

#[test]
fn an_out_scalar_that_is_not_a_scalar_is_refused_by_name() {
    // A struct cannot be one: the shim reads the local back as a single value.
    let err = generate("z", &symbols(&[("f", sym(&["out_scalar:SF_INFO"], "nil"))])).unwrap_err();
    assert!(err.contains("not a scalar"), "unexpected: {err}");
}

// ── More than one out-parameter ──────────────────────────────────────────

#[test]
fn two_named_outs_come_back_under_their_own_keys() {
    let s = sym(&["out_scalar:int@quot", "out_scalar:int@rem"], "nil");
    let src = generate("z", &symbols(&[("divmod", s)])).unwrap();
    assert!(src.contains(r#"jade_shim_struct("divmod_result", 2)"#), "should build a pair:\n{src}");
    assert!(src.contains(r#"strdup("quot")"#), "missing key:\n{src}");
    assert!(src.contains(r#"strdup("rem")"#), "missing key:\n{src}");
    assert!(!src.contains(r#"strdup("ret")"#), "a void return has no ret key:\n{src}");
}

#[test]
fn a_return_value_joins_named_outs_under_ret() {
    let s = sym(&["scalar:int64_t", "out_scalar:int@quot", "out_scalar:int@rem"], "scalar:int64_t");
    let src = generate("z", &symbols(&[("divmod", s)])).unwrap();
    assert!(src.contains(r#"jade_shim_struct("divmod_result", 3)"#), "three keys:\n{src}");
    assert!(src.contains(r#"strdup("ret")"#), "missing ret:\n{src}");
}

#[test]
fn one_out_beside_a_return_still_comes_back_as_ret_and_out() {
    // The shape that existed before multiple outs, unchanged.
    let s = sym(&["scalar:int64_t", "out_scalar:int"], "scalar:int64_t");
    let src = generate("z", &symbols(&[("f", s)])).unwrap();
    assert!(src.contains(r#"jade_shim_struct("f_result", 2)"#), "{src}");
    assert!(src.contains(r#"strdup("out")"#), "the lone out keeps the name `out`:\n{src}");
}

#[test]
fn two_out_structs_declare_two_distinct_locals() {
    // The scratch used to be a fixed name, so a second out-struct emitted the
    // same declaration twice and the shim did not compile.
    let s = sym(&["out_struct:A@first", "out_struct:B@second"], "nil");
    let src = generate_with(
        "z",
        &symbols(&[("f", s)]),
        &[("A", &[("x", "int")]), ("B", &[("y", "int")])],
        &["z.h"],
    )
    .unwrap();
    assert!(src.contains("A ostruct0;"), "{src}");
    assert!(src.contains("B ostruct1;"), "{src}");
}

#[test]
fn a_multi_out_wrapper_compiles() {
    let s = sym(&["out_scalar:int@quot", "out_scalar:int@rem"], "scalar:int64_t");
    let src = generate("z", &symbols(&[("divmod", s)])).unwrap();
    compiles(&src, &[]).expect("the generated C must compile");
}

#[test]
fn a_wrapper_mixing_two_kinds_of_scratch_compiles() {
    // An out-struct and an out-scalar in one wrapper: two different scratch
    // kinds, two different result slots.
    let s = sym(&["out_struct:INFO@info", "out_scalar:int@count"], "int");
    let src = generate_with(
        "z",
        &symbols(&[("f", s)]),
        &[("INFO", &[("rate", "int"), ("name", "str")])],
        &["z.h"],
    )
    .unwrap();
    compiles(
        &src,
        &[("z.h", "typedef struct { int rate; const char* name; } INFO;\nint f(INFO*, int*);\n")],
    )
    .expect("the generated C must compile");
}

// ── Fixed-size array fields ──────────────────────────────────────────────

#[test]
fn a_char_row_reads_as_characters_and_casts_through_unsigned() {
    // `char` is signed on x86 Linux and unsigned on ARM macOS. Without the cast
    // a byte of 0x80 sign-extends to 0xFFFFFF80, which is not a Unicode scalar,
    // and the far side raises — on one platform only.
    let fields: &[(&str, &str)] = &[("mnemonic", "array<char>:32")];
    let s = sym(&["out_struct:INSN"], "nil");
    let src = generate_with("cs", &symbols(&[("f", s)]), &[("INSN", fields)], &["cs.h"]).unwrap();
    assert!(src.contains("jade_shim_array(32)"), "no row:\n{src}");
    assert!(src.contains("(uint32_t)(unsigned char)"), "missing the signedness cast:\n{src}");
    assert!(src.contains("JADE_FFI_CHAR"), "should read as characters:\n{src}");
}

#[test]
fn a_row_longer_than_the_field_is_refused_rather_than_truncated() {
    // Dropping the tail silently is the failure this generator exists to avoid.
    // Shorter is filled with zeros, which is what an omitted field already gets.
    let fields: &[(&str, &str)] = &[("name", "array<char>:8")];
    let s = sym(&["in_struct:REC"], "int");
    let src = generate_with("z", &symbols(&[("f", s)]), &[("REC", fields)], &["z.h"]).unwrap();
    assert!(src.contains("->len > 8"), "no length check:\n{src}");
    assert!(src.contains("= 0;\n"), "short rows should zero-fill:\n{src}");
}

#[test]
fn a_character_that_does_not_fit_in_a_byte_is_refused() {
    // The one place the byte-per-character mapping is not symmetric: every byte
    // is a character, but not every character fits in a byte.
    let fields: &[(&str, &str)] = &[("name", "array<char>:8")];
    let s = sym(&["in_struct:REC"], "int");
    let src = generate_with("z", &symbols(&[("f", s)]), &[("REC", fields)], &["z.h"]).unwrap();
    assert!(src.contains("> 0xFF"), "no range check:\n{src}");
}

#[test]
fn an_array_field_shim_compiles_against_a_real_header() {
    let header = "#include <stdint.h>\n\
                  typedef struct { unsigned int id; char mnemonic[32]; uint8_t bytes[8]; } INSN;\n\
                  extern void fill(INSN* i);\n\
                  extern int put(const INSN* i);\n";
    let fields: &[(&str, &str)] =
        &[("id", "int"), ("mnemonic", "array<char>:32"), ("bytes", "array<int>:8")];
    let syms = symbols(&[
        ("fill", sym(&["out_struct:INSN"], "nil")),
        ("put", sym(&["in_struct:INSN"], "int")),
    ]);
    let src = generate_with("cs", &syms, &[("INSN", fields)], &["fixture.h"]).unwrap();
    if let Err(e) = compiles(&src, &[("fixture.h", header)]) {
        panic!("array field shim does not compile:\n{e}\n--- source ---\n{src}");
    }
}

#[test]
fn an_array_spelling_is_not_legal_as_an_argument() {
    // Field types and argument types are separate vocabularies on purpose: the
    // wrapper has nothing to do with a fixed-size row in an `args` list.
    let err =
        generate("z", &symbols(&[("f", sym(&["array<char>:32"], "scalar:int64_t"))])).unwrap_err();
    assert!(err.contains("array<char>:32"), "should name it: {err}");
}

#[test]
fn a_routed_callback_prefers_the_librarys_own_cookie() {
    // Without routing there is one slot per symbol, so a second registration
    // silently takes the first one's answers — it runs and answers the wrong
    // caller, which is worse than not running.
    let s = sym(&["callback:void(int, void*)", "callback_data"], "nil");
    let src = generate_with("ar", &symbols(&[("go", s)]), &[], &["ares.h"]).unwrap();
    assert!(src.contains("(void*)argv[0].data.as_fn"), "should hand over the pointer:\n{src}");
    assert!(src.contains("a1 ? (const JadeFn*)a1 : jade_cb_go_0"), "should read it back:\n{src}");
}

#[test]
fn an_unrouted_callback_still_uses_the_shared_slot() {
    // A library with no context parameter leaves nothing to route through, and
    // that has to keep working — it is what `store`/`pump` in the parity fixture
    // exercises.
    let src = generate("z", &symbols(&[("go", sym(&["callback:void(int)"], "nil"))])).unwrap();
    assert!(src.contains("const JadeFn* cb = jade_cb_go_0;"), "should use the slot:\n{src}");
}

#[test]
fn a_callback_data_with_no_callback_is_refused() {
    let s = sym(&["callback_data"], "nil");
    let err = generate_with("ar", &symbols(&[("go", s)]), &[], &["ares.h"]).unwrap_err();
    assert!(err.contains("takes no callback"), "should say why: {err}");
}

#[test]
fn a_function_like_macro_cannot_intercept_the_call() {
    // glib declares `g_atomic_pointer_add` and then defines a macro of the same
    // name whose `_Static_assert` rejects a `void*`. The macro won, the shim did
    // not compile, and one such symbol refuses the whole dependency — so glib
    // bound 1357 symbols and could not be used at all.
    let header = "extern int go(int a);\n#define go(a) (\"a macro won\")\n";
    let s = sym(&["int"], "int");
    let src = generate_with("m", &symbols(&[("go", s)]), &[], &["fixture.h"]).unwrap();
    assert!(src.contains("(go)("), "the call must be parenthesised:\n{src}");
    if let Err(e) = compiles(&src, &[("fixture.h", header)]) {
        panic!("a macro of the same name broke the shim:\n{e}\n--- source ---\n{src}");
    }
}

#[test]
fn two_callbacks_on_one_symbol_get_a_trampoline_each() {
    // brotli's decoder takes two, and one name for both is two definitions of
    // the same C function — which does not compile, and a shim that does not
    // compile refuses the whole dependency rather than the symbol.
    let header = "extern void go(void (*a)(int), void (*b)(int, int));\n";
    let s = sym(&["callback:void(int)", "callback:void(int, int)"], "nil");
    let src = generate_with("ar", &symbols(&[("go", s)]), &[], &["fixture.h"]).unwrap();
    assert!(src.contains("jade_cbt_go_0"), "{src}");
    assert!(src.contains("jade_cbt_go_1"), "{src}");
    if let Err(e) = compiles(&src, &[("fixture.h", header)]) {
        panic!("two-callback shim does not compile:\n{e}\n--- source ---\n{src}");
    }
}

#[test]
fn a_context_slot_cannot_tell_two_callbacks_apart() {
    // The library passes the same value back to both, so routing through it
    // sends both answers to whichever registered last — the bug `callback_data`
    // exists to prevent.
    let s =
        sym(&["callback:void(int, void*)", "callback:void(int, void*)", "callback_data"], "nil");
    let e = generate_with("ar", &symbols(&[("go", s)]), &[], &["fixture.h"]).unwrap_err();
    assert!(e.contains("more than one callback"), "{e}");
}

#[test]
fn a_routed_callback_shim_compiles() {
    let header = "extern void go(void (*cb)(int, void*), void* data);\n";
    let s = sym(&["callback:void(int, void*)", "callback_data"], "nil");
    let src = generate_with("ar", &symbols(&[("go", s)]), &[], &["fixture.h"]).unwrap();
    if let Err(e) = compiles(&src, &[("fixture.h", header)]) {
        panic!("routed callback shim does not compile:\n{e}\n--- source ---\n{src}");
    }
}

// ── A scalar in the library's own C type ─────────────────────────────────
//
// Without a header the shim writes the declaration itself, and `int`, `float`
// and `bool` are Jade's widths rather than the library's. Where the two
// disagree the declaration is a lie the compiler believes, and nothing reports
// it — so those three are refused there, and `scalar:<ctype>` is what gets past.

#[test]
fn a_jade_width_is_refused_where_the_shim_writes_the_declaration() {
    for t in ["int", "float", "bool"] {
        let in_args = generate("glib", &symbols(&[("f", sym(&[t], "str"))]))
            .expect_err("`{t}` in args should be refused with no header");
        assert!(in_args.contains("'f'"), "should name the symbol: {in_args}");
        assert!(in_args.contains(&format!("`{t}`")), "should name the type: {in_args}");

        let in_ret = generate("glib", &symbols(&[("f", sym(&["str"], t))]))
            .expect_err("`{t}` as a return should be refused with no header");
        assert!(in_ret.contains("as its return"), "should say where: {in_ret}");
    }
}

#[test]
fn a_header_leaves_the_jade_widths_exactly_as_they_were() {
    // With a header the spelling is only a marshalling tag: the real prototype
    // governs the widths, so nothing here has to change and nothing does.
    let syms = symbols(&[
        ("f", sym(&["int", "float", "bool"], "int")),
        ("g", sym(&["str"], "float")),
        ("h", sym(&[], "bool")),
    ]);
    let src = generate_with("glib", &syms, &[], &["glib.h"]).unwrap();
    assert!(!src.contains("extern"), "a header declares the symbols, not the shim:\n{src}");
    assert!(src.contains("(f)(argv[0].data.as_int"), "should still marshal as before:\n{src}");
}

#[test]
fn the_width_refusal_points_at_a_header_before_it_offers_a_spelling() {
    // A header answers the question for every symbol at once and cannot be got
    // wrong, and most people who land here have one. The explicit spelling is
    // the fallback, filled in with the symbol's own arguments so that what is
    // left to supply is exactly what Jade could not work out.
    let s = sym(&["str", "str", "int"], "str");
    let err = generate("glib", &symbols(&[("g_uri_escape_string", s)])).unwrap_err();

    assert!(
        err.contains("jade pkg bind glib --header"),
        "the header comes first, and settles every symbol: {err}"
    );
    assert!(
        err.contains(r#"args = ["str", "str", "scalar:<ctype>"]"#),
        "the stanza should carry the symbol's own arguments: {err}"
    );
    assert!(err.contains(r#"ret  = "str""#), "a sound return should be left alone: {err}");
    assert!(err.contains("size_t"), "the accepted C types should be named: {err}");
}

#[test]
fn a_named_c_type_is_declared_and_converted_at_the_boundary() {
    // glib's third parameter is a 32-bit `gboolean`, and the shim used to
    // declare it `int64_t`.
    let s = sym(&["str", "scalar:int"], "str");
    let src = generate("glib", &symbols(&[("g_uri_escape_string", s)])).unwrap();

    assert!(
        src.contains("extern const char* g_uri_escape_string(const char*, int);"),
        "the declaration should carry the library's own type:\n{src}"
    );
    // Jade's side is unchanged: still an ordinary int, checked as one.
    assert!(src.contains("if (argv[1].tag != JADE_FFI_INT) return 1;"), "no tag check:\n{src}");
    assert!(
        src.contains("(g_uri_escape_string)(argv[0].data.as_str, (int)argv[1].data.as_int)"),
        "the conversion belongs to the shim:\n{src}"
    );
}

#[test]
fn a_named_c_type_return_is_read_at_the_width_the_function_wrote() {
    // The dangerous half. A value passed too wide usually survives, because the
    // callee reads only the part it wants; a `float` read as a `double` is not
    // an approximate number but a meaningless one.
    let src =
        generate("m", &symbols(&[("scalef", sym(&["scalar:float"], "scalar:float"))])).unwrap();
    assert!(src.contains("extern float scalef(float);"), "bad decl:\n{src}");
    assert!(src.contains("    float r = (scalef)"), "should read a float:\n{src}");
    assert!(src.contains("out->tag = JADE_FFI_FLOAT;"), "Jade still gets a float:\n{src}");
    assert!(src.contains("out->data.as_float = r;"), "and it widens on the way out:\n{src}");
}

#[test]
fn a_named_c_type_shim_compiles() {
    let syms = symbols(&[
        ("escape", sym(&["str", "scalar:int"], "str")),
        ("scalef", sym(&["scalar:float", "scalar:unsigned long"], "scalar:float")),
        ("flag", sym(&["scalar:bool"], "scalar:_Bool")),
        ("count", failing_sym(&["scalar:size_t"], "scalar:int32_t", CFailure::Negative)),
    ]);
    let src = generate("g", &syms).unwrap();
    if let Err(e) = compiles(&src, &[]) {
        panic!("named-C-type shim does not compile:\n{e}\n--- source ---\n{src}");
    }
}

#[test]
fn a_c_type_the_shim_cannot_convert_is_refused_by_name() {
    // The accepted set is exactly what `c_scalar` knows, which is what
    // `out_scalar` and `inout_scalar` already resolve through — so the two
    // cannot come to mean different things by the same spelling.
    let err = generate("g", &symbols(&[("f", sym(&["scalar:gboolean"], "str"))])).unwrap_err();
    assert!(err.contains("scalar:gboolean"), "should name it: {err}");
    assert!(err.contains("double"), "should list what works: {err}");

    // And a pointer is not a scalar. `str` is the spelling for a string,
    // because who owns it is a question a width cannot answer.
    let e = generate("g", &symbols(&[("f", sym(&["scalar:char*"], "str"))])).unwrap_err();
    assert!(e.contains("plain identifier"), "the text goes straight into the shim: {e}");
}

#[test]
fn an_out_buffers_count_may_be_spelled_as_a_c_type() {
    // The count is an ordinary `int` argument, so the headerless refusal reaches
    // it too — and the rule that the buffer is followed by an integer has to
    // hold for either spelling of one. Asked by tag rather than by variant.
    let s = failing_sym(
        &["scalar:int", "out_buffer:char", "scalar:size_t"],
        "scalar:int",
        CFailure::Negative,
    );
    let src = generate("z", &symbols(&[("rd", s)])).unwrap();
    assert!(src.contains("extern int rd(int, char*, size_t);"), "bad decl:\n{src}");
    assert!(src.contains("int64_t n_elem1 = argv[1].data.as_int;"), "count not read:\n{src}");
    if let Err(e) = compiles(&src, &[]) {
        panic!("counted-buffer shim does not compile:\n{e}\n--- source ---\n{src}");
    }
}

#[test]
fn a_negative_convention_on_an_unsigned_return_can_never_fire() {
    // `(r) < 0` on an unsigned type compiles to `false`, so the symbol binds,
    // runs, and hands every failure back as an ordinary result. A Jade `int` is
    // always signed, so nothing could reach this before `scalar:` let a return
    // name a C type of its own.
    let s = failing_sym(&["str"], "scalar:size_t", CFailure::Negative);
    let err = generate("z", &symbols(&[("f", s)])).unwrap_err();
    assert!(err.contains("never negative"), "should say why: {err}");
    assert!(err.contains("nonzero"), "should name a fix: {err}");

    // Plain `char` is the one spelling C leaves to the platform — signed on x86
    // Linux, unsigned on ARM macOS — so the test would fire on one and not the
    // other.
    let c = failing_sym(&["str"], "scalar:char", CFailure::Negative);
    let err = generate("z", &symbols(&[("f", c)])).unwrap_err();
    assert!(err.contains("signed char"), "should name the spelling that works: {err}");

    // A signed return is exactly what the convention is for.
    let ok = failing_sym(&["str"], "scalar:int32_t", CFailure::Negative);
    assert!(generate("z", &symbols(&[("f", ok)])).is_ok());
}
