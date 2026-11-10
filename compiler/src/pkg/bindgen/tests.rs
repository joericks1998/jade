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
    let out = from_header(&h, &[], &[], only, None);
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
fn a_type_reachable_only_by_its_tag_keeps_the_keyword() {
    // `typedef struct X_s X;` is far more common than the `typedef struct X X;`
    // every other fixture here uses, and the difference was invisible until it
    // reached the compiler: `normalize` drops `struct` so a lookup does not
    // have to care how the type was written, and the stripped name was then
    // used as the source text too. `Ctx_s` is not a type in C — only
    // `struct Ctx_s` is — so the shim failed to compile with "must use 'struct'
    // tag to refer to type".
    let b = bind(
        "struct Ctx_s;\n\
         struct Ctx_s* ctx_new(void);\n\
         int ctx_free(struct Ctx_s* c);\n\
         int ctx_open(const char* p, struct Ctx_s** out);\n",
    );
    assert_eq!(ret(&b, "ctx_new"), "handle<struct Ctx_s>");
    assert_eq!(args(&b, "ctx_free"), ["handle<struct Ctx_s>"]);
    assert_eq!(args(&b, "ctx_open"), ["str", "out_handle:struct Ctx_s"]);
}

#[test]
fn a_typedef_of_a_pointer_to_a_tagged_struct_keeps_the_keyword_too() {
    // `typedef struct Pool_s *Pool;` reaches the same place by a different
    // route: the typedef names the *pointer*, so it cannot stand in for the
    // pointee, and the tag is the only spelling of the thing pointed at.
    let b = bind(
        "typedef struct Pool_s *Pool;\n\
         Pool pool_new(void);\n\
         int pool_free(Pool p);\n",
    );
    assert_eq!(ret(&b, "pool_new"), "handle<struct Pool_s>");
    assert_eq!(args(&b, "pool_free"), ["handle<struct Pool_s>"]);
}

#[test]
fn a_tag_that_a_typedef_also_names_stays_bare() {
    // `typedef struct sqlite3 sqlite3;` makes the bare name a type, so adding
    // the keyword would be noise — and it is the shape every handle in this
    // file used before, which is exactly why the bug above went unnoticed.
    let b = bind(
        "typedef struct sqlite3 sqlite3;\n\
         int sqlite3_close(sqlite3* db);\n",
    );
    assert_eq!(args(&b, "sqlite3_close"), ["handle<sqlite3>"]);
}

#[test]
fn a_struct_out_parameter_reachable_only_by_its_tag_keeps_the_keyword() {
    // The same flaw, on the other spec that writes a C type into the shim.
    // The generated table is keyed to match the spec, since that is the name
    // the shim looks the definition up by.
    let b = bind(
        "struct Ctx_s;\n\
         struct Info { int rate; int chans; };\n\
         int info_get(struct Ctx_s* c, struct Info* out);\n",
    );
    assert_eq!(args(&b, "info_get"), ["handle<struct Ctx_s>", "out_struct:struct Info"]);
    assert!(b.structs.contains_key("struct Info"), "structs table: {:?}", b.structs.keys());
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
fn a_struct_read_as_input_is_one_the_caller_builds() {
    let b = bind("typedef struct { int a; } S;\nint f(const S* s);\n");
    assert_eq!(args(&b, "f"), ["in_struct:S"]);
}

#[test]
fn a_struct_by_value_is_skipped() {
    let b = bind("typedef struct { int a; } S;\nint f(S s);\n");
    assert!(why_skipped(&b, "f").contains("by value"));
}

#[test]
fn a_struct_returned_by_value_is_bound() {
    // The other direction, and much easier: nothing crosses the boundary but
    // the value, so there is no allocation and no ownership to settle.
    // `ZSTD_bounds ZSTD_cParam_getBounds(ZSTD_cParameter)` is this shape.
    let b = bind(
        "#include <stddef.h>\n\
         typedef struct { size_t error; int lowerBound; int upperBound; } BOUNDS;\n\
         BOUNDS getBounds(int p);\n",
    );
    assert_eq!(ret(&b, "getBounds"), "struct:BOUNDS");
    assert!(b.structs.contains_key("BOUNDS"), "the field table must come out too");
}

// ── What it refuses ──────────────────────────────────────────────────────

#[test]
fn callbacks_varargs_and_void_pointers_are_named_not_silently_dropped() {
    // The skip report is the feature. A generator that binds two thirds of an
    // API and reports success is how you find the missing third at run time.
    let b = bind(
        "typedef struct rec rec;\n\
         int with_cb(int (*cb)(rec*), int n);\n\
         int fmt(const char* f, ...);\n\
         void release(void* p);\n",
    );
    // A callback whose *own* signature the FFI cannot carry — here a parameter
    // that is a pointer to something opaque, which the trampoline has no way to
    // hand Jade.
    assert!(why_skipped(&b, "with_cb").contains("callback"));
    assert!(why_skipped(&b, "fmt").contains("varargs"));
    // A lone `void *` on a call that reports nothing is a deallocator, and
    // handing one shim-owned scratch would have the library free it and the
    // shim free it again on the way out.
    assert!(why_skipped(&b, "release").contains("frees what it is given"));
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
         int cb1(void* (*f)(int));\n\
         int cb2(void* (*f)(int));\n\
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
    let err = from_header(&h, &[], &[], None, None).expect_err("should refuse");
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

    let b = from_header(&h, &[], &[], None, None).map_err(|e| format!("bind failed: {e}"))?;
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
fn a_tag_only_library_binds_and_compiles_end_to_end() {
    // Every other round-trip fixture writes `typedef struct X X;`, where the
    // tag and the typedef share a name and the missing `struct` keyword makes
    // no difference. That is why a whole class of real header — zstd, and
    // anything else using `typedef struct X_s X;` — generated a shim that would
    // not compile, with nothing in the suite to notice. This covers each of the
    // three specs that write a C type name: a handle, an out-handle, and an
    // out-struct.
    round_trip(
        "struct Ctx_s;\n\
         struct Info { int rate; int chans; };\n\
         typedef struct Pool_s *Pool;\n\
         struct Ctx_s* ctx_new(void);\n\
         int ctx_free(struct Ctx_s* c);\n\
         int ctx_open(const char* p, struct Ctx_s** out);\n\
         int info_get(struct Ctx_s* c, struct Info* out);\n\
         Pool pool_new(void);\n\
         int pool_free(Pool p);\n",
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
         int f_out_scalar(opaque* h, int* written);\n\
         void f_two_outs(unsigned long long* progress_in, unsigned long long* progress_out);\n\
         int f_ret_and_two_outs(int a, int* quot, int* rem);\n\
         void f_void(void);\n\
         typedef const void* ctx_t;\n\
         typedef int cbret_t;\n\
         int f_cb_typedefs(cbret_t (*cb)(int, ctx_t), ctx_t data);\n",
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
        "jade_shim_f_out_scalar",
        "jade_shim_f_two_outs",
        "jade_shim_f_ret_and_two_outs",
        "jade_shim_f_bytes",
        "jade_shim_f_out_buffer",
        "jade_shim_f_out_struct",
        "jade_shim_f_void",
    ] {
        assert!(src.contains(expect), "{expect} missing from the shim");
    }
}

// ── Callbacks ────────────────────────────────────────────────────────────

#[test]
fn a_callback_keeps_the_librarys_own_c_types() {
    // Not translated to Jade's. The shim declares a function pointer the
    // library will store and call, so `int` has to stay `int` — widening it is
    // not a truncation but an incompatible function pointer.
    let b = bind("int each(int n, int (*cb)(int, const char*));\n");
    assert_eq!(args(&b, "each"), ["int", "callback:int(int, const char *)"]);
}

#[test]
fn a_void_returning_callback_maps() {
    let b = bind("int walk(void (*cb)(int));\n");
    assert_eq!(args(&b, "walk"), ["callback:void(int)"]);
}

#[test]
fn a_callback_the_trampoline_cannot_marshal_is_skipped_by_that_reason() {
    // A callback that hands back a pointer is the shape that genuinely does not
    // fit: Jade has no value to return there. Brotli's allocator hooks are this.
    let b = bind("int go(void* (*cb)(int));\n");
    assert!(why_skipped(&b, "go").contains("callback"), "{:?}", b.skipped);
    assert!(!b.symbols.contains_key("go"));
}

#[test]
fn a_refused_callback_names_the_spelling_that_gets_past_it() {
    // Passing null is what tells brotli to fall back on malloc, and it is never
    // inferred: a library that requires a real pointer there gets a null
    // dereference with no diagnostic, which is the worst failure this generator
    // can produce.
    let b = bind("int go(void* (*cb)(int));\n");
    assert!(why_skipped(&b, "go").contains("null_ptr"), "{:?}", b.skipped);
}

#[test]
fn a_callbacks_user_data_parameter_does_not_make_it_unbindable() {
    // C has no closures, so a callback that needs context takes a `void *`. A
    // Jade function carries its own environment, so the trampoline accepts the
    // parameter — the library will pass one — and does not forward it. Refusing
    // the whole callback over the one parameter Jade has no use for made every
    // c-ares callback unbindable.
    let b = bind("int go(int (*cb)(int fd, int kind, void* data));\n");
    assert_eq!(args(&b, "go"), ["callback:int(int, int, void *)"]);
}

#[test]
fn the_void_pointer_beside_a_callback_is_context_and_routes_each_registration() {
    // `ares_set_socket_callback(ch, cb, void *data)`. Decided by position,
    // because the position is the convention.
    //
    // Bound `callback_data`, not `null_ptr`. Both hand the library something it
    // gives back untouched; only this one makes it a per-registration cookie, so
    // two callbacks registered through one symbol stay apart instead of both
    // resolving to whichever was passed last.
    let b = bind(
        "typedef struct chan chan;\n\
         void set_cb(chan* c, int (*cb)(int, void*), void* data);\n",
    );
    assert_eq!(args(&b, "set_cb"), ["handle<chan>", "callback:int(int, void *)", "callback_data"]);
    // And with the slot filled there is nothing left to warn about.
    assert!(
        !b.assumed.iter().any(|(s, _)| s == "set_cb"),
        "a routed callback needs no warning: {:?}",
        b.assumed
    );
}

// ── A pointer to a pointer ───────────────────────────────────────────────

#[test]
fn a_tbd_stub_is_told_apart_from_a_corrupt_file() {
    // The shape Apple's SDK actually ships. Worth telling apart because a user
    // who points at one has not made a mistake — on a modern macOS it is the
    // only handle the SDK offers for a system library.
    let stub = b"--- !tapi-tbd\ntbd-version:     4\ntargets:         [ arm64e-macos ]\n\
                 install-name:    '/usr/lib/libz.1.dylib'\ncurrent-version: 1.2.12\n";
    assert_eq!(
        super::tbd_install_name(stub).as_deref(),
        Some("/usr/lib/libz.1.dylib"),
        "should read the library it stands for"
    );
    // Shorter than the window it reads, which is the common case — a bare
    // `get(..4096)` answered None for every real stub.
    assert!(
        super::tbd_install_name(b"--- !tapi-tbd\ninstall-name: '/usr/lib/libc.dylib'\n").is_some()
    );
    // And it is not a loadable object, which is why the plain check refuses it.
    assert!(!super::bytes_are_loadable_object(stub));
    // Anything else is not a stub, however broken.
    assert!(super::tbd_install_name(b"\x7fELF\x02\x01\x01").is_none());
    assert!(super::tbd_install_name(b"--- !tapi-tbd\nno install name here\n").is_none());
}

#[test]
fn a_name_pointed_at_inside_the_callers_own_data_comes_back_as_a_string() {
    // `fdt_getprop_by_offset` points `namep` into the device tree it was handed,
    // so nothing was allocated and nothing has to be released.
    let b = bind(
        "const void* getprop_by_offset(const void* fdt, int offset, const char** namep, int* lenp);\n",
    );
    assert_eq!(args(&b, "getprop_by_offset"), ["bytes_ptr", "int", "out_str:char", "ret_len:int"]);
}

#[test]
fn a_writable_string_pointer_names_the_spelling_rather_than_guessing_who_frees_it() {
    // The same C shape with the opposite ownership, and the header does not say
    // which. Guessing one way leaks on every call and the other frees memory
    // that was never allocated.
    let b = bind("int str_from(char** out, int flags);\n");
    let why = why_skipped(&b, "str_from");
    assert!(why.contains("out_alloc_str"), "{why}");
    assert!(why.contains("frees_with"), "{why}");
}

// ── Types come from the whole translation unit ───────────────────────────
//
// Functions are bound from the named header alone; the types they are written
// in terms of are not. A library that splits its types into `git2/types.h` and
// declares functions against them in twenty other headers used to report every
// one of those functions as taking an unsupported type.

/// Write a main header plus its includes into one directory, then bind the
/// main one. `files` is `(name, contents)`, the first being the entry point.
fn bind_tree(files: &[(&str, &str)], exported: Option<&[&str]>) -> Result<Binding, String> {
    let dir = std::env::temp_dir().join(format!(
        "jade-bindgen-tree-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, src) in files {
        // Names may carry a subdirectory, so an angled include like
        // `<pkg/other.h>` can be written the way a real library writes it.
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, src).unwrap();
    }
    let set = exported
        .map(|e| e.iter().map(|s| s.to_string()).collect::<std::collections::HashSet<String>>());
    let out = from_header(&dir.join(files[0].0), &[], &[], None, set.as_ref());
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
fn a_type_defined_in_an_included_header_is_still_a_handle() {
    let b = bind_tree(
        &[
            ("main.h", "#include \"types.h\"\nint obj_value(Obj* o);\n"),
            ("types.h", "struct Obj_s;\ntypedef struct Obj_s Obj;\n"),
        ],
        None,
    )
    .expect("should bind");
    assert_eq!(args(&b, "obj_value"), ["handle<Obj>"]);
}

#[test]
fn a_struct_defined_in_an_included_header_still_gets_a_field_table() {
    let b = bind_tree(
        &[
            ("main.h", "#include \"types.h\"\nint info_get(struct Info* out);\n"),
            ("types.h", "struct Info { int rate; int channels; };\n"),
        ],
        None,
    )
    .expect("should bind");
    assert_eq!(args(&b, "info_get"), ["out_struct:struct Info"]);
    assert!(b.structs.contains_key("struct Info"), "{:?}", b.structs);
}

#[test]
fn only_the_named_headers_functions_are_bound() {
    // The included header's `helper` is in the same translation unit and must
    // not come along — that is what keeps the C standard library out.
    let b = bind_tree(
        &[
            ("main.h", "#include \"other.h\"\nint mine(int a);\n"),
            ("other.h", "int helper(int);\n"),
        ],
        None,
    )
    .expect("should bind");
    assert!(b.symbols.contains_key("mine"));
    assert!(!b.symbols.contains_key("helper"), "{:?}", b.symbols.keys());
}

// ── Umbrella headers ─────────────────────────────────────────────────────

#[test]
fn an_umbrella_header_binds_what_the_library_exports() {
    // `lzma.h` and `git2.h` declare nothing themselves. The export table is
    // what says which of the included declarations belong to this library.
    let b = bind_tree(
        &[
            ("umbrella.h", "#include <stdio.h>\n#include \"core.h\"\n"),
            ("core.h", "int ctx_get(int a);\nint ctx_set(int a);\n"),
        ],
        Some(&["ctx_get", "ctx_set"]),
    )
    .expect("should bind");
    assert_eq!(b.swept, Some(crate::pkg::bindgen::Swept { n: 2, umbrella: true }));
    assert!(b.symbols.contains_key("ctx_get"));
    assert!(b.symbols.contains_key("ctx_set"));
    // stdio.h is in the same translation unit and is not this library.
    assert!(!b.symbols.contains_key("fopen"), "{:?}", b.symbols.keys());
}

#[test]
fn an_umbrella_header_with_no_export_table_says_what_it_needs() {
    let err = bind_tree(
        &[("umbrella.h", "#include \"core.h\"\n"), ("core.h", "int ctx_get(int a);\n")],
        None,
    )
    .expect_err("cannot be read without the library");
    assert!(err.contains("umbrella"), "{err}");
    assert!(err.contains("--path"), "should name the fix: {err}");
}

#[test]
fn a_header_that_declares_its_own_also_binds_what_it_includes() {
    // `ares.h` declares seventy-odd symbols of its own *and* includes
    // `ares_dns_record.h`, which declares sixty-three more the library exports.
    // While the rule was all-or-nothing those sixty-three were invisible.
    let b = bind_tree(
        &[
            ("main.h", "#include \"other.h\"\nint mine(int a);\n"),
            ("other.h", "int helper(int);\n"),
        ],
        Some(&["mine", "helper"]),
    )
    .expect("should bind");
    assert_eq!(b.swept, Some(crate::pkg::bindgen::Swept { n: 1, umbrella: false }));
    assert!(b.symbols.contains_key("mine"));
    assert!(b.symbols.contains_key("helper"), "{:?}", b.symbols.keys());
}

#[test]
fn an_included_declaration_the_library_does_not_export_is_left_alone() {
    // The export table is the whole test. `stdio.h` rides in on every header
    // and belongs to nobody.
    let b = bind_tree(
        &[
            ("main.h", "#include <stdio.h>\n#include \"other.h\"\nint mine(int a);\n"),
            ("other.h", "int helper(int);\n"),
        ],
        Some(&["mine"]),
    )
    .expect("should bind");
    assert_eq!(b.swept, None);
    assert!(!b.symbols.contains_key("helper"), "{:?}", b.symbols.keys());
    assert!(!b.symbols.contains_key("fopen"), "{:?}", b.symbols.keys());
}

#[test]
fn without_an_export_table_only_the_headers_own_declarations_are_bound() {
    // Nothing exact to test an include against, so the named header is the
    // whole scope — which is what it was for every header before.
    let b = bind_tree(
        &[
            ("main.h", "#include \"other.h\"\nint mine(int a);\n"),
            ("other.h", "int helper(int);\n"),
        ],
        None,
    )
    .expect("should bind");
    assert_eq!(b.swept, None);
    assert!(!b.symbols.contains_key("helper"), "{:?}", b.symbols.keys());
}

// ── Enums ────────────────────────────────────────────────────────────────

#[test]
fn an_enum_return_is_an_int() {
    // Status-code enums are how most C libraries report failure. Leaving them
    // unbindable cost 60 of liblzma's 114 symbols.
    let b = bind("typedef enum { OK = 0, BAD = 1 } status;\nstatus go(int a);\n");
    assert_eq!(ret(&b, "go"), "int");
}

#[test]
fn an_enum_parameter_is_an_int() {
    let b = bind("typedef enum { A, B } mode;\nint go(mode m);\n");
    assert_eq!(args(&b, "go"), ["int"]);
}

#[test]
fn a_tagged_enum_used_without_a_typedef_is_an_int() {
    let b = bind("enum mode { A, B };\nint go(enum mode m);\n");
    assert_eq!(args(&b, "go"), ["int"]);
}

#[test]
fn an_enum_named_by_a_typedef_of_its_own_tag_is_an_int() {
    // `typedef enum lzma_ret lzma_ret;` — the tag and the typedef share a name,
    // so stripping the keyword aliases the name to itself.
    let b = bind("enum lzma_ret { OK };\ntypedef enum lzma_ret lzma_ret;\nint go(lzma_ret r);\n");
    assert_eq!(args(&b, "go"), ["int"]);
}

#[test]
fn an_enum_library_binds_and_compiles_end_to_end() {
    round_trip(
        "typedef enum { S_OK = 0, S_ERR = 1 } status;\n\
         typedef struct ctx ctx;\n\
         status ctx_open(const char* path, ctx** out);\n\
         status ctx_step(ctx* c, status hint);\n",
    )
    .expect("an enum-heavy library should bind and compile");
}

// ── One unbindable symbol must not take the dependency with it ───────────

#[test]
fn a_symbol_whose_struct_has_no_carryable_field_is_skipped_not_emitted() {
    // The sqlite3_snapshot / zip_file_attributes shape. Emitting the symbol
    // while dropping its field table left the manifest referring to a
    // `[structs]` entry that was never written, and the shim generator refuses
    // the *whole dependency* over one such symbol — so a single opaque blob
    // made an otherwise fine library uninstallable.
    // A function pointer, because a fixed-size byte array is no longer opaque —
    // `unsigned char hidden[48]` is what sqlite3_snapshot actually is, and it
    // carries now, which is the point of array fields. What stays uncarryable
    // is a field with no Jade shape at all.
    let b = bind(
        "typedef struct snap { void (*cb)(void); } snap;\n\
         void snap_free(snap* s);\n\
         int plain_add(int a, int b);\n",
    );
    assert!(b.symbols.contains_key("plain_add"));
    assert!(!b.symbols.contains_key("snap_free"), "{:?}", b.symbols.keys());
    assert!(why_skipped(&b, "snap_free").contains("no field the FFI can carry"));
    // Nothing left dangling: every out_struct spec names a table that exists.
    for spec in b.symbols.values().flat_map(|s| s.args.iter()) {
        if let Some(name) = spec.strip_prefix("out_struct:") {
            assert!(b.structs.contains_key(name), "{name} has no field table");
        }
    }
}

#[test]
fn a_library_with_one_unbindable_struct_still_compiles_its_shim() {
    round_trip(
        "typedef struct snap { unsigned char hidden[48]; } snap;\n\
         void snap_free(snap* s);\n\
         int plain_add(int a, int b);\n",
    )
    .expect("the rest of the library should still bind");
}

// ── Include roots ────────────────────────────────────────────────────────
//
// A header is rarely self-contained, and the two ways it reaches its
// neighbours need two different directories. Neither was passed to clang, and
// each one cost a real library outright: libfdt could not be parsed at all,
// and neither could brotli.

#[test]
fn include_roots_lists_the_header_directory_then_the_one_above_it() {
    let roots = include_roots(std::path::Path::new("/tmp/inc/pkg/main.h"), &[]);
    assert_eq!(roots, ["/tmp/inc/pkg", "/tmp/inc"], "{roots:?}");
}

#[test]
fn a_directory_the_caller_named_wins_over_one_guessed_from_the_path() {
    // A guessed root can be wide enough to shadow the header the caller meant,
    // so an explicit -I is searched first.
    let roots =
        include_roots(std::path::Path::new("/tmp/inc/pkg/main.h"), &["/tmp/mine".to_string()]);
    assert_eq!(roots[0], "/tmp/mine", "{roots:?}");
}

#[test]
fn include_roots_does_not_repeat_a_directory() {
    let roots =
        include_roots(std::path::Path::new("/tmp/inc/pkg/main.h"), &["/tmp/inc/pkg".to_string()]);
    assert_eq!(roots, ["/tmp/inc/pkg", "/tmp/inc"], "{roots:?}");
}

#[test]
fn a_header_including_a_sibling_with_angle_brackets_parses() {
    // The libfdt shape: `#include <libfdt_env.h>` beside the header. An angled
    // include does not search the including file's own directory, so without
    // that directory on the search path clang cannot parse the header at all.
    let b = bind_tree(
        &[
            ("main.h", "#include <side.h>\nint mine(Side* s);\n"),
            ("side.h", "typedef struct Side Side;\n"),
        ],
        None,
    )
    .expect("a sibling header should resolve");
    assert_eq!(args(&b, "mine"), ["handle<Side>"]);
}

#[test]
fn a_header_including_through_its_parent_directory_parses() {
    // The brotli shape: `brotli/encode.h` does `#include <brotli/port.h>`,
    // which resolves against the directory *above* the header.
    let b = bind_tree(
        &[
            ("pkg/main.h", "#include <pkg/side.h>\nint mine(Side* s);\n"),
            ("pkg/side.h", "typedef struct Side Side;\n"),
        ],
        None,
    )
    .expect("an include through the parent directory should resolve");
    assert_eq!(args(&b, "mine"), ["handle<Side>"]);
}

// ── The library decides what it really has ───────────────────────────────

#[test]
fn a_symbol_the_header_declares_and_the_library_does_not_export_is_skipped() {
    // A header is written for the newest version while the built artifact may
    // have been configured without some of it. Binding one of those produces a
    // shim that compiles and then fails to *link* — and the linker takes the
    // whole dependency down over it, which is what libbrotlienc did.
    let b =
        bind_tree(&[("main.h", "int shipped(int a);\nint absent(int a);\n")], Some(&["shipped"]))
            .expect("should bind");
    assert!(b.symbols.contains_key("shipped"));
    assert!(!b.symbols.contains_key("absent"), "{:?}", b.symbols.keys());
    assert!(why_skipped(&b, "absent").contains("not exported"), "{:?}", b.skipped);
}

#[test]
fn with_no_export_table_every_declared_symbol_is_still_bound() {
    // A URL dependency has no artifact to read, and an unreadable table proves
    // nothing. The filter only applies when the library could actually be asked.
    let b = bind_tree(&[("main.h", "int a(int x);\nint b(int x);\n")], None).expect("should bind");
    assert_eq!(b.symbols.len(), 2, "{:?}", b.symbols.keys());
}

// ── Caller-held state is not an out-parameter ────────────────────────────
//
// Three different things wear the shape "writable pointer to a struct the
// header defines", and treating them all as out-parameters is how twelve of
// liblzma's symbols came to bind into shims that ran and did nothing.

#[test]
fn a_struct_the_caller_keeps_between_calls_is_one_jade_holds() {
    // The lzma_stream shape: pointer fields the FFI cannot carry, threaded
    // through a sequence of calls. An out_struct shim zeroes a fresh local every
    // call, so the encoder would initialise a stream and throw it away and the
    // next call would run against a different zeroed one. Held on the C heap
    // instead, so every call gets the same pointer and the fields that cannot
    // travel stay where the library put them.
    let b = bind(
        "typedef struct { const unsigned char* next_in; unsigned long avail_in; \
           unsigned char* next_out; unsigned long avail_out; void* internal; } strm;\n\
         int strm_start(strm* s, int preset);\n\
         int strm_code(strm* s, int action);\n\
         void strm_end(strm* s);\n",
    );
    for sym in ["strm_start", "strm_code", "strm_end"] {
        assert_eq!(args(&b, sym)[0], "handle<strm>", "{sym}");
    }
    let def = &b.structs["strm"];
    assert!(def.held, "the table must say so, or no `strm_new` is written");
}

#[test]
fn a_held_structs_buffer_fields_are_found_by_the_same_rule_a_parameter_list_uses() {
    // C encodes the idiom the same way in a struct definition: the pointer, then
    // the count. These are the fields that make a held struct necessary in the
    // first place, so a held struct without them is a handle you can make and
    // never feed.
    let b = bind(
        "typedef struct { const unsigned char* next_in; unsigned long avail_in; \
           unsigned char* next_out; unsigned long avail_out; void* internal; } strm;\n\
         int strm_start(strm* s, int preset);\n\
         int strm_code(strm* s, int action);\n",
    );
    let bufs = &b.structs["strm"].buffers;
    assert_eq!(bufs.len(), 2, "{bufs:?}");
    assert_eq!(
        (bufs[0].ptr.as_str(), bufs[0].len.as_str(), bufs[0].writable),
        ("next_in", "avail_in", false)
    );
    assert_eq!(
        (bufs[1].ptr.as_str(), bufs[1].len.as_str(), bufs[1].writable),
        ("next_out", "avail_out", true)
    );
}

#[test]
fn a_reserved_field_is_never_read_as_a_buffer() {
    // `lzma_stream` ends in four `void *reserved_ptr` and several
    // `reserved_int`, and two of them sit next to each other in exactly the
    // pointer-then-count order. A setter for one would offer a way to write
    // where the library requires a zero.
    let b = bind(
        "typedef struct { const unsigned char* next_in; unsigned long avail_in; \
           void* internal; void* reserved_ptr4; unsigned long seek_pos; } strm;\n\
         int strm_start(strm* s, int preset);\n\
         int strm_code(strm* s, int action);\n",
    );
    let ptrs: Vec<&str> = b.structs["strm"].buffers.iter().map(|x| x.ptr.as_str()).collect();
    assert_eq!(ptrs, ["next_in"], "{:?}", b.structs["strm"].buffers);
}

#[test]
fn a_struct_the_library_hands_out_is_a_handle_not_an_out_parameter() {
    // The library allocates it, so Jade holds it. This is the same answer the
    // return position already gives the same type.
    let b = bind(
        "typedef struct { int a; void* guts; } ctx;\n\
         ctx* ctx_new(void);\n\
         int ctx_get(ctx* c);\n\
         int ctx_set(ctx* c, int v);\n",
    );
    assert_eq!(ret(&b, "ctx_new"), "handle<ctx>");
    assert_eq!(args(&b, "ctx_get"), ["handle<ctx>"]);
    assert_eq!(args(&b, "ctx_set"), ["handle<ctx>", "int"]);
}

#[test]
fn a_struct_written_through_a_double_pointer_is_also_handed_out() {
    let b = bind(
        "typedef struct { int a; void* guts; } ctx;\n\
         int ctx_open(const char* path, ctx** out);\n\
         int ctx_get(ctx* c);\n",
    );
    assert_eq!(args(&b, "ctx_get"), ["handle<ctx>"]);
}

#[test]
fn a_record_filled_by_several_functions_is_still_an_out_parameter() {
    // libsndfile's SF_INFO is passed to three `sf_open` variants and is exactly
    // what out-parameters exist for. Appearing in several functions is not on
    // its own a reason to refuse — only losing a field as well is.
    let b = bind(
        "#include <stdint.h>\n\
         typedef struct { int64_t frames; int rate; const char* title; } SF_INFO;\n\
         int sf_open(const char* p, int mode, SF_INFO* info);\n\
         int sf_open_fd(int fd, int mode, SF_INFO* info);\n\
         int sf_open_virtual(int v, int mode, SF_INFO* info);\n",
    );
    assert_eq!(args(&b, "sf_open"), ["str", "int", "out_struct:SF_INFO"]);
    assert_eq!(args(&b, "sf_open_fd"), ["int", "int", "out_struct:SF_INFO"]);
    assert!(b.structs.contains_key("SF_INFO"));
}

#[test]
fn a_record_with_one_uncarryable_field_is_still_an_out_parameter() {
    // Losing a field is not on its own a reason to refuse either. A record
    // filled by one call is read once and discarded, so the dropped field costs
    // nothing — which is the behaviour the field-dropping rule already had.
    let b = bind("typedef struct { int ok; void* opaque; int also_ok; } S;\nvoid f(S* s);\n");
    assert_eq!(args(&b, "f"), ["out_struct:S"]);
    let names: Vec<&str> = b.structs["S"].fields.iter().map(|(f, _)| f.as_str()).collect();
    assert_eq!(names, ["ok", "also_ok"]);
}

// ── A blob with no length beside it ──────────────────────────────────────

#[test]
fn a_read_only_byte_pointer_alone_is_a_borrowed_blob() {
    // Every libfdt call takes `const void *fdt` alone and reads the length out
    // of the blob's own header. There is nowhere to pass a size, so refusing the
    // shape refused most of the library.
    let b = bind(
        "#include <stddef.h>\n\
         int fdt_check_header(const void* fdt);\n\
         int fdt_path_offset(const void* fdt, const char* path);\n",
    );
    assert_eq!(args(&b, "fdt_check_header"), ["bytes_ptr"]);
    assert_eq!(args(&b, "fdt_path_offset"), ["bytes_ptr", "str"]);
}

#[test]
fn a_borrowed_blob_is_reported_as_an_assumption() {
    // Jade cannot check the extent — the library takes it from the data — so a
    // truncated blob reads past the end. That belongs in the report.
    let b = bind("int f(const void* blob);\n");
    let why = b.assumed.iter().find(|(s, _)| s == "f").map(|(_, w)| w.clone()).unwrap_or_default();
    assert!(why.contains("reads past the end"), "should say what it cannot check: {why:?}");
}

#[test]
fn a_blob_next_to_a_length_still_takes_the_length_with_it() {
    // The pair is better than the pointer alone whenever there is one, because
    // the library is then told the extent rather than trusting the data.
    let b = bind("#include <stddef.h>\nint f(const void* p, size_t n);\n");
    assert_eq!(args(&b, "f"), ["bytes"]);
}

#[test]
fn a_writable_blob_is_revised_in_place_and_handed_back() {
    // Every libfdt writer takes `void *fdt` and edits the device tree where it
    // sits. A Jade blob is immutable, so the shim works on a copy and the edit
    // comes back as a return rather than as a mutation nothing declared.
    let b = bind("int fdt_nop_property(void* fdt, int nodeoffset, const char* name);\n");
    assert_eq!(args(&b, "fdt_nop_property"), ["inout_bytes", "int", "str"]);
}

#[test]
fn a_lone_void_pointer_is_refused_because_it_may_free_what_it_is_given() {
    // `ares_free_string(void *str)` releases what it is handed. Passing it the
    // shim's own scratch would have the library free it and the shim free it
    // again on the way out.
    let b = bind("void ares_free_string(void* str);\n");
    assert!(
        why_skipped(&b, "ares_free_string").contains("frees what it is given"),
        "{:?}",
        b.skipped
    );
}

#[test]
fn two_revised_blobs_take_their_keys_from_the_header() {
    // `fdt_overlay_apply(void *fdt, void *fdto)` has two results. Leaving them
    // unnamed let the symbol reach the shim generator, which refuses the whole
    // dependency rather than the one symbol.
    let b = bind("int fdt_overlay_apply(void* fdt, void* fdto);\n");
    assert_eq!(args(&b, "fdt_overlay_apply"), ["inout_bytes@fdt", "inout_bytes@fdto"]);
}

// ── A returned pointer, sized by a parameter ─────────────────────────────

#[test]
fn a_returned_pointer_sized_by_a_named_length_becomes_a_blob() {
    // `fdt_getprop` is the main read call in libfdt and has no other spelling:
    // the bytes are the return value and the count comes back through `lenp`.
    let b = bind(
        "const void* fdt_getprop(const void* fdt, int nodeoffset, const char* name, int* lenp);\n",
    );
    assert_eq!(ret(&b, "fdt_getprop"), "bytes");
    assert_eq!(args(&b, "fdt_getprop"), ["bytes_ptr", "int", "str", "ret_len:int"]);
}

#[test]
fn a_returned_pointer_with_no_named_length_stays_refused() {
    // Nothing in the types tells `int *lenp` from the second value a call
    // happens to write back, so without the name there is nothing to size from.
    let b = bind("const void* f(const void* blob, int* nextoffset);\n");
    assert!(why_skipped(&b, "f").contains("unsupported type"), "{:?}", b.skipped);
}

// ── Which integer is a length ────────────────────────────────────────────

#[test]
fn an_offset_after_a_blob_is_not_read_as_its_length() {
    // `nodeoffset` is the single most common name to follow a byte pointer in
    // these headers. Reading it as a length *drops* it and hands the library a
    // size it never computed.
    let b = bind("int fdt_path_offset_at(const void* fdt, int nodeoffset, const char* p);\n");
    assert_eq!(args(&b, "fdt_path_offset_at"), ["bytes_ptr", "int", "str"]);
}

#[test]
fn the_names_a_real_length_goes_by_are_all_recognised() {
    // Taken from every such parameter across the survey headers rather than
    // invented: srcSize, dstCapacity, namelen, buflen, in_size.
    for name in ["n", "len", "size", "srcSize", "dstCapacity", "buflen", "in_size", "nbytes"] {
        let b = bind(&format!("#include <stddef.h>\nint f(const void* p, size_t {name});\n"));
        assert_eq!(args(&b, "f"), ["bytes"], "{name} should read as a length");
    }
}

#[test]
fn a_name_that_counts_nothing_leaves_the_integer_as_an_argument() {
    // The safe direction: the int is still passed, the caller just supplies it.
    for name in ["nodeoffset", "offset", "val", "family", "index", "phandle", "stroffset"] {
        let b = bind(&format!("#include <stddef.h>\nint f(const void* p, size_t {name});\n"));
        assert_eq!(args(&b, "f"), ["bytes_ptr", "int"], "{name} should not read as a length");
    }
}

#[test]
fn a_pointer_named_like_a_position_is_not_a_buffer_of_the_count_beside_it() {
    // `lzma_stream_buffer_decode(…, size_t *in_pos, size_t in_size, …)` has
    // exactly the shape of a buffer and its count, and is a position beside an
    // unrelated size. Reading it as a buffer allocated `in_size` of them and
    // handed the library a pointer to scratch instead of to the position.
    let b = bind(
        "#include <stddef.h>\n#include <stdint.h>\n\
         int decode(const uint8_t* in, size_t* in_pos, size_t in_size);\n",
    );
    assert_eq!(args(&b, "decode"), ["bytes_ptr", "out_scalar:unsigned long", "int"]);
}

#[test]
fn a_pointer_named_like_a_buffer_still_is_one() {
    // The shape that has to survive the rule above: `sf_read_short(short *buf,
    // int n)` really is a buffer and its count.
    let b = bind("int read_short(short* buf, int n);\n");
    assert_eq!(args(&b, "read_short"), ["out_buffer:short", "int"]);
}

// ── A struct read as input ───────────────────────────────────────────────

#[test]
fn a_const_struct_pointer_is_an_input_the_caller_builds() {
    // The library reads it and forgets it, so nothing owns anything across the
    // boundary. Jade builds one and the shim copies it into a real C local.
    let b = bind(
        "#include <stdint.h>\n\
         typedef struct { int version; int64_t size; const char* tag; } FLAGS;\n\
         int cmp(const FLAGS* a, const FLAGS* b);\n",
    );
    assert_eq!(args(&b, "cmp"), ["in_struct:FLAGS", "in_struct:FLAGS"]);
    assert!(b.structs.contains_key("FLAGS"), "the field table must come out too");
}

#[test]
fn an_input_struct_that_would_lose_a_field_is_held_instead() {
    // The asymmetry with `out_struct`, which tolerates a dropped field: losing
    // an output is visible in what comes back, losing an input is not. So the
    // caller does not build this one at all — they hold it, and the fields that
    // cannot travel are filled by whichever library calls know how.
    let b = bind("typedef struct { int ok; void* options; } F;\nint use_it(const F* f);\n");
    assert_eq!(args(&b, "use_it"), ["handle<F>"]);
    assert!(b.structs["F"].held);
}

#[test]
fn a_struct_pointer_beside_an_unrelated_int_is_one_struct_not_an_array() {
    // `cs_op_count(csh, const cs_insn *insn, unsigned op_type)` reads as "a
    // const pointer followed by an int", which is also the shape of an array
    // and its count. The parameter's own name breaks the tie.
    let b = bind(
        "typedef struct { int a; int b; } INSN;\n\
         int op_count(const INSN* insn, unsigned int op_type);\n",
    );
    assert_eq!(args(&b, "op_count"), ["in_struct:INSN", "int"]);
}

#[test]
fn a_struct_pointer_beside_a_count_is_still_refused_as_an_array() {
    // `ares_process_fds(ch, const ares_fd_events_t *events, size_t nevents)`
    // really is an array. Guessing the other way would hand the library one
    // struct and tell it there were twenty, so a count-like name keeps the
    // refusal.
    let b = bind(
        "#include <stddef.h>\n\
         typedef struct { int fd; int flags; } EV;\n\
         int process(const EV* events, size_t nevents);\n",
    );
    assert!(!b.symbols.contains_key("process"), "{:?}", b.symbols.get("process"));
    assert!(why_skipped(&b, "process").contains("elements rather than bytes"), "{:?}", b.skipped);
}

#[test]
fn a_writable_byte_pointer_with_no_length_is_a_buffer_the_caller_sizes() {
    // `lzma_stream_footer_encode(const lzma_stream_flags*, uint8_t *out)` writes
    // exactly twelve bytes and says so nowhere a generator can read. Reading
    // `out` as one value written back declared a one-byte local and passed its
    // address — a stack overflow the C compiler cannot see.
    let b = bind(
        "#include <stdint.h>\n\
         typedef struct { int version; } F;\n\
         int footer_encode(const F* f, uint8_t* out);\n",
    );
    assert_eq!(args(&b, "footer_encode"), ["in_struct:F", "sized_buffer:unsigned char"]);
    let why = b.assumed.iter().find(|(s, _)| s == "footer_encode").map(|(_, w)| w.clone());
    assert!(why.is_some_and(|w| w.contains("yours to say")), "should say whose job it is");
}

#[test]
fn a_writable_int_pointer_with_no_length_is_still_an_out_scalar() {
    // The shape the byte rule must not catch: `fdt_next_tag(fdt, off, int
    // *nextoffset)` really is one value written back.
    let b = bind("int next_tag(int off, int* nextoffset);\n");
    assert_eq!(args(&b, "next_tag"), ["int", "out_scalar:int"]);
}

#[test]
fn caller_held_state_needs_both_signals() {
    // The same struct as the state test, but used by one function only: a
    // record, and it binds. Neither signal refuses on its own.
    let b = bind(
        "typedef struct { const unsigned char* next_in; unsigned long avail_in; } strm;\n\
         int strm_once(strm* s);\n",
    );
    assert_eq!(args(&b, "strm_once"), ["out_struct:strm"]);
}

#[test]
fn a_caller_held_state_library_still_binds_and_compiles_the_rest() {
    round_trip(
        "typedef struct { const unsigned char* next_in; void* internal; } strm;\n\
         int strm_code(strm* s, int action);\n\
         void strm_end(strm* s);\n\
         const char* strm_version(void);\n\
         int strm_preset(int level);\n",
    )
    .expect("the symbols that do not touch the state struct should still bind");
}

// ── Scalars written through a pointer ────────────────────────────────────

#[test]
fn a_scalar_written_through_a_pointer_is_an_out_scalar() {
    let b = bind("int measure(const char* path, unsigned long* size);\n");
    assert_eq!(args(&b, "measure"), ["str", "out_scalar:unsigned long"]);
}

#[test]
fn an_out_scalar_is_an_assumption_and_names_the_fix() {
    // Some of these are read *and* written — a position the caller sets and
    // the library advances. A zeroed local is right for one call and wrong on
    // the second, which shows up as corrupt output rather than an error.
    let b = bind("int measure(const char* path, unsigned long* size);\n");
    let why = &b.assumed.iter().find(|(s, _)| s == "measure").expect("should be assumed").1;
    assert!(why.contains("inout_scalar"), "the note must name the fix: {why}");
}

#[test]
fn a_const_pointer_to_a_scalar_is_not_an_out_scalar() {
    // The shim would have to construct the value, and `const` says the library
    // only reads it — so this stays a refusal rather than becoming a silent
    // zero.
    let b = bind("int f(const int* in);\n");
    assert!(!b.symbols.contains_key("f"), "{:?}", b.symbols.keys());
}

#[test]
fn a_buffer_still_wins_over_an_out_scalar() {
    // A writable pointer *next to a length* is a buffer, and that rule has to
    // be tested first or every out_buffer would become an out_scalar.
    let b = bind("int rd(int fd, char* buf, int n);\n");
    assert_eq!(args(&b, "rd"), ["int", "out_buffer:char", "int"]);
}

// ── More than one out-parameter ──────────────────────────────────────────

#[test]
fn two_out_parameters_take_the_headers_own_names() {
    // Inventing `out0`/`out1` is the objection that kept multiple outs out of
    // the design in the first place. The library already named them.
    let b = bind(
        "void get_progress(unsigned long long *progress_in, unsigned long long *progress_out);\n",
    );
    assert_eq!(
        args(&b, "get_progress"),
        ["out_scalar:unsigned long long@progress_in", "out_scalar:unsigned long long@progress_out"]
    );
}

#[test]
fn one_out_parameter_is_left_unnamed() {
    // There is nothing to tell apart, and every binding that already exists has
    // to regenerate unchanged.
    let b = bind("int one(int a, int* only);\n");
    assert_eq!(args(&b, "one"), ["int", "out_scalar:int"]);
}

#[test]
fn a_multi_out_symbol_whose_parameters_are_unnamed_is_skipped() {
    let b = bind("void f(int*, int*);\n");
    assert!(!b.symbols.contains_key("f"));
    assert!(why_skipped(&b, "f").contains("does not name them"), "{:?}", b.skipped);
}

#[test]
fn two_out_parameters_that_both_read_the_return_value_are_refused() {
    // An out_buffer takes the C return as its element count, and there is only
    // one of it. The shim refuses this too; mirroring it here matters because
    // the shim refuses the whole dependency rather than the symbol.
    let b = bind("int two(char* a, int na, char* b, int nb);\n");
    assert!(!b.symbols.contains_key("two"));
    assert!(why_skipped(&b, "two").contains("both read the C return value"), "{:?}", b.skipped);
}

#[test]
fn a_multi_out_library_binds_and_compiles_end_to_end() {
    round_trip(
        "void get_progress(unsigned long long *progress_in, unsigned long long *progress_out);\n\
         int divmod(int a, int b, int *quot, int *rem);\n\
         int one_out(int a, int *only);\n",
    )
    .expect("multi-out should bind and compile");
}

// ── Fixed-size array fields ──────────────────────────────────────────────

#[test]
fn a_fixed_size_array_field_is_carried_as_a_row() {
    // An array of things Jade has maps to an array of them, and the element
    // type decides what they are. Plain `char` is text and `unsigned char` is
    // data, the same rule a pointer parameter follows.
    let b = bind(
        "#include <stdint.h>\n\
         typedef struct { unsigned int id; char mnemonic[32]; uint8_t bytes[24]; int r[4]; } insn;\n\
         void fill(insn* i);\n",
    );
    let fields: Vec<(&str, &str)> =
        b.structs["insn"].fields.iter().map(|(f, t)| (f.as_str(), t.as_str())).collect();
    assert_eq!(
        fields,
        [
            ("id", "int"),
            ("mnemonic", "array<char>:32"),
            ("bytes", "array<int>:24"),
            ("r", "array<int>:4"),
        ]
    );
}

#[test]
fn a_struct_that_is_only_rows_is_still_held() {
    // `fd_set` is one `int fds_bits[32]`, filled by `ares_fds` and read by
    // `ares_process`. Rows made it carryable, which nearly turned it into an
    // out-parameter — and an out-parameter is a zeroed local every call, so
    // `ares_process` would have received an empty set and quietly done nothing.
    let b = bind(
        "typedef struct { int fds_bits[32]; } fdset;\n\
         int sel_fds(int n, fdset* r, fdset* w);\n\
         void sel_process(int n, fdset* r, fdset* w);\n",
    );
    assert_eq!(args(&b, "sel_process")[1], "handle<fdset>");
    assert!(b.structs["fdset"].held, "a bag of rows has to survive between calls");
}

#[test]
fn a_record_of_scalars_filled_by_several_calls_is_still_an_out_parameter() {
    // The shape the rule above must not catch. libsndfile's SF_INFO is filled
    // by three `sf_open` variants and read once each time; it is never handed
    // back, so there is no state to carry.
    let b = bind(
        "#include <stdint.h>\n\
         typedef struct { int64_t frames; int rate; } SF_INFO;\n\
         int sf_open(const char* p, int mode, SF_INFO* i);\n\
         int sf_open_fd(int fd, int mode, SF_INFO* i);\n",
    );
    assert_eq!(args(&b, "sf_open")[2], "out_struct:SF_INFO");
    assert!(!b.structs["SF_INFO"].held);
}

// ── A count is not a status ──────────────────────────────────────────────

#[test]
fn a_count_returned_beside_a_handle_is_not_read_as_a_status() {
    // `size_t cs_disasm(…, cs_insn **insn)` returns how many instructions were
    // written. Read as a status, a successful disassembly of three raised.
    let b = bind(
        "#include <stddef.h>\n\
         typedef struct insn insn;\n\
         size_t disasm(const void* code, size_t n, insn** out);\n",
    );
    assert!(b.symbols["disasm"].fails_when.is_none(), "a count must not raise on success");
    let why = b.assumed.iter().find(|(s, _)| s == "disasm").map(|(_, w)| w.clone());
    assert!(why.is_some_and(|w| w.contains("fails_when")), "should name the spelling");
}

#[test]
fn a_status_returned_beside_a_handle_still_raises() {
    // The shape that must survive it: `sqlite3_open(path, &db) -> int`.
    let b = bind(
        "typedef struct sqlite3 sqlite3;\n\
         int sqlite3_open(const char* path, sqlite3** db);\n",
    );
    assert_eq!(b.symbols["sqlite3_open"].fails_when, Some(crate::project::CFailure::Nonzero));
}

#[test]
fn a_callback_parameter_keeps_its_typedef_and_says_how_to_marshal_it() {
    // The trampoline is declared with the spelling the library used, because a
    // typedef expanded to its underlying type makes a function pointer C
    // considers incompatible — and the shim then fails to compile, taking the
    // whole dependency with it. The category is what Jade marshals as.
    let b = bind(
        "typedef int sock_t;\n\
         typedef enum { NO = 0, YES = 1 } flag_t;\n\
         void set_cb(int (*cb)(sock_t fd, flag_t ok, void* data));\n",
    );
    assert_eq!(args(&b, "set_cb"), ["callback:int(int:sock_t, int:flag_t, void *)"]);
}

#[test]
fn a_callback_that_delivers_a_blob_carries_it_as_one() {
    // `ares_callback` is `(void *arg, int status, int timeouts, unsigned char
    // *abuf, int alen)` — every DNS answer arrives that way, so without this
    // c-ares can register a query and never see its result.
    let b = bind("void go(void (*cb)(void* arg, int status, unsigned char* abuf, int alen));\n");
    assert_eq!(args(&b, "go"), ["callback:void(void *, int, bytes:unsigned char *, int)"]);
}

#[test]
fn the_user_data_slot_is_not_mistaken_for_a_blob() {
    // A `void *` is byte-like and the parameter after it is often an int, so
    // the pair reads as a buffer and its length. `ares_callback` opens with
    // exactly that shape, and reading it as a blob loses the real one.
    let b = bind("void go(void (*cb)(void* arg, int status));\n");
    assert_eq!(args(&b, "go"), ["callback:void(void *, int)"]);
}

#[test]
fn a_callback_with_no_context_slot_says_registrations_will_collide() {
    // Only warned about where it is actually true. A library that keys its
    // callbacks some other way — by an int id, a channel, a file descriptor —
    // offers nothing to route through, so the one-slot-per-symbol behaviour
    // stands and `reg(0, double)` then `reg(1, triple)` sends both answers to
    // `triple`. There is no spelling that fixes it, so the warning says so
    // rather than naming a remedy that does not apply.
    let b = bind("void go(int (*cb)(int));\n");
    let why = b.assumed.iter().find(|(s, _)| s == "go").map(|(_, w)| w.clone());
    assert!(
        why.is_some_and(|w| w.contains("no context parameter")),
        "should say why they collide: {:?}",
        b.assumed
    );
}

// ── Reading the export table ─────────────────────────────────────────────
//
// Everything downstream compares a header's declaration against a name from
// this table, so a name read wrongly is a symbol reported as missing from a
// library that has it — or, worse, a symbol matched that the library does not
// have, which binds clean and fails at load. Both directions are pinned here.

/// A scratch directory of this test's own.
fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "jade-symtab-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Compile a real shared library, so the whole path — `nm`, the format, the
/// names — is exercised rather than only the string rules.
fn compile_lib(dir: &std::path::Path, name: &str, source: &str) -> Option<std::path::PathBuf> {
    let src = dir.join(format!("{name}.c"));
    std::fs::write(&src, source).unwrap();
    let ext = if cfg!(target_os = "macos") { "dylib" } else { "so" };
    let out = dir.join(format!("lib{name}.{ext}"));

    let mut cc = std::process::Command::new("cc");
    if cfg!(target_os = "macos") {
        cc.arg("-dynamiclib");
    } else {
        cc.arg("-shared");
    }
    let done = cc.arg("-fPIC").arg(&src).arg("-o").arg(&out).output().ok()?;
    done.status.success().then_some(out)
}

#[test]
fn a_versioned_symbol_is_the_plain_name_the_header_declares() {
    // liblzma exports `lzma_version_number@@XZ_5.0` and declares
    // `lzma_version_number`. Compared raw, the library "does not export" a
    // symbol it plainly does, and 56 of the bench image's libraries bind
    // nothing at all for this one reason.
    assert_eq!(plain_name("lzma_version_number@@XZ_5.0", ObjectFormat::Elf), "lzma_version_number");
}

#[test]
fn a_non_default_version_is_cut_at_its_single_at_sign() {
    // Both forms sit in the same dynsym: `@@` is the default version, `@` an
    // older one kept for compatibility.
    assert_eq!(plain_name("memcpy@GLIBC_2.2.5", ObjectFormat::Elf), "memcpy");
}

#[test]
fn a_symbol_and_its_older_version_are_one_name() {
    // The two spellings of the same function must collapse, or the coverage
    // count says a library has more entry points than it has.
    let names: std::collections::HashSet<&str> =
        ["memcpy@@GLIBC_2.14", "memcpy@GLIBC_2.2.5", "memcpy"]
            .iter()
            .map(|n| plain_name(n, ObjectFormat::Elf))
            .collect();
    assert_eq!(names.len(), 1, "one function, one name: {names:?}");
}

#[test]
fn a_partly_versioned_library_keeps_all_of_its_symbols() {
    // zlib is the dangerous case rather than the loud one: it bound, printed
    // `added zlib`, and dropped exactly the 40 versioned symbols, with nothing
    // in the exit status or the manifest saying half the API was missing.
    let table = ["adler32", "deflateBound@@ZLIB_1.2.0", "crc32_z@@ZLIB_1.2.9", "inflate"];
    let names: std::collections::HashSet<&str> =
        table.iter().map(|n| plain_name(n, ObjectFormat::Elf)).collect();
    for declared in ["adler32", "deflateBound", "crc32_z", "inflate"] {
        assert!(names.contains(declared), "{declared} is declared and exported: {names:?}");
    }
}

#[test]
fn an_elf_name_keeps_every_leading_underscore() {
    // GMP's whole public API is `__gmpz_*`; the familiar `mpz_init` names are
    // `#define`s onto them. Stripping here skipped all 371 declarations as
    // "not exported by the library".
    for name in ["_one", "__two", "___three", "__gmpz_init"] {
        assert_eq!(plain_name(name, ObjectFormat::Elf), name);
    }
}

#[test]
fn an_elf_library_exporting_an_underscored_name_does_not_answer_to_the_bare_one() {
    // The reverse direction, and the worse one: stripping on the export side
    // made a header declaring `alpha` match a library exporting `_alpha`.
    // Nothing was skipped, nothing was assumed, and the program died at load
    // with `undefined symbol: alpha`.
    assert_ne!(plain_name("_alpha", ObjectFormat::Elf), "alpha");
}

#[test]
fn a_mach_o_name_loses_exactly_one_underscore() {
    // The prefix Mach-O adds, and nothing beyond it: a C function really named
    // `_one` is `__one` in the table and must read back as `_one`.
    assert_eq!(plain_name("_foo", ObjectFormat::MachO), "foo");
    assert_eq!(plain_name("__one", ObjectFormat::MachO), "_one");
    assert_eq!(plain_name("_mid_dle", ObjectFormat::MachO), "mid_dle");
}

#[test]
fn the_format_is_read_from_the_file_rather_than_the_host() {
    // A Mac handed a `.so` is ordinary — a checked-in Linux artifact, a
    // container's /usr/lib mounted to look at — and the host's rule reads every
    // one of its names wrongly. So the artifact is asked, and this test says
    // the same thing whichever platform it runs on.
    let dir = scratch("magic");
    let elf = dir.join("elf.so");
    std::fs::write(&elf, b"\x7fELF\x02\x01\x01\x00").unwrap();
    let macho = dir.join("macho.dylib");
    std::fs::write(&macho, b"\xcf\xfa\xed\xfe\x0c\x00\x00\x01").unwrap();

    assert_eq!(object_format(&elf), Some(ObjectFormat::Elf));
    assert_eq!(symbol_format(&elf), ObjectFormat::Elf);
    assert_eq!(object_format(&macho), Some(ObjectFormat::MachO));
    assert_eq!(symbol_format(&macho), ObjectFormat::MachO);

    // Not an object file at all, which the loadable-object check still reads
    // from the same four bytes.
    let text = dir.join("notes.txt");
    std::fs::write(&text, b"just a file").unwrap();
    assert_eq!(object_format(&text), None);
    assert!(!is_loadable_object(&text));
    assert!(is_loadable_object(&elf) && is_loadable_object(&macho));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_placeholder_list_keeps_underscored_api_and_drops_what_cannot_be_called() {
    // `__gmpz_init` is public API; `_Z3fooi` is a C++ function that no C
    // prototype can reach; `_init` belongs to the loader. A leading underscore
    // decides none of that, which is why it is no longer the test.
    assert!(is_callable_name("__gmpz_init"));
    assert!(is_callable_name("_one"));
    assert!(is_callable_name("_Zebra"), "a C name that happens to start _Z");
    assert!(!is_callable_name("_Z3fooi"));
    assert!(!is_callable_name("_ZN3foo3barEv"));
    assert!(!is_callable_name("_init"));
}

#[test]
fn a_real_library_reports_its_underscored_symbols_as_exported() {
    // The repro from the field, built here: five functions, only the leading
    // position varying. Two of the five used to bind.
    let dir = scratch("under");
    let Some(lib) = compile_lib(
        &dir,
        "us",
        "int plain(int x){return x+1;}\n\
         int _one(int x){return x+2;}\n\
         int __two(int x){return x+3;}\n\
         int ___three(int x){return x+4;}\n\
         int mid_dle(int x){return x+5;}\n",
    ) else {
        eprintln!("skipping: no C compiler");
        return;
    };

    let exported = exported_symbols(&lib).expect("a freshly built library has a symbol table");
    for name in ["plain", "_one", "__two", "___three", "mid_dle"] {
        assert!(exported.contains(name), "{name} is exported: {exported:?}");
    }

    // And the same names are offered for a prototype when no header is found,
    // which is the half a user without a header sees.
    let placeholders = placeholder_symbols(&lib);
    for name in ["plain", "_one", "__two", "___three", "mid_dle"] {
        assert!(placeholders.contains_key(name), "{name} should be listed: {placeholders:?}");
    }

    let _ = std::fs::remove_dir_all(&dir);
}
