//! Dense storage for rows that are never removed or replaced.

pub struct AppendArena<T, Id> {
    values: Vec<T>,
    from_index: fn(usize) -> Id,
    to_index: fn(Id) -> usize,
}

impl<T: Clone, Id> Clone for AppendArena<T, Id> {
    fn clone(&self) -> Self {
        Self {
            values: self.values.clone(),
            from_index: self.from_index,
            to_index: self.to_index,
        }
    }
}

impl<T, Id: Copy> AppendArena<T, Id> {
    pub fn new(from_index: fn(usize) -> Id, to_index: fn(Id) -> usize) -> Self {
        Self {
            values: Vec::new(),
            from_index,
            to_index,
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
        (self.from_index)(self.values.len())
    }

    pub fn reserve(&mut self, additional: usize) {
        self.values.reserve(additional);
    }

    // Generated code checks can_insert, calls next_id once, then reserves before
    // building the row's cyclic links. Push must not convert the ID again.
    pub fn push(&mut self, value: T) {
        assert!(self.can_insert(), "table capacity exhausted");
        self.values.push(value);
    }

    pub fn get(&self, id: Id) -> Option<&T> {
        self.values.get((self.to_index)(id))
    }

    pub fn get_mut(&mut self, id: Id) -> Option<&mut T> {
        self.values.get_mut((self.to_index)(id))
    }

    pub fn iter(&self) -> impl Iterator<Item = (Id, &T)> {
        self.values
            .iter()
            .enumerate()
            .map(|(index, value)| ((self.from_index)(index), value))
    }
}
