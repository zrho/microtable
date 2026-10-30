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

Declare named indexes on the struct to combine fields or compute keys:

```rust
#[minitable::table]
#[index(name = tenant_email, unique, fields(tenant, email))]
#[index(name = tenant_team, many, fields(tenant, team))]
#[index(name = normalized_email, unique, key_type = String,
        key = |row| format!("{}:{}", row.tenant, row.email.to_ascii_lowercase()))]
#[derive(Debug)]
struct Account {
    tenant: u32,
    email: String,
    team: String,
}

let mut accounts = AccountTable::new();
let id = accounts.insert(Account {
    tenant: 1,
    email: "Ada@example.com".into(),
    team: "engineering".into(),
}).unwrap();
assert_eq!(accounts.get_by_tenant_email(&1, "Ada@example.com").unwrap().0, id);
assert_eq!(accounts.iter_by_tenant_team(&1, "engineering").count(), 1);
assert_eq!(accounts.get_by_normalized_email("1:ada@example.com").unwrap().0, id);
```

Named indexes support both `unique` and `many`, and coexist with field attributes.
Names must be distinct from every other index name, including field indexes.
`fields(...)` accepts one or more distinct field names; several fields are stored
as a tuple key. Composite lookups take one borrowed argument per field, in the
declared order, with no heap allocation or component clones. Each component
requires `Clone + Eq + Hash`.

Computed indexes require an explicit `key_type` and a `key` function path or
non-capturing closure, coerced to `fn(&Row) -> Key`. Only the resulting key type
needs `Clone + Eq + Hash`; source fields need no additional bounds. Computations
should be deterministic functions of the row. Each result is evaluated once per
attempted write that reaches index validation and cached in the private wrapper.
Removal and lookup do not rerun computations. Keys may borrow data with an
existing row lifetime, but cannot borrow from the row's owned fields. A computed
tuple key is queried using a reference to that tuple; separate borrowed arguments
are generated for `fields(...)` declarations.

Insertion errors expose the rejected `row` and a `kind`:
`InsertErrorKind::Unique { field }` or `InsertErrorKind::CapacityExhausted`.
Replacement errors similarly expose the rejected row, with
`ReplaceErrorKind::Unique { field }` or `ReplaceErrorKind::NotFound`.
For a named index, `field` contains the index name. Field indexes are validated
in field order, then named indexes in declaration order.
Every returned error leaves the table unchanged. Replacements can keep their
own unique values. Error types implement `Debug` when the row does, so using
`unwrap()` on a write result requires a `Debug` row.

Indexes use `indexmap::IndexMap` with `std::hash::RandomState`. Unique constraints
are checked through borrowed raw entries, retaining the computed hash for
insertion. Keys are cloned only after all checks pass; replacements also skip
cloning unchanged unique keys. Generated code does not clone index keys on
rejected writes; user-provided computed-key functions may themselves allocate
or clone while producing their result.
Index entries use `swap_remove` for expected constant-time deletion; their
positions are independent of the arena-based row IDs. Iteration order remains
unspecified.

Each many index stores a hash map from a distinct field value to a list head.
The doubly linked list's previous/next IDs live in a private wrapper around each
row, with independent links for each many index. There is no vector or set of IDs
allocated for each value. Insert, remove, replace, and unique lookup take expected
constant time per index, excluding field hashing/cloning costs. A many lookup
takes expected constant time to find the head, then constant time per matching
row. Full-table iteration scans allocated slots, including holes. Iteration order
is unspecified.

Rows occupy reusable slots maintained by a free list. Removing one row never
shifts another row's ID. The macro selects separate generational or
non-generational arena implementations at compile time. Occupancy is stored in
a `bitvec` bitmap, using one bit per allocated slot. A slot is a union of the row
and a one-word free-list link, so vacant slots do not reserve additional link
space alongside a row. Only the generational arena stores a separate `u64`
generation vector; the non-generational arena has no generation metadata or
generation checks. Unsafe union access is confined to the shared slot-storage
module, which owns occupancy tracking and drops each live row exactly once,
including cleanup after a row destructor panics.

Generational IDs contain the configured slot integer
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

The API supports named-field structs with field, composite, and computed indexes.
Conditionally compiled fields are rejected. Serialization and mutable row
references are outside the current API.

Run the checks with `cargo test --workspace`, `cargo fmt --all --check`, and
`cargo clippy --workspace --all-targets -- -D warnings`.

The arena tests also pass with
`MIRIFLAGS=-Zmiri-tree-borrows cargo +nightly miri test -p minitable --lib`.
With `bitvec` 1.1.1, default Miri reports a Stacked Borrows violation inside
`BitVec::push`, independently reproducible without an arena. Tree Borrows passes
the arena tests, but `bitvec` still emits an integer-to-pointer provenance
warning, which limits Miri's validation coverage.
