use jade_runtime::coll::DictObj;
use super::*;

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
        CStr::from_ptr(v.data.as_str as *const std::ffi::c_char)
            .to_string_lossy()
            .into_owned()
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
    let v = vm_to_ffi(&VmValue::Dict(DictObj::new()), &mut scratch);
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
    let original = VmValue::Dict(d);

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
        VmValue::Dict(inner),
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
        JADE_TAG_NIL, JADE_TAG_INT, JADE_TAG_FLOAT, JADE_TAG_BOOL,
        JADE_TAG_STR, JADE_TAG_ERROR, JADE_TAG_ARRAY, JADE_TAG_DICT,
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
    let v = vm_to_ffi(&VmValue::Dict(d), &mut scratch);
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
