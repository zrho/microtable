//! Tables generated from named-field structs or enums by [`macro@table`].
//!
//! ```
//! #[microtable::table]
//! #[derive(Debug)]
//! struct Person {
//!     #[indexed(unique)]
//!     email: String,
//!     #[indexed(many)]
//!     team: String,
//! }
//! let mut people = PersonTable::new();
//! let id = people.insert(Person { email: "a@example.com".into(), team: "red".into() }).unwrap();
//! assert_eq!(people.get_by_email("a@example.com").unwrap().0, id);
//! assert_eq!(people.iter_by_team("red").count(), 1);
//! people.remove(id);
//! assert!(people.get(id).is_none());
//! ```
//!
//! Named indexes can combine fields or compute a key:
//!
//! ```
//! #[microtable::table]
//! #[index(name = tenant_name, unique, fields(tenant, name))]
//! #[index(name = normalized, many, key_type = String, key = |row| row.name.to_ascii_lowercase())]
//! #[derive(Debug)]
//! struct User { tenant: u32, name: String }
//! let mut users = UserTable::new();
//! let id = users.insert(User { tenant: 1, name: "Ada".into() }).unwrap();
//! assert_eq!(users.get_by_tenant_name(&1, "Ada").unwrap().0, id);
//! assert_eq!(users.iter_by_normalized("ada").next().unwrap().0, id);
//! ```
//!
//! # Enum tables
//!
//! Every variant must be a single-field tuple variant, `Variant(Payload)`.
//! Unit variants, record variants, and tuple variants with multiple fields are
//! rejected. Variants support only named computed indexes declared with
//! `#[index(name = ..., unique|many, key_type = Type, key = function_or_closure,
//! cached = ...)]`. The extractor is a function path or non-capturing closure
//! of type `fn(&Payload) -> Key`, not a function of the enum. Computed indexes
//! default to `cached = true`; `cached = false` is also supported. Payload
//! indexes are not inherited. Field indexes, `fields(...)`, and enum-level
//! index attributes are rejected.
//!
//! Index names must be distinct within each variant, but can be reused in
//! different variants. Lookup methods include the snake_case variant name:
//! for example, `get_person_by_email` or `iter_device_by_kind`. Unique lookups
//! return `Option<(Id, &Enum)>`; many lookups iterate over `(Id, &Enum)` pairs,
//! not payload references.
//!
//! All variants share one arena and ID domain. Unique constraints apply only
//! within the indexed variant. Mutable `replace` can change a row's variant
//! and checks constraints before changing the table. The `append_only` option
//! also works for enums and omits `replace` and `remove`. Generated row metadata
//! is an enum that stores index links and cached hashes only for the active
//! variant, not for every variant.
//!
//! # Row IDs and storage indices
//!
//! Row IDs default to `usize`. A custom `id = Type` requires both
//! `id_from_index: fn(usize) -> Id` and `id_to_index: fn(Id) -> usize`.
//! Callbacks can be function paths or non-capturing closures. They must be
//! deterministic, nonpanicking inverses over valid slot indices. IDs require
//! `Copy + Eq`, but no numeric or `Hash` bounds. Internal index entries and row
//! links use the ID type; storage offsets, lengths, bitmap positions, and
//! free-list links use `usize`. ID types do not impose automatic capacity limits.
//!
//! # Append-only tables
//!
//! `#[microtable::table(append_only)]` generates an append-only table. It supports
//! insertion and immutable access, but does not generate `remove` or `replace`.
//! `append_only = false` is the default. Cache options and ID conversions work
//! in both modes.
//!
//! Append-only rows use dense vector storage, without a free list or occupancy
//! bitmap. Each many index uses one successor ID per row in a circular singly
//! linked list. Cached indexes do not need per-row hashes in this mode. IDs stay
//! valid for the lifetime of the table, but vector growth can move row values.
//! Iteration order is unspecified.
//!
//! # Index storage and key requirements
//!
//! Field and composite indexes default to noncached storage. Computed indexes
//! default to cached storage. Both `#[indexed(unique)]` / `#[indexed(many)]` and
//! `#[index(...)]` accept `cached`, `cached = true`, or `cached = false`. For
//! example, `#[indexed(unique, cached)]` caches a field key, while
//! `#[index(name = pair, many, fields(tenant, name), cached = false)]` derives
//! its key from stored rows.
//!
//! Noncached field and composite components require `Eq + Hash`, not `Clone`.
//! Cached components require `Clone + Eq + Hash`. A computed index declares
//! `key_type` and a `key` function path or non-capturing closure of type
//! `fn(&Row) -> Key` for structs or `fn(&Payload) -> Key` for enum variants.
//! Its result requires only `Eq + Hash`, even when cached:
//! the result is moved into the map. Source fields need no additional bounds.
//!
//! Cached computed keys are evaluated once per attempted write that reaches
//! validation. They are stored in the index map, not the row wrapper. Lookup,
//! removal, and rehash do not rerun these computations. Noncached computed keys
//! are recomputed from stored rows for equality checks, lookups, and rehashing.
//! These functions must be deterministic and must not panic. Computed keys can
//! borrow data with an existing row lifetime, but cannot borrow from the row's
//! owned fields. All stored keys must retain stable hash and equality behavior.
//!
//! Indexes use `hashbrown` with a shared `std::hash::RandomState` hasher.
//! Noncached indexes use `HashTable<Id>` and derive keys from rows. Cached
//! indexes use `HashMap` and own the key. Cached keys include a representative
//! row ID as metadata that does not affect hashing or equality. Each entry holds
//! one row ID for a unique index or a group-head ID for a many index.
//! Many-index links live in a private row wrapper. Mutable tables store per-row
//! cached hashes for detachment and detach rows before replacement or removal.
//! Iteration order is unspecified.
#![deny(unsafe_code)]

mod arena;

pub use microtable_macros::table;

/// Why an insertion failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertErrorKind {
    Unique {
        field: &'static str,
    },
    /// The storage cannot represent another row. Does not report allocation failures.
    CapacityExhausted,
}

/// An insertion failure, including ownership of the rejected row.
#[derive(Debug, PartialEq, Eq)]
pub struct InsertError<T> {
    pub kind: InsertErrorKind,
    pub row: T,
}

impl<T> std::fmt::Display for InsertError<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            InsertErrorKind::Unique { field } => {
                write!(f, "unique index `{field}` already contains this value")
            }
            InsertErrorKind::CapacityExhausted => f.write_str("table capacity exhausted"),
        }
    }
}
impl<T: std::fmt::Debug> std::error::Error for InsertError<T> {}

/// Why a replacement failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplaceErrorKind {
    NotFound,
    Unique { field: &'static str },
}

/// A replacement failure, including ownership of the rejected row.
#[derive(Debug, PartialEq, Eq)]
pub struct ReplaceError<T> {
    pub kind: ReplaceErrorKind,
    pub row: T,
}

impl<T> std::fmt::Display for ReplaceError<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            ReplaceErrorKind::NotFound => f.write_str("row ID is not present in this table"),
            ReplaceErrorKind::Unique { field } => {
                write!(f, "unique index `{field}` already contains this value")
            }
        }
    }
}
impl<T: std::fmt::Debug> std::error::Error for ReplaceError<T> {}

/// Implementation details used by generated code; not a stable public API.
#[doc(hidden)]
pub mod __private {
    pub use crate::arena::{AppendArena, Arena};
    pub use hashbrown::hash_map::RawEntryMut;
    pub use hashbrown::{HashMap, HashTable};

    // The representative ID is metadata, not part of key equality or hashing.
    // Keeping it beside the key permits removal by ID without computing the key.
    pub struct CachedKey<K, Id> {
        pub key: K,
        pub id: Id,
    }

    impl<K: std::hash::Hash, Id> std::hash::Hash for CachedKey<K, Id> {
        fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
            self.key.hash(state);
        }
    }
    impl<K: PartialEq, Id> PartialEq for CachedKey<K, Id> {
        fn eq(&self, other: &Self) -> bool {
            self.key == other.key
        }
    }
    impl<K: Eq, Id> Eq for CachedKey<K, Id> {}

    pub struct Links<Id> {
        pub prev: Option<Id>,
        pub next: Option<Id>,
    }
    impl<Id> Default for Links<Id> {
        fn default() -> Self {
            Self {
                prev: None,
                next: None,
            }
        }
    }
}
