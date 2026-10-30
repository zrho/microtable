//! Attribute expansion for `minitable`.
#![forbid(unsafe_code)]

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{Fields, ItemStruct, ext::IdentExt, parse::Parser, parse_macro_input, spanned::Spanned};

/// Generate a table and a typed row ID from a named-field struct.
///
/// Options: `table = Name`, `id = Name`, `index_type = u32`, and
/// `generational = false`. Field attributes: `#[indexed(unique)]` or `#[indexed(many)]`.
#[proc_macro_attribute]
pub fn table(args: TokenStream, input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as ItemStruct);
    expand(args.into(), input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

#[derive(Clone, Copy)]
enum Kind {
    Unique,
    Many,
}

// Only propagate conditional compilation, not derives or representation attributes.
// cfg_attr can mix both, and can nest other cfg_attr attributes.
fn conditional_meta(meta: &syn::Meta) -> syn::Result<Option<syn::Meta>> {
    if meta.path().is_ident("cfg") {
        return Ok(Some(meta.clone()));
    }
    if !meta.path().is_ident("cfg_attr") {
        return Ok(None);
    }
    let syn::Meta::List(list) = meta else {
        return Err(syn::Error::new(meta.span(), "expected cfg_attr arguments"));
    };
    let arguments = list.parse_args_with(
        syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
    )?;
    let mut arguments = arguments.iter();
    let predicate = arguments
        .next()
        .ok_or_else(|| syn::Error::new(meta.span(), "expected cfg_attr predicate"))?;
    let mut attributes = Vec::new();
    for argument in arguments {
        if let Some(attribute) = conditional_meta(argument)? {
            attributes.push(attribute);
        }
    }
    if attributes.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        syn::parse_quote!(cfg_attr(#predicate, #(#attributes),*)),
    ))
}

fn expand(args: TokenStream2, mut row: ItemStruct) -> syn::Result<TokenStream2> {
    let name = row.ident.clone();
    let vis = &row.vis;
    let mut table = format_ident!("{}Table", name.unraw());
    let mut id = format_ident!("{}Id", name.unraw());
    let mut index_type: syn::Ident = syn::parse_quote!(usize);
    let mut generational = true;
    let mut seen = std::collections::HashSet::new();
    syn::meta::parser(|meta| {
        let key = meta
            .path
            .get_ident()
            .ok_or_else(|| meta.error("expected an option name"))?
            .to_string();
        if !seen.insert(key.clone()) {
            return Err(meta.error("duplicate table option"));
        }
        match key.as_str() {
            "table" => table = meta.value()?.parse()?,
            "id" => id = meta.value()?.parse()?,
            "index_type" => index_type = meta.value()?.parse()?,
            "generational" => generational = meta.value()?.parse::<syn::LitBool>()?.value,
            _ => return Err(meta.error("expected table, id, index_type, or generational")),
        }
        Ok(())
    })
    .parse2(args)?;
    if !matches!(
        index_type.to_string().as_str(),
        "u8" | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
    ) {
        return Err(syn::Error::new(
            index_type.span(),
            "index_type must be a primitive integer type",
        ));
    }
    if table == name || id == name || table == id {
        return Err(syn::Error::new(
            name.span(),
            "row, table, and ID names must be distinct",
        ));
    }
    let Fields::Named(fields) = &mut row.fields else {
        return Err(syn::Error::new(
            row.span(),
            "table requires a named-field struct",
        ));
    };
    let mut indexes = Vec::new();
    for field in &mut fields.named {
        let mut kind = None;
        let mut retained = Vec::new();
        for attr in std::mem::take(&mut field.attrs) {
            if attr.path().is_ident("indexed") {
                if kind.is_some() {
                    return Err(syn::Error::new(attr.span(), "duplicate indexed attribute"));
                }
                let mode: syn::Ident = attr.parse_args()?;
                kind = Some(match mode.to_string().as_str() {
                    "unique" => Kind::Unique,
                    "many" => Kind::Many,
                    _ => return Err(syn::Error::new(mode.span(), "expected unique or many")),
                });
            } else {
                if attr.path().is_ident("cfg") || attr.path().is_ident("cfg_attr") {
                    return Err(syn::Error::new(
                        attr.span(),
                        "conditionally compiled fields are not supported",
                    ));
                }
                retained.push(attr);
            }
        }
        field.attrs = retained;
        if let Some(kind) = kind {
            indexes.push((field.ident.clone().unwrap(), field.ty.clone(), kind));
        }
    }
    let runtime = match proc_macro_crate::crate_name("minitable") {
        Ok(proc_macro_crate::FoundCrate::Itself) => quote!(::minitable),
        Ok(proc_macro_crate::FoundCrate::Name(name)) => {
            let ident = format_ident!("{}", name);
            quote!(::#ident)
        }
        Err(err) => return Err(syn::Error::new(name.span(), err.to_string())),
    };
    let entry = format_ident!("__{}Entry", table.unraw());
    let mut cfg = Vec::new();
    for attr in &row.attrs {
        if let Some(meta) = conditional_meta(&attr.meta)? {
            cfg.push(quote!(#[#meta]));
        }
    }
    let original_generics = &row.generics;
    let (_, row_args, row_where) = original_generics.split_for_impl();
    let mut table_generics = row.generics.clone();
    for (_, ty, _) in &indexes {
        table_generics
            .make_where_clause()
            .predicates
            .push(syn::parse_quote!(
                #ty: ::std::clone::Clone + ::std::cmp::Eq + ::std::hash::Hash
            ));
    }
    let (impl_generics, type_generics, table_where) = table_generics.split_for_impl();
    let mut map_fields = Vec::new();
    let mut map_defaults = Vec::new();
    let mut link_fields = Vec::new();
    let mut link_defaults = Vec::new();
    let mut checks_insert = Vec::new();
    let mut checks_replace = Vec::new();
    let mut clone_insert_keys = Vec::new();
    let mut clone_replace_keys = Vec::new();
    let mut commit_insert = Vec::new();
    let mut commit_replace = Vec::new();
    let mut attach = Vec::new();
    let mut detach = Vec::new();
    let mut detach_many = Vec::new();
    let mut lookups = Vec::new();
    for (field, ty, kind) in &indexes {
        let map = format_ident!("__index_{}", field.unraw());
        let key = format_ident!("__key_{}", field.unraw());
        let unique_entry = format_ident!("__entry_{}", field.unraw());
        let hash = format_ident!("__hash_{}", field.unraw());
        let links = format_ident!("__links_{}", field.unraw());
        map_fields.push(quote!(#map: #runtime::__private::HashMap<#ty, #id>));
        map_defaults.push(quote!(#map: ::std::default::Default::default()));
        match kind {
            Kind::Unique => {
                checks_insert.push(quote! {
                    let #hash = ::std::hash::BuildHasher::hash_one(self.#map.hasher(), &row.#field);
                    let #unique_entry = match self.#map.raw_entry_mut().from_key_hashed_nocheck(#hash, &row.#field) {
                        #runtime::__private::RawEntryMut::Vacant(entry) => entry,
                        #runtime::__private::RawEntryMut::Occupied(_) => {
                            return ::std::result::Result::Err(#runtime::InsertError { row, kind: #runtime::InsertErrorKind::Unique { field: ::std::stringify!(#field) } });
                        }
                    };
                });
                checks_replace.push(quote! {
                    let #hash = ::std::hash::BuildHasher::hash_one(self.#map.hasher(), &row.#field);
                    let #unique_entry = match self.#map.raw_entry_mut().from_key_hashed_nocheck(#hash, &row.#field) {
                        #runtime::__private::RawEntryMut::Occupied(entry) if *entry.get() != id => {
                            return ::std::result::Result::Err(#runtime::ReplaceError { row, kind: #runtime::ReplaceErrorKind::Unique { field: ::std::stringify!(#field) } });
                        }
                        entry => entry,
                    };
                });
                clone_insert_keys.push(quote!(let #key = ::std::clone::Clone::clone(&row.#field);));
                clone_replace_keys.push(quote! {
                    let #key = match &#unique_entry {
                        #runtime::__private::RawEntryMut::Vacant(_) => ::std::option::Option::Some(::std::clone::Clone::clone(&row.#field)),
                        #runtime::__private::RawEntryMut::Occupied(_) => ::std::option::Option::None,
                    };
                });
                // Reuse the hash from validation. Cloned keys must preserve Eq/Hash,
                // as required for any Clone + Eq + Hash map key.
                commit_insert.push(quote!(#unique_entry.insert_hashed_nocheck(#hash, #key, id);));
                commit_replace.push(quote! {
                    match #unique_entry {
                        // An occupied entry was already validated to belong to this row.
                        #runtime::__private::RawEntryMut::Occupied(_) => {}
                        #runtime::__private::RawEntryMut::Vacant(entry) => {
                            entry.insert_hashed_nocheck(#hash, #key.expect("cloned vacant key"), id);
                            self.#map.remove(&old.row.#field);
                        }
                    }
                });
                detach.push(quote!(self.#map.remove(&old.row.#field);));
                let method = format_ident!("get_by_{}", field.unraw());
                lookups.push(quote! {
                    /// Find a row through a unique index. Supports borrowed keys such as `str`.
                    #vis fn #method<__MinitableQuery: ?::std::marker::Sized>(&self, value: &__MinitableQuery) -> ::std::option::Option<(#id, &#name #row_args)>
                    where #ty: ::std::borrow::Borrow<__MinitableQuery>, __MinitableQuery: ::std::hash::Hash + ::std::cmp::Eq {
                        let id = *self.#map.get(value)?;
                        self.get(id).map(|row| (id, row))
                    }
                });
            }
            Kind::Many => {
                let clone_key = quote!(let #key = ::std::clone::Clone::clone(&row.#field););
                clone_insert_keys.push(clone_key.clone());
                clone_replace_keys.push(clone_key);
                link_fields.push(quote!(#links: #runtime::__private::Links<#id>));
                link_defaults.push(quote!(#links: ::std::default::Default::default()));
                attach.push(quote! {
                    let head = self.#map.insert(#key, id);
                    self.__rows.get_mut(id).expect("present row").#links.next = head;
                    if let ::std::option::Option::Some(head) = head {
                        self.__rows.get_mut(head).expect("present head").#links.prev = ::std::option::Option::Some(id);
                    }
                });
                let unlink = quote! {
                    if let ::std::option::Option::Some(prev) = old.#links.prev {
                        self.__rows.get_mut(prev).expect("present predecessor").#links.next = old.#links.next;
                    } else if let ::std::option::Option::Some(next) = old.#links.next {
                        *self.#map.get_mut(&old.row.#field).expect("present index") = next;
                    } else {
                        self.#map.remove(&old.row.#field);
                    }
                    if let ::std::option::Option::Some(next) = old.#links.next {
                        self.__rows.get_mut(next).expect("present successor").#links.prev = old.#links.prev;
                    }
                };
                detach.push(unlink.clone());
                detach_many.push(unlink);
                let method = format_ident!("iter_by_{}", field.unraw());
                lookups.push(quote! {
                    /// Iterate over matching rows in unspecified order using the intrusive list.
                    #vis fn #method<__MinitableQuery: ?::std::marker::Sized>(&self, value: &__MinitableQuery) -> impl ::std::iter::Iterator<Item = (#id, &#name #row_args)> + '_
                    where #ty: ::std::borrow::Borrow<__MinitableQuery>, __MinitableQuery: ::std::hash::Hash + ::std::cmp::Eq {
                        let mut next = self.#map.get(value).copied();
                        ::std::iter::from_fn(move || {
                            let id = next?;
                            let entry = self.__rows.get(id).expect("present indexed row");
                            next = entry.#links.next;
                            ::std::option::Option::Some((id, &entry.row))
                        })
                    }
                });
            }
        }
    }
    let generation_field = generational.then(|| quote!(generation: u64,));
    let generation_init = generational.then(|| quote!(generation,));
    let generation_read = if generational {
        quote!(self.generation)
    } else {
        quote!(0)
    };
    let generation_method = generational.then(|| {
        quote! {
            /// The generation of this slot. Changes whenever a slot is reused.
            #vis fn generation(self) -> u64 { self.generation }
        }
    });
    Ok(quote! {
        #row

        #(#cfg)*
        #[doc = "Typed row ID. IDs are local to a table instance; they do not identify that instance."]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #vis struct #id { index: #index_type, #generation_field }

        #(#cfg)*
        impl #id {
            /// The backing slot index.
            #vis fn index(self) -> #index_type { self.index }
            #generation_method
        }

        #(#cfg)*
        impl #runtime::__private::RowId for #id {
            const GENERATIONAL: bool = #generational;
            fn from_parts(index: usize, generation: u64) -> ::std::option::Option<Self> {
                let _ = generation;
                ::std::option::Option::Some(Self { index: <#index_type as ::std::convert::TryFrom<usize>>::try_from(index).ok()?, #generation_init })
            }
            fn index(self) -> usize { self.index as usize }
            fn generation(self) -> u64 { #generation_read }
        }

        #(#cfg)*
        struct #entry #original_generics #row_where {
            row: #name #row_args,
            #(#link_fields,)*
        }

        #(#cfg)*
        #[doc = "A table with stable row IDs and generated indexes. Mutation is through checked replacement."]
        #vis struct #table #table_generics #table_where {
            __rows: #runtime::__private::Arena<#entry #row_args, #id>,
            #(#map_fields,)*
        }

        #(#cfg)*
        impl #impl_generics ::std::default::Default for #table #type_generics #table_where {
            fn default() -> Self {
                Self { __rows: ::std::default::Default::default(), #(#map_defaults,)* }
            }
        }

        #(#cfg)*
        impl #impl_generics #table #type_generics #table_where {
            /// Create an empty table.
            #vis fn new() -> Self { ::std::default::Default::default() }
            /// Number of live rows.
            #vis fn len(&self) -> usize { self.__rows.len() }
            /// Whether the table contains no rows.
            #vis fn is_empty(&self) -> bool { self.len() == 0 }
            /// Get a row by ID. Stale generational IDs return `None`.
            #vis fn get(&self, id: #id) -> ::std::option::Option<&#name #row_args> {
                self.__rows.get(id).map(|entry| &entry.row)
            }
            /// Whether this ID currently identifies a row.
            #vis fn contains(&self, id: #id) -> bool { self.get(id).is_some() }
            /// Iterate over live rows in unspecified order. Cost is proportional to allocated slots.
            #vis fn iter(&self) -> impl ::std::iter::Iterator<Item = (#id, &#name #row_args)> + '_ {
                self.__rows.iter().map(|(id, entry)| (id, &entry.row))
            }
            /// Insert a row after checking every unique index. Errors preserve the row.
            #vis fn insert(&mut self, row: #name #row_args) -> ::std::result::Result<#id, #runtime::InsertError<#name #row_args>> {
                #(#checks_insert)*
                if !self.__rows.can_insert() {
                    return ::std::result::Result::Err(#runtime::InsertError { row, kind: #runtime::InsertErrorKind::CapacityExhausted });
                }
                #(#clone_insert_keys)*
                let entry = #entry { row, #(#link_defaults,)* };
                let id = match self.__rows.insert(entry) {
                    ::std::result::Result::Ok(id) => id,
                    ::std::result::Result::Err(entry) => return ::std::result::Result::Err(#runtime::InsertError { row: entry.row, kind: #runtime::InsertErrorKind::CapacityExhausted }),
                };
                #(#commit_insert)*
                #(#attach)*
                ::std::result::Result::Ok(id)
            }
            /// Replace a row without changing its ID, returning the previous row.
            /// Constraint failures leave the table unchanged and return the rejected row.
            #vis fn replace(&mut self, id: #id, row: #name #row_args) -> ::std::result::Result<#name #row_args, #runtime::ReplaceError<#name #row_args>> {
                if !self.contains(id) {
                    return ::std::result::Result::Err(#runtime::ReplaceError { row, kind: #runtime::ReplaceErrorKind::NotFound });
                }
                #(#checks_replace)*
                #(#clone_replace_keys)*
                let entry = #entry { row, #(#link_defaults,)* };
                let old = ::std::mem::replace(self.__rows.get_mut(id).expect("present row"), entry);
                #(#commit_replace)*
                #(#detach_many)*
                #(#attach)*
                ::std::result::Result::Ok(old.row)
            }
            /// Remove a row and its index entries. Other row IDs remain unchanged.
            #vis fn remove(&mut self, id: #id) -> ::std::option::Option<#name #row_args> {
                let old = self.__rows.remove(id)?;
                self.__detach(&old);
                ::std::option::Option::Some(old.row)
            }
            fn __detach(&mut self, old: &#entry #row_args) {
                let _ = old;
                #(#detach)*
            }
            #(#lookups)*
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_declarations_have_diagnostics() {
        for (args, item, message) in [
            (
                quote!(),
                quote!(
                    struct Row(u32);
                ),
                "named-field",
            ),
            (
                quote!(),
                quote!(
                    struct Row {
                        #[indexed(wrong)]
                        key: u32,
                    }
                ),
                "unique or many",
            ),
            (
                quote!(),
                quote!(
                    struct Row {
                        #[indexed(unique)]
                        #[indexed(many)]
                        key: u32,
                    }
                ),
                "duplicate indexed",
            ),
            (
                quote!(index_type = bool),
                quote!(
                    struct Row {}
                ),
                "primitive integer",
            ),
            (
                quote!(generational = true, generational = false),
                quote!(
                    struct Row {}
                ),
                "duplicate table option",
            ),
            (
                quote!(unknown = true),
                quote!(
                    struct Row {}
                ),
                "expected table",
            ),
        ] {
            let error = expand(args, syn::parse2(item).unwrap()).unwrap_err();
            assert!(error.to_string().contains(message), "{error}");
        }
    }
}
