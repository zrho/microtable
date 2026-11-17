//! Trait support selected by the table's derive attributes.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Token, ext::IdentExt, punctuated::Punctuated};

// Keep cfg_attr conditions so trait support follows conditional derives too.
fn conditions(meta: &syn::Meta, name: &str) -> syn::Result<Vec<TokenStream>> {
    if meta.path().is_ident("derive") {
        let syn::Meta::List(list) = meta else {
            return Ok(Vec::new());
        };
        let paths = list.parse_args_with(Punctuated::<syn::Path, Token![,]>::parse_terminated)?;
        return Ok(paths
            .iter()
            .filter(|path| {
                path.segments
                    .last()
                    .is_some_and(|segment| segment.ident == name)
            })
            .map(|_| quote!(all()))
            .collect());
    }
    if meta.path().is_ident("cfg_attr") {
        let syn::Meta::List(list) = meta else {
            return Ok(Vec::new());
        };
        let args = list.parse_args_with(Punctuated::<syn::Meta, Token![,]>::parse_terminated)?;
        let mut args = args.iter();
        let Some(predicate) = args.next() else {
            return Ok(Vec::new());
        };
        let mut result = Vec::new();
        for attr in args {
            for condition in conditions(attr, name)? {
                result.push(quote!(all(#predicate, #condition)));
            }
        }
        return Ok(result);
    }
    Ok(Vec::new())
}

pub(super) fn enabled(attrs: &[syn::Attribute], name: &str) -> syn::Result<TokenStream> {
    let mut predicates = Vec::new();
    for attr in attrs {
        predicates.extend(conditions(&attr.meta, name)?);
    }
    Ok(quote!(any(#(#predicates),*)))
}

// Use field bounds instead of derive's generic-parameter bounds. This also
// supports associated key types without requiring unrelated parameters to clone.
pub(super) fn with_clone(item: TokenStream, enabled: &TokenStream) -> TokenStream {
    let input: syn::DeriveInput = syn::parse2(item.clone()).expect("generated type");
    let name = &input.ident;
    // Layout, documentation, and lint attributes belong to the struct, not
    // this implementation. Only conditional compilation must be repeated.
    let attrs = input
        .attrs
        .iter()
        .filter_map(|attr| {
            super::conditional_meta(&attr.meta)
                .expect("validated generated attributes")
                .map(|meta| quote!(#[#meta]))
        })
        .collect::<Vec<_>>();
    let mut generics = input.generics.clone();
    let mut bound_fields = |fields: &syn::Fields| {
        for field in fields {
            let ty = &field.ty;
            generics
                .make_where_clause()
                .predicates
                .push(syn::parse_quote!(#ty: ::std::clone::Clone));
        }
    };
    let body = match &input.data {
        syn::Data::Struct(data) => {
            bound_fields(&data.fields);
            let fields = data
                .fields
                .iter()
                .map(|field| field.ident.as_ref().expect("named field"));
            quote!(Self { #(#fields: ::std::clone::Clone::clone(&self.#fields),)* })
        }
        _ => unreachable!("generated struct"),
    };
    let (impl_generics, args, where_clause) = generics.split_for_impl();
    quote! {
        #item
        #(#attrs)*
        #[cfg(#enabled)]
        impl #impl_generics ::std::clone::Clone for #name #args #where_clause {
            fn clone(&self) -> Self { #body }
        }
    }
}

pub(super) fn debug_impl(
    table: &syn::Ident,
    generics: &syn::Generics,
    row: TokenStream,
    id: &syn::Type,
    cfg: &[TokenStream],
    enabled: &TokenStream,
) -> TokenStream {
    let mut generics = generics.clone();
    generics
        .make_where_clause()
        .predicates
        .push(syn::parse_quote!(#row: ::std::fmt::Debug));
    generics
        .make_where_clause()
        .predicates
        .push(syn::parse_quote!(#id: ::std::fmt::Debug));
    let (impl_generics, args, where_clause) = generics.split_for_impl();
    let name = table.unraw().to_string();
    quote! {
        #(#cfg)*
        #[cfg(#enabled)]
        impl #impl_generics ::std::fmt::Debug for #table #args #where_clause {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.debug_struct(#name).field("rows", &::std::fmt::from_fn(|f| {
                    f.debug_map().entries(self.iter()).finish()
                })).finish()
            }
        }
    }
}
