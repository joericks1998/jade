//! How a parameter or result type is spelled in the code the macro writes.
//!
//! A type in the user's signature may carry lifetimes, named or elided. The wrapper is
//! not generic, so it spells each type twice: with every lifetime `'static`, where a
//! type is only named to reach its C form or its description, which do not depend on
//! lifetimes; and with every named lifetime `'_`, where a value is lifted and rustc
//! infers how long the borrow lasts.

use syn::visit_mut::VisitMut;

/// Purpose: This function spells a type with every lifetime `'static`.
///
/// Inputs:
/// - `t`: the type.
///
/// Outputs: the type, for naming its C form or description.
pub(crate) fn statik(t: &syn::Type) -> syn::Type {
    struct V;
    impl VisitMut for V {
        fn visit_lifetime_mut(&mut self, l: &mut syn::Lifetime) {
            *l = syn::Lifetime::new("'static", l.span());
        }
        fn visit_type_reference_mut(&mut self, r: &mut syn::TypeReference) {
            if r.lifetime.is_none() {
                r.lifetime = Some(syn::Lifetime::new("'static", proc_macro2::Span::call_site()));
            }
            syn::visit_mut::visit_type_reference_mut(self, r);
        }
    }
    let mut t = t.clone();
    V.visit_type_mut(&mut t);
    t
}

/// Purpose: This function spells a type with every named lifetime `'_`.
///
/// Inputs:
/// - `t`: the type.
///
/// Outputs: the type, for lifting a value whose borrow rustc infers.
pub(crate) fn inferred(t: &syn::Type) -> syn::Type {
    struct V;
    impl VisitMut for V {
        fn visit_lifetime_mut(&mut self, l: &mut syn::Lifetime) {
            *l = syn::Lifetime::new("'_", l.span());
        }
    }
    let mut t = t.clone();
    V.visit_type_mut(&mut t);
    t
}

/// Purpose: This function says whether a type is `str` or `[u8]`, which a reference
/// argument borrows from the Jade program with no copy.
pub(crate) fn lent_without_copy(t: &syn::Type) -> bool {
    match t {
        syn::Type::Path(p) => p.qself.is_none() && p.path.is_ident("str"),
        syn::Type::Slice(s) => {
            matches!(&*s.elem, syn::Type::Path(p) if p.qself.is_none() && p.path.is_ident("u8"))
        }
        _ => false,
    }
}

/// Purpose: This function finds an `impl Trait` anywhere inside a type.
pub(crate) fn find_impl_trait(t: &syn::Type) -> Option<&syn::TypeImplTrait> {
    match t {
        syn::Type::ImplTrait(i) => Some(i),
        syn::Type::Reference(r) => find_impl_trait(&r.elem),
        syn::Type::Slice(s) => find_impl_trait(&s.elem),
        syn::Type::Array(a) => find_impl_trait(&a.elem),
        syn::Type::Paren(p) => find_impl_trait(&p.elem),
        syn::Type::Group(g) => find_impl_trait(&g.elem),
        syn::Type::Tuple(t) => t.elems.iter().find_map(find_impl_trait),
        syn::Type::Path(p) => p.path.segments.iter().find_map(|s| match &s.arguments {
            syn::PathArguments::AngleBracketed(a) => a.args.iter().find_map(|g| match g {
                syn::GenericArgument::Type(t) => find_impl_trait(t),
                _ => None,
            }),
            _ => None,
        }),
        _ => None,
    }
}
