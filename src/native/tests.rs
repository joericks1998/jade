use super::*;
use jade_runtime::coll::DictObj;

const ZERO: Span = Span { line: 0, col: 0 };

// NOTE: no dlopen of real shared libraries here — `load_native_package` /
// `NativeLibFn::call` require a native `.so`/`.dylib` with a `jade_pkg_init`
// symbol. We test the PURE C-ABI value conversions in both directions, which is
// where the interesting logic (and the unsafe union access) lives.

// ── JadeVal::nil ──────────────────────────────────────────────────────────

#[test]
fn nil_has_nil_tag() {
    let v = JadeVal::nil();
    assert_eq!(v.tag, JADE_TAG_NIL);
    assert_eq!(unsafe { v.data.as_nil }, 0);
}

// ── vm_to_ffi ─────────────────────────────────────────────────────────────

#[test]
fn vm_to_ffi_nil() {
    let mut scratch = Vec::new();
    let v = vm_to_ffi(&VmValue::Nil, &mut scratch);
    assert_eq!(v.tag, JADE_TAG_NIL);
}

#[test]
fn vm_to_ffi_int() {
    let mut scratch = Vec::new();
    let v = vm_to_ffi(&VmValue::Int(-42), &mut scratch);
    assert_eq!(v.tag, JADE_TAG_INT);
    assert_eq!(unsafe { v.data.as_int }, -42);
}

#[test]
fn vm_to_ffi_float() {
    let mut scratch = Vec::new();
    let v = vm_to_ffi(&VmValue::Float(3.5), &mut scratch);
    assert_eq!(v.tag, JADE_TAG_FLOAT);
    assert_eq!(unsafe { v.data.as_float }, 3.5);
}

#[test]
fn vm_to_ffi_bool() {
    let mut scratch = Vec::new();
    let t = vm_to_ffi(&VmValue::Bool(true), &mut scratch);
    let f = vm_to_ffi(&VmValue::Bool(false), &mut scratch);
    assert_eq!(t.tag, JADE_TAG_BOOL);
    assert_eq!(unsafe { t.data.as_bool }, 1);
    assert_eq!(unsafe { f.data.as_bool }, 0);
}

#[test]
fn vm_to_ffi_str_pushes_cstring_and_points_at_it() {
    let mut scratch = Vec::new();
    let v = vm_to_ffi(&VmValue::Str("hi".to_string().into()), &mut scratch);
    assert_eq!(v.tag, JADE_TAG_STR);
    assert_eq!(scratch.len(), 1, "CString must be kept alive in scratch");
    // Pointer must resolve back to the original bytes.
    let read = unsafe {
        CStr::from_ptr(v.data.as_str as *const std::ffi::c_char).to_string_lossy().into_owned()
    };
    assert_eq!(read, "hi");
}

#[test]
fn vm_to_ffi_unsupported_kind_becomes_nil() {
    let mut scratch = Vec::new();
    // A prompt has no ABI representation — native fns can't consume it. (Dicts
    // and arrays, which used to fall here too, now marshal — see the round-trips.)
    let v = vm_to_ffi(&VmValue::Prompt("p".to_string()), &mut scratch);
    assert_eq!(v.tag, JADE_TAG_NIL);
    assert!(scratch.is_empty());
}

#[test]
fn vm_to_ffi_dict_tag() {
    let mut scratch = Vec::new();
    let v = vm_to_ffi(&VmValue::dict(DictObj::new()), &mut scratch);
    assert_eq!(v.tag, JADE_TAG_DICT);
    unsafe { ffi_free(&v) };
}

// ── ffi_to_vm ─────────────────────────────────────────────────────────────

#[test]
fn ffi_to_vm_nil() {
    let v = JadeVal::nil();
    assert!(matches!(ffi_to_vm(&v, ZERO).unwrap(), VmValue::Nil));
}

#[test]
fn ffi_to_vm_int() {
    let v = JadeVal { tag: JADE_TAG_INT, _pad: [0; 7], data: JadeValData { as_int: 7 } };
    match ffi_to_vm(&v, ZERO).unwrap() {
        VmValue::Int(7) => {}
        other => panic!("got {:?}", other),
    }
}

#[test]
fn ffi_to_vm_float() {
    let v = JadeVal { tag: JADE_TAG_FLOAT, _pad: [0; 7], data: JadeValData { as_float: 2.25 } };
    match ffi_to_vm(&v, ZERO).unwrap() {
        VmValue::Float(f) => assert_eq!(f, 2.25),
        other => panic!("got {:?}", other),
    }
}

#[test]
fn ffi_to_vm_bool() {
    let v = JadeVal { tag: JADE_TAG_BOOL, _pad: [0; 7], data: JadeValData { as_bool: 1 } };
    match ffi_to_vm(&v, ZERO).unwrap() {
        VmValue::Bool(b) => assert!(b),
        other => panic!("got {:?}", other),
    }
}

#[test]
fn ffi_to_vm_str() {
    let cs = CString::new("hello").unwrap();
    let v = JadeVal {
        tag: JADE_TAG_STR,
        _pad: [0; 7],
        data: JadeValData { as_str: cs.as_ptr() as *const u8 },
    };
    match ffi_to_vm(&v, ZERO).unwrap() {
        VmValue::Str(s) => assert_eq!(s, "hello"),
        other => panic!("got {:?}", other),
    }
}

#[test]
fn ffi_to_vm_error_tag_is_err() {
    let cs = CString::new("boom").unwrap();
    let v = JadeVal {
        tag: JADE_TAG_ERROR,
        _pad: [0; 7],
        data: JadeValData { as_str: cs.as_ptr() as *const u8 },
    };
    match ffi_to_vm(&v, ZERO).unwrap_err() {
        JadeError::IoError { message, .. } => assert_eq!(message, "boom"),
        other => panic!("expected IoError, got {:?}", other),
    }
}

#[test]
fn ffi_to_vm_unknown_tag_is_err() {
    let v = JadeVal { tag: 99, _pad: [0; 7], data: JadeValData { as_nil: 0 } };
    match ffi_to_vm(&v, ZERO).unwrap_err() {
        JadeError::IoError { message, .. } => assert!(message.contains("unknown tag")),
        other => panic!("expected IoError, got {:?}", other),
    }
}

// ── round-trips ───────────────────────────────────────────────────────────

#[test]
fn roundtrip_primitives() {
    let mut scratch = Vec::new();
    for original in [VmValue::Nil, VmValue::Int(123), VmValue::Bool(false)] {
        let ffi = vm_to_ffi(&original, &mut scratch);
        let back = ffi_to_vm(&ffi, ZERO).unwrap();
        assert_eq!(format!("{:?}", original), format!("{:?}", back));
    }
}

#[test]
fn roundtrip_str() {
    let mut scratch = Vec::new();
    let original = VmValue::Str("round".to_string().into());
    let ffi = vm_to_ffi(&original, &mut scratch);
    match ffi_to_vm(&ffi, ZERO).unwrap() {
        VmValue::Str(s) => assert_eq!(s, "round"),
        other => panic!("got {:?}", other),
    }
}

#[test]
fn roundtrip_dict() {
    let mut d = DictObj::new();
    d.insert("a".to_string(), VmValue::Int(1));
    d.insert("b".to_string(), VmValue::Str("x".to_string().into()));
    let original = VmValue::dict(d);

    let mut scratch = Vec::new();
    let ffi = vm_to_ffi(&original, &mut scratch);
    assert_eq!(ffi.tag, JADE_TAG_DICT);
    let back = ffi_to_vm(&ffi, ZERO).unwrap();
    unsafe { ffi_free(&ffi) };

    match back {
        VmValue::Dict(d) => {
            assert_eq!(d.len(), 2);
            assert!(matches!(d.get("a"), Some(VmValue::Int(1))));
            match d.get("b") {
                Some(VmValue::Str(s)) => assert_eq!(s, "x"),
                other => panic!("got {:?}", other),
            }
        }
        other => panic!("got {:?}", other),
    }
}

#[test]
fn roundtrip_array_nested() {
    let mut inner = DictObj::new();
    inner.insert("k".to_string(), VmValue::Int(9));
    let original = make_array(vec![
        VmValue::Int(1),
        VmValue::Str("two".to_string().into()),
        VmValue::dict(inner),
    ]);

    let mut scratch = Vec::new();
    let ffi = vm_to_ffi(&original, &mut scratch);
    assert_eq!(ffi.tag, JADE_TAG_ARRAY);
    let back = ffi_to_vm(&ffi, ZERO).unwrap();
    unsafe { ffi_free(&ffi) };

    assert_eq!(crate::vm::value_to_display(&back), "[1, two, {\"k\": 9}]");
}

#[test]
fn tag_constants_are_distinct() {
    let tags = [
        JADE_TAG_NIL,
        JADE_TAG_INT,
        JADE_TAG_FLOAT,
        JADE_TAG_BOOL,
        JADE_TAG_STR,
        JADE_TAG_ERROR,
        JADE_TAG_ARRAY,
        JADE_TAG_DICT,
        JADE_TAG_STRUCT,
        JADE_TAG_BYTES,
        JADE_TAG_HANDLE,
    ];
    for i in 0..tags.len() {
        for j in (i + 1)..tags.len() {
            assert_ne!(tags[i], tags[j]);
        }
    }
}

// ── Struct round-trip ─────────────────────────────────────────────────────
//
// A struct is a dict that also carries its type name. The name is the reason it
// crosses as its own tag rather than as a dict: a receiver can refuse a struct
// that is not the type it expects, where a dict with the wrong keys reads as a
// set of nils and fails silently. These check both halves of that — the fields
// survive in declaration order, and the name survives with them.

fn sample_struct() -> VmValue {
    let mut obj = StructObj::<VmValue>::new("InferRequest");
    obj.set_field("input", VmValue::Str("hi".into()));
    obj.set_field("grammar", VmValue::Nil);
    obj.set_field("anchor", VmValue::Str("<tool>".into()));
    VmValue::Struct(Arc::new(Mutex::new(obj)))
}

#[test]
fn vm_to_ffi_struct_carries_type_name_and_fields_in_order() {
    let mut scratch = Vec::new();
    let v = vm_to_ffi(&sample_struct(), &mut scratch);
    assert_eq!(v.tag, JADE_TAG_STRUCT);

    let st = unsafe { &*v.data.as_struct };
    let name = unsafe { CStr::from_ptr(st.type_name as *const c_char) };
    assert_eq!(name.to_str().unwrap(), "InferRequest");
    assert_eq!(st.len, 3);

    let keys: Vec<String> = (0..st.len)
        .map(|i| unsafe {
            CStr::from_ptr(*st.keys.add(i) as *const c_char).to_string_lossy().into_owned()
        })
        .collect();
    assert_eq!(keys, ["input", "grammar", "anchor"], "declaration order, not sorted");

    unsafe { ffi_free(&v) };
}

#[test]
fn struct_survives_a_round_trip() {
    let mut scratch = Vec::new();
    let v = vm_to_ffi(&sample_struct(), &mut scratch);
    let back = ffi_to_vm(&v, ZERO).expect("struct should convert back");
    unsafe { ffi_free(&v) };

    let VmValue::Struct(arc) = back else { panic!("expected a struct back") };
    let guard = arc.lock();
    assert_eq!(guard.type_name(), "InferRequest");
    let fields: Vec<&String> = guard.fields().iter().map(|(k, _)| k).collect();
    assert_eq!(fields, ["input", "grammar", "anchor"]);
    assert!(matches!(guard.get_field("input"), Some(VmValue::Str(s)) if s.as_str() == "hi"));
    assert!(matches!(guard.get_field("grammar"), Some(VmValue::Nil)));
}

/// A struct nested inside a dict has to be deep-copied and freed like any other
/// container node — a leak or double-free here only shows up under load.
#[test]
fn a_struct_nested_in_a_dict_round_trips() {
    let mut d: DictObj<VmValue> = DictObj::new();
    d.insert("req", sample_struct());
    let mut scratch = Vec::new();
    let v = vm_to_ffi(&VmValue::dict(d), &mut scratch);
    let back = ffi_to_vm(&v, ZERO).expect("dict should convert back");
    unsafe { ffi_free(&v) };

    let VmValue::Dict(out) = back else { panic!("expected a dict back") };
    let Some(VmValue::Struct(arc)) = out.get("req") else { panic!("nested struct lost") };
    assert_eq!(arc.lock().type_name(), "InferRequest");
}

/// An empty struct still carries its name — the name is what a receiver checks,
/// so it must survive even when there is nothing else to copy.
#[test]
fn an_empty_struct_keeps_its_type_name() {
    let obj = StructObj::<VmValue>::new("Marker");
    let mut scratch = Vec::new();
    let v = vm_to_ffi(&VmValue::Struct(Arc::new(Mutex::new(obj))), &mut scratch);
    let back = ffi_to_vm(&v, ZERO).expect("empty struct should convert back");
    unsafe { ffi_free(&v) };

    let VmValue::Struct(arc) = back else { panic!("expected a struct back") };
    assert_eq!(arc.lock().type_name(), "Marker");
    assert_eq!(arc.lock().len(), 0);
}

// ── The ABI name of a struct ─────────────────────────────────────────────────

/// A struct crosses the boundary under its *source* name.
///
/// `aot/imports.rs` renames an imported module-global `Foo` to `Foo$2`, and that
/// name ends up in the compiled library. The number describes the importing
/// program's module graph, so it is meaningless to the other side of the call: a
/// provider package built with `use ovata::infer` returned frames named `Token$0`,
/// and the caller rejected them as an unknown frame type.
#[test]
fn the_import_mangling_suffix_is_stripped_at_the_boundary() {
    assert_eq!(super::abi_type_name("Token$0"), "Token");
    assert_eq!(super::abi_type_name("InferRequest$12"), "InferRequest");
}

/// Only a trailing `$<digits>` is mangling. Nothing else is touched, and a name
/// with no suffix is returned as-is.
#[test]
fn a_name_without_the_mangling_suffix_is_untouched() {
    for name in ["Token", "Token$", "Token$a", "Token$1a", "$0", ""] {
        assert_eq!(super::abi_type_name(name), name, "rewrote `{name}`");
    }
}

// ── Bytes round-trip ──────────────────────────────────────────────────────
//
// Bytes cross as their own counted tag rather than as a string, because a blob
// may contain NUL and need not be valid UTF-8 — a `char*` would truncate one and
// corrupt the other. These tests exist because the tag shipped without them: the
// VM implemented tag 9 in all three directions while the AOT marshaller
// (runtime_aot/native.c) had no arm at all, so the same package worked under
// `jade run` and sent nil under `jade build`. What made that easy to miss was
// that nothing tested the tag on either side.

fn sample_bytes() -> VmValue {
    // Deliberately not text: an embedded NUL, a high byte, an invalid UTF-8
    // lead byte. Every one of these survives a counted blob and none survives a
    // C string.
    VmValue::Bytes(Arc::new(jade_runtime::bytesf::BytesObj::new(
        vec![0x00, 0xFF, 0x41, 0x00, 0x80],
        jade_runtime::trust::TRUSTED,
    )))
}

#[test]
fn vm_to_ffi_bytes_carries_a_counted_payload() {
    let mut scratch = Vec::new();
    let v = vm_to_ffi(&sample_bytes(), &mut scratch);
    assert_eq!(v.tag, JADE_TAG_BYTES);

    let b = unsafe { &*v.data.as_bytes };
    assert_eq!(b.len, 5, "the length travels; it is not inferred from a NUL");
    let octets = unsafe { std::slice::from_raw_parts(b.data, b.len) };
    assert_eq!(octets, [0x00, 0xFF, 0x41, 0x00, 0x80]);

    unsafe { ffi_free(&v) };
}

#[test]
fn bytes_survive_a_round_trip() {
    let mut scratch = Vec::new();
    let v = vm_to_ffi(&sample_bytes(), &mut scratch);
    let back = ffi_to_vm(&v, ZERO).expect("bytes should convert back");
    unsafe { ffi_free(&v) };

    let VmValue::Bytes(b) = back else { panic!("expected bytes back") };
    assert_eq!(b.as_slice(), [0x00, 0xFF, 0x41, 0x00, 0x80]);
}

/// Data from a native package is from outside the program, exactly as a file
/// read is, so it arrives tainted however it was sent.
#[test]
fn inbound_bytes_are_tainted() {
    let mut scratch = Vec::new();
    let v = vm_to_ffi(&sample_bytes(), &mut scratch); // sent TRUSTED
    let back = ffi_to_vm(&v, ZERO).expect("bytes should convert back");
    unsafe { ffi_free(&v) };

    let VmValue::Bytes(b) = back else { panic!("expected bytes back") };
    assert_eq!(b.trust, jade_runtime::trust::TAINTED);
}

/// Zero-length is the case a length-carrying ABI has to get right on its own:
/// there is no terminator to fall back on, and `malloc(0)` may return null,
/// which the free path cannot tell from a failed allocation.
#[test]
fn empty_bytes_round_trip() {
    let empty = VmValue::Bytes(Arc::new(jade_runtime::bytesf::BytesObj::new(
        Vec::new(),
        jade_runtime::trust::TRUSTED,
    )));
    let mut scratch = Vec::new();
    let v = vm_to_ffi(&empty, &mut scratch);
    assert_eq!(v.tag, JADE_TAG_BYTES);
    assert_eq!(unsafe { (*v.data.as_bytes).len }, 0);

    let back = ffi_to_vm(&v, ZERO).expect("empty bytes should convert back");
    unsafe { ffi_free(&v) };

    let VmValue::Bytes(b) = back else { panic!("expected bytes back") };
    assert!(b.as_slice().is_empty());
}

/// Nested in a container, bytes are one more node the deep copy has to own and
/// free — a leak or double-free here only shows up under load.
#[test]
fn bytes_nested_in_an_array_and_a_dict_round_trip() {
    let arr = crate::builtins::make_array(vec![VmValue::Int(1), sample_bytes()]);
    let mut scratch = Vec::new();
    let v = vm_to_ffi(&arr, &mut scratch);
    let back = ffi_to_vm(&v, ZERO).expect("array should convert back");
    unsafe { ffi_free(&v) };
    let VmValue::Array(out) = back else { panic!("expected an array back") };
    let Some(VmValue::Bytes(b)) = out.lock().get(1).cloned() else { panic!("nested bytes lost") };
    assert_eq!(b.as_slice(), [0x00, 0xFF, 0x41, 0x00, 0x80]);

    let mut d: DictObj<VmValue> = DictObj::new();
    d.insert("blob", sample_bytes());
    let mut scratch = Vec::new();
    let v = vm_to_ffi(&VmValue::dict(d), &mut scratch);
    let back = ffi_to_vm(&v, ZERO).expect("dict should convert back");
    unsafe { ffi_free(&v) };
    let VmValue::Dict(out) = back else { panic!("expected a dict back") };
    let Some(VmValue::Bytes(b)) = out.get("blob") else { panic!("nested bytes lost") };
    assert_eq!(b.as_slice(), [0x00, 0xFF, 0x41, 0x00, 0x80]);
}

/// End to end: a mangled struct arrives under the name the receiver knows.
#[test]
fn a_mangled_struct_round_trips_under_its_source_name() {
    let mut obj = StructObj::<VmValue>::new("Token$0");
    obj.set_field("text", VmValue::Str("hi".to_string().into()));
    let mut scratch = Vec::new();
    let v = vm_to_ffi(&VmValue::Struct(Arc::new(Mutex::new(obj))), &mut scratch);
    let back = ffi_to_vm(&v, ZERO).expect("struct should convert back");
    unsafe { ffi_free(&v) };

    let VmValue::Struct(arc) = back else { panic!("expected a struct back") };
    assert_eq!(arc.lock().type_name(), "Token", "the caller must see the protocol name");
}

// ── Handle round-trip ─────────────────────────────────────────────────────
//
// A handle is a pointer Jade holds and hands back without ever reading. Two
// things have to survive the trip and one thing must not happen: the address
// must come back bit-identical (a handle that shifts by a byte is a crash
// inside the library), the type name must come back with it (that is what makes
// `sqlite3` and `sqlite3_stmt` different values), and `ffi_free` must not touch
// the pointee.

/// A stand-in for a pointer a package would return. Never dereferenced — which
/// is exactly the contract, so an obviously-bogus address is the honest fixture.
const FAKE_PTR: usize = 0xDEAD_BEE0;

fn sample_handle(ptr: usize, ty: &str) -> VmValue {
    VmValue::Handle(Arc::new(jade_runtime::handle::HandleObj::new(
        ptr,
        std::ffi::CString::new(ty).unwrap(),
    )))
}

#[test]
fn vm_to_ffi_handle_carries_the_pointer_and_the_type_name() {
    let mut scratch = Vec::new();
    let v = vm_to_ffi(&sample_handle(FAKE_PTR, "sqlite3"), &mut scratch);
    assert_eq!(v.tag, JADE_TAG_HANDLE);

    let h = unsafe { &*v.data.as_handle };
    assert_eq!(h.ptr as usize, FAKE_PTR);
    let name = unsafe { CStr::from_ptr(h.type_name as *const c_char) };
    assert_eq!(name.to_str().unwrap(), "sqlite3");

    unsafe { ffi_free(&v) };
}

#[test]
fn a_handle_survives_a_round_trip_unchanged() {
    let mut scratch = Vec::new();
    let v = vm_to_ffi(&sample_handle(FAKE_PTR, "SNDFILE"), &mut scratch);
    let back = ffi_to_vm(&v, ZERO).expect("handle should convert back");
    unsafe { ffi_free(&v) };

    let VmValue::Handle(h) = back else { panic!("expected a handle back") };
    assert_eq!(h.ptr, FAKE_PTR, "the address must survive bit-identical");
    assert!(h.is_type("SNDFILE"));
}

#[test]
fn the_type_name_is_copied_not_borrowed() {
    // The wire struct must own its name: the Arc backing the source value is
    // dropped here, and reading a freed name would be a use-after-free that a
    // borrowed pointer would hide until it randomly did not.
    let mut scratch = Vec::new();
    let v = vm_to_ffi(&sample_handle(FAKE_PTR, "gzFile"), &mut scratch);
    drop(scratch);

    let h = unsafe { &*v.data.as_handle };
    let name = unsafe { CStr::from_ptr(h.type_name as *const c_char) };
    assert_eq!(name.to_str().unwrap(), "gzFile");
    unsafe { ffi_free(&v) };
}

#[test]
fn freeing_a_handle_leaves_the_pointee_alone() {
    // The real assertion is that this does not crash or corrupt: `ffi_free`
    // must not pass `ptr` to free(). A genuinely heap-allocated block stands in
    // for the library's object so a stray free() would be caught by the
    // allocator rather than silently tolerated as it would be for a fake
    // address.
    let owned: Box<u64> = Box::new(0x1234_5678_9ABC_DEF0);
    let addr = Box::into_raw(owned) as usize;

    let mut scratch = Vec::new();
    let v = vm_to_ffi(&sample_handle(addr, "sqlite3"), &mut scratch);
    unsafe { ffi_free(&v) };

    // Still ours, still intact, still ours to release.
    let owned = unsafe { Box::from_raw(addr as *mut u64) };
    assert_eq!(*owned, 0x1234_5678_9ABC_DEF0);
}

#[test]
fn handles_of_different_types_stay_distinct() {
    // The point of carrying a name. These two would be interchangeable without
    // it, and passing a statement where a connection belongs is a segfault
    // inside SQLite rather than anything Jade could report.
    let mut scratch = Vec::new();
    let db = vm_to_ffi(&sample_handle(FAKE_PTR, "sqlite3"), &mut scratch);
    let stmt = vm_to_ffi(&sample_handle(FAKE_PTR, "sqlite3_stmt"), &mut scratch);

    let a = ffi_to_vm(&db, ZERO).unwrap();
    let b = ffi_to_vm(&stmt, ZERO).unwrap();
    unsafe {
        ffi_free(&db);
        ffi_free(&stmt);
    }

    let (VmValue::Handle(a), VmValue::Handle(b)) = (a, b) else { panic!("expected handles") };
    assert_eq!(a.ptr, b.ptr, "same address, deliberately");
    assert_ne!(a, b, "but not the same value, because the types differ");
    assert!(a.is_type("sqlite3") && !a.is_type("sqlite3_stmt"));
}

#[test]
fn a_null_handle_wrapper_becomes_nil_rather_than_a_null_read() {
    let v = JadeVal {
        tag: JADE_TAG_HANDLE,
        _pad: [0; 7],
        data: JadeValData { as_handle: std::ptr::null_mut() },
    };
    assert!(matches!(ffi_to_vm(&v, ZERO), Ok(VmValue::Nil)));
    // The free gate must tolerate it too, since a package can return one.
    unsafe { ffi_free(&v) };
}

#[test]
fn a_handle_nested_in_a_container_round_trips() {
    // Containers copy their elements through vm_to_ffi_owned rather than
    // vm_to_ffi, so the nested path is a separate arm from the top-level one and
    // needs its own coverage — that distinction is what the bytes bug turned on.
    let arr =
        crate::builtins::make_array(vec![VmValue::Int(1), sample_handle(FAKE_PTR, "FT_Face")]);
    let mut scratch = Vec::new();
    let v = vm_to_ffi(&arr, &mut scratch);
    let back = ffi_to_vm(&v, ZERO).expect("array should convert back");
    unsafe { ffi_free(&v) };

    let VmValue::Array(out) = back else { panic!("expected an array back") };
    let Some(VmValue::Handle(h)) = out.lock().get(1).cloned() else {
        panic!("nested handle lost");
    };
    assert_eq!(h.ptr, FAKE_PTR);
    assert!(h.is_type("FT_Face"));
}

#[test]
fn a_handle_renders_by_type_and_never_by_address() {
    // Both engines must print the same text — the parity gate diffs stdout — and
    // an address differs every run, so printing one would fail the gate for any
    // program holding a handle.
    let a = crate::vm::value_to_display(&sample_handle(0x1000, "sqlite3"));
    let b = crate::vm::value_to_display(&sample_handle(0x2000, "sqlite3"));
    assert_eq!(a, "handle<sqlite3>");
    assert_eq!(a, b);
}

// ── char across the boundary ──────────────────────────────────────────────

#[test]
fn a_char_survives_the_round_trip() {
    // `char` is a first-class Jade type that could not cross the FFI in any
    // position before ABI 5, which is why a C `char[32]` field had nothing to
    // become — an array of characters needs characters.
    for ch in ['j', 'é', '中', '\u{10FFFF}'] {
        let v = VmValue::Char(jade_runtime::trust::JChar::trusted(ch));
        let ffi = vm_to_ffi_owned(&v);
        assert_eq!(ffi.tag, JADE_TAG_CHAR, "wrong tag for {ch:?}");
        let back = ffi_to_vm(&ffi, Span { line: 0, col: 0 }).expect("should convert back");
        match back {
            VmValue::Char(c) => assert_eq!(c.ch(), ch),
            other => panic!("expected a char, got {other:?}"),
        }
    }
}

#[test]
fn a_char_from_a_package_is_tainted_whatever_it_claimed() {
    // Data coming back from a native package is from outside the program, as a
    // returned string and a returned blob already are. `TRUSTED` is zero, so
    // honouring the incoming bit would mark a char trusted for no better reason
    // than that the package zeroed its struct.
    let ffi =
        JadeVal { tag: JADE_TAG_CHAR, _pad: [0; 7], data: JadeValData { as_char: 'j' as u32 } };
    match ffi_to_vm(&ffi, Span { line: 0, col: 0 }).expect("convert") {
        VmValue::Char(c) => assert!(c.is_tainted(), "a char from a package must be tainted"),
        other => panic!("expected a char, got {other:?}"),
    }
}

#[test]
fn a_scalar_that_is_not_a_character_is_refused_by_name() {
    // A package can put anything in 32 bits. The surrogate range and everything
    // past U+10FFFF are not characters, and replacing them silently would
    // corrupt the data the tag claims to carry.
    for raw in [0xD800u32, 0x110000, 0xFFFF_FFFF] {
        let ffi = JadeVal { tag: JADE_TAG_CHAR, _pad: [0; 7], data: JadeValData { as_char: raw } };
        let err = ffi_to_vm(&ffi, Span { line: 0, col: 0 }).unwrap_err();
        assert!(format!("{err}").contains("not a Unicode scalar"), "{err}");
    }
}
