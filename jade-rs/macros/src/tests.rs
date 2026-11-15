//! The macro's refusals, each checked for the fix it names, and the shape of what it
//! writes. That the written code compiles and runs is `jade-rs`'s tests' job.

use quote::quote;

use super::expand;

/// Purpose: This function expands an item and returns the error's text.
fn refusal(item: proc_macro2::TokenStream) -> String {
    match expand(quote!(), item, "demo") {
        Ok(out) => panic!("expected a refusal, got {out}"),
        Err(e) => e.to_string(),
    }
}

/// Purpose: This function expands an item that must be accepted.
fn accepted(item: proc_macro2::TokenStream) -> String {
    match expand(quote!(), item, "demo") {
        Ok(out) => out.to_string(),
        Err(e) => panic!("expected an expansion, got: {e}"),
    }
}

#[test]
fn a_function_gets_a_wrapper_a_companion_and_a_record() {
    let out = accepted(quote! {
        pub fn resize(img: &[u8], width: u32) -> Result<Vec<u8>, String> { todo!() }
    });
    assert!(out.contains("fn __jade_demo_resize ("), "{out}");
    assert!(out.contains("fn __jade_demo_resize__free ("), "{out}");
    assert!(out.contains("no_mangle"), "{out}");
    assert!(out.contains("\"demo.resize\""), "{out}");
    assert!(out.contains("jade_meta"), "{out}");
}

#[test]
fn a_reference_to_a_struct_is_lent_from_a_value_the_wrapper_owns() {
    let out = accepted(quote! {
        pub fn area(p: &Point) -> f64 { 0.0 }
    });
    assert!(out.contains("Borrowed"), "{out}");
}

#[test]
fn named_lifetimes_are_accepted_and_rewritten() {
    let out = accepted(quote! {
        pub fn first<'a>(s: &'a str) -> &'a str { s }
    });
    // The function itself is written back unchanged; the wrapper after it is not
    // generic, so no named lifetime survives into it.
    let wrapper = out.split("__jade_demo_first (").nth(1).unwrap_or_default();
    assert!(wrapper.contains("'static"), "{out}");
    assert!(!wrapper.contains("'a"), "{out}");
}

#[test]
fn signatures_no_wrapper_can_call_are_refused_with_the_fix() {
    let cases = [
        (
            quote!(
                pub async fn f() {}
            ),
            "async fn",
        ),
        (
            quote!(
                pub unsafe fn f() {}
            ),
            "safe function",
        ),
        (
            quote!(
                pub fn f<T>(x: T) {}
            ),
            "generic function",
        ),
        (
            quote!(
                pub fn f(x: impl Into<String>) {}
            ),
            "concrete type",
        ),
        (
            quote!(
                pub fn f() -> impl Iterator<Item = i64> {
                    0..1
                }
            ),
            "return a Vec",
        ),
        (
            quote!(
                pub fn f(x: &mut Vec<i64>) {}
            ),
            "return the new one",
        ),
        (
            quote!(
                pub fn f(x: *const u8) {}
            ),
            "raw pointer",
        ),
        (
            quote!(
                pub fn f((a, b): (i64, i64)) {}
            ),
            "plain name",
        ),
        (
            quote!(
                pub extern "C" fn f() {}
            ),
            "remove `extern`",
        ),
    ];
    for (item, want) in cases {
        let got = refusal(item.clone());
        assert!(got.contains(want), "{item}: wanted '{want}' in '{got}'");
    }
}

#[test]
fn a_method_is_refused() {
    let got = refusal(quote!(
        pub fn f(&self) {}
    ));
    assert!(got.contains("method"), "{got}");
}

#[test]
fn a_struct_gets_a_mirror_and_conversions() {
    let out = accepted(quote! {
        pub struct Point { pub x: f64, pub y: f64 }
    });
    assert!(out.contains("struct __JadeC_Point"), "{out}");
    assert!(out.contains("repr (C)"), "{out}");
    assert!(out.contains("\"demo::Point\""), "{out}");
    assert!(out.contains("offset_of"), "{out}");
}

#[test]
fn structs_that_cannot_be_a_class_are_refused_with_the_fix() {
    let cases = [
        (
            quote!(
                pub struct P {
                    x: f64,
                }
            ),
            "mark it `pub`",
        ),
        (
            quote!(
                pub struct P<T> {
                    pub x: T,
                }
            ),
            "concrete, owned field types",
        ),
        (
            quote!(
                pub struct P<'a> {
                    pub x: &'a str,
                }
            ),
            "concrete, owned field types",
        ),
        (
            quote!(
                pub struct P(pub f64);
            ),
            "fields names",
        ),
        (
            quote!(
                pub struct P;
            ),
            "fields names",
        ),
        (
            quote!(
                pub enum E {
                    A,
                }
            ),
            "enum",
        ),
    ];
    for (item, want) in cases {
        let got = refusal(item.clone());
        assert!(got.contains(want), "{item}: wanted '{want}' in '{got}'");
    }
}

#[test]
fn the_runtime_path_can_be_renamed() {
    let out = expand(
        quote!(crate = "jade_rs"),
        quote!(
            pub fn f() {}
        ),
        "demo",
    )
    .map(|t| t.to_string())
    .unwrap_or_else(|e| e.to_string());
    assert!(out.contains(":: jade_rs :: __private"), "{out}");
    let bad = expand(
        quote!(name = "x"),
        quote!(
            pub fn f() {}
        ),
        "demo",
    )
    .map(|t| t.to_string())
    .unwrap_or_else(|e| e.to_string());
    assert!(bad.contains("crate = \\\"name\\\"") || bad.contains("crate = \"name\""), "{bad}");
}
