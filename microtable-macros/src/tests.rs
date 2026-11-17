//! Parser and generated-signature diagnostics.
use super::*;

fn parse(tokens: Tokens) -> syn::Result<Declaration> {
    syn::parse2(tokens)
}

#[test]
fn parses_external_rows_shared_generics_and_options() {
    let declaration = parse(quote! {
        #[derive(Debug, Clone)]
        #[table(append_only, hasher = std::hash::BuildHasherDefault<std::collections::hash_map::DefaultHasher>)]
        pub struct People<'a, T, const N: usize>([external::Person<'a, T, N>]) where T: Eq;
        #[id] pub struct PersonId(u16);
        #[index(unique, optional)]
        fn name(row: &external::Person<'a, T, N>) -> Option<(&T, &'a str)> { row.name() }
        #[index(many, cached)]
        fn group(row: &external::Person<'a, T, N>) -> &'a str { row.group() }
    }).unwrap();
    assert!(declaration.append_only);
    assert_eq!(declaration.indexes.len(), 2);
    assert!(declaration.indexes[0].optional);
    assert!(declaration.indexes[1].cached);
    assert!(!declaration.indexes[0].cached);
}

#[test]
fn invalid_declarations_have_specific_diagnostics() {
    let cases = [
        (
            quote!(
                struct Rows {
                    row: Row,
                }
            ),
            "expected struct Table([Row])",
        ),
        (
            quote!(
                struct Rows(Row);
            ),
            "expected [Row]",
        ),
        (
            quote!(
                struct Rows([Row], [Row]);
            ),
            "exactly one",
        ),
        (
            quote!(
                struct Rows(pub [Row]);
            ),
            "visibility",
        ),
        (
            quote!(
                #[table(wrong)]
                struct Rows([Row]);
            ),
            "expected append_only",
        ),
        (
            quote!(
                #[table(append_only, append_only)]
                struct Rows([Row]);
            ),
            "duplicate table option",
        ),
        (
            quote!(
                struct Rows([Row]);
                struct Id(u32);
            ),
            "requires #[id]",
        ),
        (
            quote!(
                struct Rows([Row]);
                #[id]
                struct Id(u64);
            ),
            "u8, u16, u32, or usize",
        ),
        (
            quote!(
                struct Rows([Row]);
                #[id]
                struct Id(pub u8);
            ),
            "private",
        ),
        (
            quote!(
                struct Rows([Row]);
                #[id]
                struct Id<T>(u8);
            ),
            "cannot have generics",
        ),
        (
            quote!(
                struct Rows([Row]);
                #[id]
                struct Id(u8);
                #[id]
                struct Other(u8);
            ),
            "duplicate ID",
        ),
        (
            quote!(
                struct Rows([Row]);
                fn key(row: &Row) -> u8 {
                    0
                }
            ),
            "requires #[index",
        ),
        (
            quote!(
                struct Rows([Row]);
                #[index(cached)]
                fn key(row: &Row) -> u8 {
                    0
                }
            ),
            "requires unique or many",
        ),
        (
            quote!(
                struct Rows([Row]);
                #[index(unique, many)]
                fn key(row: &Row) -> u8 {
                    0
                }
            ),
            "either unique or many",
        ),
        (
            quote!(
                struct Rows([Row]);
                #[index(unique, cached, cached)]
                fn key(row: &Row) -> u8 {
                    0
                }
            ),
            "duplicate index option",
        ),
        (
            quote!(
                struct Rows([Row]);
                #[index(unique, optional)]
                fn key(row: &Row) -> u8 {
                    0
                }
            ),
            "must return Option<Key>",
        ),
        (
            quote!(
                struct Rows([Row]);
                #[index(unique)]
                fn key<T>(row: &Row) -> T {
                    todo!()
                }
            ),
            "share table generics",
        ),
        (
            quote!(
                struct Rows([Row]);
                #[index(unique)]
                async fn key(row: &Row) -> u8 {
                    0
                }
            ),
            "ordinary safe function",
        ),
        (
            quote!(
                struct Rows([Row]);
                #[index(unique)]
                fn key(row: &mut Row) -> u8 {
                    0
                }
            ),
            "immutable reference",
        ),
        (
            quote!(
                struct Rows([Row]);
                #[index(unique)]
                fn key(row: &Other) -> u8 {
                    0
                }
            ),
            "declared row type",
        ),
        (
            quote!(
                struct Rows([Row]);
                #[index(unique)]
                fn key(row: Row) -> u8 {
                    0
                }
            ),
            "must be &Row",
        ),
        (
            quote!(
                struct Rows([Row]);
                #[index(unique)]
                fn key() -> u8 {
                    0
                }
            ),
            "exactly one argument",
        ),
        (
            quote!(
                struct Rows([Row]);
                #[index(unique)]
                fn key(row: &Row) {}
            ),
            "explicit key return type",
        ),
        (
            quote!(
                struct Rows([Row]);
                #[index(unique)]
                fn new(row: &Row) -> u8 {
                    0
                }
            ),
            "generated table method",
        ),
        (
            quote!(
                struct Rows([Row]);
                #[index(unique)]
                fn key(row: &Row) -> u8 {
                    0
                }
                #[index(many)]
                fn key(row: &Row) -> u8 {
                    0
                }
            ),
            "duplicate index name",
        ),
        (
            quote!(
                struct Rows([Row]);
                #[index(unique)]
                fn key(row: &Row) -> u8 {
                    0
                }
                #[index(many)]
                fn get_by_key(row: &Row) -> u8 {
                    0
                }
            ),
            "generated lookup method",
        ),
        (
            quote!(
                struct Rows<'__mt_table>([Row]);
            ),
            "reserved",
        ),
        (
            quote!(
                struct Rows<__MtQuery>([Row]);
            ),
            "reserved",
        ),
        (
            quote!(
                struct Rows([Row]);
                #[index(unique, cached)]
                fn key(row: &Row) -> &str {
                    &row.name
                }
            ),
            "cannot borrow",
        ),
        (
            quote!(
                struct Rows([Row]);
                #[index(unique, cached)]
                fn key(row: &Row) -> Key<'_> {
                    row.key()
                }
            ),
            "cannot borrow",
        ),
    ];
    for (tokens, expected) in cases {
        let error = match parse(tokens.clone()) {
            Ok(_) => panic!("accepted invalid declaration: {tokens}"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains(expected),
            "expected {expected:?}, got {error} in {tokens}"
        );
    }
}

#[test]
fn bounds_do_not_put_elided_borrowed_keys_on_the_table() {
    let mut generics = syn::Generics::default();
    indexes::add_key_bounds(&mut generics, &syn::parse_quote!(Vec<&str>));
    indexes::add_key_bounds(&mut generics, &syn::parse_quote!(BorrowedKey<'_>));
    assert!(generics.where_clause.is_none());
    indexes::add_key_bounds(&mut generics, &syn::parse_quote!((&T, Option<&str>)));
    let where_clause = generics.where_clause.unwrap();
    let output = quote!(#where_clause).to_string();
    assert!(!output.contains("static"));
    assert!(!output.contains("for <"));
    assert!(output.contains("T :"));
}
