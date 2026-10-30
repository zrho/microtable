//! Tables generated from named-field structs by [`macro@table`].
//!
//! ```
//! #[minitable::table(index_type = u32)]
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
//! #[minitable::table]
//! #[index(name = tenant_name, unique, fields(tenant, name))]
//! #[index(name = normalized, many, key_type = String,
//!         key = |row| row.name.to_ascii_lowercase())]
//! #[derive(Debug)]
//! struct User { tenant: u32, name: String }
//! let mut users = UserTable::new();
//! let id = users.insert(User { tenant: 1, name: "Ada".into() }).unwrap();
//! assert_eq!(users.get_by_tenant_name(&1, "Ada").unwrap().0, id);
//! assert_eq!(users.iter_by_normalized("ada").next().unwrap().0, id);
//! ```
#![deny(unsafe_code)]

mod arena;

pub use minitable_macros::table;

/// Why an insertion failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertErrorKind {
    Unique { field: &'static str },
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
            InsertErrorKind::CapacityExhausted => f.write_str("row ID capacity exhausted"),
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
    pub use crate::arena::{
        generational::Arena as GenerationalArena, non_generational::Arena as NonGenerationalArena,
    };
    pub use indexmap::IndexMap;
    pub use indexmap::map::{RawEntryApiV1, raw_entry_v1::RawEntryMut};

    pub trait RowId: Copy + Eq {
        fn from_parts(index: usize, generation: u64) -> Option<Self>;
        fn index(self) -> usize;
        fn generation(self) -> u64;
    }

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
