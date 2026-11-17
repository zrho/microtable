# microtable

A Rust library that generates tables with stable row IDs and hash-based
indexes. The workspace contains `microtable` (runtime storage and errors)
and `microtable-macros` (the attribute macro, re-exported by `microtable`).

```rust
#[microtable::table]
#[derive(Debug)]
pub struct Person {
    #[indexed(unique)]
    email: String,
    #[indexed(many)]
    team: String,
    name: String,
}

let mut people = PersonTable::new();
let id: usize = people.insert(Person {
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

The macro preserves the original row type and its ordinary attributes. It generates
`PersonTable` with the row type's visibility. Rows can have lifetimes,
type parameters, const parameters, and where clauses.

Configure the table and use a custom ID type with:

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
struct PersonKey(usize);

#[microtable::table(
    table = People,
    id = PersonKey,
    id_from_index = |index| PersonKey(index),
    id_to_index = |id| id.0,
)]
struct Person {
    name: String,
}
```

The table name defaults to `<Row>Table`. Row IDs default to `usize`.
A custom ID requires both `id_from_index: fn(usize) -> Id` and
`id_to_index: fn(Id) -> usize`. Each callback accepts a function path, such as
`PersonKey::new`, or a non-capturing closure. The ID type must implement
`Copy + Eq`. Both callbacks are infallible: they must be deterministic,
nonpanicking inverses for all valid slot indices. There are no automatic
capacity limits for narrow custom IDs. The mapping must support every slot
index that the table uses. Slot offsets, lengths, and free-list links use `usize`;
row references in indexes and list links use the ID type.

Generated tables provide:

| Method | Result |
| --- | --- |
| `new()`, `default()` | Empty table |
| `len()`, `is_empty()` | Live row count |
| `insert(row)` | `Result<Id, (Row, Id)>`, returning the rejected row and blocking ID on a unique conflict |
| `get(id)` | `Option<&Row>` |
| `contains(id)` | Whether the ID currently exists |
| `replace(id, row)` | `Result<Row, ReplaceError<Row>>`, returning the old row |
| `remove(id)` | `Option<Row>` |
| `iter()` | Iterator of `(Id, &Row)` |
| `get_by_<field>(&key)` | `Option<(Id, &Row)>` for a unique index |
| `iter_by_<field>(&key)` | Iterator of `(Id, &Row)` for a many index |

Noncached indexed fields require `Eq + Hash`; cached indexed fields also require
`Clone`. Non-indexed fields have no additional bounds. Lookups support borrowed
keys through `Borrow`, such as a `str` lookup against a `String` field. Row access
is immutable; change rows through `replace` in mutable tables.

## Append-only tables

Use `#[microtable::table(append_only)]` or `append_only = true` when rows must
not be removed or replaced. These tables retain `insert`, read methods, full
and indexed iteration, `new`, `default`, `len`, and `is_empty`; `remove` and
`replace` are not generated. Index cache options and defaults and custom IDs
work as in mutable tables.

Append-only rows use a dense `Vec`, with no vacant slots or free list. Each many
index stores one plain ID successor per row in a circular group, with a stable
representative row ID. There are no per-row cached hashes. Group iteration
visits each matching row once; all iteration order is unspecified.

## Enum tables

Every enum variant must be a single-field tuple variant, `Variant(Payload)`.
Unit variants, record variants, and tuple variants with multiple fields are rejected.

```rust
#[derive(Debug)]
struct Person { email: String }
#[derive(Debug)]
struct Device { kind: String }

#[microtable::table]
#[derive(Debug)]
enum Entry {
    #[index(name = email, unique, key_type = String,
            key = |person| person.email.clone())]
    Person(Person),
    #[index(name = kind, many, key_type = String,
            key = |device| device.kind.clone(), cached = false)]
    Device(Device),
}

let mut entries = EntryTable::new();
let id = entries.insert(Entry::Person(Person { email: "ada@example.com".into() })).unwrap();
entries.insert(Entry::Device(Device { kind: "sensor".into() })).unwrap();
assert_eq!(entries.get_person_by_email("ada@example.com").unwrap().0, id);
assert_eq!(entries.iter_device_by_kind("sensor").count(), 1);
```

Only named computed `#[index(name = ..., unique|many, key_type = Type,
key = function_or_closure, cached = ...)]` indexes are supported on variants.
The extractor is a function path or non-capturing closure of type
`fn(&Payload) -> Key`, not a function of the enum. Computed indexes default to
`cached = true`; `cached = false` is also supported. Payload indexes are not
inherited. Field indexes, `fields(...)`, and enum-level index attributes are
rejected.

Index names must be distinct within a variant, but different variants can use
the same name. Lookup methods include the snake_case variant name, such as
`get_person_by_email` and `iter_device_by_kind`. They return `(Id, &Entry)`
pairs through an `Option` or iterator, as in struct tables, not payload references.
All variants share one arena and ID domain. Unique constraints apply only within
the indexed variant. In mutable tables, `replace` can change a row's variant;
constraints are checked before the table changes. `append_only` also works for
enum tables and omits `replace` and `remove`.

Generated row metadata is an enum: it stores index links and cached hashes only
for the active variant, not for every variant.

## Named indexes

Declare named indexes on a struct to combine fields or compute keys:

```rust
#[microtable::table]
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
For structs, names must be distinct from every other index name, including field indexes.
`fields(...)` accepts one or more distinct field names. Composite lookups take
one borrowed argument per field, in the declared order.

Field and composite indexes default to noncached storage; computed indexes
default to cached storage. Both `#[indexed(unique, ...)]` /
`#[indexed(many, ...)]` and `#[index(...)]` accept `cached` (equivalent to
`cached = true`) or `cached = false`.

Computed indexes require an explicit `key_type` and a `key` function path or
non-capturing closure, coerced to `fn(&Row) -> Key`. The result needs only
`Eq + Hash`, not `Clone`, even when cached: the key is moved into the map. Source
fields need no additional bounds. Cached keys are computed once per attempted
write that reaches validation and stored in the index map, not the row wrapper.
Lookup, removal, and rehash do not rerun cached computations. Noncached computed
keys are recomputed from stored rows for equality checks, lookups, and rehashing;
these functions must be deterministic and must not panic. Keys may borrow data
with an existing row lifetime, but cannot borrow from the row's owned fields.
A computed tuple key is queried using a reference to that tuple.

Insertion returns `Err((rejected_row, blocking_id))` for the first unique index
conflict in validation order. The rejected row is returned to the caller, not
dropped automatically. The blocking ID identifies the existing row. Unique
conflicts leave the table and all indexes unchanged.
Field indexes are validated in field order, then named indexes in declaration
order. Enum indexes are validated in declaration order within the active variant.

Structural capacity checks panic with `table capacity exhausted` after uniqueness
checks and before table mutation. This means real storage or slot-index-space
exhaustion, not a narrow-ID limit. Allocation failures can panic or abort; they
are not returned as errors.

Replacement errors expose the rejected `row` and a `kind`:
`ReplaceErrorKind::Unique { field }` or `ReplaceErrorKind::NotFound`.
For a named index, `field` contains the index name. Every returned replacement
error leaves the table unchanged. Replacements can keep their own unique values.
Using `unwrap()` on an insert result requires both `Row: Debug` and `Id: Debug`.
Using `unwrap()` on a replacement result requires `Row: Debug`.

## Hash builder

Tables use `std::collections::hash_map::RandomState` by default. Select a different
hash builder by type:

```rust
#[microtable::table(
    hasher = std::hash::BuildHasherDefault<std::collections::hash_map::DefaultHasher>
)]
struct Record {
    #[indexed(unique)]
    key: String,
}
```

The type must implement `std::hash::BuildHasher + Clone + Default`. The table
creates one shared builder with `Default` and clones it into cached index maps.
Clones must produce equivalent hashes for the same keys. All noncached hashing,
queries, and rehashing use the chosen builder too. This option works for structs,
enums, mutable tables, and append-only tables. It accepts only a type, not an
initializer; `new()` and `default()` remain the constructors.

The default builder protects against hash collision attacks. Deterministic or
constant-hash test builders are not recommended for hostile input.

Indexes use `hashbrown`. Noncached indexes use `HashTable<Id>` and derive keys
from rows. Cached indexes use
`HashMap` and own their keys. Each cached key includes its representative row ID
as metadata that does not affect hashing or equality. Each entry holds one row
ID for a unique index or a representative ID for a many index. In mutable
tables, per-row cached hashes support detaching cached index entries. Rows are
detached from indexes before replacement or removal.

In mutable tables, each many index uses a doubly linked list with previous/next
IDs in a private row wrapper, with independent links for each many index. There is no vector or
set of IDs allocated for each value. Insert, remove, replace, and unique lookup
take expected constant time per index, excluding key computation, hashing,
equality, and cloning costs. A many lookup takes expected constant time to find
the head, then constant time per matching row. Full-table iteration scans
allocated slots, including holes. Iteration order is unspecified.

Mutable rows occupy reusable slots maintained by a free list. Removing one row
never shifts another row's ID. Occupancy is stored in a `bitvec` bitmap, using one bit
per allocated slot. A slot is a union of the row and a one-word free-list link,
so vacant slots do not reserve additional link space alongside a row. Unsafe
union access is confined to the slot-storage module, which owns occupancy
tracking and drops each live row exactly once, including cleanup after a row
destructor panics.

In mutable tables, an old ID can refer to a replacement row after slot reuse.

IDs are local to a table instance: they carry no table-instance identifier and
must not be used to address a different instance. Index keys must retain stable
hash/equality behavior while stored, including through interior mutability.
User-defined hashing, equality, and cloning should not panic; table consistency
after a panic in those operations is not guaranteed.

The API supports named-field structs with field, composite, and computed indexes,
and single-field tuple enum variants with named computed indexes.
Conditionally compiled fields are rejected. Serialization and mutable row
references are outside the current API.

Run the checks with `cargo test --workspace`, `cargo fmt --all --check`, and
`cargo clippy --workspace --all-targets -- -D warnings`.

The arena tests also pass with
`MIRIFLAGS=-Zmiri-tree-borrows cargo +nightly miri test -p microtable --lib`.
With `bitvec` 1.1.1, default Miri reports a Stacked Borrows violation inside
`BitVec::push`, independently reproducible without an arena. Tree Borrows passes
the arena tests, but `bitvec` still emits an integer-to-pointer provenance
warning, which limits Miri's validation coverage.
