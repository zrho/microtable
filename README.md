# minitable

A Rust library that generates tables with stable, typed row IDs and hash-based
field indexes. The workspace contains `minitable` (runtime storage and errors)
and `minitable-macros` (the attribute macro, re-exported by `minitable`).

```rust
#[minitable::table(index_type = u32)]
#[derive(Debug)]
pub struct Person {
    #[indexed(unique)]
    email: String,
    #[indexed(many)]
    team: String,
    name: String,
}

let mut people = PersonTable::new();
let id: PersonId = people.insert(Person {
    email: "ada@example.com".into(),
    team: "engineering".into(),
    name: "Ada".into(),
}).unwrap();

assert_eq!(people.get(id).unwrap().name, "Ada");
assert_eq!(people.get_by_email("ada@example.com").unwrap().0, id);
for (id, person) in people.iter_by_team("engineering") {
    println!("{id:?}: {}", person.name);
}

// Replacement preserves the ID and checks all unique constraints first.
let previous = people.replace(id, Person {
    email: "ada@example.com".into(),
    team: "research".into(),
    name: "Ada".into(),
}).unwrap();
assert_eq!(previous.team, "engineering");
assert_eq!(people.iter_by_team("engineering").count(), 0);
assert_eq!(people.iter_by_team("research").count(), 1);

people.remove(id).unwrap();
assert!(people.get(id).is_none());
```

The macro preserves the original struct and its ordinary attributes. It generates
`PersonTable` and `PersonId` with the struct's visibility. Rows can have lifetimes,
type parameters, const parameters, and where clauses.

Configure the generated types with:

```rust
#[minitable::table(
    table = People,
    id = PersonKey,
    index_type = u16,
    generational = false,
)]
struct Person {
    name: String,
}
```

Defaults are `<Struct>Table`, `<Struct>Id`, `usize`, and `generational = true`.
`index_type` accepts primitive signed or unsigned integer types. Only nonnegative
slot indices are used, and storage is also limited by the platform's `usize`.
For example, `u8` supports 256 allocated slots, while `i8` supports 128.

Generated tables provide:

| Method | Result |
| --- | --- |
| `new()`, `default()` | Empty table |
| `len()`, `is_empty()` | Live row count |
| `insert(row)` | `Result<Id, InsertError<Row>>` |
| `get(id)` | `Option<&Row>` |
| `contains(id)` | Whether the ID currently exists |
| `replace(id, row)` | `Result<Row, ReplaceError<Row>>`, returning the old row |
| `remove(id)` | `Option<Row>` |
| `iter()` | Iterator of `(Id, &Row)` |
| `get_by_<field>(&key)` | `Option<(Id, &Row)>` for a unique index |
| `iter_by_<field>(&key)` | Iterator of `(Id, &Row)` for a many index |

Indexed fields require `Clone + Eq + Hash`. Non-indexed fields have no additional
bounds. Lookups support borrowed keys through `Borrow`, such as a `str` lookup
against a `String` field. Row access is immutable; change rows through `replace`.

Insertion errors expose the rejected `row` and a `kind`:
`InsertErrorKind::Unique { field }` or `InsertErrorKind::CapacityExhausted`.
Replacement errors similarly expose the rejected row, with
`ReplaceErrorKind::Unique { field }` or `ReplaceErrorKind::NotFound`.
Every returned error leaves the table unchanged. Replacements can keep their
own unique values. Error types implement `Debug` when the row does, so using
`unwrap()` on a write result requires a `Debug` row.

Indexes use `hashbrown` maps with `std::hash::RandomState`. Unique constraints
are checked through borrowed raw entries, retaining the computed hash for
insertion. Keys are cloned only after all checks pass; replacements also skip
cloning unchanged unique keys. Rejected writes do not clone index keys.

Each many index stores a hash map from a distinct field value to a list head.
The doubly linked list's previous/next IDs live in a private wrapper around each
row, with independent links for each many index. There is no vector or set of IDs
allocated for each value. Insert, remove, replace, and unique lookup take expected
constant time per index, excluding field hashing/cloning costs. A many lookup
takes expected constant time to find the head, then constant time per matching
row. Full-table iteration scans allocated slots, including holes. Iteration order
is unspecified.

Rows occupy reusable slots maintained by a free list. Removing one row never
shifts another row's ID. Generational IDs contain the configured slot integer
and a separate `u64` generation; a removed ID cannot access its replacement. A
slot is permanently retired if its generation counter is exhausted. Without
generations, an ID contains only the configured integer, and an old ID can refer
to a replacement row after slot reuse. `id.index()` exposes the backing integer;
generational IDs also provide `id.generation()`.

IDs are local to a table instance: they carry no table-instance identifier and
must not be used to address a different instance. Index keys must retain stable
hash/equality behavior while stored, including through interior mutability.
User-defined hashing, equality, and cloning should not panic; table consistency
after a panic in those operations is not guaranteed.

The initial API supports named-field structs and single-field indexes.
Conditionally compiled fields are rejected. Composite indexes, serialization,
and mutable row references are outside the current API.

Run the checks with `cargo test --workspace`, `cargo fmt --all --check`, and
`cargo clippy --workspace --all-targets -- -D warnings`.
