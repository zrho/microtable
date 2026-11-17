//! Generate a shared table for enum payloads with variant-local indexes.

use std::collections::HashSet;

use heck::ToSnakeCase;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{ext::IdentExt, spanned::Spanned, visit_mut::VisitMut};

use super::{Index, IndexKey, attributes, indexes};

pub(super) fn expand(args: TokenStream2, mut row: syn::ItemEnum) -> syn::Result<TokenStream2> {
    let name = row.ident.clone();
    let vis = &row.vis;
    if row.variants.is_empty() {
        return Err(syn::Error::new(
            name.span(),
            "table enums require at least one variant",
        ));
    }
    let options: attributes::TableOptions = attributes::parse(args)?;
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
    let (id, id_from_index, id_to_index): (syn::Type, _, _) =
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
    for attr in &row.attrs {
        if attr.path().is_ident("index") || attr.path().is_ident("indexed") {
            return Err(syn::Error::new(
                attr.span(),
                "enum indexes must be declared with #[index(...)] on a variant",
            ));
        }
    }
    let mut all_indexes = Vec::new();
    let mut names = HashSet::new();
    for variant in &mut row.variants {
        let syn::Fields::Unnamed(fields) = &variant.fields else {
            return Err(syn::Error::new(
                variant.span(),
                "table enum variants require exactly one unnamed payload field",
            ));
        };
        if fields.unnamed.len() != 1 {
            return Err(syn::Error::new(
                fields.span(),
                "table enum variants require exactly one unnamed payload field",
            ));
        }
        for attr in &fields.unnamed[0].attrs {
            reject_cfg(attr)?;
            if attr.path().is_ident("index") || attr.path().is_ident("indexed") {
                return Err(syn::Error::new(
                    attr.span(),
                    "declare payload indexes with #[index(...)] on the variant, not its field",
                ));
            }
        }
        let mut variant_indexes = Vec::new();
        let mut retained = Vec::new();
        let prefix = variant.ident.unraw().to_string().to_snake_case();
        for attr in std::mem::take(&mut variant.attrs) {
            reject_cfg(&attr)?;
            if attr.path().is_ident("indexed") {
                return Err(syn::Error::new(
                    attr.span(),
                    "use #[index(name = ..., key_type = ..., key = ...)] on enum variants",
                ));
            }
            if !attr.path().is_ident("index") {
                retained.push(attr);
                continue;
            }
            let options: attributes::IndexOptions = attributes::parse_attribute(&attr)?;
            let kind = options.mode.kind(attr.span())?;
            if options.fields.is_some() {
                return Err(syn::Error::new(
                    attr.span(),
                    "fields(...) is not supported on enum variants; specify key_type and key",
                ));
            }
            let (Some(ty), Some(expression)) = (options.key_type, options.key) else {
                return Err(syn::Error::new(
                    attr.span(),
                    "enum indexes require both key_type = Type and key = function_or_closure",
                ));
            };
            let index_name = format_ident!(
                "{}_by_{}",
                prefix,
                options.name.unraw(),
                span = options.name.span()
            );
            if !names.insert(index_name.to_string()) {
                return Err(syn::Error::new(
                    options.name.span(),
                    "duplicate qualified index name",
                ));
            }
            variant_indexes.push(Index {
                name: index_name,
                lookup_name: options.name,
                kind,
                key: IndexKey::Computed {
                    ty: Box::new(ty),
                    expression: Box::new(expression),
                },
                cached: options.mode.cached.unwrap_or(true),
            });
        }
        variant.attrs = retained;
        all_indexes.push(variant_indexes);
    }
    let runtime = match proc_macro_crate::crate_name("microtable") {
        Ok(proc_macro_crate::FoundCrate::Itself) => quote!(::microtable),
        Ok(proc_macro_crate::FoundCrate::Name(name)) => {
            let ident = format_ident!("{}", name);
            quote!(::#ident)
        }
        Err(err) => return Err(syn::Error::new(name.span(), err.to_string())),
    };
    let mut cfg = Vec::new();
    for attr in &row.attrs {
        if let Some(meta) = super::conditional_meta(&attr.meta)? {
            cfg.push(quote!(#[#meta]));
        }
    }
    let entry = format_ident!("__{}Entry", table.unraw());
    let metadata = format_ident!("__{}Metadata", table.unraw());
    let (_, args, _) = row.generics.split_for_impl();
    let row_type: syn::TypePath = syn::parse_quote!(#name #args);
    let mut resolve_self = ResolveSelf { row_type };
    let mut original_generics = row.generics.clone();
    resolve_self.visit_generics_mut(&mut original_generics);
    let (row_impl, row_args, row_where) = original_generics.split_for_impl();
    let mut table_generics = original_generics.clone();
    table_generics
        .make_where_clause()
        .predicates
        .push(syn::parse_quote!(
            #hasher: ::std::hash::BuildHasher + ::std::clone::Clone + ::std::default::Default
        ));
    table_generics
        .make_where_clause()
        .predicates
        .push(syn::parse_quote!(#id: ::std::marker::Copy + ::std::cmp::Eq));
    for index in all_indexes.iter().flatten() {
        for ty in index.types() {
            table_generics
                .make_where_clause()
                .predicates
                .push(syn::parse_quote!(#ty: ::std::cmp::Eq + ::std::hash::Hash));
        }
    }
    let (impl_generics, type_generics, table_where) = table_generics.split_for_impl();
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
    let mut metadata_structs = Vec::new();
    let mut metadata_variants = Vec::new();
    let mut accessors = Vec::new();
    let mut map_fields = Vec::new();
    let mut map_defaults = Vec::new();
    let mut lookups = Vec::new();
    let mut insert_arms = Vec::new();
    let mut replace_arms = Vec::new();
    let mut detach_arms = Vec::new();
    for (position, (variant, indexes)) in row.variants.iter().zip(&all_indexes).enumerate() {
        let variant_name = &variant.ident;
        let syn::Fields::Unnamed(fields) = &variant.fields else {
            unreachable!("validated variant fields");
        };
        let mut payload = fields.unnamed[0].ty.clone();
        resolve_self.visit_type_mut(&mut payload);
        let meta_struct = format_ident!("__{}Variant{}Metadata", table.unraw(), position);
        let meta_variant = format_ident!("__Variant{}", position);
        let accessor = format_ident!(
            "__variant_{}_{}",
            variant_name.unraw().to_string().to_snake_case(),
            position
        );
        let accessor_mut = format_ident!("{}_mut", accessor);
        let indexes::Code {
            map_fields: fields,
            map_defaults: defaults,
            entry_fields,
            entry_defaults,
            prepare,
            checks_insert,
            checks_replace,
            reserve,
            groups_insert,
            keys_insert,
            keys_replace,
            detach_remove,
            commit,
            lookups: methods,
            ..
        } = indexes::Code::generate(
            indexes,
            &indexes::Context {
                runtime: &runtime,
                name: &name,
                row_args: quote!(#row_args),
                id: &id,
                hasher: &hasher,
                vis,
                append_only,
                variant: Some(indexes::VariantContext {
                    variant: variant_name,
                    payload: &payload,
                    accessor: &accessor,
                    accessor_mut: &accessor_mut,
                }),
            },
        );
        map_fields.extend(fields);
        map_defaults.extend(defaults);
        lookups.extend(methods);
        metadata_structs.push(quote! {
            #(#cfg)*
            struct #meta_struct #original_generics #row_where {
                #(#entry_fields,)*
                __marker: ::std::marker::PhantomData<fn() -> #name #row_args>,
            }
        });
        metadata_variants.push(quote!(#meta_variant(#meta_struct #row_args)));
        accessors.push(quote! {
            fn #accessor(&self) -> &#meta_struct #row_args {
                #[allow(unreachable_patterns)]
                match &self.metadata {
                    #metadata::#meta_variant(metadata) => metadata,
                    _ => ::std::unreachable!("matching variant metadata"),
                }
            }
            fn #accessor_mut(&mut self) -> &mut #meta_struct #row_args {
                #[allow(unreachable_patterns)]
                match &mut self.metadata {
                    #metadata::#meta_variant(metadata) => metadata,
                    _ => ::std::unreachable!("matching variant metadata"),
                }
            }
        });
        let make_entry = quote! {
            let entry = #entry {
                row,
                metadata: #metadata::#meta_variant(#meta_struct {
                    #(#entry_defaults,)*
                    __marker: ::std::marker::PhantomData,
                }),
            };
        };
        insert_arms.push(quote! {
            #name::#variant_name(_) => {
                #(#prepare)*
                #(#checks_insert)*
                #capacity_check
                #(#groups_insert)*
                #(#keys_insert)*
                #reserve_rows
                #(#reserve)*
                #make_entry
                #insert_row
                #(#commit)*
                ::std::result::Result::Ok(id)
            }
        });
        replace_arms.push(quote! {
            #name::#variant_name(_) => {
                #(#prepare)*
                #(#checks_replace)*
                #(#keys_replace)*
                #(#reserve)*
                self.__detach(id);
                #make_entry
                let old = ::std::mem::replace(self.__rows.get_mut(id).expect("present row"), entry);
                #(#commit)*
                ::std::result::Result::Ok(old.row)
            }
        });
        detach_arms.push(quote! {
            #name::#variant_name(_) => { #(#detach_remove)* }
        });
    }
    let mutations = (!append_only).then(|| quote! {
        fn __detach(&mut self, id: #id) {
            match &self.__rows.get(id).expect("present row").row {
                #(#detach_arms,)*
            }
        }
        #vis fn replace(&mut self, id: #id, row: #name #row_args) -> ::std::result::Result<#name #row_args, #runtime::ReplaceError<#name #row_args>> {
            if !self.contains(id) {
                return ::std::result::Result::Err(#runtime::ReplaceError { row, kind: #runtime::ReplaceErrorKind::NotFound });
            }
            match &row { #(#replace_arms,)* }
        }
        #vis fn remove(&mut self, id: #id) -> ::std::option::Option<#name #row_args> {
            if !self.contains(id) {
                return ::std::option::Option::None;
            }
            self.__detach(id);
            self.__rows.remove(id).map(|entry| entry.row)
        }
    });
    Ok(quote! {
        #row

        #(#metadata_structs)*

        #(#cfg)*
        enum #metadata #original_generics #row_where {
            #(#metadata_variants,)*
        }

        #(#cfg)*
        struct #entry #original_generics #row_where {
            row: #name #row_args,
            metadata: #metadata #row_args,
        }

        #(#cfg)*
        impl #row_impl #entry #row_args #row_where {
            #(#accessors)*
        }

        #(#cfg)*
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
            #vis fn new() -> Self { ::std::default::Default::default() }
            #vis fn len(&self) -> usize { self.__rows.len() }
            #vis fn is_empty(&self) -> bool { self.len() == 0 }
            #vis fn get(&self, id: #id) -> ::std::option::Option<&#name #row_args> {
                self.__rows.get(id).map(|entry| &entry.row)
            }
            #vis fn contains(&self, id: #id) -> bool { self.get(id).is_some() }
            #vis fn iter(&self) -> impl ::std::iter::Iterator<Item = (#id, &#name #row_args)> + '_ {
                self.__rows.iter().map(|(id, entry)| (id, &entry.row))
            }
            /// Insert a row, returning its ID or the rejected row and first conflicting ID.
            /// Unique conflicts leave the table unchanged.
            /// Panics if row storage capacity is exhausted.
            #vis fn insert(&mut self, row: #name #row_args) -> ::std::result::Result<#id, (#name #row_args, #id)> {
                match &row { #(#insert_arms,)* }
            }
            #mutations
            #(#lookups)*
        }
    })
}

// `Self` in enum payloads and bounds refers to the enum, not to the generated
// table or metadata types where these types are used.
struct ResolveSelf {
    row_type: syn::TypePath,
}

impl VisitMut for ResolveSelf {
    fn visit_type_path_mut(&mut self, path: &mut syn::TypePath) {
        syn::visit_mut::visit_type_path_mut(self, path);
        if path.qself.is_none()
            && path
                .path
                .segments
                .first()
                .is_some_and(|segment| segment.ident == "Self")
        {
            let suffix = path
                .path
                .segments
                .iter()
                .skip(1)
                .cloned()
                .collect::<Vec<_>>();
            *path = self.row_type.clone();
            path.path.segments.extend(suffix);
        }
    }
}

fn reject_cfg(attr: &syn::Attribute) -> syn::Result<()> {
    if attr.path().is_ident("cfg") || attr.path().is_ident("cfg_attr") {
        return Err(syn::Error::new(
            attr.span(),
            "cfg and cfg_attr are not supported on table enum variants or payload fields",
        ));
    }
    Ok(())
}
