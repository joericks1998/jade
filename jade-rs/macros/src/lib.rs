//! `#[jade::export]`: makes a Rust function or struct callable from Jade.
//!
//! On a function it writes a `#[no_mangle] extern "C"` wrapper and a `__free`
//! companion (`func.rs`); on a struct, a `#[repr(C)]` mirror and the conversions both
//! ways (`record.rs`). On both it adds a description record to the built library,
//! which the Jade compiler reads to type the calls. The traits and types the
//! generated code calls are in `jade-rs`, which re-exports this macro; see its
//! README and `design/v2-rust-packages.md`.
//!
//! The work is done on `proc_macro2` tokens by [`expand`], so the tests below can run
//! it without a compiler around them.

use proc_macro2::TokenStream;

mod func;
mod record;
mod types;

/// Purpose: This attribute exports a function or struct to Jade.
///
/// Inputs:
/// - `attr`: empty, or `crate = "name"` when the package renamed its `jade-rs`
///   dependency to something other than `jade`.
/// - `item`: the function or struct.
///
/// Outputs: the item, unchanged, followed by what Jade needs to call it.
#[proc_macro_attribute]
pub fn export(
    attr: proc_macro::TokenStream,
    item: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    let krate = match std::env::var("CARGO_CRATE_NAME") {
        Ok(k) => k,
        Err(_) => {
            return syn::Error::new(
                proc_macro2::Span::call_site(),
                "#[jade::export] names its symbols after the crate, and CARGO_CRATE_NAME is \
                 not set; build the package with cargo",
            )
            .to_compile_error()
            .into();
        }
    };
    expand(attr.into(), item.into(), &krate).unwrap_or_else(|e| e.to_compile_error()).into()
}

/// Purpose: This struct holds the attribute's arguments.
pub(crate) struct Options {
    /// The path the generated code reaches `jade-rs` through, `::jade` unless the
    /// package renamed the dependency.
    pub runtime: syn::Path,
}

impl Options {
    /// Purpose: This function reads `crate = "name"`, the one argument there is.
    fn parse(attr: TokenStream) -> syn::Result<Options> {
        let mut runtime: syn::Path = syn::parse_quote!(::jade);
        let parser = syn::meta::parser(|meta| {
            if meta.path.is_ident("crate") {
                let s: syn::LitStr = meta.value()?.parse()?;
                let name: syn::Ident = s.parse()?;
                runtime = syn::parse_quote!(::#name);
                Ok(())
            } else {
                Err(meta.error(
                    "#[jade::export] takes no arguments but `crate = \"name\"`, for a package \
                     that renamed its jade-rs dependency",
                ))
            }
        });
        syn::parse::Parser::parse2(parser, attr)?;
        Ok(Options { runtime })
    }
}

/// Purpose: This function expands the attribute.
///
/// Inputs:
/// - `attr`: the attribute's arguments.
/// - `item`: the item it is on.
/// - `krate`: the crate's name, which the symbols carry.
///
/// Outputs: the expansion, or an error naming the fix.
pub(crate) fn expand(
    attr: TokenStream,
    item: TokenStream,
    krate: &str,
) -> syn::Result<TokenStream> {
    let opts = Options::parse(attr)?;
    match syn::parse2::<syn::Item>(item)? {
        syn::Item::Fn(f) => func::expand(f, &opts, krate),
        syn::Item::Struct(s) => record::expand(s, &opts, krate),
        other => Err(syn::Error::new_spanned(
            other,
            "#[jade::export] goes on a function or a struct with named fields; an enum, a \
             trait or an impl block cannot cross to Jade yet",
        )),
    }
}

/// Purpose: This function turns a Rust identifier into the name Jade sees, dropping
/// the `r#` of a raw identifier.
pub(crate) fn plain(id: &syn::Ident) -> String {
    let s = id.to_string();
    s.strip_prefix("r#").map(str::to_string).unwrap_or(s)
}

#[cfg(test)]
mod tests;
