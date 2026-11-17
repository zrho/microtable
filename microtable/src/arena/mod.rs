//! Stable row storage with caller-provided ID conversions.

pub use append::AppendArena;

mod append;

use slots::Slots;

// The union's active variant is controlled exclusively by Slots' occupancy bits.
// All unsafe code is confined to this module; Arena uses its safe interface.
#[allow(unsafe_code)]
mod slots;

pub struct Arena<T, Id> {
    slots: Slots<T>,
    from_index: fn(usize) -> Id,
    to_index: fn(Id) -> usize,
}

impl<T: Clone, Id> Clone for Arena<T, Id> {
    fn clone(&self) -> Self {
        Self {
            slots: self.slots.clone(),
            from_index: self.from_index,
            to_index: self.to_index,
        }
    }
}

impl<T, Id: Copy> Arena<T, Id> {
    pub fn new(from_index: fn(usize) -> Id, to_index: fn(Id) -> usize) -> Self {
        Self {
            slots: Slots::default(),
            from_index,
            to_index,
        }
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn next_id(&self) -> Id {
        (self.from_index)(self.slots.next_index())
    }

    // Structural capacity only; allocation failure is not recoverable here.
    pub fn can_insert(&self) -> bool {
        self.slots.can_insert()
    }

    pub fn insert(&mut self, value: T) -> Id {
        assert!(self.can_insert(), "table capacity exhausted");
        let id = self.next_id();
        self.slots.insert(value);
        id
    }

    pub fn get(&self, id: Id) -> Option<&T> {
        self.slots.get((self.to_index)(id))
    }

    pub fn get_mut(&mut self, id: Id) -> Option<&mut T> {
        self.slots.get_mut((self.to_index)(id))
    }

    pub fn remove(&mut self, id: Id) -> Option<T> {
        self.slots.remove((self.to_index)(id))
    }

    pub fn iter(&self) -> impl Iterator<Item = (Id, &T)> {
        self.slots
            .iter()
            .map(|(index, value)| ((self.from_index)(index), value))
    }
}

// Vec allocations must fit in isize::MAX bytes. ZSTs only limit length.
fn vec_can_grow<T>(len: usize) -> bool {
    let Some(next_len) = len.checked_add(1) else {
        return false;
    };
    let size = std::mem::size_of::<T>();
    size == 0 || next_len <= isize::MAX as usize / size
}

#[cfg(test)]
mod tests;
