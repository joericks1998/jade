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
    assert!(
        src.contains("if (argv[0].tag != JADE_FFI_INT) return 1;"),
        "missing tag check:\n{src}"
    );
    assert!(
        src.contains("if (argv[1].tag != JADE_FFI_STR) return 1;"),
        "missing tag check:\n{src}"
    );
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
    let src =
        generate("z", &symbols(&[("gzopen", failing_sym(&["str", "str"], "int", CFailure::Null))]))
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
    let never =
        generate("m", &symbols(&[("f", failing_sym(&["int"], "int", CFailure::Never))])).unwrap();
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
    assert!(
        generate("l", &symbols(&[("f", failing_sym(&["int"], "nil", CFailure::Never))])).is_ok()
    );
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

// ── Buffers ──────────────────────────────────────────────────────────────
//
// The Jade-facing arity is deliberately smaller than the C one. `x_read(h, buf,
// n)` is called from Jade as `x_read(h, n)` and hands back the bytes, because a
// Jade blob is immutable — three methods, none of them a write — and letting a
// C library scribble into one would break that for the FFI's convenience.

#[test]
fn an_input_blob_becomes_a_pointer_and_a_length() {
    let src = generate("z", &symbols(&[("put", sym(&["bytes"], "int"))])).unwrap();
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
    let src = generate("fdt", &symbols(&[("check", sym(&["bytes_ptr"], "int"))])).unwrap();
    assert!(src.contains("extern int64_t check(const void*);"), "bad decl:\n{src}");
    assert!(src.contains("if (argv[0].tag != JADE_FFI_BYTES) return 1;"), "no tag check:\n{src}");
    assert!(src.contains("check(argv[0].data.as_bytes"), "should pass the pointer:\n{src}");
    assert!(!src.contains("as_bytes->len"), "must not invent a length:\n{src}");
}

#[test]
fn a_lengthless_blob_shim_compiles() {
    let syms = symbols(&[
        ("check", sym(&["bytes_ptr"], "int")),
        ("at", sym(&["bytes_ptr", "str"], "int")),
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
    let src = generate("fdt", &symbols(&[("nop", sym(&["inout_bytes", "int"], "int"))])).unwrap();
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
    let s = failing_sym(&["inout_bytes@a", "inout_bytes@b"], "int", CFailure::Nonzero);
    let src = generate("fdt", &symbols(&[("apply", s)])).unwrap();
    let raise = src.split("out->tag = JADE_FFI_ERROR").next().unwrap_or_default();
    assert!(raise.contains("free(iobuf0);") && raise.contains("free(iobuf1);"), "leak:\n{src}");
}

#[test]
fn a_revised_blob_shim_compiles() {
    let syms = symbols(&[("nop", sym(&["inout_bytes", "int", "str"], "int"))]);
    let src = generate("fdt", &syms).unwrap();
    if let Err(e) = compiles(&src, &[]) {
        panic!("in-place blob shim does not compile:\n{e}\n--- source ---\n{src}");
    }
}

#[test]
fn a_null_blob_passes_null_and_zero_rather_than_dereferencing() {
    let src = generate("z", &symbols(&[("put", sym(&["bytes"], "int"))])).unwrap();
    assert!(src.contains("? (const void*)argv[0].data.as_bytes->data : NULL"), "unguarded:\n{src}");
}

#[test]
fn an_out_buffer_takes_no_jade_argument_and_returns_bytes() {
    let s = failing_sym(&["int", "out_buffer:short", "int"], "int", CFailure::Negative);
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
    let s = failing_sym(&["int", "out_buffer:char", "int"], "int", CFailure::Negative);
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
    let s = sym(&["int", "out_buffer:char", "int"], "int");
    let src = generate("z", &symbols(&[("rd", s)])).unwrap();
    assert!(src.contains("r > n_elem1 ? n_elem1 : r"), "missing clamp:\n{src}");
}

#[test]
fn an_out_buffer_needs_a_count_after_it() {
    let s = sym(&["int", "out_buffer:char"], "int");
    let err = generate("z", &symbols(&[("rd", s)])).unwrap_err();
    assert!(err.contains("followed by an `int`"), "unexpected: {err}");
    assert!(err.contains("how many"), "should say what the count is for: {err}");
}

#[test]
fn an_out_buffer_symbol_must_return_the_count() {
    let s = sym(&["out_buffer:char", "int"], "str");
    let err = generate("z", &symbols(&[("rd", s)])).unwrap_err();
    assert!(err.contains("number of elements written"), "unexpected: {err}");
}

#[test]
fn at_most_one_out_parameter_may_read_the_c_return_value() {
    // Two out_buffers would both want the return value as their element count,
    // and there is only one of it.
    let s = sym(&["out_buffer:char", "int", "out_buffer:char", "int"], "int");
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
    let s = sym(&["out_scalar:int@ret", "out_scalar:int@b"], "int");
    let err = generate("z", &symbols(&[("f", s)])).unwrap_err();
    assert!(err.contains("reserved"), "unexpected: {err}");
}

#[test]
fn a_c_type_that_is_not_an_identifier_is_refused() {
    // The text goes straight into generated C, so this is an injection guard as
    // much as a typo guard.
    for bad in ["short; evil()", "char*", "1int", ""] {
        let s = sym(&[format!("out_buffer:{bad}").as_str(), "int"], "int");
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
    let lone = sym(&["bytes_ptr", "ret_len:int"], "int");
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
    let syms = symbols(&[("getprop", sym(&["bytes_ptr", "int", "str", "ret_len:int"], "bytes"))]);
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
    assert!(src.contains("f(&istruct0, argv[1].data.as_int)"), "not passed by address:\n{src}");
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
    let plain = generate("m", &symbols(&[("hypot", sym(&["float", "float"], "float"))])).unwrap();
    assert!(!plain.contains("jade_shim_bytes"), "unused helper emitted:\n{plain}");
    assert!(!plain.contains("jade_shim_struct"), "unused helper emitted:\n{plain}");

    let buf = generate("z", &symbols(&[("rd", sym(&["out_buffer:char", "int"], "int"))])).unwrap();
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
        ("rd", failing_sym(&["int", "out_buffer:short", "int"], "int", CFailure::Negative)),
        ("put", sym(&["bytes", "int"], "int")),
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
    let src =
        generate("lz", &symbols(&[("enc", sym(&["sized_buffer:unsigned char"], "int"))])).unwrap();
    assert!(src.contains("if (argv[0].tag != JADE_FFI_INT) return 1;"), "no count check:\n{src}");
    assert!(src.contains("calloc((size_t)(n_want0 ? n_want0 : 1)"), "not allocated:\n{src}");
    // All of it: the call reports a status, so there is nothing to trim by.
    assert!(src.contains("jade_shim_bytes(sbuf0, (size_t)n_want0"), "not handed back:\n{src}");
    assert!(src.contains("free(sbuf0);"), "leaked:\n{src}");
}

#[test]
fn a_negative_or_absurd_count_is_refused_before_anything_is_allocated() {
    let src =
        generate("lz", &symbols(&[("enc", sym(&["sized_buffer:unsigned char"], "int"))])).unwrap();
    assert!(src.contains("if (n_want0 < 0)"), "unguarded:\n{src}");
}

#[test]
fn a_caller_sized_buffer_shim_compiles() {
    let syms = symbols(&[("enc", sym(&["int", "sized_buffer:unsigned char"], "int"))]);
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

    let paired = generate("fdt", &symbols(&[("f", sym(&["out_str:char"], "int"))])).unwrap();
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

#[test]
fn a_symbol_that_would_shadow_a_shim_helper_is_refused_by_name() {
    // Every wrapper is `jade_shim_<symbol>`, and so is every helper. A library
    // exporting `bytes` would define one of them twice, and the C compiler
    // reports that against generated source hundreds of lines from anything the
    // reader wrote.
    let err = generate("z", &symbols(&[("bytes", sym(&["int"], "int"))])).unwrap_err();
    assert!(err.contains("defined twice"), "should say why: {err}");
    assert!(err.contains("'bytes'"), "should name it: {err}");
}

// ── A callback's user-data slot ──────────────────────────────────────────

#[test]
fn a_callbacks_user_data_is_accepted_and_not_forwarded() {
    // The library will pass one, so the C signature must have it. Jade has
    // nothing to do with it, so nothing is forwarded.
    let s = sym(&["callback:int(int, void*)"], "int");
    let src = generate("ar", &symbols(&[("go", s)])).unwrap();
    assert!(src.contains("static int jade_cbt_go(int a0, void* a1)"), "bad signature:\n{src}");
    assert!(src.contains("(void)a1;"), "should be explicitly unused:\n{src}");
    // One forwarded argument, and it is the first.
    assert!(src.contains("cbargs[0].data.as_int = (int64_t)a0;"), "bad marshal:\n{src}");
    assert!(src.contains("invoke(jade_cb_go->host, 1, cbargs"), "wrong arity:\n{src}");
}

#[test]
fn a_null_pointer_stands_in_for_what_cannot_be_carried() {
    let s = sym(&["null_ptr", "int"], "int");
    let src = generate_with("br", &symbols(&[("go", s)]), &[], &["brotli.h"]).unwrap();
    assert!(src.contains("go(NULL, argv[0].data.as_int)"), "should pass null:\n{src}");
    assert!(src.contains("if (argc != 1) return 1;"), "should take no argument:\n{src}");
}

#[test]
fn a_null_pointer_needs_a_header_to_stand_in_against() {
    // Without one the shim declares the symbol itself, and it does not know what
    // type the null is standing in for.
    let err = generate("br", &symbols(&[("go", sym(&["null_ptr"], "int"))])).unwrap_err();
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
    assert!(src.contains("BOUNDS r = bounds("), "not received by value:\n{src}");
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
    let src = generate("db", &symbols(&[("close", sym(&["handle<sqlite3>"], "int"))])).unwrap();
    assert!(
        src.contains(r#"jade_shim_unwrap(&argv[0], "sqlite3", &h0)"#),
        "missing unwrap:\n{src}"
    );
    assert!(src.contains("close((sqlite3*)h0)"), "should pass the unwrapped pointer:\n{src}");
    // Checked before the call, so the library never sees a wrong-typed pointer.
    let unwrap_at = src.find("jade_shim_unwrap").unwrap();
    let call_at = src.find("close((sqlite3*)").unwrap();
    assert!(unwrap_at < call_at, "the type check must precede the call:\n{src}");
}

#[test]
fn the_wrong_handle_type_is_refused_rather_than_dereferenced() {
    // The check is the entire reason a handle carries a name.
    let src = generate("db", &symbols(&[("step", sym(&["handle<sqlite3_stmt>"], "int"))])).unwrap();
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
    let s = failing_sym(&["str", "out_handle:sqlite3"], "int", CFailure::Nonzero);
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
    let src = generate("db", &symbols(&[("op", sym(&["out_handle:T"], "int"))])).unwrap();
    assert!(src.contains("if (!ohandle0) {"), "must check it was written:\n{src}");
    assert!(src.contains("out->tag = JADE_FFI_NIL;"), "should be nil, not a null handle:\n{src}");
}

#[test]
fn a_handle_and_a_scalar_keep_their_argument_positions() {
    // The unwrap uses the Jade index, which is easy to get wrong once some
    // arguments consume a slot and others do not.
    let s = sym(&["int", "handle<sqlite3>", "str"], "int");
    let src = generate("db", &symbols(&[("f", s)])).unwrap();
    assert!(src.contains(r#"jade_shim_unwrap(&argv[1], "sqlite3", &h1)"#), "wrong index:\n{src}");
    assert!(
        src.contains("f(argv[0].data.as_int, (sqlite3*)h1, argv[2].data.as_str)"),
        "bad call:\n{src}"
    );
}

#[test]
fn the_handle_helper_is_emitted_for_any_of_the_three_forms() {
    let plain = generate("m", &symbols(&[("f", sym(&["int"], "int"))])).unwrap();
    assert!(!plain.contains("jade_shim_handle"), "unused helper emitted:\n{plain}");

    for spec in
        [sym(&["handle<T>"], "int"), sym(&["int"], "handle<T>"), sym(&["out_handle:T"], "int")]
    {
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
    let s = sym(&["int", "callback:int(int, const char*)"], "int");
    let src = generate("z", &symbols(&[("each", s)])).unwrap();
    // The library's own C types, not Jade's widened ones: this declares a
    // function pointer the library will store and call, so `int` must be `int`.
    // Widening it is not a truncation but an incompatible function pointer.
    assert!(
        src.contains("static int jade_cbt_each(int a0, const char* a1)"),
        "bad trampoline:\n{src}"
    );
    assert!(
        src.contains("extern int64_t each(int64_t, int (*)(int, const char*));"),
        "bad decl:\n{src}"
    );
    assert!(
        src.contains("each(argv[0].data.as_int, jade_cbt_each)"),
        "should pass the trampoline:\n{src}"
    );
}

#[test]
fn the_registration_lasts_exactly_one_call() {
    // A library that stores the callback and invokes it later must find an
    // empty slot, not a stale pointer into an interpreter that has moved on.
    let s = sym(&["callback:int(int)"], "int");
    let src = generate("z", &symbols(&[("go", s)])).unwrap();
    assert!(src.contains("jade_cb_go = argv[0].data.as_fn;"), "should register:\n{src}");
    assert!(src.contains("jade_cb_go = NULL;"), "should unregister:\n{src}");
    assert!(src.contains("if (!jade_cb_go)"), "should answer neutrally when empty:\n{src}");
    assert!(src.contains("_Thread_local"), "the slot must be per-thread:\n{src}");
}

#[test]
fn a_raise_inside_a_callback_is_deferred_rather_than_unwound() {
    // Longjmping out of the trampoline would unwind through the C library's
    // own frames, past whatever it was in the middle of.
    let s = sym(&["callback:int(int)"], "int");
    let src = generate("z", &symbols(&[("go", s)])).unwrap();
    assert!(src.contains("jade_cb_failed_go = 1;"), "should record the failure:\n{src}");
    assert!(src.contains("if (jade_cb_failed_go) {"), "should check after the call:\n{src}");
    assert!(src.contains("the callback raised"), "should surface it:\n{src}");
    // Recorded inside the trampoline, surfaced only after the library returns.
    let record = src.find("jade_cb_failed_go = 1;").unwrap();
    let surface = src.find("if (jade_cb_failed_go) {").unwrap();
    assert!(record < surface, "the raise must surface after the call, not during it");
}

#[test]
fn a_void_callback_and_a_zero_argument_callback_both_work() {
    let src = generate(
        "z",
        &symbols(&[
            ("a", sym(&["callback:void(int)"], "int")),
            ("b", sym(&["callback:int()"], "int")),
        ]),
    )
    .unwrap();
    assert!(src.contains("static void jade_cbt_a(int a0)"), "void callback:\n{src}");
    assert!(src.contains("static int jade_cbt_b(void)"), "no-arg callback:\n{src}");
}

#[test]
fn a_malformed_or_unrepresentable_callback_is_refused() {
    for bad in
        ["callback:int", "callback:int(", "callback:int(struct foo)", "callback:double*(int)"]
    {
        let s = sym(&[bad], "int");
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
        generate("z", &symbols(&[("f", sym(&["int", "out_scalar:uint32_t"], "nil"))])).unwrap();
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
    let src = generate("z", &symbols(&[("f", sym(&["int", "inout_scalar:int"], "nil"))])).unwrap();
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
    let s = sym(&["int", "out_scalar:int@quot", "out_scalar:int@rem"], "int");
    let src = generate("z", &symbols(&[("divmod", s)])).unwrap();
    assert!(src.contains(r#"jade_shim_struct("divmod_result", 3)"#), "three keys:\n{src}");
    assert!(src.contains(r#"strdup("ret")"#), "missing ret:\n{src}");
}

#[test]
fn one_out_beside_a_return_still_comes_back_as_ret_and_out() {
    // The shape that existed before multiple outs, unchanged.
    let s = sym(&["int", "out_scalar:int"], "int");
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
    let s = sym(&["out_scalar:int@quot", "out_scalar:int@rem"], "int");
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
