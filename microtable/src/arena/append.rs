//! Dense storage for rows that are never removed or replaced.

use super::ArenaId;
use std::marker::PhantomData;

pub struct AppendArena<T, Id> {
    values: Vec<T>,
    id: PhantomData<Id>,
}

impl<T: Clone, Id> Clone for AppendArena<T, Id> {
    fn clone(&self) -> Self {
        Self {
            values: self.values.clone(),
            id: PhantomData,
        }
    }
}

impl<T, Id: ArenaId> Default for AppendArena<T, Id> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, Id: ArenaId> AppendArena<T, Id> {
    pub fn new() -> Self {
        Self {
            values: Vec::new(),
            id: PhantomData,
        }
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    // Structural capacity only; allocation failure is not recoverable here.
    pub fn can_insert(&self) -> bool {
        super::vec_can_grow::<T>(self.values.len())
    }

    pub fn next_id(&self) -> Id {
        Id::from_index(self.values.len())
    }

    pub fn reserve(&mut self, additional: usize) {
        let required = self
            .values
            .len()
            .checked_add(additional)
            .expect("table capacity exhausted");
        assert!(
            super::vec_can_hold::<T>(required),
            "table capacity exhausted"
        );
        super::check_id_capacity::<Id>(required);
        self.values.reserve(additional);
    }

    // Generated code checks can_insert, calls next_id once, then reserves before
    // building the row's cyclic links. Push must not convert the ID again.
    pub fn push(&mut self, value: T) {
        assert!(self.can_insert(), "table capacity exhausted");
        self.values.push(value);
    }

    pub fn get(&self, id: Id) -> Option<&T> {
        self.values.get(id.into_index())
    }

    pub fn get_mut(&mut self, id: Id) -> Option<&mut T> {
        self.values.get_mut(id.into_index())
    }

    pub fn iter(&self) -> impl Iterator<Item = (Id, &T)> {
        self.values
            .iter()
            .enumerate()
            .map(|(index, value)| (Id::from_index(index), value))
    }
}
