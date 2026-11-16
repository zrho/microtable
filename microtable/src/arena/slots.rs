//! Compact shared storage. Invariants:
//! - `slots.len() == occupied.len()` and `len` is the number of set bits.
//! - A set bit means `value` is initialized and exclusively owned by this storage.
//! - Outside destruction, a clear bit means `next_free` is initialized,
//!   with no live `T` in that slot.
//! - Free-list links point only to vacant slots, with no cycles.
//!
//! No caller can access the union or mutate the occupancy bits directly.

use bitvec::{slice::BitSlice, vec::BitVec};
use std::{mem::ManuallyDrop, num::NonZeroUsize};

// Encoding index + 1 allows a free-list link to occupy a single machine word.
type Link = Option<NonZeroUsize>;

union Slot<T> {
    value: ManuallyDrop<T>,
    next_free: Link,
}

pub(super) struct Slots<T> {
    slots: Vec<Slot<T>>,
    occupied: BitVec,
    free: Link,
    len: usize,
}

impl<T> Default for Slots<T> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            occupied: BitVec::new(),
            free: None,
            len: 0,
        }
    }
}

impl<T> Slots<T> {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn next_index(&self) -> usize {
        self.free.map_or(self.slots.len(), |link| link.get() - 1)
    }

    fn can_insert_at(slots_len: usize, len: usize, has_free: bool) -> bool {
        if len.checked_add(1).is_none() {
            return false;
        }
        has_free
            || (super::vec_can_grow::<Slot<T>>(slots_len)
                // Every slot index must support the nonzero index + 1 encoding.
                && slots_len
                    .checked_add(1)
                    .is_some_and(|next_len| next_len <= BitSlice::<usize>::MAX_BITS))
    }

    pub fn can_insert(&self) -> bool {
        Self::can_insert_at(self.slots.len(), self.len, self.free.is_some())
    }

    pub fn insert(&mut self, value: T) -> usize {
        assert!(self.can_insert(), "table capacity exhausted");
        let index = self.next_index();
        if self.free.is_some() {
            // SAFETY: The free-list head is vacant and has initialized next_free.
            self.free = unsafe { self.slots[index].next_free };
            self.slots[index].value = ManuallyDrop::new(value);
            self.occupied.set(index, true);
        } else {
            // Both reservations (including bitvec's length-limit check) finish
            // before moving value into a union that needs manual destruction.
            self.slots.reserve(1);
            self.occupied.reserve(1);
            self.slots.push(Slot {
                value: ManuallyDrop::new(value),
            });
            self.occupied.push(true);
        }
        self.len += 1;
        index
    }

    pub fn get(&self, index: usize) -> Option<&T> {
        if !self.occupied.get(index).is_some_and(|bit| *bit) {
            return None;
        }
        // SAFETY: The set occupancy bit proves value is initialized. The shared
        // borrow of self prevents mutation or removal for this reference's life.
        Some(unsafe { &self.slots[index].value })
    }

    pub fn get_mut(&mut self, index: usize) -> Option<&mut T> {
        if !self.occupied.get(index).is_some_and(|bit| *bit) {
            return None;
        }
        // SAFETY: The bit proves value is initialized; &mut self ensures exclusivity.
        Some(unsafe { &mut self.slots[index].value })
    }

    pub fn remove(&mut self, index: usize) -> Option<T> {
        if !self.occupied.get(index).is_some_and(|bit| *bit) {
            return None;
        }
        // Compute the link before taking ownership, so no potential panic can
        // leave an occupied bit marking a value that has already been moved out.
        let link = NonZeroUsize::new(index.checked_add(1).expect("valid slot index"));
        self.occupied.set(index, false);
        // SAFETY: This was an occupied slot. Clear the bit before moving T out,
        // and immediately initialize next_free. No user code runs in between.
        let value = unsafe { ManuallyDrop::take(&mut self.slots[index].value) };
        self.slots[index].next_free = self.free;
        self.free = link;
        self.len -= 1;
        Some(value)
    }

    pub fn iter(&self) -> impl Iterator<Item = (usize, &T)> {
        self.occupied.iter_ones().map(|index| {
            // SAFETY: iter_ones yields only occupied indices. Matching lengths
            // ensure the slot exists, and &self prevents changing its state.
            (index, unsafe { &*self.slots[index].value })
        })
    }
}

struct DropRemaining<'a, T> {
    slots: &'a mut [Slot<T>],
    occupied: &'a mut BitVec,
    next: usize,
}

impl<T> DropRemaining<'_, T> {
    fn drop_values(&mut self) {
        for index in self.next..self.slots.len() {
            self.next = index + 1;
            if self.occupied[index] {
                self.occupied.set(index, false);
                // SAFETY: Only occupied slots own T. Clear the bit and advance
                // next first, so unwinding can never attempt to drop T twice.
                unsafe {
                    ManuallyDrop::drop(&mut self.slots[index].value);
                }
            }
        }
    }
}

impl<T> Drop for DropRemaining<'_, T> {
    fn drop(&mut self) {
        self.drop_values();
    }
}

impl<T> Drop for Slots<T> {
    fn drop(&mut self) {
        let mut guard = DropRemaining {
            slots: &mut self.slots,
            occupied: &mut self.occupied,
            next: 0,
        };
        // If a destructor panics, guard's Drop continues from the following slot.
        guard.drop_values();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{align_of, size_of};

    #[test]
    fn slot_overlays_value_and_single_word_link() {
        assert_eq!(size_of::<Slot<u8>>(), size_of::<usize>());
        assert_eq!(size_of::<Slot<[u8; 64]>>(), 64);
        #[repr(align(64))]
        struct Aligned;
        assert_eq!(size_of::<Slot<Aligned>>(), 64);
        assert_eq!(align_of::<Slot<Aligned>>(), 64);

        let slot_limit = isize::MAX as usize / size_of::<Slot<u8>>();
        let limit = slot_limit.min(BitSlice::<usize>::MAX_BITS);
        assert!(Slots::<u8>::can_insert_at(limit - 1, limit - 1, false));
        assert!(!Slots::<u8>::can_insert_at(limit, limit, false));
        assert!(Slots::<u8>::can_insert_at(limit, limit - 1, true));
        assert!(!Slots::<u8>::can_insert_at(usize::MAX, 0, false));
        assert!(!Slots::<u8>::can_insert_at(0, usize::MAX, true));
        assert!(super::super::vec_can_grow::<()>(usize::MAX - 1));
        assert!(!super::super::vec_can_grow::<()>(usize::MAX));
        assert!(super::super::vec_can_grow::<u8>(isize::MAX as usize - 1));
        assert!(!super::super::vec_can_grow::<u8>(isize::MAX as usize));
    }
}
