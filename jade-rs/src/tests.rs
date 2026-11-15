//! The generated code, compiled and called the way a Jade program calls it: each
//! argument as a pointer to its C form, the result through `out`, and the status
//! returned. The functions below are exported by the real macro.

use crate::__private::{COpt, CVec, Lower, Meta, Type};

/// A struct that crosses.
#[crate::export]
pub struct Point {
    pub x: f64,
    pub y: f64,
    pub label: String,
}

#[crate::export]
pub fn add(a: i64, b: i64) -> i64 {
    a + b
}

#[crate::export]
pub fn shout(s: &str) -> Result<String, String> {
    if s.is_empty() {
        Err("nothing to shout".to_string())
    } else {
        Ok(s.to_uppercase())
    }
}

// The lifetime is named on purpose: the wrapper must not carry it.
#[crate::export]
#[allow(clippy::needless_lifetimes)]
pub fn first<'a>(s: &'a str) -> &'a str {
    s.split(' ').next().unwrap_or("")
}

#[crate::export]
pub fn midpoint(a: &Point, b: Point) -> Point {
    Point { x: (a.x + b.x) / 2.0, y: (a.y + b.y) / 2.0, label: format!("{}-{}", a.label, b.label) }
}

#[crate::export]
pub fn total(xs: &[i64]) -> Option<i64> {
    if xs.is_empty() {
        None
    } else {
        Some(xs.iter().sum())
    }
}

#[crate::export]
pub fn boom(n: u32) -> u32 {
    if n > 1 {
        panic!("{n} is too many");
    }
    n
}

#[crate::export]
pub fn nothing() {}

/// Purpose: This function lends a string the way a Jade program does.
fn lend(s: &str) -> CVec<u8> {
    CVec { ptr: s.as_ptr() as *mut u8, len: s.len(), cap: 0 }
}

/// Purpose: This function copies a returned string out, as the program does.
fn text(c: &CVec<u8>) -> String {
    // SAFETY: `c` came back from a call, which handed over a `Vec<u8>` of UTF-8.
    unsafe { String::from_utf8(c.as_slice().to_vec()).unwrap() }
}

#[test]
fn scalars_cross_and_the_status_is_zero() {
    let (a, b) = (40i64, 2i64);
    let mut out = 0i64;
    let mut err = CVec { ptr: std::ptr::null_mut(), len: 0, cap: 0 };
    // SAFETY: each pointer is valid for the call.
    let st = unsafe { __jade_jade_add(&a, &b, &mut out, &mut err) };
    assert_eq!((st, out), (0, 42));
}

#[test]
fn an_err_raises_its_text_and_ok_hands_over_a_string() {
    let mut out = CVec { ptr: std::ptr::null_mut(), len: 0, cap: 0 };
    let mut err = out;
    let arg = lend("hi");
    // SAFETY: as above; the companion frees what the call handed over.
    unsafe {
        assert_eq!(__jade_jade_shout(&arg, &mut out, &mut err), 0);
        assert_eq!(text(&out), "HI");
        __jade_jade_shout__free(&mut out, std::ptr::null_mut());
        let empty = lend("");
        assert_eq!(__jade_jade_shout(&empty, &mut out, &mut err), 1);
        assert_eq!(text(&err), "nothing to shout");
        __jade_jade_shout__free(std::ptr::null_mut(), &mut err);
    }
}

#[test]
fn a_borrowed_result_is_copied_before_the_argument_is_let_go() {
    let mut out = CVec { ptr: std::ptr::null_mut(), len: 0, cap: 0 };
    let mut err = out;
    let arg = lend("hello world");
    // SAFETY: as above.
    unsafe {
        assert_eq!(__jade_jade_first(&arg, &mut out, &mut err), 0);
        assert_ne!(out.ptr as *const u8, arg.ptr as *const u8);
        assert_eq!(text(&out), "hello");
        __jade_jade_first__free(&mut out, std::ptr::null_mut());
    }
}

#[test]
fn a_struct_crosses_both_ways_by_its_mirror() {
    let a = __JadeC_Point { x: 0.0, y: 0.0, label: lend("a") };
    let b = __JadeC_Point { x: 2.0, y: 4.0, label: lend("b") };
    let mut out = __JadeC_Point { x: 0.0, y: 0.0, label: lend("") };
    let mut err = CVec { ptr: std::ptr::null_mut(), len: 0, cap: 0 };
    // SAFETY: as above.
    unsafe {
        assert_eq!(__jade_jade_midpoint(&a, &b, &mut out, &mut err), 0);
        assert_eq!((out.x, out.y), (1.0, 2.0));
        assert_eq!(text(&out.label), "a-b");
        __jade_jade_midpoint__free(&mut out, std::ptr::null_mut());
    }
}

#[test]
fn an_array_is_lent_and_an_optional_comes_back() {
    let xs = [1i64, 2, 3];
    let arg = CVec { ptr: xs.as_ptr() as *mut i64, len: 3, cap: 0 };
    let mut out = COpt { some: 0, value: 0i64 };
    let mut err = CVec { ptr: std::ptr::null_mut(), len: 0, cap: 0 };
    // SAFETY: as above.
    unsafe {
        assert_eq!(__jade_jade_total(&arg, &mut out, &mut err), 0);
        assert_eq!((out.some, out.value), (1, 6));
        let none = CVec { ptr: std::ptr::null_mut(), len: 0, cap: 0 };
        assert_eq!(__jade_jade_total(&none, &mut out, &mut err), 0);
        assert_eq!(out.some, 0);
    }
}

#[test]
fn a_panic_raises_naming_the_function_and_never_unwinds_out() {
    let n = 5u32;
    let mut out = 0u32;
    let mut err = CVec { ptr: std::ptr::null_mut(), len: 0, cap: 0 };
    // SAFETY: as above.
    unsafe {
        assert_eq!(__jade_jade_boom(&n, &mut out, &mut err), 1);
        assert_eq!(text(&err), "jade.boom panicked: 5 is too many");
        __jade_jade_boom__free(std::ptr::null_mut(), &mut err);
    }
}

#[test]
fn a_function_returning_nothing_returns_nil() {
    let mut out = 9u8;
    let mut err = CVec { ptr: std::ptr::null_mut(), len: 0, cap: 0 };
    // SAFETY: as above.
    assert_eq!(unsafe { __jade_jade_nothing(&mut out, &mut err) }, 0);
    assert_eq!(out, 0);
    assert_eq!(<() as Type>::JADE.as_str(), "nil");
}

#[test]
fn types_are_described_in_jade_spelling() {
    assert_eq!(<Vec<String> as Type>::JADE.as_str(), "[str]");
    assert_eq!(<Vec<u8> as Type>::JADE.as_str(), "bytes");
    assert_eq!(<Option<Vec<Point>> as Type>::JADE.as_str(), "[jade::Point]?");
    assert_eq!(<usize as Type>::JADE.as_str(), "ulong");
    assert_eq!(<Point as Type>::JADE.as_str(), "jade::Point");
}

#[test]
fn a_record_is_magic_length_and_json() {
    const M: Meta = Meta::new().str("{\"a\":").num(1200).str(",\"b\":").bool(true).str("}");
    let r: [u8; M.record_len()] = M.record();
    assert_eq!(&r[..8], b"JADEMETA");
    let n = u32::from_le_bytes([r[8], r[9], r[10], r[11]]) as usize;
    assert_eq!(std::str::from_utf8(&r[12..12 + n]).unwrap(), "{\"a\":1200,\"b\":true}");
}

#[test]
fn lowering_and_freeing_a_nested_value_releases_everything() {
    let v: Vec<Option<String>> = vec![Some("x".into()), None, Some("yz".into())];
    let mut c = v.lower();
    assert_eq!(c.len, 3);
    // SAFETY: `c` came from `lower` and is freed once.
    unsafe { <Vec<Option<String>> as Lower>::free(&mut c) };
}
