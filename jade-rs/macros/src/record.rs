//! `#[jade::export]` on a struct, and the record every export adds to the library.
//!
//! For `pub struct Point { pub x: f64, pub y: f64 }` in crate `geo` this writes,
//! beside the struct:
//!
//! - `__JadeC_Point`, a `#[repr(C)]` struct with each field's C form, in order. The
//!   Rust compiler lays it out by C's rules, and the Jade backend reads it the same way.
//! - `Type`, `Elem`, `Lift` and `Lower` for `Point`, converting field by field.
//! - a record naming the struct, its fields' Jade types, and the mirror's size,
//!   alignment and field offsets, so the compiler can refuse a layout it would read
//!   differently.
//!
//! The struct becomes a Jade class named `geo::Point` in descriptions, which a program
//! writes as `geo.Point`, with every field public.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::{plain, Options};

/// Purpose: This function expands the attribute on a struct.
///
/// Inputs:
/// - `s`: the struct.
/// - `opts`: the attribute's arguments.
/// - `krate`: the crate's name.
///
/// Outputs: the struct and what Jade needs to pass it, or an error naming the fix.
pub(crate) fn expand(s: syn::ItemStruct, opts: &Options, krate: &str) -> syn::Result<TokenStream> {
    let rt = &opts.runtime;
    if !s.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &s.generics,
            "a struct with generics or lifetimes cannot cross to Jade: a Jade class has one \
             layout. Write the struct with concrete, owned field types",
        ));
    }
    let syn::Fields::Named(fields) = &s.fields else {
        return Err(syn::Error::new_spanned(
            &s.fields,
            "give the struct's fields names: it becomes a Jade class, whose fields are named",
        ));
    };
    for f in &fields.named {
        if !matches!(f.vis, syn::Visibility::Public(_)) {
            return Err(syn::Error::new_spanned(
                f,
                "every field of an exported struct is public in Jade, so mark it `pub`; keep a \
                 struct with private fields on the Rust side",
            ));
        }
    }

    let name = &s.ident;
    let vis = &s.vis;
    let jade_name = plain(name);
    let mirror = format_ident!("__JadeC_{}", jade_name);
    let idents: Vec<&syn::Ident> = fields.named.iter().filter_map(|f| f.ident.as_ref()).collect();
    let types: Vec<&syn::Type> = fields.named.iter().map(|f| &f.ty).collect();
    let jade_type = format!("{krate}::{jade_name}");

    let mut meta = quote! {
        #rt::__private::Meta::new()
            .str("{\"v\":")
            .num(#rt::__private::FORMAT)
            .str(concat!(",\"kind\":\"struct\",\"crate\":\"", #krate, "\",\"name\":\"",
                         #jade_name, "\",\"size\":"))
            .num(::core::mem::size_of::<#mirror>())
            .str(",\"align\":")
            .num(::core::mem::align_of::<#mirror>())
            .str(",\"fields\":[")
    };
    for (i, (id, ty)) in idents.iter().zip(&types).enumerate() {
        let sep = if i == 0 { "" } else { "," };
        let fname = plain(id);
        meta = quote! {
            #meta
                .str(concat!(#sep, "{\"name\":\"", #fname, "\",\"type\":\""))
                .meta(&<#ty as #rt::__private::Type>::JADE)
                .str("\",\"offset\":")
                .num(::core::mem::offset_of!(#mirror, #id))
                .str("}")
        };
    }
    let record = emit(rt, quote!(#meta.str("]}")));

    Ok(quote! {
        #s

        #[doc(hidden)]
        #[repr(C)]
        #[derive(Clone, Copy)]
        #[allow(non_camel_case_types)]
        #vis struct #mirror {
            #(pub #idents: <#types as #rt::__private::Type>::C,)*
        }

        unsafe impl #rt::__private::Type for #name {
            type C = #mirror;
            const JADE: #rt::__private::Meta = #rt::__private::Meta::new().str(#jade_type);
        }

        unsafe impl #rt::__private::Elem for #name {}

        impl<'__jade> #rt::__private::Lift<'__jade> for #name {
            #[allow(unused_unsafe)]
            unsafe fn lift(
                c: &'__jade #mirror,
            ) -> ::core::result::Result<Self, ::std::string::String> {
                ::core::result::Result::Ok(#name {
                    #(#idents: unsafe {
                        <#types as #rt::__private::Lift<'__jade>>::lift(&c.#idents)
                    }?,)*
                })
            }
        }

        impl #rt::__private::Lower for #name {
            fn lower(self) -> #mirror {
                let #name { #(#idents),* } = self;
                #mirror {
                    #(#idents: <#types as #rt::__private::Lower>::lower(#idents),)*
                }
            }
            #[allow(unused_unsafe, unused_variables)]
            unsafe fn free(c: &mut #mirror) {
                #(unsafe { <#types as #rt::__private::Lower>::free(&mut c.#idents) };)*
            }
        }

        #record
    })
}

/// Purpose: This function writes the static that puts one description record in the
/// built library.
///
/// Inputs:
/// - `rt`: the path to `jade-rs`.
/// - `meta`: an expression building the record's JSON with `Meta`.
///
/// Outputs: an anonymous `const` holding a `#[used]` static in the `jade_meta`
/// section, where the compiler looks for it.
pub(crate) fn emit(rt: &syn::Path, meta: TokenStream) -> TokenStream {
    quote! {
        const _: () = {
            const __JADE_M: #rt::__private::Meta = #meta;
            #[used]
            #[cfg_attr(target_vendor = "apple", unsafe(link_section = "__DATA,__jade_meta"))]
            #[cfg_attr(not(target_vendor = "apple"), unsafe(link_section = ".jade_meta"))]
            static __JADE_META: [u8; __JADE_M.record_len()] = __JADE_M.record();
        };
    }
}
