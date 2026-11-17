//! Attribute syntax and option decoding.

use darling::{FromMeta, ast::NestedMeta, util::Flag};
use proc_macro2::TokenStream;
use quote::ToTokens;
use syn::{parse::Parse, punctuated::Punctuated, spanned::Spanned};

#[derive(Default, FromMeta)]
pub(super) struct TableOptions {
    #[darling(default)]
    pub append_only: bool,
    pub table: Option<syn::Ident>,
    pub id: Option<syn::Type>,
    pub hasher: Option<syn::Type>,
    pub id_from_index: Option<syn::Expr>,
    pub id_to_index: Option<syn::Expr>,
}

#[derive(Default, FromMeta)]
pub(super) struct IndexMode {
    pub unique: Flag,
    pub many: Flag,
    pub cached: Option<bool>,
}

impl IndexMode {
    pub fn kind(&self, span: proc_macro2::Span) -> syn::Result<super::Kind> {
        match (self.unique.is_present(), self.many.is_present()) {
            (true, false) => Ok(super::Kind::Unique),
            (false, true) => Ok(super::Kind::Many),
            (true, true) => Err(syn::Error::new(span, "choose either unique or many")),
            (false, false) => Err(syn::Error::new(span, "index requires unique or many")),
        }
    }
}

#[derive(FromMeta)]
pub(super) struct IndexOptions {
    pub name: syn::Ident,
    #[darling(flatten)]
    pub mode: IndexMode,
    pub fields: Option<FieldList>,
    pub key_type: Option<syn::Type>,
    pub key: Option<syn::Expr>,
}

pub(super) struct FieldList(pub Vec<syn::Ident>);

impl FromMeta for FieldList {
    fn from_list(items: &[NestedMeta]) -> darling::Result<Self> {
        items
            .iter()
            .map(|item| match item {
                NestedMeta::Meta(syn::Meta::Path(path)) => path
                    .get_ident()
                    .cloned()
                    .ok_or_else(|| darling::Error::custom("expected a field name").with_span(path)),
                _ => Err(darling::Error::custom("expected a field name").with_span(item)),
            })
            .collect::<darling::Result<Vec<_>>>()
            .map(Self)
    }
}

// Rust types need not be valid expressions (for example, `&'a str` or
// `Wrapper<T>`). Convert bare type values to the string form that darling's
// FromMeta implementation for syn::Type accepts. All option decoding and
// duplicate/unknown option checks remain in darling.
struct OptionMeta(syn::Meta);

impl Parse for OptionMeta {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let fork = input.fork();
        let path: syn::Path = fork.parse()?;
        if (path.is_ident("id") || path.is_ident("key_type") || path.is_ident("hasher"))
            && fork.peek(syn::Token![=])
        {
            let path: syn::Path = input.parse()?;
            let eq_token = input.parse()?;
            let ty: syn::Type = input.parse()?;
            let value = syn::LitStr::new(&ty.to_token_stream().to_string(), ty.span());
            Ok(Self(syn::Meta::NameValue(syn::MetaNameValue {
                path,
                eq_token,
                value: syn::parse_quote!(#value),
            })))
        } else {
            input.parse().map(Self)
        }
    }
}

pub(super) fn parse<T: FromMeta>(tokens: TokenStream) -> syn::Result<T> {
    use syn::parse::Parser;

    let items = Punctuated::<OptionMeta, syn::Token![,]>::parse_terminated
        .parse2(tokens)?
        .into_iter()
        .map(|option| NestedMeta::Meta(option.0))
        .collect::<Vec<_>>();
    T::from_list(&items).map_err(Into::into)
}

pub(super) fn parse_attribute<T: FromMeta>(attr: &syn::Attribute) -> syn::Result<T> {
    let list = attr.meta.require_list()?;
    parse(list.tokens.clone())
}
