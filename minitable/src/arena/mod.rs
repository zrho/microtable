//! Stable union-backed slots with generation tracking selected at compile time.

use crate::__private::RowId;
use slots::Slots;
use std::marker::PhantomData;

// The union's active variant is controlled exclusively by Slots' occupancy bits.
// All unsafe code is confined to this module; Arena uses its safe interface.
#[allow(unsafe_code)]
mod slots;

pub struct Arena<T, Id, const GENERATIONAL: bool> {
    slots: Slots<T>,
    // Always empty, with zero capacity, when GENERATIONAL is false.
    generations: Vec<u64>,
    marker: PhantomData<Id>,
}

impl<T, Id, const GENERATIONAL: bool> Default for Arena<T, Id, GENERATIONAL> {
    fn default() -> Self {
        Self {
            slots: Slots::default(),
            generations: Vec::new(),
            marker: PhantomData,
        }
    }
}

impl<T, Id: RowId, const GENERATIONAL: bool> Arena<T, Id, GENERATIONAL> {
    pub fn len(&self) -> usize {
        self.slots.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn generation(&self, index: usize) -> u64 {
        if GENERATIONAL {
            self.generations.get(index).copied().unwrap_or(0)
        } else {
            0
        }
    }

    fn next_id(&self) -> Option<Id> {
        let index = self.slots.next_index();
        Id::from_parts(index, self.generation(index))
    }

    pub fn can_insert(&self) -> bool {
        self.next_id().is_some()
    }

    pub fn insert(&mut self, value: T) -> Result<Id, T> {
        let Some(id) = self.next_id() else {
            return Err(value);
        };
        let appended = GENERATIONAL && self.slots.next_index() == self.generations.len();
        if appended {
            self.generations.reserve(1);
        }
        self.slots.insert(value);
        // Reserve before mutating slot storage so push cannot allocate or panic
        // after the value becomes occupied. Non-generational arenas skip both.
        if appended {
            self.generations.push(0);
        }
        Ok(id)
    }

    pub fn get(&self, id: Id) -> Option<&T> {
        let index = id.index();
        if GENERATIONAL && *self.generations.get(index)? != id.generation() {
            return None;
        }
        self.slots.get(index)
    }

    pub fn get_mut(&mut self, id: Id) -> Option<&mut T> {
        let index = id.index();
        if GENERATIONAL && *self.generations.get(index)? != id.generation() {
            return None;
        }
        self.slots.get_mut(index)
    }

    pub fn remove(&mut self, id: Id) -> Option<T> {
        let index = id.index();
        let generation = if GENERATIONAL {
            let generation = *self.generations.get(index)?;
            if generation != id.generation() {
                return None;
            }
            generation
        } else {
            0
        };
        let reusable = !GENERATIONAL || generation < u64::MAX;
        let value = self.slots.remove(index, reusable)?;
        // Retire exhausted generational slots instead of allowing generation wrap.
        if GENERATIONAL && reusable {
            self.generations[index] = generation + 1;
        }
        Some(value)
    }

    pub fn iter(&self) -> impl Iterator<Item = (Id, &T)> {
        self.slots.iter().map(|(index, value)| {
            (
                Id::from_parts(index, self.generation(index)).expect("valid occupied slot"),
                value,
            )
        })
    }
}

#[cfg(test)]
mod tests;
