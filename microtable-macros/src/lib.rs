//! Function-like table declarations for `microtable`.
#![forbid(unsafe_code)]

use proc_macro::TokenStream;
use proc_macro2::TokenStream as Tokens;
use quote::{format_ident, quote};
use std::collections::HashSet;
use syn::{
    ext::IdentExt,
    parse::{Parse, ParseStream},
    parse_macro_input,
    spanned::Spanned,
};

mod indexes;
#[cfg(test)]
mod tests;
mod traits;

#[proc_macro]
pub fn table(input: TokenStream) -> TokenStream {
    let declaration = parse_macro_input!(input as Declaration);
    expand(declaration)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

#[derive(Clone, Copy)]
enum Kind {
    Unique,
    Many,
}

struct Index {
    function: syn::ItemFn,
    kind: Kind,
    cached: bool,
    dense: bool,
    optional: bool,
    key: syn::Type,
}

struct Declaration {
    table: syn::ItemStruct,
    row: syn::Type,
    id: Option<syn::ItemStruct>,
    indexes: Vec<Index>,
    append_only: bool,
    hasher: syn::Type,
}

fn take_attribute(
    attrs: &mut Vec<syn::Attribute>,
    name: &str,
) -> syn::Result<Option<syn::Attribute>> {
    let mut result = None;
    let mut retained = Vec::new();
    for attr in std::mem::take(attrs) {
        if attr.path().is_ident(name) {
            if result.is_some() {
                return Err(syn::Error::new(
                    attr.span(),
                    format!("duplicate {name} attribute"),
                ));
            }
            result = Some(attr);
        } else {
            retained.push(attr);
        }
    }
    *attrs = retained;
    Ok(result)
}

impl Parse for Declaration {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut table: syn::ItemStruct = input.parse()?;
        for param in &table.generics.params {
            let ident = match param {
                syn::GenericParam::Type(param) => &param.ident,
                syn::GenericParam::Const(param) => &param.ident,
                syn::GenericParam::Lifetime(param) => &param.lifetime.ident,
            };
            let name = ident.to_string();
            if name.starts_with("__mt") || name.starts_with("__Mt") {
                return Err(syn::Error::new(
                    ident.span(),
                    "generic names starting with __mt or __Mt are reserved",
                ));
            }
        }
        let mut append_only = false;
        let mut hasher = syn::parse_quote!(::std::collections::hash_map::RandomState);
        if let Some(attr) = take_attribute(&mut table.attrs, "table")? {
            let mut seen = HashSet::new();
            attr.parse_nested_meta(|meta| {
                let name = meta
                    .path
                    .get_ident()
                    .ok_or_else(|| meta.error("expected a table option"))?
                    .to_string();
                if !seen.insert(name.clone()) {
                    return Err(meta.error("duplicate table option"));
                }
                match name.as_str() {
                    "append_only" => {
                        append_only = true;
                        Ok(())
                    }
                    "hasher" => {
                        hasher = meta.value()?.parse()?;
                        Ok(())
                    }
                    _ => Err(meta.error("expected append_only or hasher = Type")),
                }
            })?;
        }
        let syn::Fields::Unnamed(fields) = &table.fields else {
            return Err(syn::Error::new(
                table.span(),
                "expected struct Table([Row]);",
            ));
        };
        if fields.unnamed.len() != 1 {
            return Err(syn::Error::new(
                fields.span(),
                "table requires exactly one [Row] field",
            ));
        }
        let field = &fields.unnamed[0];
        if !field.attrs.is_empty() || !matches!(field.vis, syn::Visibility::Inherited) {
            return Err(syn::Error::new(
                field.span(),
                "table row field cannot have attributes or visibility",
            ));
        }
        let syn::Type::Slice(slice) = &field.ty else {
            return Err(syn::Error::new(field.ty.span(), "expected [Row]"));
        };
        let row = (*slice.elem).clone();
        let mut id = None;
        let mut indexes = Vec::new();
        let mut names = HashSet::new();
        while !input.is_empty() {
            let item: syn::Item = input.parse()?;
            match item {
                syn::Item::Struct(mut item) => {
                    let attr = take_attribute(&mut item.attrs, "id")?.ok_or_else(|| {
                        syn::Error::new(item.span(), "ID declaration requires #[id]")
                    })?;
                    if !matches!(attr.meta, syn::Meta::Path(_)) {
                        return Err(syn::Error::new(attr.span(), "#[id] has no options"));
                    }
                    if id.is_some() {
                        return Err(syn::Error::new(item.span(), "duplicate ID declaration"));
                    }
                    if !indexes.is_empty() {
                        return Err(syn::Error::new(
                            item.span(),
                            "declare the ID before index extractors",
                        ));
                    }
                    validate_id(&item)?;
                    id = Some(item);
                }
                syn::Item::Fn(mut function) => {
                    let attr = take_attribute(&mut function.attrs, "index")?.ok_or_else(|| {
                        syn::Error::new(function.span(), "extractor requires #[index(unique|many)]")
                    })?;
                    let mut kind = None;
                    let mut cached = false;
                    let mut dense = false;
                    let mut optional = false;
                    let mut seen = HashSet::new();
                    attr.parse_nested_meta(|meta| {
                        let name = meta
                            .path
                            .get_ident()
                            .ok_or_else(|| meta.error("expected an index option"))?
                            .to_string();
                        if !seen.insert(name.clone()) {
                            return Err(meta.error("duplicate index option"));
                        }
                        match name.as_str() {
                            "unique" | "many" => {
                                if kind.is_some() {
                                    return Err(meta.error("choose either unique or many"));
                                }
                                kind = Some(if name == "unique" {
                                    Kind::Unique
                                } else {
                                    Kind::Many
                                });
                                Ok(())
                            }
                            "cached" => {
                                cached = true;
                                Ok(())
                            }
                            "dense" => {
                                dense = true;
                                Ok(())
                            }
                            "optional" => {
                                optional = true;
                                Ok(())
                            }
                            _ => {
                                Err(meta.error("expected unique, many, cached, dense, or optional"))
                            }
                        }
                    })?;
                    let kind = kind.ok_or_else(|| {
                        syn::Error::new(attr.span(), "index requires unique or many")
                    })?;
                    if dense && cached {
                        return Err(syn::Error::new(
                            attr.span(),
                            "dense and cached are mutually exclusive",
                        ));
                    }
                    for attr in &function.attrs {
                        if attr.path().is_ident("cfg") || attr.path().is_ident("cfg_attr") {
                            return Err(syn::Error::new(
                                attr.span(),
                                "conditional extractor attributes are not supported",
                            ));
                        }
                    }
                    validate_function(&function, &row)?;
                    let name = function.sig.ident.unraw().to_string();
                    if name.starts_with("__")
                        || [
                            "new", "default", "len", "is_empty", "next_id", "get", "contains",
                            "iter", "insert", "replace", "remove", "clone", "fmt",
                        ]
                        .contains(&name.as_str())
                    {
                        return Err(syn::Error::new(
                            function.sig.ident.span(),
                            "extractor name conflicts with a generated table method",
                        ));
                    }
                    if !names.insert(name) {
                        return Err(syn::Error::new(
                            function.sig.ident.span(),
                            "duplicate index name",
                        ));
                    }
                    let syn::ReturnType::Type(_, output) = &function.sig.output else {
                        unreachable!()
                    };
                    let key = if optional {
                        option_inner(output)?.clone()
                    } else {
                        (**output).clone()
                    };
                    if cached && has_input_borrow(&key, &function.sig) {
                        return Err(syn::Error::new(
                            key.span(),
                            "cached keys cannot borrow the temporary row input; use an owned key or an explicit external lifetime",
                        ));
                    }
                    indexes.push(Index {
                        function,
                        kind,
                        cached,
                        dense,
                        optional,
                        key,
                    });
                }
                item => {
                    return Err(syn::Error::new(
                        item.span(),
                        "expected #[id] struct or #[index(...)] fn extractor",
                    ));
                }
            }
        }
        let lookup_names: HashSet<_> = indexes
            .iter()
            .map(|index| {
                let prefix = if matches!(index.kind, Kind::Unique) {
                    "get"
                } else {
                    "iter"
                };
                format!("{prefix}_by_{}", index.function.sig.ident.unraw())
            })
            .collect();
        for index in &indexes {
            if lookup_names.contains(&index.function.sig.ident.unraw().to_string()) {
                return Err(syn::Error::new(
                    index.function.sig.ident.span(),
                    "extractor name conflicts with a generated lookup method",
                ));
            }
        }
        if let Some(id) = &id
            && id.ident == table.ident
        {
            return Err(syn::Error::new(
                id.ident.span(),
                "table and ID names must differ",
            ));
        }
        Ok(Self {
            table,
            row,
            id,
            indexes,
            append_only,
            hasher,
        })
    }
}

fn option_inner(ty: &syn::Type) -> syn::Result<&syn::Type> {
    if let syn::Type::Path(path) = ty
        && let Some(last) = path.path.segments.last()
        && last.ident == "Option"
        && let syn::PathArguments::AngleBracketed(args) = &last.arguments
        && args.args.len() == 1
        && let syn::GenericArgument::Type(ty) = &args.args[0]
    {
        return Ok(ty);
    }
    Err(syn::Error::new(
        ty.span(),
        "optional index must return Option<Key>",
    ))
}

fn validate_function(function: &syn::ItemFn, row: &syn::Type) -> syn::Result<()> {
    let sig = &function.sig;
    if !sig.generics.params.is_empty() || sig.generics.where_clause.is_some() {
        return Err(syn::Error::new(
            sig.generics.span(),
            "extractors share table generics and cannot declare their own generics or where clause",
        ));
    }
    if sig.asyncness.is_some()
        || sig.constness.is_some()
        || sig.unsafety.is_some()
        || sig.abi.is_some()
        || sig.variadic.is_some()
    {
        return Err(syn::Error::new(
            sig.span(),
            "extractor must be an ordinary safe function",
        ));
    }
    if sig.inputs.len() != 1 {
        return Err(syn::Error::new(
            sig.inputs.span(),
            "extractor requires exactly one argument: row: &Row",
        ));
    }
    let syn::FnArg::Typed(arg) = &sig.inputs[0] else {
        return Err(syn::Error::new(
            sig.inputs.span(),
            "extractor cannot have a receiver",
        ));
    };
    let syn::Type::Reference(reference) = &*arg.ty else {
        return Err(syn::Error::new(
            arg.ty.span(),
            "extractor argument must be &Row",
        ));
    };
    if reference.mutability.is_some()
        || quote!(#row).to_string() != {
            let ty = &reference.elem;
            quote!(#ty).to_string()
        }
    {
        return Err(syn::Error::new(
            arg.ty.span(),
            "extractor argument must be an immutable reference to the declared row type",
        ));
    }
    if matches!(sig.output, syn::ReturnType::Default) {
        return Err(syn::Error::new(
            sig.span(),
            "extractor requires an explicit key return type",
        ));
    }
    Ok(())
}

fn has_input_borrow(ty: &syn::Type, sig: &syn::Signature) -> bool {
    use syn::visit_mut::VisitMut;
    struct Check {
        found: bool,
        input: Option<syn::Lifetime>,
    }
    impl VisitMut for Check {
        fn visit_type_reference_mut(&mut self, node: &mut syn::TypeReference) {
            if node.lifetime.is_none()
                || node
                    .lifetime
                    .as_ref()
                    .is_some_and(|lt| lt.ident == "_" || Some(lt) == self.input.as_ref())
            {
                self.found = true;
            }
            syn::visit_mut::visit_type_reference_mut(self, node);
        }
        fn visit_lifetime_mut(&mut self, lifetime: &mut syn::Lifetime) {
            if lifetime.ident == "_" || Some(&*lifetime) == self.input.as_ref() {
                self.found = true;
            }
        }
    }
    let input = match &sig.inputs[0] {
        syn::FnArg::Typed(arg) => match &*arg.ty {
            syn::Type::Reference(r) => r.lifetime.clone(),
            _ => None,
        },
        _ => None,
    };
    let mut check = Check {
        found: false,
        input,
    };
    check.visit_type_mut(&mut ty.clone());
    check.found
}

fn validate_id(item: &syn::ItemStruct) -> syn::Result<()> {
    if !item.generics.params.is_empty() || item.generics.where_clause.is_some() {
        return Err(syn::Error::new(
            item.generics.span(),
            "ID cannot have generics",
        ));
    }
    let syn::Fields::Unnamed(fields) = &item.fields else {
        return Err(syn::Error::new(
            item.span(),
            "expected struct Id(u8|u16|u32|usize);",
        ));
    };
    if fields.unnamed.len() != 1 {
        return Err(syn::Error::new(
            fields.span(),
            "ID requires one unsigned integer field",
        ));
    }
    let field = &fields.unnamed[0];
    if !matches!(field.vis, syn::Visibility::Inherited) || !field.attrs.is_empty() {
        return Err(syn::Error::new(
            field.span(),
            "ID backing field must be private and have no attributes",
        ));
    }
    let syn::Type::Path(path) = &field.ty else {
        return Err(syn::Error::new(
            field.ty.span(),
            "ID must use u8, u16, u32, or usize",
        ));
    };
    if !["u8", "u16", "u32", "usize"]
        .iter()
        .any(|name| path.path.is_ident(name))
    {
        return Err(syn::Error::new(
            field.ty.span(),
            "ID must use u8, u16, u32, or usize",
        ));
    }
    Ok(())
}

fn id_definition(item: &syn::ItemStruct, cfg: &[Tokens], runtime: &Tokens) -> Tokens {
    let name = &item.ident;
    let vis = &item.vis;
    let attrs: Vec<_> = item
        .attrs
        .iter()
        .filter(|attr| !attr.path().is_ident("derive"))
        .collect();
    let syn::Fields::Unnamed(fields) = &item.fields else {
        unreachable!()
    };
    let primitive = &fields.unnamed[0].ty;
    let nonzero = match quote!(#primitive).to_string().as_str() {
        "u8" => quote!(::std::num::NonZeroU8),
        "u16" => quote!(::std::num::NonZeroU16),
        "u32" => quote!(::std::num::NonZeroU32),
        _ => quote!(::std::num::NonZeroUsize),
    };
    quote! {
        #(#cfg)*
        const _: () = assert!(::std::mem::size_of::<#primitive>() <= ::std::mem::size_of::<usize>(), "ID width exceeds target pointer width");
        #(#cfg)* #(#attrs)*
        #[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash)]
        #vis struct #name(#nonzero);
        #(#cfg)*
        impl #name {
            #vis fn new(index: usize) -> Self { Self::try_new(index).expect("table capacity exhausted") }
            #vis fn try_new(index: usize) -> ::std::option::Option<Self> {
                let encoded = index.checked_add(1)?;
                let encoded = <#primitive as ::std::convert::TryFrom<usize>>::try_from(encoded).ok()?;
                #nonzero::new(encoded).map(Self)
            }
            #vis fn index(self) -> usize { (self.0.get() as usize) - 1 }
        }
        #(#cfg)*
        impl ::std::convert::TryFrom<usize> for #name {
            type Error = &'static str;
            fn try_from(index: usize) -> ::std::result::Result<Self, Self::Error> { Self::try_new(index).ok_or("table capacity exhausted") }
        }
        #(#cfg)*
        impl ::std::convert::From<#name> for usize { fn from(id: #name) -> usize { id.index() } }
        #(#cfg)*
        impl #runtime::__private::ArenaId for #name {
            fn from_index(index: usize) -> Self { Self::new(index) }
            fn into_index(self) -> usize { self.index() }
        }
    }
}

// Retain conditional compilation on all generated items, without carrying derives.
fn conditional_meta(meta: &syn::Meta) -> syn::Result<Option<syn::Meta>> {
    if meta.path().is_ident("cfg") {
        return Ok(Some(meta.clone()));
    }
    if !meta.path().is_ident("cfg_attr") {
        return Ok(None);
    }
    let list = meta.require_list()?;
    let args = list.parse_args_with(
        syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
    )?;
    let mut args = args.iter();
    let predicate = args
        .next()
        .ok_or_else(|| syn::Error::new(meta.span(), "expected cfg_attr predicate"))?;
    let attrs = args
        .map(conditional_meta)
        .collect::<syn::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    Ok((!attrs.is_empty()).then(|| syn::parse_quote!(cfg_attr(#predicate, #(#attrs),*))))
}

// Strip only the traits generated by this macro. Preserve other attributes,
// including nested cfg_attr layout and lint attributes, on the table itself.
fn table_meta(meta: &syn::Meta) -> syn::Result<Option<syn::Meta>> {
    if meta.path().is_ident("cfg") {
        return Ok(None);
    }
    if meta.path().is_ident("derive") {
        let paths = meta.require_list()?.parse_args_with(
            syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
        )?;
        let paths = paths
            .into_iter()
            .filter(|path| {
                !path
                    .segments
                    .last()
                    .is_some_and(|segment| segment.ident == "Debug" || segment.ident == "Clone")
            })
            .collect::<Vec<_>>();
        return Ok((!paths.is_empty()).then(|| syn::parse_quote!(derive(#(#paths),*))));
    }
    if meta.path().is_ident("cfg_attr") {
        let args = meta.require_list()?.parse_args_with(
            syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
        )?;
        let mut args = args.iter();
        let predicate = args
            .next()
            .ok_or_else(|| syn::Error::new(meta.span(), "expected cfg_attr predicate"))?;
        let attrs = args
            .map(table_meta)
            .collect::<syn::Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        return Ok(
            (!attrs.is_empty()).then(|| syn::parse_quote!(cfg_attr(#predicate, #(#attrs),*)))
        );
    }
    Ok(Some(meta.clone()))
}

fn expand(declaration: Declaration) -> syn::Result<Tokens> {
    let Declaration {
        table,
        row,
        id: id_item,
        indexes,
        append_only,
        hasher,
    } = declaration;
    let name = &table.ident;
    let vis = &table.vis;
    let runtime = match proc_macro_crate::crate_name("microtable") {
        Ok(proc_macro_crate::FoundCrate::Itself) => quote!(::microtable),
        Ok(proc_macro_crate::FoundCrate::Name(name)) => {
            let ident = format_ident!("{}", name);
            quote!(::#ident)
        }
        Err(error) => return Err(syn::Error::new(name.span(), error.to_string())),
    };
    let cfg = table
        .attrs
        .iter()
        .map(|attr| conditional_meta(&attr.meta))
        .collect::<syn::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .map(|meta| quote!(#[#meta]))
        .collect::<Vec<_>>();
    let id: syn::Type = id_item
        .as_ref()
        .map(|item| {
            let name = &item.ident;
            syn::parse_quote!(#name)
        })
        .unwrap_or_else(|| syn::parse_quote!(usize));
    let id_def = id_item
        .as_ref()
        .map(|item| id_definition(item, &cfg, &runtime));
    let mut generics = table.generics.clone();
    generics.make_where_clause().predicates.push(syn::parse_quote!(#hasher: ::std::hash::BuildHasher + ::std::clone::Clone + ::std::default::Default));
    for index in &indexes {
        if index.dense {
            let key = &index.key;
            generics
                .make_where_clause()
                .predicates
                .push(syn::parse_quote!(#key: ::std::marker::Copy + ::std::convert::Into<usize>));
        } else {
            // Borrowed outputs are local values, never fields. Bound their referents
            // rather than adding a higher-ranked bound that would require Row: 'static.
            indexes::add_key_bounds(&mut generics, &index.key);
        }
    }
    let (ig, args, wc) = generics.split_for_impl();
    let entry = format_ident!("__{}Entry", name.unraw());
    let arena = if append_only {
        quote!(#runtime::__private::AppendArena)
    } else {
        quote!(#runtime::__private::Arena)
    };
    let code = indexes::generate(
        &indexes,
        &indexes::Context {
            row: &row,
            id: &id,
            hasher: &hasher,
            vis,
            runtime: &runtime,
            append_only,
            generics: &generics,
            table: name,
        },
    );
    let indexes::Code {
        map_fields,
        map_defaults,
        entry_fields,
        entry_defaults,
        helpers,
        prepare,
        checks_insert,
        checks_replace,
        checks_capacity,
        reserve,
        detach,
        commit,
        lookups,
        iterators,
    } = code;
    let clone = traits::enabled(&table.attrs, "Clone")?;
    let debug = traits::enabled(&table.attrs, "Debug")?;
    let entry_def = traits::with_clone(
        quote! {
            #(#cfg)* struct #entry #generics #wc { row: #row, #(#entry_fields,)* }
        },
        &clone,
    );
    let attrs = table
        .attrs
        .iter()
        .map(|attr| table_meta(&attr.meta))
        .collect::<syn::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .map(|meta| quote!(#[#meta]))
        .collect::<Vec<_>>();
    let table_def = traits::with_clone(
        quote! {
            #(#cfg)* #(#attrs)* #vis struct #name #generics #wc {
                __rows: #arena<#entry #args, #id>, __hasher: #hasher, #(#map_fields,)*
            }
        },
        &clone,
    );
    let debug_impl = traits::debug_impl(name, &generics, quote!(#row), &id, &cfg, &debug);
    let release = indexes
        .iter()
        .filter(|index| !index.cached)
        .map(|index| {
            let key = format_ident!("__key_{}", index.function.sig.ident.unraw());
            quote!(#[allow(dropping_copy_types, clippy::drop_non_drop)] ::std::mem::drop(#key);)
        })
        .collect::<Vec<_>>();
    let reserve_rows = append_only.then(|| quote!(self.__rows.reserve(1);));
    let insert_row = if append_only {
        quote!(self.__rows.push(entry);)
    } else {
        quote!(let __actual_id = self.__rows.insert(entry); debug_assert!(__actual_id == id);)
    };
    let mutations = (!append_only).then(|| quote! {
        #vis fn replace(&mut self, id: #id, row: #row) -> ::std::result::Result<#row, #runtime::ReplaceError<#row>> {
            if !self.contains(id) { return ::std::result::Result::Err(#runtime::ReplaceError { row, kind: #runtime::ReplaceErrorKind::NotFound }); }
            #(#prepare)*
            let __conflict = (|| { #(#checks_replace)* ::std::option::Option::None })();
            #(#release)*
            if let ::std::option::Option::Some(kind) = __conflict { return ::std::result::Result::Err(#runtime::ReplaceError { row, kind }); }
            #(#checks_capacity)* #(#reserve)* #(#detach)*
            let entry = #entry { row, #(#entry_defaults,)* };
            let old = ::std::mem::replace(self.__rows.get_mut(id).expect("present row"), entry);
            #(#commit)*
            ::std::result::Result::Ok(old.row)
        }
        #vis fn remove(&mut self, id: #id) -> ::std::option::Option<#row> {
            if !self.contains(id) { return ::std::option::Option::None; }
            #(#detach)* self.__rows.remove(id).map(|entry| entry.row)
        }
    });
    let iterators = iterators
        .into_iter()
        .map(|tokens| {
            let file: syn::File = syn::parse2(tokens).expect("generated iterators");
            let items = file
                .items
                .into_iter()
                .map(|item| quote!(#(#cfg)* #item))
                .collect::<Vec<_>>();
            quote!(#(#items)*)
        })
        .collect::<Vec<_>>();
    Ok(quote! {
        #id_def #entry_def #table_def #debug_impl
        #(#iterators)*
        #(#cfg)* impl #ig ::std::ops::Index<#id> for #name #args #wc {
            type Output = #row;

            fn index(&self, id: #id) -> &Self::Output {
                self.get(id).expect("row ID is not present in this table")
            }
        }
        #(#cfg)* impl #ig ::std::default::Default for #name #args #wc {
            fn default() -> Self {
                let __hasher: #hasher = ::std::default::Default::default();
                Self { __rows: #arena::new(), #(#map_defaults,)* __hasher }
            }
        }
        #(#cfg)* impl #ig #name #args #wc {
            #vis fn new() -> Self { ::std::default::Default::default() }
            #vis fn len(&self) -> usize { self.__rows.len() }
            #vis fn is_empty(&self) -> bool { self.len() == 0 }
            #vis fn next_id(&self) -> #id {
                assert!(self.__rows.can_insert(), "table capacity exhausted");
                self.__rows.next_id()
            }
            #vis fn get(&self, id: #id) -> ::std::option::Option<&#row> { self.__rows.get(id).map(|entry| &entry.row) }
            #vis fn contains(&self, id: #id) -> bool { self.get(id).is_some() }
            #vis fn iter(&self) -> impl ::std::iter::Iterator<Item = (#id, &#row)> + '_ { self.__rows.iter().map(|(id, entry)| (id, &entry.row)) }
            #vis fn insert(&mut self, row: #row) -> ::std::result::Result<#id, (#row, #id)> {
                #(#prepare)*
                let __conflict = (|| { #(#checks_insert)* ::std::option::Option::None })();
                #(#release)*
                if let ::std::option::Option::Some(existing) = __conflict { return ::std::result::Result::Err((row, existing)); }
                // next_id converts before any reservation or index mutation.
                let id = self.next_id();
                #(#checks_capacity)* #reserve_rows #(#reserve)*
                let entry = #entry { row, #(#entry_defaults,)* };
                #insert_row #(#commit)* ::std::result::Result::Ok(id)
            }
            #mutations #(#helpers)* #(#lookups)*
        }
    })
}
