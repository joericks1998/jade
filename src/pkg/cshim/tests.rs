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
                },
            )
        })
        .collect();
    let headers: Vec<String> = headers.iter().map(|h| h.to_string()).collect();
    super::generate(name, syms, &structs, &headers)
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
    assert!(src.contains("if (argv[0].tag != JADE_FFI_BYTES) return 1;"), "missing tag check:\n{src}");
    assert!(src.contains("as_bytes->data"), "should pass the pointer:\n{src}");
    assert!(src.contains("as_bytes->len"), "should pass the length:\n{src}");
    // One Jade argument, two C parameters.
    assert!(src.contains("if (argc != 1) return 1;"), "arity should be the Jade one:\n{src}");
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
    assert!(src.contains("extern int64_t sf_read_short(int64_t, short*, int64_t);"), "bad decl:\n{src}");
    assert!(src.contains("if (argc != 2) return 1;"), "buffer must not consume an argument:\n{src}");

    // Sized from the argument after it, which is the element count.
    assert!(src.contains("int64_t n_elem = argv[1].data.as_int;"), "wrong count source:\n{src}");
    assert!(src.contains("sizeof(short)"), "must size by the element type:\n{src}");

    // The return value is the fill count, so it sizes the blob rather than
    // coming back separately.
    assert!(src.contains("out->tag = JADE_FFI_BYTES;"), "should return bytes:\n{src}");
    assert!(src.contains("free(obuf);"), "scratch must be released:\n{src}");
}

#[test]
fn a_failing_out_buffer_call_frees_its_scratch_before_raising() {
    // A raise that leaks the scratch would leak once per failed call, which on
    // a read loop hitting EOF is every iteration.
    let s = failing_sym(&["int", "out_buffer:char", "int"], "int", CFailure::Negative);
    let src = generate("z", &symbols(&[("rd", s)])).unwrap();
    let fail_block = &src[src.find("if ((r) < 0)").expect("failure test")..];
    let raise_at = fail_block.find("JADE_FFI_ERROR").unwrap();
    let free_at = fail_block.find("free(obuf);").unwrap();
    assert!(free_at < raise_at, "scratch must be freed before the error return:\n{src}");
}

#[test]
fn a_short_read_is_clamped_to_what_was_allocated() {
    // A library reporting more than it was given would otherwise make the copy
    // read past the scratch.
    let s = sym(&["int", "out_buffer:char", "int"], "int");
    let src = generate("z", &symbols(&[("rd", s)])).unwrap();
    assert!(src.contains("r > n_elem ? n_elem : r"), "missing clamp:\n{src}");
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
fn at_most_one_out_parameter() {
    let s = sym(&["out_buffer:char", "int", "out_buffer:char", "int"], "int");
    let err = generate("z", &symbols(&[("rd", s)])).unwrap_err();
    assert!(err.contains("at most one"), "unexpected: {err}");
}

#[test]
fn a_c_type_that_is_not_an_identifier_is_refused() {
    // The text goes straight into generated C, so this is an injection guard as
    // much as a typo guard.
    for bad in ["short; evil()", "char*", "1int", ""] {
        let s = sym(&[format!("out_buffer:{bad}").as_str(), "int"], "int");
        assert!(
            generate("z", &symbols(&[("rd", s)])).is_err(),
            "should refuse out_buffer:{bad}"
        );
    }
}

// ── Struct out-parameters ────────────────────────────────────────────────

const SF_INFO: &[(&str, &str)] = &[
    ("frames", "int"),
    ("samplerate", "int"),
    ("channels", "int"),
];

#[test]
fn a_struct_out_parameter_is_a_zeroed_local_passed_by_address() {
    let s = failing_sym(&["str", "int", "out_struct:SF_INFO"], "int", CFailure::Null);
    let src = generate_with(
        "snd",
        &symbols(&[("sf_open", s)]),
        &[("SF_INFO", SF_INFO)],
        &["sndfile.h"],
    )
    .unwrap();

    assert!(src.contains("#include <sndfile.h>"), "must include the header:\n{src}");
    assert!(src.contains("SF_INFO ostruct;"), "must declare a real local:\n{src}");
    assert!(src.contains("memset(&ostruct, 0, sizeof ostruct);"), "must zero it:\n{src}");
    assert!(src.contains("&ostruct"), "must pass its address:\n{src}");
    assert!(src.contains("if (argc != 2) return 1;"), "out-param takes no Jade arg:\n{src}");
}

#[test]
fn the_header_declares_the_symbol_rather_than_the_shim() {
    // A hand-written prototype that disagrees with the real one — `int` where
    // the library says `long` — truncates silently at run time. Letting the
    // header win turns that into a compile error, which is the whole reason to
    // require one.
    let s = sym(&["str", "int", "out_struct:SF_INFO"], "int");
    let src = generate_with("snd", &symbols(&[("sf_open", s)]), &[("SF_INFO", SF_INFO)], &["sndfile.h"]).unwrap();
    assert!(!src.contains("extern int64_t sf_open"), "must not redeclare:\n{src}");
}

#[test]
fn a_returned_value_and_a_filled_struct_come_back_as_ret_and_out() {
    let s = sym(&["str", "int", "out_struct:SF_INFO"], "int");
    let src = generate_with("snd", &symbols(&[("sf_open", s)]), &[("SF_INFO", SF_INFO)], &["sndfile.h"]).unwrap();
    assert!(src.contains(r#"jade_shim_struct("sf_open_result", 2)"#), "missing pair:\n{src}");
    assert!(src.contains(r#"res->keys[0] = strdup("ret");"#), "missing ret:\n{src}");
    assert!(src.contains(r#"res->keys[1] = strdup("out");"#), "missing out:\n{src}");
}

#[test]
fn a_void_call_returns_the_filled_struct_directly() {
    // With nothing else to report there is no pair to make, so the common case
    // stays clean rather than paying for the general one.
    let s = sym(&["out_struct:SF_INFO"], "nil");
    let src = generate_with("snd", &symbols(&[("stat_it", s)]), &[("SF_INFO", SF_INFO)], &["sndfile.h"]).unwrap();
    assert!(!src.contains("_result"), "should not wrap:\n{src}");
    assert!(src.contains("out->data.as_struct = ostruct_j;"), "should return it directly:\n{src}");
}

#[test]
fn struct_field_strings_are_copied_not_borrowed() {
    // A value inside a container is container-owned, so Jade's ffi_free frees
    // it. Handing over a pointer into a stack local would be a free of the
    // stack.
    let s = sym(&["out_struct:INFO"], "nil");
    let src = generate_with("z", &symbols(&[("f", s)]), &[("INFO", &[("name", "str")])], &["z.h"]).unwrap();
    assert!(src.contains("strdup((ostruct.name)"), "field string must be copied:\n{src}");
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
    let err = generate_with("snd", &symbols(&[("f", s)]), &[("SF_INFO", SF_INFO)], &[]).unwrap_err();
    assert!(err.contains("headers"), "should ask for a header: {err}");
    assert!(err.contains("wrong offsets"), "should say why: {err}");
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
    let fields: &[(&str, &str)] = &[
        ("frames", "int"),
        ("samplerate", "int"),
        ("channels", "int"),
        ("title", "str"),
    ];
    let src = generate_with("snd", &syms, &[("SF_INFO", fields)], &["fixture.h"]).unwrap();
    if let Err(e) = compiles(&src, &[("fixture.h", header)]) {
        panic!("struct shim does not compile:\n{e}\n--- source ---\n{src}");
    }
}

#[test]
fn a_field_the_struct_does_not_have_fails_at_compile_time() {
    // The failure mode you want: naming a field that is not there is caught by
    // the C compiler against the real header, not by writing at a wrong offset.
    let header = "typedef struct { int frames; } SF_INFO;\nextern void f(SF_INFO*);\n";
    let syms = symbols(&[("f", sym(&["out_struct:SF_INFO"], "nil"))]);
    let src = generate_with("snd", &syms, &[("SF_INFO", &[("nosuch", "int")])], &["fixture.h"]).unwrap();
    let err = compiles(&src, &[("fixture.h", header)]).expect_err("should not compile");
    assert!(err.contains("nosuch"), "the error should name the field: {err}");
}
