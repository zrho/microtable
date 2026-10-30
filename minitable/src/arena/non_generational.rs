//! Stable reusable slots without generation storage or generation checks.

use super::slots::Slots;
use crate::__private::RowId;
use std::marker::PhantomData;

pub struct Arena<T, Id> {
    slots: Slots<T>,
    marker: PhantomData<Id>,
}

impl<T, Id> Default for Arena<T, Id> {
    fn default() -> Self {
        Self {
            slots: Slots::default(),
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

    pub fn can_insert(&self) -> bool {
        Id::from_parts(self.slots.next_index(), 0).is_some()
    }

    pub fn insert(&mut self, value: T) -> Result<Id, T> {
        let Some(id) = Id::from_parts(self.slots.next_index(), 0) else {
            return Err(value);
        };
        self.slots.insert(value);
        Ok(id)
    }

    pub fn get(&self, id: Id) -> Option<&T> {
        self.slots.get(id.index())
    }
    pub fn get_mut(&mut self, id: Id) -> Option<&mut T> {
        self.slots.get_mut(id.index())
    }
    pub fn remove(&mut self, id: Id) -> Option<T> {
        self.slots.remove(id.index(), true)
    }

    pub fn iter(&self) -> impl Iterator<Item = (Id, &T)> {
        self.slots.iter().map(|(index, value)| {
            (
                Id::from_parts(index, 0).expect("valid occupied slot"),
                value,
            )
        })
    }
}
