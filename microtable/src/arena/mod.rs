//! Stable row storage with statically dispatched ID conversions.

pub use append::AppendArena;

mod append;

use slots::Slots;
use std::marker::PhantomData;

pub trait ArenaId: Copy {
    fn from_index(index: usize) -> Self;
    fn into_index(self) -> usize;
}

impl ArenaId for usize {
    fn from_index(index: usize) -> Self {
        index
    }

    fn into_index(self) -> usize {
        self
    }
}

// The union's active variant is controlled exclusively by Slots' occupancy bits.
// All unsafe code is confined to this module; Arena uses its safe interface.
#[allow(unsafe_code)]
mod slots;

pub struct Arena<T, Id> {
    slots: Slots<T>,
    id: PhantomData<Id>,
}

impl<T: Clone, Id> Clone for Arena<T, Id> {
    fn clone(&self) -> Self {
        Self {
            slots: self.slots.clone(),
            id: PhantomData,
        }
    }
}

impl<T, Id: ArenaId> Default for Arena<T, Id> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, Id: ArenaId> Arena<T, Id> {
    pub fn new() -> Self {
        Self {
            slots: Slots::default(),
            id: PhantomData,
        }
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn next_id(&self) -> Id {
        Id::from_index(self.slots.next_index())
    }

    // Structural capacity only; allocation failure is not recoverable here.
    pub fn can_insert(&self) -> bool {
        self.slots.can_insert()
    }

    pub fn reserve(&mut self, additional: usize) {
        check_id_capacity::<Id>(self.slots.reservation_len(additional));
        self.slots.reserve(additional);
    }

    pub fn insert(&mut self, value: T) -> Id {
        assert!(self.can_insert(), "table capacity exhausted");
        let id = self.next_id();
        self.slots.insert(value);
        id
    }

    pub fn get(&self, id: Id) -> Option<&T> {
        self.slots.get(id.into_index())
    }

    pub fn get_mut(&mut self, id: Id) -> Option<&mut T> {
        self.slots.get_mut(id.into_index())
    }

    pub fn remove(&mut self, id: Id) -> Option<T> {
        self.slots.remove(id.into_index())
    }

    pub fn iter(&self) -> impl Iterator<Item = (Id, &T)> {
        self.slots
            .iter()
            .map(|(index, value)| (Id::from_index(index), value))
    }
}

// Vec allocations must fit in isize::MAX bytes. ZSTs only limit length.
fn vec_can_grow<T>(len: usize) -> bool {
    let Some(next_len) = len.checked_add(1) else {
        return false;
    };
    vec_can_hold::<T>(next_len)
}

fn vec_can_hold<T>(len: usize) -> bool {
    let size = std::mem::size_of::<T>();
    size == 0 || len <= isize::MAX as usize / size
}

// Validate the highest required slot without allocating or consuming an ID.
fn check_id_capacity<Id: ArenaId>(len: usize) {
    if let Some(index) = len.checked_sub(1) {
        let _ = Id::from_index(index);
    }
}

#[cfg(test)]
mod tests;
