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
    let v = vm_to_ffi(&VmValue::Str("hi".to_string()), &mut scratch);
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
fn vm_to_ffi_non_primitive_becomes_nil() {
    let mut scratch = Vec::new();
    // A Dict is non-primitive — native fns can't consume it.
    let v = vm_to_ffi(&VmValue::Dict(HashMap::new()), &mut scratch);
    assert_eq!(v.tag, JADE_TAG_NIL);
    assert!(scratch.is_empty());
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
    let original = VmValue::Str("round".to_string());
    let ffi = vm_to_ffi(&original, &mut scratch);
    match ffi_to_vm(&ffi, ZERO).unwrap() {
        VmValue::Str(s) => assert_eq!(s, "round"),
        other => panic!("got {:?}", other),
    }
}

#[test]
fn tag_constants_are_distinct() {
    let tags = [JADE_TAG_NIL, JADE_TAG_INT, JADE_TAG_FLOAT, JADE_TAG_BOOL, JADE_TAG_STR, JADE_TAG_ERROR];
    for i in 0..tags.len() {
        for j in (i + 1)..tags.len() {
            assert_ne!(tags[i], tags[j]);
        }
    }
}
