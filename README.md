# microtable

Rust tables with stable row IDs and hash-based indexes. Declare an ordinary row
type, then use `microtable::table!` to select its indexes and storage options.

## A contact table

```rust
#[derive(Debug, Clone)]
pub struct Person {
    name: String,
    email: String,
    team: String,
}

microtable::table! {
    #[derive(Debug, Clone)]
    pub struct People([Person]);

    #[id]
    pub struct PersonId(u32);

    #[index(unique)]
    fn email(person: &Person) -> &str {
        &person.email
    }

    #[index(many)]
    fn team(person: &Person) -> &str {
        &person.team
    }
}

let mut people = People::new();
let id = people.insert(Person {
    name: "Ada".into(),
    email: "ada@example.com".into(),
    team: "engineering".into(),
}).unwrap();

assert_eq!(people.get_by_email("ada@example.com").unwrap().0, id);
for (_, person) in people.iter_by_team("engineering") {
    println!("{}", person.name);
}
```

`unique` generates `get_by_<name>`, returning `Option<(Id, &Row)>`. `many`
generates `iter_by_<name>`, returning an iterator of `(Id, &Row)`.
Indexes are noncached by default and can borrow directly from the row.

Rows are immutable through the table. Use `replace` to update all indexes while
keeping the ID:

```rust
let mut updated = people.get(id).unwrap().clone();
updated.team = "research".into();
let previous = people.replace(id, updated).unwrap();
assert_eq!(previous.team, "engineering");
let ada = people.remove(id).unwrap();
```

Unique conflicts leave the table unchanged. `insert` returns the rejected row
and blocking ID as `Err((row, id))`; `replace` returns a `ReplaceError` containing
the rejected row and a `Unique { field }` or `NotFound` reason.

Tables also provide `default`, `len`, `is_empty`, `contains`, and `iter`.
`next_id` previews the next insertion ID without reserving it. Iteration order is
unspecified.

## Computed and composite keys

A different table can index the same row type differently:

```rust
microtable::table! {
    struct Directory([Person]);

    #[index(unique, cached)]
    fn normalized_email(person: &Person) -> String {
        person.email.to_ascii_lowercase()
    }

    #[index(unique)]
    fn team_name(person: &Person) -> (&str, &str) {
        (&person.team, &person.name)
    }
}

let mut directory = Directory::new();
directory.insert(ada).unwrap();
assert!(directory.get_by_normalized_email("ada@example.com").is_some());
assert!(directory.get_by_team_name(&("research", "Ada")).is_some());
```

`cached` stores the extracted key, so reads, removal, and rehashing do not rerun
its computation. Cached keys cannot borrow from the temporary row reference;
owned keys such as `String` work. Keys need `Eq + Hash`, but not `Clone` unless
needed to clone a cached table. Tuple lookups take one tuple reference.

## Optional indexes and enums

`optional` lets an extractor return `None` to exclude a row from an index. This
works with structs and with any enum shape:

```rust
#[derive(Debug)]
enum Task {
    Pending { assignee: String },
    Completed,
}

microtable::table! {
    struct Tasks([Task]);

    #[index(many, optional)]
    fn assignee(task: &Task) -> Option<&str> {
        match task {
            Task::Pending { assignee } => Some(assignee.as_str()),
            Task::Completed => None,
        }
    }
}

let mut tasks = Tasks::new();
tasks.insert(Task::Pending { assignee: "Ada".into() }).unwrap();
tasks.insert(Task::Completed).unwrap();
assert_eq!(tasks.iter_by_assignee("Ada").count(), 1);
```

Combine `optional` with `unique`, `many`, or `cached`. Without `optional`, an
`Option` return is an ordinary key: `None` participates in indexing and uniqueness.
Enum indexes inspect the whole row; uniqueness is not limited to one variant.

## IDs

Without an `#[id]` declaration, IDs are `usize`. Generated IDs accept `u8`, `u16`,
`u32`, or `usize` backing types no wider than the target's pointer width. They
store the slot index plus one in a nonzero integer, so `Option<PersonId>` needs
no extra space.

- `PersonId::new(index)` constructs an ID or panics on overflow.
- `PersonId::try_new(index)` returns `Option<PersonId>`.
- `id.index()` and `usize::from(id)` return the zero-based index.
- `PersonId::try_from(index)` provides fallible conversion from `usize`.

Generated IDs implement `Debug`, `Copy`, `Clone`, equality, ordering, and hashing.
A `u8` ID permits 255 slots; capacity exhaustion panics before insertion changes
the table. IDs are local to a table instance. Removed slots are reused, so an old
ID can later refer to a different row.

## Generics and table options

Table parameters and bounds are shared by their extractors:

```rust
microtable::table! {
    struct Values<T>([T]) where T: Eq + std::hash::Hash;

    #[index(unique)]
    fn value(row: &T) -> &T { row }
}

let values = Values::<String>::new();
assert!(values.is_empty());
```

Lifetimes, const parameters, and external borrowed data are supported too.

Use `#[table(...)]` for append-only storage or a custom hash builder:

```rust
type HistoryHasher = std::hash::BuildHasherDefault<
    std::collections::hash_map::DefaultHasher,
>;

microtable::table! {
    #[table(append_only, hasher = HistoryHasher)]
    struct ContactHistory([Person]);

    #[index(many)]
    fn email(person: &Person) -> &str { &person.email }
}
```

`append_only` uses dense storage and omits `remove` and `replace`. Hash builders
must implement `BuildHasher + Clone + Default`; clones must produce equivalent
hashes. The default is `RandomState`, which protects against hash collision
attacks. Select custom hashers with care for untrusted input.

Noncached extractors must be deterministic. Stored keys must keep stable hash
and equality behavior, including through interior mutability. Consistency after
a panic in user-defined extraction, hashing, or equality is not guaranteed.
Conditional attributes on individual extractors are not supported.

## Development

```sh
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```
