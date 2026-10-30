//! Stable reusable slots with u64 generations, stored separately from the values
//! to avoid padding every union slot for its generation counter.

use super::slots::Slots;
use crate::__private::RowId;
use std::marker::PhantomData;

pub struct Arena<T, Id> {
    slots: Slots<T>,
    generations: Vec<u64>,
    marker: PhantomData<Id>,
}

impl<T, Id> Default for Arena<T, Id> {
    fn default() -> Self {
        Self {
            slots: Slots::default(),
            generations: Vec::new(),
            marker: PhantomData,
        }
    }
}

impl<T, Id: RowId> Arena<T, Id> {
    pub fn len(&self) -> usize {
        self.slots.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn next_id(&self) -> Option<Id> {
        let index = self.slots.next_index();
        Id::from_parts(index, self.generations.get(index).copied().unwrap_or(0))
    }

    pub fn can_insert(&self) -> bool {
        self.next_id().is_some()
    }

    pub fn insert(&mut self, value: T) -> Result<Id, T> {
        let Some(id) = self.next_id() else {
            return Err(value);
        };
        let appended = self.slots.next_index() == self.generations.len();
        if appended {
            self.generations.reserve(1);
        }
        self.slots.insert(value);
        // Capacity was reserved before mutating the slot storage, so this push
        // cannot allocate or panic after the value becomes occupied.
        if appended {
            self.generations.push(0);
        }
        Ok(id)
    }

    pub fn get(&self, id: Id) -> Option<&T> {
        let index = id.index();
        if *self.generations.get(index)? != id.generation() {
            return None;
        }
        self.slots.get(index)
    }

    pub fn get_mut(&mut self, id: Id) -> Option<&mut T> {
        let index = id.index();
        if *self.generations.get(index)? != id.generation() {
            return None;
        }
        self.slots.get_mut(index)
    }

    pub fn remove(&mut self, id: Id) -> Option<T> {
        let index = id.index();
        let generation = *self.generations.get(index)?;
        if generation != id.generation() {
            return None;
        }
        let reusable = generation < u64::MAX;
        let value = self.slots.remove(index, reusable)?;
        // Retire exhausted slots permanently instead of allowing generation wrap.
        if reusable {
            self.generations[index] = generation + 1;
        }
        Some(value)
    }

    pub fn iter(&self) -> impl Iterator<Item = (Id, &T)> {
        self.slots.iter().map(|(index, value)| {
            (
                Id::from_parts(index, self.generations[index]).expect("valid occupied slot"),
                value,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena::tests::Id;

    #[test]
    fn generation_exhaustion_retires_slot() {
        let mut arena = Arena::<_, Id>::default();
        let first = arena.insert(1).unwrap();
        arena.generations[first.index()] = u64::MAX;
        let last = Id(first.0, u64::MAX);
        assert_eq!(arena.remove(last), Some(1));
        assert_eq!(arena.get(last), None);
        assert_eq!(arena.remove(last), None);
        let next = arena.insert(2).unwrap();
        assert_ne!(next.index(), first.index());
        assert_eq!(arena.len(), 1);
        assert_eq!(
            arena.iter().map(|(_, value)| *value).collect::<Vec<_>>(),
            [2]
        );
    }
}
