use super::*;

/// Write a header to a temp file and bind it.
///
/// Real clang, on real C. A mocked AST would only prove the mapper agrees with
/// my idea of clang's output, which is precisely the assumption most likely to
/// be wrong.
fn bind(src: &str) -> Binding {
    bind_filtered(src, None)
}

fn bind_filtered(src: &str, only: Option<&str>) -> Binding {
    let dir = std::env::temp_dir().join(format!(
        "jade-bindgen-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let h = dir.join("probe.h");
    std::fs::write(&h, src).unwrap();
    let out = from_header(&h, &[], only);
    let _ = std::fs::remove_dir_all(&dir);
    out.expect("binding should succeed")
}

fn args(b: &Binding, sym: &str) -> Vec<String> {
    b.symbols.get(sym).unwrap_or_else(|| panic!("{sym} not bound: {:?}", b.skipped)).args.clone()
}

fn ret(b: &Binding, sym: &str) -> String {
    b.symbols.get(sym).unwrap_or_else(|| panic!("{sym} not bound: {:?}", b.skipped)).ret.clone()
}

fn why_skipped(b: &Binding, sym: &str) -> String {
    b.skipped
        .iter()
        .find(|(s, _)| s == sym)
        .map(|(_, w)| w.clone())
        .unwrap_or_else(|| panic!("{sym} was not skipped; bound as {:?}", b.symbols.get(sym)))
}

// ── Scalars ──────────────────────────────────────────────────────────────

#[test]
fn integer_widths_all_collapse_to_int() {
    // The FFI has one integer type. A short widening to 64 bits is exact, and
    // the reverse is the implicit conversion a hand-written binding relied on
    // anyway.
    let b = bind(
        "#include <stdint.h>\n#include <stddef.h>\n\
         int f1(char a, short b, int c, long d, long long e);\n\
         int f2(unsigned char a, unsigned long b, size_t c);\n\
         int f3(int8_t a, uint64_t b, ptrdiff_t c);\n",
    );
    assert_eq!(args(&b, "f1"), ["int", "int", "int", "int", "int"]);
    assert_eq!(args(&b, "f2"), ["int", "int", "int"]);
    assert_eq!(args(&b, "f3"), ["int", "int", "int"]);
}

#[test]
fn floats_and_bools_map() {
    let b = bind("#include <stdbool.h>\ndouble f(float a, double c, bool d);\n");
    assert_eq!(args(&b, "f"), ["float", "float", "bool"]);
    assert_eq!(ret(&b, "f"), "float");
}

#[test]
fn a_void_return_is_nil_and_void_params_are_no_params() {
    let b = bind("void f(void);\n");
    assert_eq!(ret(&b, "f"), "nil");
    assert!(args(&b, "f").is_empty());
}

#[test]
fn a_const_char_pointer_is_a_string_in_both_directions() {
    let b = bind("const char* f(const char* p);\n");
    assert_eq!(args(&b, "f"), ["str"]);
    assert_eq!(ret(&b, "f"), "str");
}

// ── Handles ──────────────────────────────────────────────────────────────

#[test]
fn an_opaque_typedef_becomes_a_handle() {
    // `typedef struct X X;` with no definition is the universal C idiom for
    // "you may hold this, you may not look inside" — exactly a handle.
    let b = bind(
        "typedef struct sqlite3 sqlite3;\n\
         int sqlite3_close(sqlite3* db);\n\
         const char* sqlite3_errmsg(sqlite3* db);\n",
    );
    assert_eq!(args(&b, "sqlite3_close"), ["handle<sqlite3>"]);
    assert_eq!(args(&b, "sqlite3_errmsg"), ["handle<sqlite3>"]);
}

#[test]
fn a_pointer_to_pointer_of_an_opaque_type_is_an_out_handle() {
    // sqlite3_open(path, &db) — how every SQLite connection is made. Without
    // this the generator could bind SQLite's whole surface except the one call
    // that produces the connection, which is the same as binding none of it.
    let b = bind(
        "typedef struct sqlite3 sqlite3;\n\
         int sqlite3_open(const char* path, sqlite3** db);\n",
    );
    assert_eq!(args(&b, "sqlite3_open"), ["str", "out_handle:sqlite3"]);
    assert_eq!(ret(&b, "sqlite3_open"), "int");
}

#[test]
fn two_opaque_types_stay_distinct() {
    let b = bind(
        "typedef struct sqlite3 sqlite3;\n\
         typedef struct sqlite3_stmt sqlite3_stmt;\n\
         int step(sqlite3_stmt* s);\n\
         int close(sqlite3* d);\n",
    );
    assert_eq!(args(&b, "step"), ["handle<sqlite3_stmt>"]);
    assert_eq!(args(&b, "close"), ["handle<sqlite3>"]);
}

#[test]
fn a_returned_opaque_pointer_is_a_handle() {
    let b = bind("typedef struct F F;\nF* f_open(const char* p);\n");
    assert_eq!(ret(&b, "f_open"), "handle<F>");
}

// ── Buffers ──────────────────────────────────────────────────────────────

#[test]
fn a_const_byte_pointer_beside_a_length_is_one_bytes_argument() {
    // Two C parameters, one Jade argument — the pair has to be recognised
    // together or not at all.
    let b = bind(
        "#include <stddef.h>\n\
         unsigned long crc(unsigned long c, const unsigned char* buf, size_t len);\n",
    );
    assert_eq!(args(&b, "crc"), ["int", "bytes"]);
}

#[test]
fn a_writable_buffer_beside_a_count_is_an_assumed_out_buffer() {
    let b = bind("typedef struct F F;\nint f_read(F* f, short* buf, int n);\n");
    assert_eq!(args(&b, "f_read"), ["handle<F>", "out_buffer:short", "int"]);
    // Bound, but the guess is reported rather than buried: a non-const pointer
    // is *almost* always an out-buffer, and almost is not always.
    let (sym, why) = b.assumed.iter().find(|(s, _)| s == "f_read").expect("should be listed");
    assert_eq!(sym, "f_read");
    assert!(why.contains("change it to `bytes`"), "should name the fix: {why}");
}

#[test]
fn a_writable_buffer_with_no_count_returned_is_skipped() {
    // The shim sizes the result from the return value; without one there is
    // nothing to size it by.
    let b = bind("void f(char* buf, int n);\n");
    assert!(why_skipped(&b, "f").contains("buffer"));
}

// ── Structs ──────────────────────────────────────────────────────────────

#[test]
fn a_writable_struct_pointer_is_an_out_parameter_and_the_table_follows() {
    let b = bind(
        "#include <stdint.h>\n\
         typedef struct { int64_t frames; int rate; const char* title; } SF_INFO;\n\
         int sf_open(const char* p, int mode, SF_INFO* info);\n",
    );
    assert_eq!(args(&b, "sf_open"), ["str", "int", "out_struct:SF_INFO"]);

    let s = b.structs.get("SF_INFO").expect("struct table should be emitted");
    assert_eq!(
        s.fields,
        [
            ("frames".to_string(), "int".to_string()),
            ("rate".to_string(), "int".to_string()),
            ("title".to_string(), "str".to_string()),
        ]
    );
}

#[test]
fn an_unrepresentable_field_is_dropped_rather_than_the_whole_struct() {
    let b = bind(
        "typedef struct { int ok; void* opaque; int also_ok; } S;\n\
         void f(S* s);\n",
    );
    let s = b.structs.get("S").expect("should still bind");
    let names: Vec<&str> = s.fields.iter().map(|(f, _)| f.as_str()).collect();
    assert_eq!(names, ["ok", "also_ok"], "the unusable field should be dropped, not the struct");
}

#[test]
fn a_struct_read_as_input_is_skipped_because_the_shim_cannot_do_that_direction() {
    let b = bind("typedef struct { int a; } S;\nint f(const S* s);\n");
    assert!(why_skipped(&b, "f").contains("input"));
}

#[test]
fn a_struct_by_value_is_skipped() {
    let b = bind("typedef struct { int a; } S;\nint f(S s);\n");
    assert!(why_skipped(&b, "f").contains("by value"));
}

// ── What it refuses ──────────────────────────────────────────────────────

#[test]
fn callbacks_varargs_and_void_pointers_are_named_not_silently_dropped() {
    // The skip report is the feature. A generator that binds two thirds of an
    // API and reports success is how you find the missing third at run time.
    let b = bind(
        "int with_cb(int (*cb)(int), int n);\n\
         int fmt(const char* f, ...);\n\
         int with_ud(void* user_data);\n",
    );
    assert!(why_skipped(&b, "with_cb").contains("callback"));
    assert!(why_skipped(&b, "fmt").contains("varargs"));
    assert!(why_skipped(&b, "with_ud").contains("void"));
    assert!(b.symbols.is_empty(), "none of these should be bound");
}

#[test]
fn an_inline_definition_is_skipped_because_it_exports_no_symbol() {
    let b = bind("static inline int add(int a, int b) { return a + b; }\n");
    assert!(why_skipped(&b, "add").contains("inline"));
}

#[test]
fn declarations_from_included_headers_are_not_bound() {
    // Without the file filter this would bind every declaration in stdio.h.
    let b = bind("#include <stdio.h>\nint mine(int x);\n");
    assert!(b.symbols.contains_key("mine"));
    assert!(!b.symbols.contains_key("printf"), "should not reach into stdio.h");
    assert!(!b.symbols.contains_key("fopen"), "should not reach into stdio.h");
    assert!(b.symbols.len() == 1, "bound: {:?}", b.symbols.keys().collect::<Vec<_>>());
}

// ── Inference of the failure convention ──────────────────────────────────

#[test]
fn a_pointer_returning_open_gets_a_null_convention() {
    let b = bind("typedef struct F F;\nF* f_open(const char* p);\n");
    assert_eq!(b.symbols["f_open"].fails_when, Some(CFailure::Null));
}

#[test]
fn a_status_beside_an_out_handle_gets_a_nonzero_convention() {
    let b = bind("typedef struct F F;\nint f_open(const char* p, F** out);\n");
    assert_eq!(b.symbols["f_open"].fails_when, Some(CFailure::Nonzero));
}

#[test]
fn a_plain_int_return_gets_no_convention() {
    // Guessing that every int is a status would make a function returning a
    // legitimate count raise on a perfectly good answer.
    let b = bind("int count_things(int a);\n");
    assert_eq!(b.symbols["count_things"].fails_when, None);
}

// ── Filtering and reporting ──────────────────────────────────────────────

#[test]
fn only_narrows_a_large_header_to_the_part_you_want() {
    let src = "int db_open(int a);\nint db_close(int a);\nint other_thing(int a);\n";
    let b = bind_filtered(src, Some("db_"));
    assert_eq!(b.symbols.len(), 2);
    assert!(b.symbols.contains_key("db_open") && b.symbols.contains_key("db_close"));
    // A filtered-out symbol is not a skip: it was never asked for.
    assert!(b.skipped.is_empty(), "filtering should not report skips: {:?}", b.skipped);
}

#[test]
fn the_report_counts_everything_and_groups_skips_by_reason() {
    let b = bind(
        "int ok1(int a);\n\
         int ok2(int a);\n\
         int cb1(int (*f)(int));\n\
         int cb2(int (*f)(int));\n\
         int va(const char* f, ...);\n",
    );
    let r = b.report();
    assert!(r.starts_with("2 bound, 0 assumed, 3 skipped"), "bad summary: {r}");
    // Two symbols skipped for one reason is one fact, printed once.
    assert_eq!(r.matches("takes a callback").count(), 1, "reasons should group: {r}");
    assert!(r.contains("cb1, cb2"), "should name them: {r}");
}

#[test]
fn a_header_that_does_not_parse_is_an_error_not_a_partial_binding() {
    // A missing include yields a half-parsed AST, and binding what survived
    // would produce a silently incomplete table.
    let dir = std::env::temp_dir().join(format!("jade-bindgen-bad-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let h = dir.join("bad.h");
    std::fs::write(&h, "#include <no_such_header_anywhere.h>\nint f(int);\n").unwrap();
    let err = from_header(&h, &[], None).expect_err("should refuse");
    assert!(err.contains("clang could not parse"), "unexpected: {err}");
    assert!(err.contains("-I"), "should suggest the fix: {err}");
    let _ = std::fs::remove_dir_all(&dir);
}

// ── The generator and the shim have to agree ─────────────────────────────
//
// These two halves are written against the same vocabulary but in different
// files, and nothing else checks that the strings one emits are strings the
// other accepts. A new spelling added to `bindgen` and not to `cshim` would
// pass every test above and then fail at `jade pkg install` on a user's
// machine — which is exactly how the bytes marshaller shipped broken.

/// Bind a header, hand the result straight to the shim generator, and compile
/// the C that comes out.
fn round_trip(header_src: &str) -> Result<String, String> {
    let dir = std::env::temp_dir().join(format!(
        "jade-bindgen-rt-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let h = dir.join("lib.h");
    std::fs::write(&h, header_src).unwrap();

    let b = from_header(&h, &[], None).map_err(|e| format!("bind failed: {e}"))?;
    let symbols: std::collections::HashMap<String, CSymbol> =
        b.symbols.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let structs: std::collections::HashMap<String, CStruct> =
        b.structs.iter().map(|(k, v)| (k.clone(), v.clone())).collect();

    let src = crate::pkg::cshim::generate("lib", &symbols, &structs, &["lib.h".to_string()])
        .map_err(|e| format!("the shim rejected what the generator emitted: {e}"))?;
    std::fs::write(dir.join("shim.c"), &src).unwrap();

    let out = std::process::Command::new("cc")
        .args(["-c", "-Wall", "-Werror"])
        .arg(format!("-I{}", dir.display()))
        .arg("-o")
        .arg(dir.join("shim.o"))
        .arg(dir.join("shim.c"))
        .output()
        .expect("cc must be available");

    let result = if out.status.success() {
        Ok(src.clone())
    } else {
        Err(format!(
            "generated shim does not compile:\n{}\n--- source ---\n{src}",
            String::from_utf8_lossy(&out.stderr)
        ))
    };
    let _ = std::fs::remove_dir_all(&dir);
    result
}

#[test]
fn a_handle_library_binds_and_compiles_end_to_end() {
    // The SQLite shape: an out-handle open, two distinct handle types, a
    // string return, and a close.
    round_trip(
        "typedef struct db db;\n\
         typedef struct cursor cursor;\n\
         int db_open(const char* path, db** out);\n\
         int db_close(db* d);\n\
         const char* db_error(db* d);\n\
         int cur_open(db* d, cursor** out);\n\
         const char* cur_next(cursor* c);\n\
         int cur_close(cursor* c);\n",
    )
    .unwrap();
}

#[test]
fn a_buffer_and_struct_library_binds_and_compiles_end_to_end() {
    round_trip(
        "#include <stddef.h>\n\
         #include <stdint.h>\n\
         typedef struct { int64_t frames; int rate; const char* title; } info_t;\n\
         typedef struct snd snd;\n\
         int snd_open(const char* p, snd** out);\n\
         int snd_read(snd* s, short* buf, int n);\n\
         int snd_write(snd* s, const void* data, size_t len);\n\
         int snd_stat(snd* s, info_t* out);\n",
    )
    .unwrap();
}

#[test]
fn every_spelling_the_generator_emits_is_one_the_shim_accepts() {
    // Drives one header through every mapping the generator has, so a spelling
    // added on one side and not the other cannot pass unnoticed.
    let src = round_trip(
        "#include <stddef.h>\n\
         #include <stdint.h>\n\
         typedef long long big_t;\n\
         typedef struct opaque opaque;\n\
         typedef struct { int a; const char* b; double c; } rec_t;\n\
         int f_scalars(char a, short b, int c, long d, float e, double g, _Bool h);\n\
         big_t f_typedef_int(big_t x);\n\
         const char* f_str(const char* s);\n\
         opaque* f_ret_handle(void);\n\
         int f_out_handle(opaque** out);\n\
         int f_handle_arg(opaque* h);\n\
         int f_bytes(const void* d, size_t n);\n\
         int f_out_buffer(opaque* h, char* buf, int n);\n\
         int f_out_struct(opaque* h, rec_t* out);\n\
         void f_void(void);\n",
    )
    .unwrap();

    // And that each really did map, rather than being quietly skipped into a
    // trivially-compiling file.
    for expect in [
        "jade_shim_f_scalars",
        "jade_shim_f_typedef_int",
        "jade_shim_f_str",
        "jade_shim_f_ret_handle",
        "jade_shim_f_out_handle",
        "jade_shim_f_handle_arg",
        "jade_shim_f_bytes",
        "jade_shim_f_out_buffer",
        "jade_shim_f_out_struct",
        "jade_shim_f_void",
    ] {
        assert!(src.contains(expect), "{expect} missing from the shim");
    }
}
