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
    from_index: fn(usize) -> Option<Id>,
    to_index: fn(Id) -> Option<usize>,
}

impl<T, Id: Copy> Arena<T, Id> {
    pub fn new(from_index: fn(usize) -> Option<Id>, to_index: fn(Id) -> Option<usize>) -> Self {
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

    fn next_id(&self) -> Option<Id> {
        (self.from_index)(self.slots.next_index())
    }

    pub fn can_insert(&self) -> bool {
        self.next_id().is_some()
    }

    pub fn insert(&mut self, value: T) -> Result<Id, T> {
        let Some(id) = self.next_id() else {
            return Err(value);
        };
        self.slots.insert(value);
        Ok(id)
    }

    pub fn get(&self, id: Id) -> Option<&T> {
        let index = (self.to_index)(id)?;
        self.slots.get(index)
    }

    pub fn get_mut(&mut self, id: Id) -> Option<&mut T> {
        let index = (self.to_index)(id)?;
        self.slots.get_mut(index)
    }

    pub fn remove(&mut self, id: Id) -> Option<T> {
        let index = (self.to_index)(id)?;
        self.slots.remove(index)
    }

    pub fn iter(&self) -> impl Iterator<Item = (Id, &T)> {
        self.slots.iter().map(|(index, value)| {
            (
                (self.from_index)(index).expect("valid occupied slot"),
                value,
            )
        })
    }
}

#[cfg(test)]
mod tests;
