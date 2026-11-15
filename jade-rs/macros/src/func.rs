//! `#[jade::export]` on a function.
//!
//! For `pub fn resize(img: &[u8], width: u32) -> Result<Vec<u8>, String>` in crate
//! `imgtools` this writes, beside the function:
//!
//! - `__jade_imgtools_resize(a0, a1, out, err) -> i32`, `#[no_mangle] extern "C"`.
//!   Every argument is a pointer to its C form and the result goes through `out`, so
//!   the Jade backend never has to apply a platform's rules for passing a struct or
//!   a narrow integer by value. It returns 0, or 1 with a message in `err`.
//! - `__jade_imgtools_resize__free(out, err)`, which drops what the call handed over.
//! - a description record (see `jade-rs/src/meta.rs`).

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::types::{find_impl_trait, inferred, lent_without_copy, statik};
use crate::{plain, Options};

/// How the wrapper passes one argument to the function.
enum Pass {
    /// Lifted as the parameter's own type: a value, or `&str` and `&[u8]` borrowed
    /// straight from the Jade program.
    Value(syn::Type),
    /// A `&T` for any other `T`: the wrapper lifts the owned form and lends it.
    Lend(syn::Type),
}

/// Purpose: This function expands the attribute on a function.
///
/// Inputs:
/// - `f`: the function.
/// - `opts`: the attribute's arguments.
/// - `krate`: the crate's name.
///
/// Outputs: the function and its wrapper, companion and record, or an error naming
/// the fix.
pub(crate) fn expand(f: syn::ItemFn, opts: &Options, krate: &str) -> syn::Result<TokenStream> {
    refuse(&f.sig)?;
    let rt = &opts.runtime;
    let name = &f.sig.ident;
    let jade_name = plain(name);
    let symbol = format_ident!("__jade_{}_{}", krate, jade_name);
    let free_symbol = format_ident!("__jade_{}_{}__free", krate, jade_name);
    let display = format!("{krate}.{jade_name}");
    let symbol_text = symbol.to_string();

    let mut passes = Vec::new();
    let mut names = Vec::new();
    for input in &f.sig.inputs {
        let syn::FnArg::Typed(pt) = input else {
            return Err(syn::Error::new_spanned(
                input,
                "a method cannot be exported yet; write a function that takes the struct as \
                 its first argument",
            ));
        };
        let syn::Pat::Ident(id) = &*pt.pat else {
            return Err(syn::Error::new_spanned(
                &pt.pat,
                "write a plain name for each parameter of an exported function; Jade passes \
                 each argument whole",
            ));
        };
        names.push(plain(&id.ident));
        passes.push(classify(&pt.ty)?);
    }

    let ret: syn::Type = match &f.sig.output {
        syn::ReturnType::Default => syn::parse_quote!(()),
        syn::ReturnType::Type(_, t) => {
            if let Some(i) = find_impl_trait(t) {
                return Err(syn::Error::new_spanned(
                    i,
                    "an exported function cannot return `impl Trait` yet: return a Vec rather \
                     than an iterator (iterators will connect to Jade's pipes)",
                ));
            }
            (**t).clone()
        }
    };
    let ret_s = statik(&ret);

    let ptrs: Vec<_> = (0..passes.len()).map(|i| format_ident!("__jade_a{i}")).collect();
    let vals: Vec<_> = (0..passes.len()).map(|i| format_ident!("__jade_v{i}")).collect();
    let mut params = Vec::new();
    let mut lifts = Vec::new();
    let mut args = Vec::new();
    let mut described = Vec::new();
    for ((pass, ptr), val) in passes.iter().zip(&ptrs).zip(&vals) {
        match pass {
            Pass::Value(t) => {
                let ts = statik(t);
                let te = inferred(t);
                params.push(quote!(#ptr: *const <#ts as #rt::__private::Type>::C));
                lifts.push(quote!(let #val = unsafe { #rt::__private::lift::<#te>(#ptr) }?;));
                args.push(quote!(#val));
                described.push(quote!(<#ts as #rt::__private::Type>::JADE));
            }
            Pass::Lend(t) => {
                let ts = statik(t);
                let te = inferred(t);
                let owned_s = quote!(<#ts as #rt::__private::Borrowed>::Owned);
                let owned_e = quote!(<#te as #rt::__private::Borrowed>::Owned);
                params.push(quote!(#ptr: *const <#owned_s as #rt::__private::Type>::C));
                lifts.push(quote!(let #val = unsafe { #rt::__private::lift::<#owned_e>(#ptr) }?;));
                args.push(quote!(::core::borrow::Borrow::borrow(&#val)));
                described.push(quote!(<#owned_s as #rt::__private::Type>::JADE));
            }
        }
    }

    let mut meta = quote! {
        #rt::__private::Meta::new()
            .str("{\"v\":")
            .num(#rt::__private::FORMAT)
            .str(concat!(",\"kind\":\"fn\",\"crate\":\"", #krate, "\",\"name\":\"", #jade_name,
                         "\",\"symbol\":\"", #symbol_text, "\",\"params\":["))
    };
    for (i, (n, d)) in names.iter().zip(&described).enumerate() {
        let sep = if i == 0 { "" } else { "," };
        meta = quote! {
            #meta
                .str(concat!(#sep, "{\"name\":\"", #n, "\",\"type\":\""))
                .meta(&#d)
                .str("\"}")
        };
    }
    let meta = quote! {
        #meta
            .str("],\"ret\":\"")
            .meta(&<#ret_s as #rt::__private::Return>::JADE)
            .str("\",\"raises\":")
            .bool(<#ret_s as #rt::__private::Return>::RAISES)
            .str("}")
    };
    let record = crate::record::emit(rt, meta);

    Ok(quote! {
        #f

        #[doc(hidden)]
        #[unsafe(no_mangle)]
        #[allow(non_snake_case, unused_unsafe, clippy::missing_safety_doc)]
        pub unsafe extern "C" fn #symbol(
            #(#params,)*
            __jade_out: *mut <#ret_s as #rt::__private::Return>::C,
            __jade_err: *mut #rt::__private::CVec<u8>,
        ) -> i32 {
            unsafe {
                #rt::__private::call(#display, __jade_out, __jade_err, || {
                    #(#lifts)*
                    ::core::result::Result::Ok(#name(#(#args),*))
                })
            }
        }

        #[doc(hidden)]
        #[unsafe(no_mangle)]
        #[allow(non_snake_case, unused_unsafe, clippy::missing_safety_doc)]
        pub unsafe extern "C" fn #free_symbol(
            __jade_out: *mut <#ret_s as #rt::__private::Return>::C,
            __jade_err: *mut #rt::__private::CVec<u8>,
        ) {
            unsafe { #rt::__private::free::<#ret_s>(__jade_out, __jade_err) }
        }

        #record
    })
}

/// Purpose: This function refuses a signature no wrapper can call, naming why.
fn refuse(sig: &syn::Signature) -> syn::Result<()> {
    let err = |tokens: &dyn quote::ToTokens, m: &str| Err(syn::Error::new_spanned(tokens, m));
    if let Some(a) = &sig.asyncness {
        return err(
            a,
            "an async fn cannot be exported yet; export a function that blocks on it, and use \
             Jade's tasks to run it beside other work",
        );
    }
    if let Some(u) = &sig.unsafety {
        return err(
            u,
            "an unsafe fn cannot be exported: Jade would call it with whatever a program \
             passes. Export a safe function that checks what the unsafe one needs",
        );
    }
    if let Some(abi) = &sig.abi {
        return err(abi, "remove `extern`: #[jade::export] writes the extern \"C\" wrapper itself");
    }
    if let Some(v) = &sig.variadic {
        return err(v, "a variadic function cannot be exported; take a Vec");
    }
    for p in &sig.generics.params {
        match p {
            syn::GenericParam::Lifetime(_) => {}
            other => {
                return err(
                    other,
                    "a generic function cannot be exported: Jade calls one compiled function, so \
                     write it for the types a Jade program passes",
                );
            }
        }
    }
    Ok(())
}

/// Purpose: This function decides how the wrapper passes one parameter.
fn classify(t: &syn::Type) -> syn::Result<Pass> {
    if let Some(i) = find_impl_trait(t) {
        return Err(syn::Error::new_spanned(
            i,
            "`impl Trait` cannot be exported: Jade calls one compiled function, so name a \
             concrete type",
        ));
    }
    match t {
        syn::Type::Reference(r) if r.mutability.is_some() => Err(syn::Error::new_spanned(
            r,
            "a `&mut` argument cannot cross: the Jade program would not see what the function \
             changes. Take the value and return the new one instead",
        )),
        syn::Type::Reference(r) if lent_without_copy(&r.elem) => Ok(Pass::Value(t.clone())),
        syn::Type::Reference(r) => Ok(Pass::Lend((*r.elem).clone())),
        syn::Type::Ptr(p) => Err(syn::Error::new_spanned(
            p,
            "a raw pointer cannot cross; Jade passes values. Wrap what it points at in a type \
             that crosses",
        )),
        _ => Ok(Pass::Value(t.clone())),
    }
}
