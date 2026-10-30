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
#![forbid(unsafe_code)]

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
    use std::marker::PhantomData;

    pub use indexmap::IndexMap;
    pub use indexmap::map::{RawEntryApiV1, raw_entry_v1::RawEntryMut};

    pub trait RowId: Copy + Eq {
        const GENERATIONAL: bool;
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

    struct Slot<T> {
        value: Option<T>,
        generation: u64,
        next_free: Option<usize>,
    }

    pub struct Arena<T, Id> {
        slots: Vec<Slot<T>>,
        free: Option<usize>,
        len: usize,
        marker: PhantomData<Id>,
    }

    impl<T, Id: RowId> Default for Arena<T, Id> {
        fn default() -> Self {
            Self {
                slots: Vec::new(),
                free: None,
                len: 0,
                marker: PhantomData,
            }
        }
    }

    impl<T, Id: RowId> Arena<T, Id> {
        pub fn len(&self) -> usize {
            self.len
        }
        pub fn is_empty(&self) -> bool {
            self.len == 0
        }

        pub fn can_insert(&self) -> bool {
            self.free.is_some() || Id::from_parts(self.slots.len(), 0).is_some()
        }

        pub fn insert(&mut self, value: T) -> Result<Id, T> {
            let index = self.free.unwrap_or(self.slots.len());
            let generation = self.free.map_or(0, |i| self.slots[i].generation);
            let Some(id) = Id::from_parts(index, generation) else {
                return Err(value);
            };
            if let Some(i) = self.free {
                let slot = &mut self.slots[i];
                self.free = slot.next_free.take();
                slot.value = Some(value);
            } else {
                self.slots.push(Slot {
                    value: Some(value),
                    generation,
                    next_free: None,
                });
            }
            self.len += 1;
            Ok(id)
        }

        pub fn get(&self, id: Id) -> Option<&T> {
            let slot = self.slots.get(id.index())?;
            if Id::GENERATIONAL && slot.generation != id.generation() {
                return None;
            }
            slot.value.as_ref()
        }

        pub fn get_mut(&mut self, id: Id) -> Option<&mut T> {
            let slot = self.slots.get_mut(id.index())?;
            if Id::GENERATIONAL && slot.generation != id.generation() {
                return None;
            }
            slot.value.as_mut()
        }

        pub fn remove(&mut self, id: Id) -> Option<T> {
            let slot = self.slots.get_mut(id.index())?;
            if Id::GENERATIONAL && slot.generation != id.generation() {
                return None;
            }
            let value = slot.value.take()?;
            self.len -= 1;
            // Retire the slot on generation exhaustion instead of reviving stale IDs.
            if !Id::GENERATIONAL || slot.generation < u64::MAX {
                if Id::GENERATIONAL {
                    slot.generation += 1;
                }
                slot.next_free = self.free;
                self.free = Some(id.index());
            }
            Some(value)
        }

        pub fn iter(&self) -> impl Iterator<Item = (Id, &T)> {
            self.slots.iter().enumerate().filter_map(|(index, slot)| {
                slot.value.as_ref().map(|value| {
                    (
                        Id::from_parts(index, slot.generation).expect("valid occupied slot"),
                        value,
                    )
                })
            })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[derive(Clone, Copy, PartialEq, Eq)]
        struct Id(usize, u64);
        impl RowId for Id {
            const GENERATIONAL: bool = true;
            fn from_parts(index: usize, generation: u64) -> Option<Self> {
                Some(Self(index, generation))
            }
            fn index(self) -> usize {
                self.0
            }
            fn generation(self) -> u64 {
                self.1
            }
        }
        #[test]
        fn generation_exhaustion_retires_slot() {
            let mut arena = Arena::<_, Id>::default();
            let first = arena.insert(1).unwrap();
            arena.slots[first.index()].generation = u64::MAX;
            let last = Id(first.index(), u64::MAX);
            assert_eq!(arena.remove(last), Some(1));
            assert_eq!(arena.get(last), None);
            let next = arena.insert(2).unwrap();
            assert_ne!(next.index(), first.index());
            assert_eq!(arena.len(), 1);
        }
    }
}
