//! Hash and dense indexes with local extractor outputs and linked many groups.
use super::{Index, Kind};
use proc_macro2::TokenStream as Tokens;
use quote::{format_ident, quote};
use syn::{ext::IdentExt, visit_mut::VisitMut};

#[derive(Default)]
pub(super) struct Code {
    pub map_fields: Vec<Tokens>,
    pub map_defaults: Vec<Tokens>,
    pub entry_fields: Vec<Tokens>,
    pub entry_defaults: Vec<Tokens>,
    pub helpers: Vec<Tokens>,
    pub prepare: Vec<Tokens>,
    pub checks_insert: Vec<Tokens>,
    pub checks_replace: Vec<Tokens>,
    pub checks_capacity: Vec<Tokens>,
    pub reserve: Vec<Tokens>,
    pub detach: Vec<Tokens>,
    pub commit: Vec<Tokens>,
    pub lookups: Vec<Tokens>,
    pub iterators: Vec<Tokens>,
}

// Elided output lifetimes refer to the extractor input, not a fixed key type.
struct Elision(syn::Lifetime);
impl VisitMut for Elision {
    fn visit_type_reference_mut(&mut self, node: &mut syn::TypeReference) {
        if node.lifetime.as_ref().is_none_or(|lt| lt.ident == "_") {
            node.lifetime = Some(self.0.clone());
        }
        syn::visit_mut::visit_type_reference_mut(self, node);
    }
    fn visit_lifetime_mut(&mut self, node: &mut syn::Lifetime) {
        if node.ident == "_" {
            *node = self.0.clone();
        }
    }
}
fn has_nested_borrow(ty: &syn::Type) -> bool {
    if matches!(ty, syn::Type::Reference(_)) {
        return false;
    }
    struct Find(bool);
    impl VisitMut for Find {
        fn visit_type_reference_mut(&mut self, _: &mut syn::TypeReference) {
            self.0 = true;
        }
        fn visit_lifetime_mut(&mut self, _: &mut syn::Lifetime) {
            self.0 = true;
        }
    }
    let mut find = Find(false);
    find.visit_type_mut(&mut ty.clone());
    find.0
}

fn query_type(ty: &syn::Type) -> syn::Type {
    struct Query;
    impl VisitMut for Query {
        fn visit_type_reference_mut(&mut self, reference: &mut syn::TypeReference) {
            reference.lifetime = Some(syn::parse_quote!('__mt_query));
            syn::visit_mut::visit_type_reference_mut(self, reference);
        }
        fn visit_lifetime_mut(&mut self, lifetime: &mut syn::Lifetime) {
            *lifetime = syn::parse_quote!('__mt_query);
        }
    }
    let mut ty = ty.clone();
    Query.visit_type_mut(&mut ty);
    ty
}

fn with_lifetime(ty: &syn::Type, lifetime: syn::Lifetime) -> syn::Type {
    let mut ty = ty.clone();
    Elision(lifetime).visit_type_mut(&mut ty);
    ty
}

pub(super) fn add_key_bounds(generics: &mut syn::Generics, ty: &syn::Type) {
    match ty {
        syn::Type::Reference(reference) => add_key_bounds(generics, &reference.elem),
        syn::Type::Tuple(tuple) => {
            for ty in &tuple.elems {
                add_key_bounds(generics, ty);
            }
        }
        syn::Type::Path(path)
            if path
                .path
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "Option") =>
        {
            if let syn::PathArguments::AngleBracketed(args) =
                &path.path.segments.last().unwrap().arguments
            {
                for arg in &args.args {
                    if let syn::GenericArgument::Type(ty) = arg {
                        add_key_bounds(generics, ty);
                    }
                }
            }
        }
        _ => {
            struct Borrowed(bool);
            impl VisitMut for Borrowed {
                fn visit_type_reference_mut(&mut self, _: &mut syn::TypeReference) {
                    self.0 = true;
                }
                fn visit_lifetime_mut(&mut self, lifetime: &mut syn::Lifetime) {
                    if lifetime.ident == "_" {
                        self.0 = true;
                    }
                }
            }
            let mut borrowed = Borrowed(false);
            borrowed.visit_type_mut(&mut ty.clone());
            // Opaque borrowed wrappers can have arbitrary lifetime bounds.
            // Let the extractor body and user bounds establish their traits;
            // an HRTB here can accidentally require the row to be 'static.
            if !borrowed.0 {
                generics
                    .make_where_clause()
                    .predicates
                    .push(syn::parse_quote!(#ty: ::std::cmp::Eq + ::std::hash::Hash));
            }
        }
    }
}

pub(super) struct Context<'a> {
    pub row: &'a syn::Type,
    pub id: &'a syn::Type,
    pub hasher: &'a syn::Type,
    pub vis: &'a syn::Visibility,
    pub runtime: &'a Tokens,
    pub append_only: bool,
    pub generics: &'a syn::Generics,
    pub table: &'a syn::Ident,
}

pub(super) fn generate(indexes: &[Index], context: &Context<'_>) -> Code {
    let Context {
        id,
        hasher,
        runtime,
        ..
    } = context;
    let append_only = context.append_only;
    let mut code = Code::default();
    for index in indexes {
        let name = &index.function.sig.ident;
        let extractor = format_ident!("__extract_{}", name.unraw());
        let map = format_ident!("__index_{}", name.unraw());
        let key = format_ident!("__key_{}", name.unraw());
        let hash = format_ident!("__hash_{}", name.unraw());
        let saved = format_ident!("__saved_{}", name.unraw());
        let links = format_ident!("__links_{}", name.unraw());
        let ty = &index.key;
        let mut function = index.function.clone();
        function.vis = syn::Visibility::Inherited;
        function.sig.ident = extractor.clone();
        function
            .sig
            .generics
            .params
            .push(syn::parse_quote!('__mt_row));
        let syn::FnArg::Typed(arg) = &mut function.sig.inputs[0] else {
            unreachable!()
        };
        let syn::Type::Reference(input) = &mut *arg.ty else {
            unreachable!()
        };
        let lifetime = input
            .lifetime
            .clone()
            .filter(|lt| lt.ident != "_")
            .unwrap_or_else(|| syn::parse_quote!('__mt_row));
        input.lifetime = Some(lifetime.clone());
        if let syn::ReturnType::Type(_, output) = &mut function.sig.output {
            Elision(lifetime).visit_type_mut(output);
        }
        // The DSL requires &Row even when Row is String or Vec<T>.
        code.helpers
            .push(quote!(#[allow(clippy::ptr_arg)] #function));
        let compute = |row: Tokens| {
            if index.optional {
                quote!(Self::#extractor(#row))
            } else {
                quote!(::std::option::Option::Some(Self::#extractor(#row)))
            }
        };
        let incoming = compute(quote!(&row));
        let stored = compute(quote!(
            &__rows.get(*__candidate).expect("present indexed row").row
        ));
        let current = compute(quote!(&self.__rows.get(id).expect("present row").row));
        let rehash = quote!(|__candidate: &#id| {
            let __key = #stored.expect("indexed key");
            ::std::hash::BuildHasher::hash_one(__hasher, &__key)
        });
        let find = if index.dense {
            quote!(self.#map.get(__hash).copied().flatten())
        } else if index.cached {
            quote!(self.#map.raw_entry().from_hash(__hash, |candidate| candidate.key == *__key).map(|(candidate, ())| candidate.id))
        } else {
            quote!({ let __rows = &self.__rows; self.#map.find(__hash, |__candidate| {
                #stored.is_some_and(|stored| stored == *__key)
            }).copied() })
        };
        // For noncached keys, end the borrow of the incoming row at validation.
        // Only hashes, dense positions, and IDs survive until the row is installed.
        if index.dense {
            let needed = format_ident!("__needed_{}", name.unraw());
            code.map_fields
                .push(quote!(#map: ::std::vec::Vec<::std::option::Option<#id>>));
            code.map_defaults.push(quote!(#map: ::std::vec::Vec::new()));
            code.prepare.push(
                quote!(let #key: ::std::option::Option<usize> = #incoming; let #hash = #key;),
            );
            code.checks_capacity.push(quote! {
                let #needed = #hash.map(|position| {
                    let length = position.checked_add(1).expect("table capacity exhausted");
                    if length > (isize::MAX as usize) / ::std::mem::size_of::<::std::option::Option<#id>>() {
                        panic!("table capacity exhausted");
                    }
                    length
                });
            });
            code.reserve.push(quote! {
                if let ::std::option::Option::Some(length) = #needed {
                    if length > self.#map.len() {
                        self.#map.resize(length, ::std::option::Option::None);
                    }
                }
            });
        } else if index.cached {
            code.map_fields.push(quote!(#map: #runtime::__private::HashMap<#runtime::__private::CachedKey<#ty, #id>, (), #hasher>));
            code.map_defaults.push(quote!(#map: #runtime::__private::HashMap::with_hasher(::std::clone::Clone::clone(&__hasher))));
            code.reserve.push(quote!(self.#map.reserve(1);));
            code.prepare.push(quote!(let #key = #incoming; let #hash = #key.as_ref().map(|key| ::std::hash::BuildHasher::hash_one(&self.__hasher, key));));
        } else {
            code.map_fields
                .push(quote!(#map: #runtime::__private::HashTable<#id>));
            code.map_defaults
                .push(quote!(#map: #runtime::__private::HashTable::new()));
            code.reserve.push(quote!({ let __rows = &self.__rows; let __hasher = &self.__hasher; self.#map.reserve(1, #rehash); }));
            code.prepare.push(quote!(let #key = #incoming; let #hash = #key.as_ref().map(|key| ::std::hash::BuildHasher::hash_one(&self.__hasher, key));));
        }
        // Only mutable cached indexes need stored hashes. Noncached indexes
        // derive membership and the old hash from the live row.
        let old_hash = if index.dense {
            current.clone()
        } else if index.cached {
            if !append_only {
                code.entry_fields
                    .push(quote!(#saved: ::std::option::Option<u64>));
                code.entry_defaults.push(quote!(#saved: #hash));
            }
            quote!(self.__rows.get(id).expect("present row").#saved)
        } else {
            quote!({ #current.map(|key| ::std::hash::BuildHasher::hash_one(&self.__hasher, &key)) })
        };
        if matches!(index.kind, Kind::Unique) {
            code.checks_insert.push(quote! {
                if let (::std::option::Option::Some(__hash), ::std::option::Option::Some(__key)) = (#hash, #key.as_ref()) {
                    if let ::std::option::Option::Some(existing) = #find { return ::std::option::Option::Some(existing); }
                }
            });
            code.checks_replace.push(quote! {
                if let (::std::option::Option::Some(__hash), ::std::option::Option::Some(__key)) = (#hash, #key.as_ref()) {
                    if let ::std::option::Option::Some(existing) = #find {
                        if existing != id { return ::std::option::Option::Some(#runtime::ReplaceErrorKind::Unique { field: ::std::stringify!(#name) }); }
                    }
                }
            });
        }
        let remove = if index.dense {
            quote!(self.#map[__old_hash] = ::std::option::Option::None;)
        } else if index.cached {
            quote! { match self.#map.raw_entry_mut().from_hash(__old_hash, |key| key.id == id) {
                #runtime::__private::RawEntryMut::Occupied(entry) => { entry.remove_entry(); },
                _ => unreachable!("present index"),
            } }
        } else {
            quote!(self.#map.find_entry(__old_hash, |stored| *stored == id).expect("present index").remove();)
        };
        let insert = if index.dense {
            quote!(self.#map[__hash] = ::std::option::Option::Some(id);)
        } else if index.cached {
            quote! { match self.#map.raw_entry_mut().from_hash(__hash, |_| false) {
                #runtime::__private::RawEntryMut::Vacant(entry) => { entry.insert_hashed_nocheck(__hash, #runtime::__private::CachedKey { key: __owned_key, id }, ()); },
                _ => unreachable!("vacant index"),
            } }
        } else {
            quote! { let __rows = &self.__rows; let __hasher = &self.__hasher; self.#map.insert_unique(__hash, id, #rehash); }
        };
        let change_head = |hash: Tokens, old: Tokens, new: Tokens| {
            if index.dense {
                quote!(self.#map[#hash] = ::std::option::Option::Some(#new);)
            } else if index.cached {
                quote! { match self.#map.raw_entry_mut().from_hash(#hash, |key| key.id == #old) {
                    #runtime::__private::RawEntryMut::Occupied(mut entry) => { entry.key_mut().id = #new; },
                    _ => unreachable!("present head"),
                } }
            } else {
                quote!(*self.#map.find_mut(#hash, |stored| *stored == #old).expect("present head") = #new;)
            }
        };
        match index.kind {
            Kind::Unique => {
                code.detach.push(
                    quote!(if let ::std::option::Option::Some(__old_hash) = #old_hash { #remove }),
                );
                let owned = index
                    .cached
                    .then(|| quote!(let __owned_key = #key.expect("prepared key");));
                code.commit.push(
                    quote!(if let ::std::option::Option::Some(__hash) = #hash { #owned #insert }),
                );
            }
            Kind::Many => {
                if append_only {
                    code.entry_fields.push(quote!(#links: #id));
                    code.entry_defaults.push(quote!(#links: id));
                } else {
                    code.entry_fields
                        .push(quote!(#links: #runtime::__private::Links<#id>));
                    code.entry_defaults
                        .push(quote!(#links: ::std::default::Default::default()));
                    let update = change_head(quote!(__old_hash), quote!(id), quote!(next));
                    code.detach.push(quote! {
                        if let ::std::option::Option::Some(__old_hash) = #old_hash {
                            let old = self.__rows.get(id).expect("present row");
                            let (prev, next) = (old.#links.prev, old.#links.next);
                            if let ::std::option::Option::Some(prev) = prev { self.__rows.get_mut(prev).expect("present predecessor").#links.next = next; }
                            else if let ::std::option::Option::Some(next) = next { #update } else { #remove }
                            if let ::std::option::Option::Some(next) = next { self.__rows.get_mut(next).expect("present successor").#links.prev = prev; }
                        }
                    });
                }
                let group_find = if index.dense {
                    quote!(let head = #find;)
                } else if index.cached {
                    quote!(let __owned_key = #key.expect("prepared key"); let __key = &__owned_key; let head = #find;)
                } else {
                    quote!(let head = { let __new_key = #current.expect("indexed key"); let __key = &__new_key; #find };)
                };
                let join = if append_only {
                    quote! {
                        let successor = self.__rows.get(head).expect("present head").#links;
                        self.__rows.get_mut(id).expect("present row").#links = successor;
                        self.__rows.get_mut(head).expect("present head").#links = id;
                    }
                } else {
                    let update = change_head(quote!(__hash), quote!(head), quote!(id));
                    quote! {
                        self.__rows.get_mut(id).expect("present row").#links.next = ::std::option::Option::Some(head);
                        self.__rows.get_mut(head).expect("present head").#links.prev = ::std::option::Option::Some(id);
                        #update
                    }
                };
                code.commit.push(quote! { if let ::std::option::Option::Some(__hash) = #hash { #group_find if let ::std::option::Option::Some(head) = head { #join } else { #insert } } });
            }
        }
        lookup(&mut code, index, context, &map, &extractor, &links);
    }
    code
}

fn lookup(
    code: &mut Code,
    index: &Index,
    context: &Context<'_>,
    map: &syn::Ident,
    extractor: &syn::Ident,
    links: &syn::Ident,
) {
    let Context {
        row,
        id,
        vis,
        table,
        ..
    } = context;
    let append_only = context.append_only;
    let table_generics = context.generics;
    let key_type = with_lifetime(&index.key, syn::parse_quote!('__mt_table));
    let (query_generics, parameter, bounds, equal, _captures) = if index.dense {
        (
            quote!(),
            quote!(value: &usize),
            quote!(),
            quote!(),
            quote!(),
        )
    } else if let syn::Type::Tuple(tuple) = &key_type {
        let mut queries = Vec::new();
        let mut parts = Vec::new();
        let mut comparisons = Vec::new();
        fn tuple_query(
            ty: &syn::Type,
            stored: Tokens,
            value: Tokens,
            queries: &mut Vec<syn::Ident>,
            bounds: &mut Vec<Tokens>,
            comparisons: &mut Vec<Tokens>,
        ) -> Tokens {
            if let syn::Type::Tuple(tuple) = ty {
                let fields = tuple
                    .elems
                    .iter()
                    .enumerate()
                    .map(|(i, ty)| {
                        let i = syn::Index::from(i);
                        tuple_query(
                            ty,
                            quote!(#stored.#i),
                            quote!(#value.#i),
                            queries,
                            bounds,
                            comparisons,
                        )
                    })
                    .collect::<Vec<_>>();
                quote!((#(#fields,)*))
            } else {
                let query = format_ident!("__MtQuery{}", queries.len());
                bounds.push(quote!(#ty: ::std::cmp::PartialEq<#query>, #query: ::std::hash::Hash));
                comparisons.push(quote!(#stored == #value));
                queries.push(query.clone());
                quote!(#query)
            }
        }
        let tuple_type = syn::Type::Tuple(tuple.clone());
        let query_type = tuple_query(
            &tuple_type,
            quote!(__stored_key),
            quote!(value),
            &mut queries,
            &mut parts,
            &mut comparisons,
        );
        (
            quote!(#(#queries),*),
            quote!(value: &#query_type),
            quote!(#(#parts,)*),
            quote!(true #(&& #comparisons)*),
            quote!(#(#queries,)*),
        )
    } else if has_nested_borrow(&index.key) {
        // Opaque borrowed wrappers cannot use Key<'table>: Borrow<Q>:
        // blanket Borrow implementations would unify the query's inner
        // lifetimes with the returned table borrow. Compare a short-lived
        // instance of the same key shape instead.
        let query = query_type(&index.key);
        (
            quote!('__mt_query),
            quote!(value: &#query),
            quote!('__mt_table: '__mt_query,),
            quote!(__stored_key == value),
            quote!(),
        )
    } else {
        (
            quote!(__MtQuery: ?::std::marker::Sized),
            quote!(value: &__MtQuery),
            quote!(#key_type: ::std::borrow::Borrow<__MtQuery>, __MtQuery: ::std::hash::Hash + ::std::cmp::Eq,),
            quote!(::std::borrow::Borrow::<__MtQuery>::borrow(__stored_key) == value),
            quote!(__MtQuery,),
        )
    };
    let stored = if index.optional {
        quote!(Self::#extractor(&self.__rows.get(*__candidate).expect("present indexed row").row))
    } else {
        quote!(::std::option::Option::Some(Self::#extractor(&self.__rows.get(*__candidate).expect("present indexed row").row)))
    };
    let find = if index.cached {
        quote!(self.#map.raw_entry().from_hash(__hash, |candidate| { let __stored_key = &candidate.key; #equal }).map(|(candidate, ())| candidate.id))
    } else {
        quote!(self.#map.find(__hash, |__candidate| { #stored.is_some_and(|__stored_key| { let __stored_key = &__stored_key; #equal }) }).copied())
    };
    let search = if index.dense {
        quote!(let head = self.#map.get(*value).copied().flatten();)
    } else {
        quote!(let __hash = ::std::hash::BuildHasher::hash_one(&self.__hasher, value); let head = #find;)
    };
    let name = &index.function.sig.ident;
    match index.kind {
        Kind::Unique => {
            let method = format_ident!("get_by_{}", name.unraw());
            code.lookups.push(quote! {
                #vis fn #method<'__mt_table, #query_generics>(&'__mt_table self, #parameter) -> ::std::option::Option<(#id, &'__mt_table #row)> where #bounds {
                    #search let id = head?; self.get(id).map(|row| (id, row))
                }
            });
        }
        Kind::Many => {
            let method = format_ident!("iter_by_{}", name.unraw());
            // A concrete iterator type has no query type parameter. RPIT must
            // capture all type parameters, including lifetimes nested in Q.
            let iterator = format_ident!("__{}IterBy{}", table.unraw(), name.unraw());
            let mut iterator_generics = table_generics.clone();
            iterator_generics
                .params
                .insert(0, syn::parse_quote!('__mt_table));
            let (iterator_impl, iterator_args, iterator_where) = iterator_generics.split_for_impl();
            let (_, table_args, _) = table_generics.split_for_impl();
            let head_field = append_only.then(|| quote!(head: ::std::option::Option<#id>,));
            let head_default = append_only.then(|| quote!(head,));
            let advance = if append_only {
                quote!(self.next = if ::std::option::Option::Some(entry.#links) == self.head { ::std::option::Option::None } else { ::std::option::Option::Some(entry.#links) };)
            } else {
                quote!(self.next = entry.#links.next;)
            };
            code.iterators.push(quote! {
                #[doc(hidden)]
                #[allow(non_camel_case_types)]
                #vis struct #iterator #iterator_generics #iterator_where {
                    table: &'__mt_table #table #table_args,
                    next: ::std::option::Option<#id>, #head_field
                }
                impl #iterator_impl ::std::iter::Iterator for #iterator #iterator_args #iterator_where {
                    type Item = (#id, &'__mt_table #row);
                    fn next(&mut self) -> ::std::option::Option<Self::Item> {
                        let id = self.next?;
                        let entry = self.table.__rows.get(id).expect("present indexed row");
                        #advance ::std::option::Option::Some((id, &entry.row))
                    }
                }
            });
            code.lookups.push(quote! {
                #vis fn #method<'__mt_table, #query_generics>(&'__mt_table self, #parameter) -> #iterator #iterator_args where #bounds {
                    #search #iterator { table: self, next: head, #head_default }
                }
            });
        }
    }
}
