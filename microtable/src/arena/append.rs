//! Dense storage for rows that are never removed or replaced.

pub struct AppendArena<T, Id> {
    values: Vec<T>,
    from_index: fn(usize) -> Option<Id>,
    to_index: fn(Id) -> Option<usize>,
}

impl<T, Id: Copy> AppendArena<T, Id> {
    pub fn new(from_index: fn(usize) -> Option<Id>, to_index: fn(Id) -> Option<usize>) -> Self {
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

    pub fn next_id(&self) -> Option<Id> {
        (self.from_index)(self.values.len())
    }

    pub fn reserve(&mut self, additional: usize) {
        self.values.reserve(additional);
    }

    // Generated code checks next_id before building the row's cyclic links.
    // Pushing must not call the ID conversion again.
    pub fn push(&mut self, value: T) {
        self.values.push(value);
    }

    pub fn get(&self, id: Id) -> Option<&T> {
        let index = (self.to_index)(id)?;
        self.values.get(index)
    }

    pub fn get_mut(&mut self, id: Id) -> Option<&mut T> {
        let index = (self.to_index)(id)?;
        self.values.get_mut(index)
    }

    pub fn iter(&self) -> impl Iterator<Item = (Id, &T)> {
        self.values.iter().enumerate().map(|(index, value)| {
            (
                (self.from_index)(index).expect("valid occupied slot"),
                value,
            )
        })
    }
}
