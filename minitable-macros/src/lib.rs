//! Attribute expansion for `minitable`.
#![forbid(unsafe_code)]

use std::collections::HashSet;

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{Fields, ItemStruct, ext::IdentExt, parse_macro_input, spanned::Spanned};

mod attributes;
use attributes::{IndexMode, IndexOptions, TableOptions};

/// Generate a table from a named-field struct.
///
/// Options: `table = Name`, `index_type = u32`, and optional `id = Type` with
/// `id_from_index = function_or_closure` and `id_to_index = function_or_closure`.
/// Field attributes: `#[indexed(unique)]` or `#[indexed(many)]`.
/// Named struct attributes: `#[index(name = pair, unique, fields(a, b))]` or
/// `#[index(name = derived, many, key_type = String, key = compute_key)]`.
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

enum IndexKey {
    Fields(Vec<(syn::Ident, syn::Type)>),
    Computed {
        ty: Box<syn::Type>,
        expression: Box<syn::Expr>,
    },
}

struct Index {
    name: syn::Ident,
    kind: Kind,
    key: IndexKey,
}

impl Index {
    fn types(&self) -> Vec<&syn::Type> {
        match &self.key {
            IndexKey::Fields(fields) => fields.iter().map(|(_, ty)| ty).collect(),
            IndexKey::Computed { ty, .. } => vec![ty],
        }
    }

    fn key_type(&self) -> TokenStream2 {
        let types = self.types();
        if types.len() == 1 {
            let ty = types[0];
            quote!(#ty)
        } else {
            quote!((#(#types,)*))
        }
    }

    // Tuple references hash identically to owned tuples without cloning components.
    fn borrowed(&self, row: &TokenStream2, computed: &TokenStream2) -> TokenStream2 {
        match &self.key {
            IndexKey::Computed { .. } => quote!(&#computed),
            IndexKey::Fields(fields) if fields.len() == 1 => {
                let field = &fields[0].0;
                quote!(&#row.#field)
            }
            IndexKey::Fields(fields) => {
                let fields = fields.iter().map(|(field, _)| field);
                quote!(&(#(&#row.#fields,)*))
            }
        }
    }

    fn equivalent(&self, row: &TokenStream2, computed: &TokenStream2) -> TokenStream2 {
        match &self.key {
            IndexKey::Fields(fields) if fields.len() > 1 => {
                let comparisons = fields.iter().enumerate().map(|(i, (field, _))| {
                    let i = syn::Index::from(i);
                    quote!(::std::cmp::PartialEq::eq(&__stored_key.#i, &#row.#field))
                });
                quote!(#(#comparisons)&&*)
            }
            _ => {
                let borrowed = self.borrowed(row, computed);
                quote!(::std::cmp::PartialEq::eq(__stored_key, #borrowed))
            }
        }
    }

    fn cloned(&self, row: &TokenStream2, computed: &TokenStream2) -> TokenStream2 {
        match &self.key {
            IndexKey::Fields(fields) if fields.len() > 1 => {
                let fields = fields.iter().map(|(field, _)| field);
                quote!((#(::std::clone::Clone::clone(&#row.#fields),)*))
            }
            _ => {
                let borrowed = self.borrowed(row, computed);
                quote!(::std::clone::Clone::clone(#borrowed))
            }
        }
    }
}

fn parse_index(attr: &syn::Attribute, fields: &syn::FieldsNamed) -> syn::Result<Index> {
    let options: IndexOptions = attributes::parse_attribute(attr)?;
    let name = options.name;
    let kind = options.mode.kind(attr.span())?;
    let key = match (options.fields, options.key_type, options.key) {
        (Some(attributes::FieldList(selected)), None, None) => {
            if selected.is_empty() {
                return Err(syn::Error::new(attr.span(), "fields must not be empty"));
            }
            let mut seen = HashSet::new();
            let mut result = Vec::new();
            for field in selected {
                if !seen.insert(field.unraw().to_string()) {
                    return Err(syn::Error::new(field.span(), "duplicate field in index"));
                }
                let definition = fields
                    .named
                    .iter()
                    .find(|definition| definition.ident.as_ref() == Some(&field))
                    .ok_or_else(|| syn::Error::new(field.span(), "unknown field in index"))?;
                result.push((field, definition.ty.clone()));
            }
            IndexKey::Fields(result)
        }
        (None, Some(ty), Some(expression)) => IndexKey::Computed {
            ty: Box::new(ty),
            expression: Box::new(expression),
        },
        _ => {
            return Err(syn::Error::new(
                attr.span(),
                "specify either fields(...) or both key_type = Type and key = function_or_closure",
            ));
        }
    };
    Ok(Index { name, kind, key })
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
    let options: TableOptions = attributes::parse(args)?;
    let table = options
        .table
        .unwrap_or_else(|| format_ident!("{}Table", name.unraw()));
    let index_type = options
        .index_type
        .unwrap_or_else(|| syn::parse_quote!(usize));
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
    if table == name {
        return Err(syn::Error::new(
            name.span(),
            "row and table names must be distinct",
        ));
    }
    let (id, id_from_index, id_to_index) =
        match (options.id, options.id_from_index, options.id_to_index) {
            (None, None, None) => (
                syn::parse_quote!(#index_type),
                quote!(|index| index),
                quote!(|id| id),
            ),
            (Some(id), Some(from), Some(to)) => (id, quote!(#from), quote!(#to)),
            _ => {
                return Err(syn::Error::new(
                    name.span(),
                    "specify id, id_from_index, and id_to_index together",
                ));
            }
        };
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
                let mode: IndexMode = attributes::parse_attribute(&attr)?;
                kind = Some(mode.kind(attr.span())?);
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
            let field_name = field.ident.clone().unwrap();
            indexes.push(Index {
                name: field_name.clone(),
                kind,
                key: IndexKey::Fields(vec![(field_name, field.ty.clone())]),
            });
        }
    }
    let mut retained = Vec::new();
    for attr in std::mem::take(&mut row.attrs) {
        if attr.path().is_ident("index") {
            indexes.push(parse_index(&attr, fields)?);
        } else {
            retained.push(attr);
        }
    }
    row.attrs = retained;
    let mut index_names = HashSet::new();
    for index in &indexes {
        if !index_names.insert(index.name.unraw().to_string()) {
            return Err(syn::Error::new(index.name.span(), "duplicate index name"));
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
    table_generics
        .make_where_clause()
        .predicates
        .push(syn::parse_quote!(
            #id: ::std::marker::Copy + ::std::cmp::Eq
        ));
    for index in &indexes {
        for ty in index.types() {
            table_generics
                .make_where_clause()
                .predicates
                .push(syn::parse_quote!(
                    #ty: ::std::clone::Clone + ::std::cmp::Eq + ::std::hash::Hash
                ));
        }
    }
    let (impl_generics, type_generics, table_where) = table_generics.split_for_impl();
    let mut map_fields = Vec::new();
    let mut map_defaults = Vec::new();
    let mut link_fields = Vec::new();
    let mut link_defaults = Vec::new();
    let mut computed_fields = Vec::new();
    let mut compute_keys = Vec::new();
    let mut computed_defaults = Vec::new();
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
    for index in &indexes {
        let field = &index.name;
        let ty = index.key_type();
        let kind = &index.kind;
        let map = format_ident!("__index_{}", field.unraw());
        let key = format_ident!("__key_{}", field.unraw());
        let unique_entry = format_ident!("__entry_{}", field.unraw());
        let hash = format_ident!("__hash_{}", field.unraw());
        let links = format_ident!("__links_{}", field.unraw());
        let computed = format_ident!("__computed_{}", field.unraw());
        if let IndexKey::Computed { ty, expression } = &index.key {
            computed_fields.push(quote!(#computed: #ty));
            computed_defaults.push(quote!(#computed));
            compute_keys.push(quote! {
                let #computed: #ty = {
                    let __compute_key: fn(&#name #row_args) -> #ty = #expression;
                    __compute_key(&row)
                };
            });
        }
        let borrowed = index.borrowed(&quote!(row), &quote!(#computed));
        let equivalent = index.equivalent(&quote!(row), &quote!(#computed));
        let cloned = index.cloned(&quote!(row), &quote!(#computed));
        let old_borrowed = index.borrowed(&quote!(old.row), &quote!(old.#computed));
        let old_equivalent = index.equivalent(&quote!(old.row), &quote!(old.#computed));
        let old_entry = quote! {
            let __old_hash = ::std::hash::BuildHasher::hash_one(self.#map.hasher(), #old_borrowed);
            let __old_entry = #runtime::__private::RawEntryApiV1::raw_entry_mut_v1(&mut self.#map)
                .from_hash(__old_hash, |__stored_key| #old_equivalent);
        };
        let remove_old = quote! {
            #old_entry
            match __old_entry {
                #runtime::__private::RawEntryMut::Occupied(entry) => { entry.swap_remove(); }
                #runtime::__private::RawEntryMut::Vacant(_) => ::std::unreachable!("present index"),
            }
        };
        let (query_generics, query_arguments, query_bounds, find) = match &index.key {
            IndexKey::Fields(fields) if fields.len() > 1 => {
                let queries: Vec<_> = (0..fields.len())
                    .map(|i| format_ident!("__MinitableQuery{i}"))
                    .collect();
                let arguments: Vec<_> = (0..fields.len())
                    .map(|i| format_ident!("__value_{i}"))
                    .collect();
                let parameters = queries
                    .iter()
                    .zip(&arguments)
                    .map(|(query, argument)| quote!(#argument: &#query));
                let bounds = fields.iter().zip(&queries).map(|((_, ty), query)| quote!(
                    #ty: ::std::borrow::Borrow<#query>, #query: ::std::hash::Hash + ::std::cmp::Eq
                ));
                let comparisons = queries.iter().zip(&arguments).enumerate().map(|(i, (query, argument))| {
                    let i = syn::Index::from(i);
                    quote!(::std::borrow::Borrow::<#query>::borrow(&__stored_key.#i) == #argument)
                });
                (
                    quote!(#(#queries: ?::std::marker::Sized),*),
                    quote!(#(#parameters),*),
                    quote!(#(#bounds),*),
                    quote!({
                        let __hash = ::std::hash::BuildHasher::hash_one(self.#map.hasher(), (#(#arguments,)*));
                        #runtime::__private::RawEntryApiV1::raw_entry_v1(&self.#map)
                            .from_hash(__hash, |__stored_key| #(#comparisons)&&*)
                            .map(|(_, id)| *id)
                    }),
                )
            }
            _ => (
                quote!(__MinitableQuery: ?::std::marker::Sized),
                quote!(value: &__MinitableQuery),
                quote!(#ty: ::std::borrow::Borrow<__MinitableQuery>, __MinitableQuery: ::std::hash::Hash + ::std::cmp::Eq),
                quote!(self.#map.get(value).copied()),
            ),
        };
        map_fields.push(quote!(#map: #runtime::__private::IndexMap<#ty, #id>));
        map_defaults.push(quote!(#map: ::std::default::Default::default()));
        match kind {
            Kind::Unique => {
                checks_insert.push(quote! {
                    let #hash = ::std::hash::BuildHasher::hash_one(self.#map.hasher(), #borrowed);
                    let #unique_entry = match #runtime::__private::RawEntryApiV1::raw_entry_mut_v1(&mut self.#map).from_hash(#hash, |__stored_key| #equivalent) {
                        #runtime::__private::RawEntryMut::Vacant(entry) => entry,
                        #runtime::__private::RawEntryMut::Occupied(_) => {
                            return ::std::result::Result::Err(#runtime::InsertError { row, kind: #runtime::InsertErrorKind::Unique { field: ::std::stringify!(#field) } });
                        }
                    };
                });
                checks_replace.push(quote! {
                    let #hash = ::std::hash::BuildHasher::hash_one(self.#map.hasher(), #borrowed);
                    let #unique_entry = match #runtime::__private::RawEntryApiV1::raw_entry_mut_v1(&mut self.#map).from_hash(#hash, |__stored_key| #equivalent) {
                        #runtime::__private::RawEntryMut::Occupied(entry) if *entry.get() != id => {
                            return ::std::result::Result::Err(#runtime::ReplaceError { row, kind: #runtime::ReplaceErrorKind::Unique { field: ::std::stringify!(#field) } });
                        }
                        entry => entry,
                    };
                });
                clone_insert_keys.push(quote!(let #key = #cloned;));
                clone_replace_keys.push(quote! {
                    let #key = match &#unique_entry {
                        #runtime::__private::RawEntryMut::Vacant(_) => ::std::option::Option::Some(#cloned),
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
                            #remove_old
                        }
                    }
                });
                detach.push(remove_old);
                let method = format_ident!("get_by_{}", field.unraw());
                lookups.push(quote! {
                    /// Find a row through a unique index. Supports borrowed keys such as `str`.
                    #vis fn #method<#query_generics>(&self, #query_arguments) -> ::std::option::Option<(#id, &#name #row_args)>
                    where #query_bounds {
                        let id = #find?;
                        self.get(id).map(|row| (id, row))
                    }
                });
            }
            Kind::Many => {
                let clone_key = quote!(let #key = #cloned;);
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
                    } else {
                        #old_entry
                        match __old_entry {
                            #runtime::__private::RawEntryMut::Occupied(mut entry) => {
                                if let ::std::option::Option::Some(next) = old.#links.next {
                                    *entry.get_mut() = next;
                                } else {
                                    entry.swap_remove();
                                }
                            }
                            #runtime::__private::RawEntryMut::Vacant(_) => ::std::unreachable!("present index"),
                        }
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
                    #vis fn #method<#query_generics>(&self, #query_arguments) -> impl ::std::iter::Iterator<Item = (#id, &#name #row_args)> + '_
                    where #query_bounds {
                        let mut next = #find;
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
    Ok(quote! {
        #row

        #(#cfg)*
        struct #entry #original_generics #row_where {
            row: #name #row_args,
            #(#link_fields,)*
            #(#computed_fields,)*
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
                Self {
                    __rows: #runtime::__private::Arena::new(
                        |index| {
                            let index = <#index_type as ::std::convert::TryFrom<usize>>::try_from(index).ok()?;
                            let convert: fn(#index_type) -> #id = #id_from_index;
                            ::std::option::Option::Some(convert(index))
                        },
                        |id| {
                            let convert: fn(#id) -> #index_type = #id_to_index;
                            <usize as ::std::convert::TryFrom<#index_type>>::try_from(convert(id)).ok()
                        },
                    ),
                    #(#map_defaults,)*
                }
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
            /// Get a row by ID. Removed IDs can refer to rows inserted into reused slots.
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
                #(#compute_keys)*
                #(#checks_insert)*
                if !self.__rows.can_insert() {
                    return ::std::result::Result::Err(#runtime::InsertError { row, kind: #runtime::InsertErrorKind::CapacityExhausted });
                }
                #(#clone_insert_keys)*
                let entry = #entry { row, #(#link_defaults,)* #(#computed_defaults,)* };
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
                #(#compute_keys)*
                #(#checks_replace)*
                #(#clone_replace_keys)*
                let entry = #entry { row, #(#link_defaults,)* #(#computed_defaults,)* };
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
                "Unknown field: `wrong`",
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
                quote!(index_type = u8, index_type = u16),
                quote!(
                    struct Row {}
                ),
                "Duplicate field `index_type`",
            ),
            (
                quote!(unknown = true),
                quote!(
                    struct Row {}
                ),
                "Unknown field: `unknown`",
            ),
            (
                quote!(),
                quote!(
                    #[index(unique, fields(a))]
                    struct Row {
                        a: u32,
                    }
                ),
                "Missing field `name`",
            ),
            (
                quote!(),
                quote!(
                    #[index(name = pair, fields(a))]
                    struct Row {
                        a: u32,
                    }
                ),
                "requires unique or many",
            ),
            (
                quote!(),
                quote!(
                    #[index(name = pair, unique, many, fields(a))]
                    struct Row {
                        a: u32,
                    }
                ),
                "either unique or many",
            ),
            (
                quote!(),
                quote!(
                    #[index(name = pair, unique, fields())]
                    struct Row {}
                ),
                "must not be empty",
            ),
            (
                quote!(),
                quote!(
                    #[index(name = pair, unique, fields(missing))]
                    struct Row {
                        a: u32,
                    }
                ),
                "unknown field",
            ),
            (
                quote!(),
                quote!(
                    #[index(name = pair, unique, fields(a, a))]
                    struct Row {
                        a: u32,
                    }
                ),
                "duplicate field",
            ),
            (
                quote!(),
                quote!(
                    #[index(name = pair, unique, key = compute)]
                    struct Row {}
                ),
                "both key_type",
            ),
            (
                quote!(),
                quote!(
                    #[index(name = pair, unique, fields(a), key_type = u32, key = compute)]
                    struct Row {
                        a: u32,
                    }
                ),
                "either fields",
            ),
            (
                quote!(),
                quote!(
                    #[index(name = a, many, fields(a))]
                    struct Row {
                        #[indexed(unique)]
                        a: u32,
                    }
                ),
                "duplicate index name",
            ),
            (
                quote!(),
                quote!(
                    #[index(name = a, name = b, unique, fields(a))]
                    struct Row {
                        a: u32,
                    }
                ),
                "Duplicate field `name`",
            ),
            (
                quote!(),
                quote!(
                    #[index(name = a, unique, unknown)]
                    struct Row {
                        a: u32,
                    }
                ),
                "Unknown field: `unknown`",
            ),
        ] {
            let error = expand(args, syn::parse2(item).unwrap()).unwrap_err();
            assert!(error.to_string().contains(message), "{error}");
        }
    }
}
