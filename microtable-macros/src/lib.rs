//! Attribute expansion for `microtable`.
#![forbid(unsafe_code)]

use std::collections::HashSet;

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{Fields, ItemStruct, ext::IdentExt, parse_macro_input, spanned::Spanned};

mod attributes;
mod enums;
mod indexes;
use attributes::{IndexMode, IndexOptions, TableOptions};

/// Generate a table from a named-field struct or singleton-tuple-variant enum.
///
/// Options: `table = Name` and optional `id = Type` with
/// `id_from_index = function_or_closure` and `id_to_index = function_or_closure`.
/// IDs default to `usize`. Custom ID callbacks convert directly to/from `usize`.
/// `hasher = Type` selects a `BuildHasher + Clone + Default` hash builder,
/// defaulting to `std::collections::hash_map::RandomState`.
/// `append_only` selects dense storage and omits removal and replacement methods.
/// Field attributes: `#[indexed(unique)]` or `#[indexed(many)]`.
/// Named struct attributes: `#[index(name = pair, unique, fields(a, b))]` or
/// `#[index(name = derived, many, key_type = String, key = compute_key)]`.
/// Both index forms accept `cached` or `cached = false`. Field/composite keys
/// default to non-cached storage; computed keys default to cached storage.
#[proc_macro_attribute]
pub fn table(args: TokenStream, input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as syn::Item);
    let result = match input {
        syn::Item::Struct(row) => expand(args.into(), row),
        syn::Item::Enum(row) => enums::expand(args.into(), row),
        item => Err(syn::Error::new(
            item.span(),
            "table requires a struct or enum",
        )),
    };
    result.unwrap_or_else(syn::Error::into_compile_error).into()
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
    lookup_name: syn::Ident,
    kind: Kind,
    key: IndexKey,
    cached: bool,
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
    let cached = options
        .mode
        .cached
        .unwrap_or(matches!(key, IndexKey::Computed { .. }));
    Ok(Index {
        lookup_name: name.clone(),
        name,
        kind,
        key,
        cached,
    })
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
    let append_only = options.append_only;
    let hasher: syn::Type = options
        .hasher
        .unwrap_or_else(|| syn::parse_quote!(::std::collections::hash_map::RandomState));
    let table = options
        .table
        .unwrap_or_else(|| format_ident!("{}Table", name.unraw()));
    if table == name {
        return Err(syn::Error::new(
            name.span(),
            "row and table names must be distinct",
        ));
    }
    let (id, id_from_index, id_to_index) =
        match (options.id, options.id_from_index, options.id_to_index) {
            (None, None, None) => (
                syn::parse_quote!(usize),
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
        let mut cached = false;
        let mut retained = Vec::new();
        for attr in std::mem::take(&mut field.attrs) {
            if attr.path().is_ident("indexed") {
                if kind.is_some() {
                    return Err(syn::Error::new(attr.span(), "duplicate indexed attribute"));
                }
                let mode: IndexMode = attributes::parse_attribute(&attr)?;
                kind = Some(mode.kind(attr.span())?);
                cached = mode.cached.unwrap_or(false);
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
                lookup_name: field_name.clone(),
                kind,
                key: IndexKey::Fields(vec![(field_name, field.ty.clone())]),
                cached,
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
    let runtime = match proc_macro_crate::crate_name("microtable") {
        Ok(proc_macro_crate::FoundCrate::Itself) => quote!(::microtable),
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
            #hasher: ::std::hash::BuildHasher + ::std::clone::Clone + ::std::default::Default
        ));
    table_generics
        .make_where_clause()
        .predicates
        .push(syn::parse_quote!(
            #id: ::std::marker::Copy + ::std::cmp::Eq
        ));
    for index in &indexes {
        for ty in index.types() {
            let clone_bound = (index.cached && matches!(index.key, IndexKey::Fields(_)))
                .then(|| quote!(::std::clone::Clone+));
            table_generics
                .make_where_clause()
                .predicates
                .push(syn::parse_quote!(
                    #ty: #clone_bound ::std::cmp::Eq + ::std::hash::Hash
                ));
        }
    }
    let (impl_generics, type_generics, table_where) = table_generics.split_for_impl();
    let indexes::Code {
        map_fields,
        map_defaults,
        entry_fields,
        entry_defaults,
        prepare,
        checks_insert,
        checks_replace,
        reserve,
        groups_insert,
        keys_insert,
        keys_replace,
        detach_replace,
        detach_remove,
        commit,
        lookups,
    } = indexes::Code::generate(
        &indexes,
        &indexes::Context {
            runtime: &runtime,
            name: &name,
            row_args: quote!(#row_args),
            id: &id,
            hasher: &hasher,
            vis,
            append_only,
            variant: None,
        },
    );
    let arena = if append_only {
        quote!(#runtime::__private::AppendArena)
    } else {
        quote!(#runtime::__private::Arena)
    };
    let next_id = append_only.then(|| quote!(let id = self.__rows.next_id();));
    let capacity_check = quote! {
        ::std::assert!(self.__rows.can_insert(), "table capacity exhausted");
        #next_id
    };
    let reserve_rows = append_only.then(|| quote!(self.__rows.reserve(1);));
    let insert_row = if append_only {
        quote!(self.__rows.push(entry);)
    } else {
        quote!(let id = self.__rows.insert(entry);)
    };
    let mutations = (!append_only).then(|| quote! {
        /// Replace a row without changing its ID, returning the previous row.
        /// Constraint failures leave the table unchanged and return the rejected row.
        #vis fn replace(&mut self, id: #id, row: #name #row_args) -> ::std::result::Result<#name #row_args, #runtime::ReplaceError<#name #row_args>> {
            if !self.contains(id) {
                return ::std::result::Result::Err(#runtime::ReplaceError { row, kind: #runtime::ReplaceErrorKind::NotFound });
            }
            #(#prepare)*
            #(#checks_replace)*
            #(#keys_replace)*
            #(#reserve)*
            #(#detach_replace)*
            let entry = #entry { row, #(#entry_defaults,)* };
            let old = ::std::mem::replace(self.__rows.get_mut(id).expect("present row"), entry);
            #(#commit)*
            ::std::result::Result::Ok(old.row)
        }
        /// Remove a row and its index entries. Other row IDs remain unchanged.
        #vis fn remove(&mut self, id: #id) -> ::std::option::Option<#name #row_args> {
            if !self.contains(id) {
                return ::std::option::Option::None;
            }
            #(#detach_remove)*
            self.__rows.remove(id).map(|entry| entry.row)
        }
    });
    let table_doc = if append_only {
        "An append-only table with stable row IDs and generated indexes."
    } else {
        "A table with stable row IDs and generated indexes. Mutation is through checked replacement."
    };
    let get_doc = if append_only {
        "Get a row by ID. IDs remain valid for the lifetime of the table."
    } else {
        "Get a row by ID. Removed IDs can refer to rows inserted into reused slots."
    };
    Ok(quote! {
        #row

        #(#cfg)*
        struct #entry #original_generics #row_where {
            row: #name #row_args,
            #(#entry_fields,)*
        }

        #(#cfg)*
        #[doc = #table_doc]
        #vis struct #table #table_generics #table_where {
            __rows: #arena<#entry #row_args, #id>,
            __hasher: #hasher,
            #(#map_fields,)*
        }

        #(#cfg)*
        impl #impl_generics ::std::default::Default for #table #type_generics #table_where {
            fn default() -> Self {
                let __hasher: #hasher = ::std::default::Default::default();
                Self {
                    __rows: #arena::new(#id_from_index, #id_to_index),
                    #(#map_defaults,)*
                    __hasher,
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
            #[doc = #get_doc]
            #vis fn get(&self, id: #id) -> ::std::option::Option<&#name #row_args> {
                self.__rows.get(id).map(|entry| &entry.row)
            }
            /// Whether this ID currently identifies a row.
            #vis fn contains(&self, id: #id) -> bool { self.get(id).is_some() }
            /// Iterate over live rows in unspecified order. Cost is proportional to allocated slots.
            #vis fn iter(&self) -> impl ::std::iter::Iterator<Item = (#id, &#name #row_args)> + '_ {
                self.__rows.iter().map(|(id, entry)| (id, &entry.row))
            }
            /// Insert a row, returning its ID or the rejected row and first conflicting ID.
            /// Unique conflicts leave the table unchanged.
            /// Panics if row storage capacity is exhausted.
            #vis fn insert(&mut self, row: #name #row_args) -> ::std::result::Result<#id, (#name #row_args, #id)> {
                #(#prepare)*
                #(#checks_insert)*
                #capacity_check
                #(#groups_insert)*
                #(#keys_insert)*
                #reserve_rows
                #(#reserve)*
                let entry = #entry { row, #(#entry_defaults,)* };
                #insert_row
                #(#commit)*
                ::std::result::Result::Ok(id)
            }
            #mutations
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
                "Unknown field: `index_type`",
            ),
            (
                quote!(table = First, table = Second),
                quote!(
                    struct Row {}
                ),
                "Duplicate field `table`",
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
        for args in [
            quote!(id = Id),
            quote!(id_from_index = Id::new),
            quote!(id_to_index = Id::index),
            quote!(id = Id, id_from_index = Id::new),
            quote!(id = Id, id_to_index = Id::index),
            quote!(id_from_index = Id::new, id_to_index = Id::index),
        ] {
            let item = syn::parse_quote!(
                struct Row {}
            );
            let error = expand(args.clone(), item).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("specify id, id_from_index, and id_to_index together")
            );
            let item = syn::parse_quote!(
                enum Row {
                    Value(()),
                }
            );
            let error = enums::expand(args, item).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("specify id, id_from_index, and id_to_index together")
            );
        }
        let item = syn::parse_quote!(
            enum Row {
                Value(()),
            }
        );
        let error = enums::expand(quote!(index_type = u8), item).unwrap_err();
        assert!(error.to_string().contains("Unknown field: `index_type`"));
        for item in [
            quote!(
                enum Row {
                    Record { key: u32 },
                }
            ),
            quote!(
                enum Row {
                    Unit,
                }
            ),
            quote!(
                enum Row {
                    Empty(),
                }
            ),
            quote!(
                enum Row {
                    Pair(u32, u32),
                }
            ),
            quote!(
                enum Row {
                    #[index(name = key, unique, fields(key))]
                    Value(u32),
                }
            ),
            quote!(
                enum Row {
                    #[index(name = key, unique, key = compute)]
                    Value(u32),
                }
            ),
            quote!(
                #[index(name = key, unique, key_type = u32, key = compute)]
                enum Row {
                    Value(u32),
                }
            ),
            quote!(
                enum Row {
                    Value(#[indexed(unique)] u32),
                }
            ),
            quote!(
                enum Row {
                    #[cfg(any())]
                    Value(u32),
                }
            ),
        ] {
            assert!(enums::expand(quote!(), syn::parse2(item).unwrap()).is_err());
        }
    }
}
