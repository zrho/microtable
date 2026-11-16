//! Generate row-backed and cached hash indexes.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use super::{Index, IndexKey, Kind};

pub(super) struct Context<'a> {
    pub runtime: &'a TokenStream,
    pub name: &'a syn::Ident,
    pub row_args: TokenStream,
    pub id: &'a syn::Type,
    pub vis: &'a syn::Visibility,
    pub append_only: bool,
}

#[derive(Default)]
pub(super) struct Code {
    pub map_fields: Vec<TokenStream>,
    pub map_defaults: Vec<TokenStream>,
    pub entry_fields: Vec<TokenStream>,
    pub entry_defaults: Vec<TokenStream>,
    pub prepare: Vec<TokenStream>,
    pub checks_insert: Vec<TokenStream>,
    pub checks_replace: Vec<TokenStream>,
    pub reserve: Vec<TokenStream>,
    pub groups_insert: Vec<TokenStream>,
    pub keys_insert: Vec<TokenStream>,
    pub keys_replace: Vec<TokenStream>,
    pub detach_replace: Vec<TokenStream>,
    pub detach_remove: Vec<TokenStream>,
    pub commit: Vec<TokenStream>,
    pub lookups: Vec<TokenStream>,
}

// Bind a borrowed key from a stored row. Computed keys in non-cached indexes
// are temporary values; field/composite keys borrow their components directly.
fn stored_key(index: &Index, context: &Context<'_>) -> TokenStream {
    let Context { name, row_args, .. } = context;
    let compute = match &index.key {
        IndexKey::Computed { ty, expression } => quote! {
            let __compute: fn(&#name #row_args) -> #ty = #expression;
            let __computed = __compute(__stored_row);
        },
        _ => quote!(),
    };
    let borrowed = index.borrowed(&quote!(__stored_row), &quote!(__computed));
    quote! {
        #compute
        let __stored_key = #borrowed;
    }
}

impl Code {
    pub fn generate(indexes: &[Index], context: &Context<'_>) -> Self {
        let mut code = Self::default();
        for index in indexes {
            code.add(index, context);
        }
        code
    }

    fn add(&mut self, index: &Index, context: &Context<'_>) {
        let Context {
            runtime,
            name,
            row_args,
            id,
            ..
        } = context;
        let field = &index.name;
        let ty = index.key_type();
        let map = format_ident!("__index_{}", field.unraw());
        let hash = format_ident!("__hash_{}", field.unraw());
        let saved_hash = format_ident!("__saved_hash_{}", field.unraw());
        let computed = format_ident!("__computed_{}", field.unraw());
        let key = format_ident!("__key_{}", field.unraw());
        let changed = format_ident!("__changed_{}", field.unraw());
        let links = format_ident!("__links_{}", field.unraw());
        let derive_stored = stored_key(index, context);
        let borrowed = index.borrowed(&quote!(row), &quote!(#computed));
        let candidate = if index.cached {
            quote!(let __stored_key = &__candidate.key;)
        } else {
            quote! {
                let __stored_row = &self.__rows.get(*__candidate).expect("present indexed row").row;
                #derive_stored
            }
        };
        let equal = if index.cached {
            index.equivalent(&quote!(row), &quote!(#computed))
        } else {
            quote!(__stored_key == #borrowed)
        };
        let find_new = if index.cached {
            quote!(self.#map.raw_entry().from_hash(#hash, |__candidate| {
                #candidate
                #equal
            }).map(|(key, ())| key.id))
        } else {
            quote!(self.#map.find(#hash, |__candidate| {
                #candidate
                #equal
            }).copied())
        };
        // Borrow the arena and hasher separately from the mutable hash table.
        let rehash = quote!(|__id: &#id| {
            let __stored_row = &__rows.get(*__id).expect("present indexed row").row;
            #derive_stored
            ::std::hash::BuildHasher::hash_one(__hasher, __stored_key)
        });
        if index.cached {
            self.map_fields.push(quote! {
                #map: #runtime::__private::HashMap<#runtime::__private::CachedKey<#ty, #id>, (), ::std::collections::hash_map::RandomState>
            });
            self.map_defaults
                .push(quote!(#map: #runtime::__private::HashMap::with_hasher(__hasher.clone())));
            if !context.append_only {
                self.entry_fields.push(quote!(#saved_hash: u64));
                self.entry_defaults.push(quote!(#saved_hash: #hash));
            }
            self.reserve.push(quote!(self.#map.reserve(1);));
        } else {
            self.map_fields
                .push(quote!(#map: #runtime::__private::HashTable<#id>));
            self.map_defaults
                .push(quote!(#map: #runtime::__private::HashTable::new()));
            self.reserve.push(quote! {
                {
                    let __rows = &self.__rows;
                    let __hasher = &self.__hasher;
                    self.#map.reserve(1, #rehash);
                }
            });
        }
        if let IndexKey::Computed { ty, expression } = &index.key {
            self.prepare.push(quote! {
                let #computed: #ty = {
                    let __compute: fn(&#name #row_args) -> #ty = #expression;
                    __compute(&row)
                };
            });
        }
        self.prepare.push(quote! {
            let #hash = ::std::hash::BuildHasher::hash_one(&self.__hasher, #borrowed);
        });
        let old_hash = if index.cached {
            quote!(self.__rows.get(id).expect("present row").#saved_hash)
        } else {
            quote!({
                let __rows = &self.__rows;
                let __hasher = &self.__hasher;
                (#rehash)(&id)
            })
        };
        let remove = if index.cached {
            quote! {
                match self.#map.raw_entry_mut().from_hash(__old_hash, |key| key.id == id) {
                    #runtime::__private::RawEntryMut::Occupied(entry) => { entry.remove_entry(); }
                    #runtime::__private::RawEntryMut::Vacant(_) => ::std::unreachable!("present index"),
                }
            }
        } else {
            quote! {
                self.#map.find_entry(__old_hash, |stored| *stored == id)
                    .expect("present index").remove();
            }
        };
        let new_key = match &index.key {
            IndexKey::Computed { .. } => quote!(#computed),
            _ => index.cloned(&quote!(row), &quote!(#computed)),
        };
        let insert = if index.cached {
            quote! {
                match self.#map.raw_entry_mut().from_hash(#hash, |_| false) {
                    #runtime::__private::RawEntryMut::Vacant(entry) => {
                        entry.insert_hashed_nocheck(#hash, #runtime::__private::CachedKey {
                            key: #key.expect("prepared key"), id,
                        }, ());
                    }
                    #runtime::__private::RawEntryMut::Occupied(_) => ::std::unreachable!("vacant index"),
                }
            }
        } else {
            quote! {
                let __rows = &self.__rows;
                let __hasher = &self.__hasher;
                self.#map.insert_unique(#hash, id, #rehash);
            }
        };
        match index.kind {
            Kind::Unique => {
                self.checks_insert.push(quote! {
                    if (#find_new).is_some() {
                        return ::std::result::Result::Err(#runtime::InsertError { row, kind: #runtime::InsertErrorKind::Unique { field: ::std::stringify!(#field) } });
                    }
                });
                self.checks_replace.push(quote! {
                    let #changed = match #find_new {
                        ::std::option::Option::Some(existing) if existing != id => {
                            return ::std::result::Result::Err(#runtime::ReplaceError { row, kind: #runtime::ReplaceErrorKind::Unique { field: ::std::stringify!(#field) } });
                        }
                        ::std::option::Option::Some(_) => false,
                        ::std::option::Option::None => true,
                    };
                });
                self.keys_insert.push(quote!(let #changed = true;));
                if index.cached {
                    self.keys_insert
                        .push(quote!(let #key = ::std::option::Option::Some(#new_key);));
                    self.keys_replace.push(quote! {
                        let #key = if #changed { ::std::option::Option::Some(#new_key) } else { ::std::option::Option::None };
                    });
                }
                let detach = quote! {
                    let __old_hash = #old_hash;
                    #remove
                };
                self.detach_remove.push(quote!({ #detach }));
                self.detach_replace.push(quote!(if #changed { #detach }));
                self.commit.push(quote!(if #changed { #insert }));
            }
            Kind::Many if context.append_only => {
                let head = format_ident!("__head_{}", field.unraw());
                let successor = format_ident!("__successor_{}", field.unraw());
                self.entry_fields.push(quote!(#links: #id));
                self.entry_defaults
                    .push(quote!(#links: #successor.unwrap_or(id)));
                self.groups_insert.push(quote! {
                    let #head = #find_new;
                    let #successor = #head.map(|head| self.__rows.get(head).expect("present representative").#links);
                });
                if index.cached {
                    self.keys_insert.push(quote! {
                        let #key = if #head.is_none() {
                            ::std::option::Option::Some(#new_key)
                        } else {
                            ::std::option::Option::None
                        };
                    });
                }
                self.commit.push(quote! {
                    if let ::std::option::Option::Some(head) = #head {
                        self.__rows.get_mut(head).expect("present representative").#links = id;
                    } else {
                        #insert
                    }
                });
            }
            Kind::Many => {
                self.entry_fields
                    .push(quote!(#links: #runtime::__private::Links<#id>));
                self.entry_defaults
                    .push(quote!(#links: ::std::default::Default::default()));
                if index.cached {
                    let prepare = quote!(let #key = ::std::option::Option::Some(#new_key););
                    self.keys_insert.push(prepare.clone());
                    self.keys_replace.push(prepare);
                }
                let update_head = if index.cached {
                    quote! {
                        match self.#map.raw_entry_mut().from_hash(__old_hash, |key| key.id == id) {
                            #runtime::__private::RawEntryMut::Occupied(mut entry) => { entry.key_mut().id = next; }
                            #runtime::__private::RawEntryMut::Vacant(_) => ::std::unreachable!("present head"),
                        }
                    }
                } else {
                    quote! {
                        *self.#map.find_mut(__old_hash, |stored| *stored == id).expect("present head") = next;
                    }
                };
                let detach = quote! {
                    {
                        let old = self.__rows.get(id).expect("present row");
                        let (prev, next) = (old.#links.prev, old.#links.next);
                        if let ::std::option::Option::Some(prev) = prev {
                            self.__rows.get_mut(prev).expect("present predecessor").#links.next = next;
                        } else {
                            let __old_hash = #old_hash;
                            if let ::std::option::Option::Some(next) = next {
                                #update_head
                            } else {
                                #remove
                            }
                        }
                        if let ::std::option::Option::Some(next) = next {
                            self.__rows.get_mut(next).expect("present successor").#links.prev = prev;
                        }
                    }
                };
                self.detach_remove.push(detach.clone());
                self.detach_replace.push(detach);
                // Find the new group after detaching the previous membership.
                // Cached keys are prepared owned values; non-cached keys come
                // from the newly installed arena row.
                let head = if index.cached {
                    quote! {
                        let head = self.#map.raw_entry().from_hash(#hash, |stored| {
                            stored.key == *#key.as_ref().expect("prepared key")
                        }).map(|(stored, ())| stored.id);
                    }
                } else {
                    let new_stored = stored_key(index, context);
                    quote! {
                        let head = {
                            let __stored_row = &self.__rows.get(id).expect("present row").row;
                            #new_stored
                            let __new_key = __stored_key;
                            self.#map.find(#hash, |__candidate| {
                                #candidate
                                __stored_key == __new_key
                            }).copied()
                        };
                    }
                };
                let update_new_head = if index.cached {
                    quote! {
                        match self.#map.raw_entry_mut().from_hash(#hash, |key| key.id == head) {
                            #runtime::__private::RawEntryMut::Occupied(mut entry) => { entry.key_mut().id = id; }
                            #runtime::__private::RawEntryMut::Vacant(_) => ::std::unreachable!("present head"),
                        }
                    }
                } else {
                    quote!(*self.#map.find_mut(#hash, |stored| *stored == head).expect("present head") = id;)
                };
                self.commit.push(quote! {
                    {
                        #head
                        self.__rows.get_mut(id).expect("present row").#links.next = head;
                        if let ::std::option::Option::Some(head) = head {
                            #update_new_head
                            self.__rows.get_mut(head).expect("present head").#links.prev = ::std::option::Option::Some(id);
                        } else {
                            #insert
                        }
                    }
                });
            }
        }
        self.lookup(index, context, &map, &candidate);
    }

    fn lookup(
        &mut self,
        index: &Index,
        context: &Context<'_>,
        map: &syn::Ident,
        candidate: &TokenStream,
    ) {
        let Context {
            name,
            row_args,
            id,
            vis,
            ..
        } = context;
        let ty = index.key_type();
        let (generics, arguments, bounds, query_hash, equal) = match &index.key {
            IndexKey::Fields(fields) if fields.len() > 1 => {
                let queries: Vec<_> = (0..fields.len())
                    .map(|i| format_ident!("__MinitableQuery{i}"))
                    .collect();
                let args: Vec<_> = (0..fields.len())
                    .map(|i| format_ident!("__value_{i}"))
                    .collect();
                let parameters = queries
                    .iter()
                    .zip(&args)
                    .map(|(query, arg)| quote!(#arg: &#query));
                let bounds = fields.iter().zip(&queries).map(|((_, ty), query)| quote!(#ty: ::std::borrow::Borrow<#query>, #query: ::std::hash::Hash + ::std::cmp::Eq));
                let comparisons = queries
                    .iter()
                    .zip(&args)
                    .enumerate()
                    .map(|(i, (query, arg))| {
                        let i = syn::Index::from(i);
                        let component = if index.cached {
                            quote!(&__stored_key.#i)
                        } else {
                            quote!(__stored_key.#i)
                        };
                        quote!(::std::borrow::Borrow::<#query>::borrow(#component) == #arg)
                    });
                (
                    quote!(#(#queries: ?::std::marker::Sized),*),
                    quote!(#(#parameters),*),
                    quote!(#(#bounds),*),
                    quote!((#(#args,)*)),
                    quote!(#(#comparisons)&&*),
                )
            }
            _ => (
                quote!(__MinitableQuery: ?::std::marker::Sized),
                quote!(value: &__MinitableQuery),
                quote!(#ty: ::std::borrow::Borrow<__MinitableQuery>, __MinitableQuery: ::std::hash::Hash + ::std::cmp::Eq),
                quote!(value),
                quote!(::std::borrow::Borrow::<__MinitableQuery>::borrow(__stored_key) == value),
            ),
        };
        let find = if index.cached {
            quote!(self.#map.raw_entry().from_hash(__hash, |__candidate| { #candidate #equal }).map(|(key, ())| key.id))
        } else {
            quote!(self.#map.find(__hash, |__candidate| { #candidate #equal }).copied())
        };
        let find = quote!({
            let __hash = ::std::hash::BuildHasher::hash_one(&self.__hasher, #query_hash);
            #find
        });
        match index.kind {
            Kind::Unique => {
                let method = format_ident!("get_by_{}", index.name.unraw());
                self.lookups.push(quote! {
                    /// Find a row through a unique index, with borrowed key support.
                    #vis fn #method<#generics>(&self, #arguments) -> ::std::option::Option<(#id, &#name #row_args)>
                    where #bounds {
                        let id = #find?;
                        self.get(id).map(|row| (id, row))
                    }
                });
            }
            Kind::Many => {
                let method = format_ident!("iter_by_{}", index.name.unraw());
                let links = format_ident!("__links_{}", index.name.unraw());
                let initialize = if context.append_only {
                    quote!(let start = #find; let mut next = start;)
                } else {
                    quote!(let mut next = #find;)
                };
                let advance = if context.append_only {
                    quote! {
                        next = if ::std::option::Option::Some(entry.#links) == start {
                            ::std::option::Option::None
                        } else {
                            ::std::option::Option::Some(entry.#links)
                        };
                    }
                } else {
                    quote!(next = entry.#links.next;)
                };
                self.lookups.push(quote! {
                    /// Iterate over matching rows in unspecified order.
                    #vis fn #method<#generics>(&self, #arguments) -> impl ::std::iter::Iterator<Item = (#id, &#name #row_args)> + '_
                    where #bounds {
                        #initialize
                        ::std::iter::from_fn(move || {
                            let id = next?;
                            let entry = self.__rows.get(id).expect("present indexed row");
                            #advance
                            ::std::option::Option::Some((id, &entry.row))
                        })
                    }
                });
            }
        }
    }
}

use syn::ext::IdentExt;
